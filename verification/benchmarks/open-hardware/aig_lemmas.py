# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded exact AIGER arithmetic components and observation-based contexts.

Small-core source ceiling:4M ANDs*8B and8M outputs*4B per side. Components
visit<=1M nodes once; a dense u32 image<=24MiB and byte marks<=6MiB. No paths
are enumerated. One component/builder at a time; expected<1GiB Python RSS.
External arithmetic/CEC children must inherit the caller's4GiB aggregate
physical/1GiB swap scope,12GiB AS and wall guard. Preparing a lemma is not proof.
"""
from array import array
from pathlib import Path
import hashlib
import json

MAX_ANDS=4_000_000
MAX_INPUTS=2_000_000
MAX_OUTPUTS=8_000_000
MAX_BYTES=256*1024**2
MAX_CONE=1_000_000


class Graph:
    def __init__(self,inputs,ands,outputs):
        self.inputs=inputs;self.ands=ands;self.outputs=outputs

    @classmethod
    def read(cls,path):
        path=Path(path)
        if path.stat().st_size>MAX_BYTES:raise ValueError('arithmetic source byte budget')
        with path.open('rb') as f:
            h=f.readline(256).split()
            if len(h)!=6 or h[0]!=b'aig':raise ValueError('expected binary combinational AIGER')
            m,i,l,o,a=map(int,h[1:])
            if l or m!=i+a or not 0<=i<=MAX_INPUTS or not 0<=a<=MAX_ANDS or not 0<o<=MAX_OUTPUTS:
                raise ValueError('arithmetic source graph/interface budget')
            outputs=array('I')
            for _ in range(o):
                line=f.readline(32)
                if not line.endswith(b'\n'):raise ValueError('AIGER output line budget')
                value=int(line)
                if not 0<=value<=2*m+1:raise ValueError('invalid output literal')
                outputs.append(value)
            def integer():
                value=0
                for shift in range(0,35,7):
                    b=f.read(1)
                    if not b or (shift==28 and b[0]>15):raise ValueError('AIGER integer overflow')
                    value|=(b[0]&127)<<shift
                    if not b[0]&128:return value
                raise ValueError('AIGER integer overflow')
            gates=array('I')
            for n in range(i+1,m+1):
                x=2*n-integer();y=x-integer()
                if not 0<=y<=x<2*n:raise ValueError('AIGER is not topological')
                gates.extend((x,y))
        return cls(i,gates,outputs)

    def write(self,path):
        path=Path(path)
        if self.inputs>MAX_INPUTS or len(self.ands)//2>MAX_ANDS or len(self.outputs)>MAX_OUTPUTS:
            raise ValueError('arithmetic output graph budget')
        def integer(f,value):
            while value>=128:f.write(bytes([(value&127)|128]));value>>=7
            f.write(bytes([value]))
        with path.open('xb') as f:
            f.write(f'aig {self.inputs+len(self.ands)//2} {self.inputs} 0 {len(self.outputs)} {len(self.ands)//2}\n'.encode())
            for literal in self.outputs:f.write(f'{literal}\n'.encode())
            for k in range(len(self.ands)//2):
                x,y=self.ands[2*k:2*k+2];lhs=2*(self.inputs+k+1)
                if not 0<=y<=x<lhs:raise ValueError('invalid emitted AIGER topology')
                integer(f,lhs-x);integer(f,x-y)


class Builder:
    def __init__(self,inputs,maximum=MAX_CONE):
        self.inputs=inputs;self.maximum=maximum;self.ands=array('I');self.unique={}

    def and_(self,a,b):
        if not a or not b or a==b^1:return 0
        if a==1 or a==b:return b
        if b==1:return a
        a,b=max(a,b),min(a,b);key=(a<<32)|b
        if key not in self.unique:
            if len(self.ands)//2>=self.maximum:raise ValueError('arithmetic node budget')
            self.unique[key]=2*(self.inputs+len(self.ands)//2+1);self.ands.extend((a,b))
        return self.unique[key]


def observation_roots(graph,manifest):
    result={}
    for p in manifest.get('observations',[]):
        key=(p['name'],p['offset'],p['width']);first=p['first_output'];end=first+p['width']
        if key in result or not 0<=first<end<=len(graph.outputs):raise ValueError('observation interface differs')
        result[key]=list(graph.outputs[first:end])
    return result


def macros(design):
    def signal(name,width):return (name,0,width)
    if design=='vexriscv-verilog':
        def arg(part):return signal('execute_MulPlugin_'+part,16 if part in ('aULow','bULow') else 17)
        return [{'name':'mul_'+name,'a':[arg(a)],'b':[arg(b)],'c':[],
                 'a_width':17,'b_width':17,'y':signal('execute_MUL_'+name,width)}
                for name,a,b,width in [('LL','aULow','bULow',32),('LH','aSLow','bHigh',34),
                                       ('HL','aHigh','bSLow',34),('HH','aHigh','bHigh',32)]]
    if design=='ibex':
        base='u_ibex_core.ex_block_i.gen_multdiv_fast.multdiv_i.'
        return [{'name':'mac','a':[signal(base+'gen_mult_fast.mult_op_a',16),signal(base+'sign_a',1)],
            'b':[signal(base+'gen_mult_fast.mult_op_b',16),signal(base+'sign_b',1)],
            'c':[signal(base+'accum',34)],'a_width':17,'b_width':17,
            'y':signal(base+'mac_res_ext',34)}]
    raise ValueError('no arithmetic observation binding')


def roles(observed,macro):
    result={}
    for role in ('a','b','c'):
        result[role]=[bit for key in macro[role] for bit in observed[tuple(key)]]
        width=macro.get(role+'_width',len(result[role]))
        if len(result[role])>width:raise ValueError('arithmetic operand width differs')
        result[role].extend([0]*(width-len(result[role])))
    result['y']=observed[tuple(macro['y'])]
    if len(result['a'])!=17 or len(result['b'])!=17 or len(result['c']) not in (0,34):
        raise ValueError('unexpected arithmetic signature')
    return result


def component(graph,values):
    """Exact Y cone, stopping at actual A/B/C nodes, preserving constants/aliases."""
    ordered=values['a']+values['b']+values['c'];cut={}
    for literal in ordered:
        if literal>=2:cut.setdefault(literal//2,len(cut)+1)
    if len(cut)>128:raise ValueError('arithmetic operand count budget')
    maximum=graph.inputs+len(graph.ands)//2
    seen=bytearray(maximum+1);todo=[x//2 for x in values['y']];count=work=0
    while todo:
        v=todo.pop();work+=1
        if work>4*MAX_CONE or len(todo)>3*MAX_CONE:raise ValueError('arithmetic cone work budget')
        if not v or v in cut or seen[v]:continue
        if v<=graph.inputs:raise ValueError('arithmetic cone has an input outside its observed operands')
        seen[v]=1;count+=1
        if count>MAX_CONE:raise ValueError('arithmetic cone node budget')
        k=2*(v-graph.inputs-1);todo.extend((graph.ands[k]//2,graph.ands[k+1]//2))
    image=array('I',[0])*(maximum+1)
    for old,new in cut.items():image[old]=2*new
    b=Builder(len(cut))
    def val(l):return image[l//2]^(l&1)
    for v in range(graph.inputs+1,maximum+1):
        if seen[v]:
            k=2*(v-graph.inputs-1);image[v]=b.and_(val(graph.ands[k]),val(graph.ands[k+1]))
    y=[val(x) for x in values['y']]
    arguments={k:[val(x) for x in values[k]] for k in ('a','b','c')}
    result=Graph(len(cut),b.ands,array('I',y))
    ny,na,nb,nc=map(lambda k:len(values[k]),('y','a','b','c'))
    spec=f'({word_spec(arguments["a"],True)})*({word_spec(arguments["b"],True)})'
    if nc:spec+=f'+({word_spec(arguments["c"],False)})'
    return result,{'source_cut_nodes':list(cut),'operand_literals':values,
        'result_width':ny,'argument_widths':[na,nb,nc],'spec':spec,
        'modulus_bits':ny,'input_signed_flag':False,'component_argument_literals':arguments,
        'domain':'all Boolean operand cut values; constants/duplicate/complemented bits retained',
        'work':work,'visited_nodes':count}


def word_spec(bits,signed):
    """Integer word expression over unsigned PI bits with explicit sign weight.

    No output variables or input assumptions: the tool checks modulo the
    actual result width. A full-width accumulator can be unsigned because
    signed/unsigned interpretations differ by precisely that modulus.
    """
    terms=[];k=0
    while k<len(bits):
        literal=bits[k];negative=signed and k==len(bits)-1
        if literal==0:k+=1;continue
        width=1
        if literal>=2 and not literal&1 and not negative:
            while (k+width<len(bits) and not (signed and k+width==len(bits)-1)
                    and bits[k+width]==literal+2*width):width+=1
        value=('1' if literal==1 else f'[1:{literal//2-1}]' if not literal&1
               else f'(1-[1:{literal//2-1}])')
        if width>1:value=f'[{width}:{literal//2-1}]'
        coefficient=-(1<<k) if negative else 1<<k
        terms.append(value if coefficient==1 else f'({coefficient}*{value})')
        k+=width
    return '+'.join(terms) if terms else '0'


def abstract_context(graph,values_by_macro):
    """Replace only certified macro results by fresh unconstrained shared bits.

    Operand functions must be independent of every replaced result. Constants,
    repeated result nodes and direct-input results currently fail closed rather
    than introducing additional alias or state constraints.
    """
    cuts={};roots=[];operands=[]
    for values in values_by_macro:
        operands.extend(values['a']+values['b']+values['c'])
        for literal in values['y']:
            v=literal//2
            if literal<2 or v<=graph.inputs or v in cuts:
                raise ValueError('arithmetic result needs an explicit constant/alias/input lemma')
            bit=len(roots);roots.append(literal)
            cuts[v]=2*(graph.inputs+bit+1)^(literal&1)
    if not 0<len(roots)<=1024:raise ValueError('arithmetic abstraction width budget')
    maximum=graph.inputs+len(graph.ands)//2
    dependent=bytearray(maximum+1)
    for v in range(graph.inputs+1,maximum+1):
        k=2*(v-graph.inputs-1)
        dependent[v]=v in cuts or dependent[graph.ands[k]//2] or dependent[graph.ands[k+1]//2]
    if any(dependent[x//2] for x in operands):
        raise ValueError('arithmetic observation operands depend on a replaced result')
    # Mark each original node once, stopping at replaced results. Never retain
    # dead multiplication cones in the external Boolean context problem.
    live=bytearray(maximum+1);todo=array('I',(x//2 for x in graph.outputs));work=0
    while todo:
        v=todo.pop();work+=1
        if work>2*MAX_ANDS+MAX_OUTPUTS or len(todo)>2*MAX_ANDS+MAX_OUTPUTS:
            raise ValueError('arithmetic context work budget')
        if not v or v<=graph.inputs or v in cuts or live[v]:continue
        live[v]=1;k=2*(v-graph.inputs-1)
        todo.extend((graph.ands[k]//2,graph.ands[k+1]//2))
    image=array('I',[0])*(maximum+1)
    for v in range(1,graph.inputs+1):image[v]=2*v
    builder=Builder(graph.inputs+len(roots),MAX_ANDS)
    def val(l):return image[l//2]^(l&1)
    for v in range(graph.inputs+1,maximum+1):
        if v in cuts:image[v]=cuts[v]
        elif live[v]:
            k=2*(v-graph.inputs-1);image[v]=builder.and_(val(graph.ands[k]),val(graph.ands[k+1]))
    result=Graph(builder.inputs,builder.ands,array('I',(val(x) for x in graph.outputs)))
    return result,{'original_inputs':graph.inputs,'abstract_bits':len(roots),
        'source_result_literals':roots,'original_outputs_retained':len(graph.outputs),
        'operand_independence_checked':True,'work':work,
        'scope':'all original inputs/states plus fresh unconstrained combinational arithmetic results'}


def reconstruct_context(original,context,result_literals):
    """Reinsert actual results; external CEC must verify this against the source."""
    if context.inputs!=original.inputs+len(result_literals):raise ValueError('context reconstruction interface')
    return graft(original,context,[2*v for v in range(1,original.inputs+1)]+result_literals)


def graft(original,piece,input_literals):
    """Reinsert an extracted graph under an explicit, complete input binding."""
    if len(input_literals)!=piece.inputs:raise ValueError('component reconstruction interface')
    if len(original.ands)+len(piece.ands)>2*MAX_ANDS:raise ValueError('component reconstruction node budget')
    gates=array('I',original.ands)
    maximum=piece.inputs+len(piece.ands)//2
    image=array('I',[0])*(maximum+1)
    for v,literal in enumerate(input_literals,1):
        if not 0<=literal<=2*(original.inputs+len(original.ands)//2)+1:
            raise ValueError('component reconstruction literal')
        image[v]=literal
    def val(l):return image[l//2]^(l&1)
    for k in range(len(piece.ands)//2):
        x,y=sorted((val(piece.ands[2*k]),val(piece.ands[2*k+1])),reverse=True)
        target=2*(original.inputs+len(gates)//2+1);gates.extend((x,y));image[piece.inputs+k+1]=target
    return Graph(original.inputs,gates,array('I',(val(x) for x in piece.outputs)))
