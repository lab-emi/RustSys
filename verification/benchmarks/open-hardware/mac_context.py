#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Check a Boolean signed multiply-add replacement in an unchanged context.

Only development tooling: Yosys and Arisca remain opaque external executables.
The word model is proved against the original RTL separately. This certificate
does NOT justify completing unknown original RTL values with zero by itself.
It proves the exact word model and its Boolean implementation interchangeable.

Memory plan: <=100,000 cells * ~512 B * four model/copy tables, plus <=2M
connections and a bounded driver/work table: <512 MiB Python; external exports
observed <512 MiB. One solver process (up to nine bounded Arisca portfolio
workers), 2 GiB physical / 4 GiB virtual guard, 180 s.
Reachability visits each node/cell once, never materializing paths.
"""
from pathlib import Path
import argparse
import copy
import re
import time

import equiv
import run
import transition

METHOD = 'boolean-mac-context-replacement-v1'
MAX_CELLS = 100000
MAX_CONNECTIONS = 2000000
MAX_BITS = 2000000
BOOLEAN = re.compile(r'\$_(?:NOT|BUF|AND|NAND|OR|NOR|XOR|XNOR|MUX|NMUX|ANDNOT|ORNOT)_')
STORAGE = re.compile(r'\$_(?:DFF|DFFE|SDFF|SDFFE|SDFFCE|DLATCH)_[PN01]+_')


def load(path, top):
    data = run.read_json(Path(path))
    if set(data['modules']) != {top}:
        raise ValueError('MAC context must be flat with one module')
    m = data['modules'][top]
    if len(m['cells']) > MAX_CELLS:
        raise ValueError('MAC context cell budget reached')
    count = sum(len(v) for c in m['cells'].values() for v in c['connections'].values())
    if count > MAX_CONNECTIONS:
        raise ValueError('MAC context connection budget reached')
    return m


def binding(m, names):
    if set(names) != {'a', 'b', 'c', 'y'}:
        raise ValueError('MAC needs explicit operand/result bindings')
    vectors = {}
    for role, parts in names.items():
        if not 0 < len(parts) <= 2:
            raise ValueError('MAC binding part budget')
        vectors[role] = [bit for name, width in parts
                         for bit in m['netnames'][name]['bits'][:width]]
        if any(width <= 0 or len(m['netnames'][name]['bits']) < width for name, width in parts):
            raise ValueError('MAC binding has a short vector')
    a, b, c, y = (vectors[n] for n in ('a', 'b', 'c', 'y'))
    if not (1 < len(a) == len(b) <= 32 and len(c) == len(y) == 2 * len(a)):
        raise ValueError('unsupported signed MAC widths')
    if (any(type(v) is not int for v in a+b+c+y)
            or len(set(a+b+c)) != len(a+b+c) or len(set(y)) != len(y)
            or set(y).intersection(a+b+c)):
        raise ValueError('MAC requires independent binary operand cut bits')
    return vectors


def drivers(m):
    table = {}; used = set()
    def drive(bits, name):
        for bit in bits:
            if type(bit) is not int or bit in table:
                raise ValueError('MAC context has multiple/non-binary drivers')
            table[bit] = name
    def consume(bits):
        for bit in bits:
            if type(bit) is int: used.add(bit)
            elif bit not in ('0', '1'): raise ValueError('MAC context contains unknown values')
    for p in m['ports'].values():
        if p['direction'] == 'input': drive(p['bits'], None)
        elif p['direction'] == 'output': consume(p['bits'])
        else: raise ValueError('MAC context has bidirectional ports')
    for name, cell in m['cells'].items():
        typ = cell['type']
        if typ == '$scopeinfo' and not cell['connections']: continue
        if not BOOLEAN.fullmatch(typ) and not STORAGE.fullmatch(typ):
            raise ValueError('MAC context has unsupported cell: '+typ)
        if set(cell['connections']) != set(cell['port_directions']):
            raise ValueError('MAC context has incomplete pin directions')
        for pin, bits in cell['connections'].items():
            direction = cell['port_directions'][pin]
            if direction == 'input': consume(bits)
            elif direction == 'output': drive(bits, name)
            else: raise ValueError('MAC cell has bidirectional pins')
    if len(table) > MAX_BITS or len(used) > MAX_BITS or not used <= table.keys():
        raise ValueError('MAC context has undriven bits or exceeds bit budget')
    # Initial conditions are preserved by the graph comparison, but an X
    # initializer would require a separate state/unknown encoding theorem.
    if any(set(str(n.get('attributes', {}).get('init', ''))) - {'0','1'}
           for n in m['netnames'].values()):
        raise ValueError('MAC context has an unsupported initial value')
    return table


def rewrite(m, vectors):
    table = drivers(m)
    result = copy.deepcopy(m); roots = set(vectors['y'])
    fresh = transition.next_bit_id(m); changes = []
    for name, cell in result['cells'].items():
        for pin, bits in cell['connections'].items():
            if cell['port_directions'][pin] != 'output': continue
            for index, bit in enumerate(bits):
                if bit in roots:
                    if not BOOLEAN.fullmatch(cell['type']):
                        raise ValueError('MAC output cannot replace storage')
                    bits[index] = fresh; fresh += 1
                    changes.append([name,pin,index,bit,bits[index]])
    if len(changes) != len(roots) or any(table.get(bit) is None for bit in roots):
        raise ValueError('MAC result is not fully driven by combinational cells')
    width = len(vectors['y']); product = list(range(fresh,fresh+width))
    parameter = lambda v: f'{v:032b}'
    for name, typ, a, b, y in [('$checked_word_mul','$mul',vectors['a'],vectors['b'],product),
                              ('$checked_word_add','$add',product,vectors['c'],vectors['y'])]:
        if name in result['cells']: raise ValueError('MAC replacement cell name collision')
        result['cells'][name] = {
            'type':typ,'parameters':{'A_SIGNED':'1','B_SIGNED':'1','A_WIDTH':parameter(len(a)),
                'B_WIDTH':parameter(len(b)),'Y_WIDTH':parameter(len(y))},
            'attributes':{},'port_directions':{'A':'input','B':'input','Y':'output'},
            'connections':{'A':a,'B':b,'Y':y}}
    return result, changes


def cone(m, vectors):
    table = drivers(m); cuts = vectors['a']+vectors['b']+vectors['c']
    seen = set(); cells = set(); todo = list(vectors['y']); work = 0
    while todo:
        bit = todo.pop()
        if type(bit) is not int or bit in cuts or bit in seen: continue
        seen.add(bit); name = table.get(bit)
        if name is None: raise ValueError('MAC cone has a hidden free input')
        cell = m['cells'][name]
        if not BOOLEAN.fullmatch(cell['type']):
            raise ValueError('MAC cone reaches storage or non-Boolean cell')
        if name in cells: continue
        cells.add(name)
        for pin, values in cell['connections'].items():
            if cell['port_directions'][pin] == 'input':
                work += len(values); todo.extend(values)
        if work > MAX_CONNECTIONS or len(seen) > MAX_BITS:
            raise ValueError('MAC cone work budget reached')
    ports = {role:{'direction':'output' if role=='y' else 'input','bits':values}
             for role,values in vectors.items()}
    return {'attributes':{'top':'1'},'ports':ports,
            'cells':{name:copy.deepcopy(m['cells'][name]) for name in sorted(cells)},
            'netnames':{name:{'hide_name':0,'bits':p['bits'],'attributes':{}} for name,p in ports.items()}}


def scripts(directory, paths, top):
    d = Path(directory)
    return {
        'base-export': (f'read_rtlil {equiv.quote(paths["snapshot"])}\nhierarchy -top {top}\n'
            'setundef -zero -undriven\nscc -expect 0\ncheck -assert\n'
            f'write_json {equiv.quote(d/"base-export.json")}\n'
            f'write_verilog -noexpr -norename {equiv.quote(d/"base-export.v")}\n'),
        'word-export': (f'read_json {equiv.quote(paths["word_json"])}\nhierarchy -top {top}\n'
            'scc -expect 0\ncheck -assert\n'
            f'write_verilog -noexpr -norename {equiv.quote(d/"word-export.v")}\n'),
        'cone-export': (f'read_json {equiv.quote(d/"cone.json")}\nhierarchy -top mac\n'
            'setattr -unset keep c:* w:*\nscc -expect 0\ncheck -assert\nopt_expr\nopt_clean\n'
            'aigmap\nopt_clean\nscc -expect 0\ncheck -assert\n'
            f'write_aiger -ascii -symbols {equiv.quote(d/"cone.aag")}\n'
            f'write_aiger -symbols {equiv.quote(d/"cone.aig")}\n')}


def check_aag(path, width):
    lines = Path(path).read_text().splitlines()
    header = lines[0].split()
    if len(header)!=6 or header[0]!='aag': raise ValueError('unexpected MAC AIGER header')
    maximum, ni, nl, no, ng = map(int,header[1:])
    if (ni != 4*width or nl or no != 2*width or ng > MAX_CONNECTIONS
            or maximum != ni+ng): raise ValueError('MAC AIGER interface mismatch')
    if list(map(int,lines[1:1+ni])) != list(range(2,2*ni+1,2)):
        raise ValueError('MAC AIGER input order mismatch')
    for index,line in enumerate(lines[1+ni+no:1+ni+no+ng],ni+1):
        lhs,a,b = map(int,line.split())
        if lhs!=2*index or not 0<=a<lhs or not 0<=b<lhs:
            raise ValueError('invalid or cyclic MAC AIGER')
    expected = [f'i{i} '+(f'a[{i}]' if i<width else f'b[{i-width}]'
                if i<2*width else f'c[{i-2*width}]') for i in range(ni)]
    expected += [f'o{i} y[{i}]' for i in range(no)]
    symbols = [line for line in lines[1+ni+no+ng:] if re.match(r'^[io][0-9]+ ',line)]
    # AIGER symbol lines may be lexicographically serialized (i10 before i2).
    # Their numeric indices, rather than encounter order, bind the interface.
    if len(symbols)!=len(expected) or set(symbols)!=set(expected):
        raise ValueError('MAC AIGER symbols/signed operand ordering differs')


def successful(text):
    return (len(re.findall(r'^\[INFO\s*\] Verification success!\s*$',text,re.M))==1
            and 'Verification failed' not in text)


def validate(directory, proof):
    d=Path(directory); c=proof['certificate']; paths={n:Path(p) for n,p in c['paths'].items()}
    required={'cone.json','cone.aag','cone.aig','arisca-verdict.stdout',
              'base-export.json','base-export.v','word-export.v'}
    for label in ('base-export','word-export','cone-export','arisca'):
        required.update(label+suffix for suffix in ('.command.json','.status.json','.stdout','.stderr','.time'))
        if label!='arisca':required.add(label+'.ys')
    if not required<=c['files'].keys() or len(c['files'])>64:
        raise ValueError('MAC proof evidence coverage/budget differs')
    if any(Path(name).name!=name for name in c['files']):
        raise ValueError('MAC proof evidence path is not local')
    if set(proof['source_sha256'])!={str(p) for p in paths.values()}:
        raise ValueError('MAC source coverage differs')
    for p,h in proof['source_sha256'].items():
        if run.digest(p)!=h: raise ValueError('MAC source changed')
    for p,h in c['files'].items():
        if run.digest(d/p)!=h: raise ValueError('MAC proof evidence changed')
    for tool in proof['tools'].values():
        if run.digest(tool['path'])!=tool['sha256']: raise ValueError('MAC proof executable changed')
    base=load(paths['base_json'],c['top']); word=load(paths['word_json'],c['top'])
    v=binding(base,c['bindings']); expected,changes=rewrite(base,v)
    if word!=expected or changes!=c['changed_drivers']:
        raise ValueError('MAC replacement changed additional logic, state, metadata or ports')
    if run.read_json(d/'cone.json')!={'modules':{'mac':cone(base,v)}}:
        raise ValueError('MAC cone differs from the exact operand/result binding')
    for label,script in scripts(d,paths,c['top']).items():
        if (d/(label+'.ys')).read_text()!=script: raise ValueError('MAC export recipe changed')
        status=run.read_json(d/(label+'.status.json'))
        if status['status']!='ok' or status['exit_code']!=0: raise ValueError('MAC export incomplete')
        if run.read_json(d/(label+'.command.json')) != [proof['tools']['yosys']['path'],'-Q','-T','-s',str(d/(label+'.ys'))]:
            raise ValueError('MAC export tool invocation differs')
    for exported,role in [('base-export.json','base_json'),('base-export.v','base'),('word-export.v','word')]:
        if run.digest(d/exported)!=run.digest(paths[role]): raise ValueError('MAC Verilog/JSON export differs')
    obligations=sum(len(p['bits']) for p in base['ports'].values() if p['direction']=='output')
    obligations+=sum(len(cell['connections']['Q']) for cell in base['cells'].values() if STORAGE.fullmatch(cell['type']))
    if proof['partitions']!=obligations or c['width']!=len(v['a']):
        raise ValueError('MAC context interface coverage differs')
    width=len(v['a']);check_aag(d/'cone.aag',width)
    args=[proof['tools']['arisca']['path'],str(d/'cone.aig'),'--spec',f'[{width}]*[{width}]+[{2*width}]',
          '--signed','--portfolio','--size-limit','100000','--log-file',str(d/'arisca-verdict.stdout')]
    if run.read_json(d/'arisca.command.json')!=['env','RUST_LOG=info',*args]:
        raise ValueError('MAC arithmetic invocation does not prove the signed specification')
    s=run.read_json(d/'arisca.status.json')
    if s['status']!='ok' or s['exit_code']!=0 or not successful((d/'arisca-verdict.stdout').read_text()):
        raise ValueError('MAC arithmetic theorem was not proved')
    return proof


def prove(directory,snapshot,base_json,base,word_json,word,library,top,bindings,yosys,arisca,seconds=180,memory_gib=4):
    top=equiv.object_name(top)
    d=Path(directory).resolve();d.mkdir(parents=True,exist_ok=False);started=time.monotonic();steps=[]
    paths={n:Path(p).resolve() for n,p in [('snapshot',snapshot),('base_json',base_json),('base',base),
          ('word_json',word_json),('word',word),('library',library)]}
    sources={str(p):run.digest(p) for p in paths.values()}
    tools={n:{'path':str(Path(p).resolve()),'sha256':run.digest(p)} for n,p in [('yosys',yosys),('arisca',arisca)]}
    try:
        m=load(paths['base_json'],top);v=binding(m,bindings);expected,changes=rewrite(m,v)
        if load(paths['word_json'],top)!=expected: raise ValueError('word MAC rewrite differs')
        run.save(d/'cone.json',{'modules':{'mac':cone(m,v)}})
        def execute(argv,label):
            left=seconds-(time.monotonic()-started)
            if left<=0: raise ValueError('MAC proof deadline reached')
            r=run.execute(argv,d,label,seconds=left,memory_gib=memory_gib,max_file_bytes=128*1024**2)
            run.save(d/(label+'.status.json'),r);steps.append(r)
            if r['status']!='ok': raise ValueError('MAC phase incomplete: '+label)
        for label,script in scripts(d,paths,top).items():
            p=d/(label+'.ys');p.write_text(script);execute([tools['yosys']['path'],'-Q','-T','-s',str(p)],label)
        width=len(v['a']);check_aag(d/'cone.aag',width)
        execute(['env','RUST_LOG=info',tools['arisca']['path'],str(d/'cone.aig'),'--spec',
            f'[{width}]*[{width}]+[{2*width}]','--signed','--portfolio','--size-limit','100000',
            '--log-file',str(d/'arisca-verdict.stdout')],'arisca')
        obligations=sum(len(p['bits']) for p in m['ports'].values() if p['direction']=='output')
        obligations+=sum(len(c['connections']['Q']) for c in m['cells'].values() if STORAGE.fullmatch(c['type']))
        c={'complete':True,'context_identity_checked':True,'all_outputs_and_storage_preserved':True,
           'binary_operand_totality_checked':True,'arithmetic_theorem_proven':True,
           'initial_relation_covers_all_gate_initial_states':True,'reset_assumptions':'none',
           'top':top,'bindings':bindings,'changed_drivers':changes,'width':width,
           'paths':{n:str(p) for n,p in paths.items()},'files':{}}
        c['files']={p.name:run.digest(p) for p in d.iterdir() if p.is_file()}
        result={'method':METHOD,'status':'ok','verdict':'proven','partitions':obligations,
                'wall_s':time.monotonic()-started,'peak_rss_kib':max([s.get('peak_rss_kib') or 0 for s in steps]+[0]),
                'source_sha256':sources,'tools':tools,'steps':steps,'certificate':c,
                'scope':{'reference':'certified word-level intermediate; original RTL proved separately',
                         'initial_state':'all implementation initial states; identical storage',
                         'reset_assumptions':'none','wire_model':'none','corner':'not_applicable',
                         'unconstrained_endpoints':'not_applicable'}}
        validate(d,result)
    except (ValueError,KeyError,AssertionError) as error:
        result={'method':METHOD,'status':'unverified','verdict':'unverified','diagnostic':str(error),
                'wall_s':time.monotonic()-started,'steps':steps,'source_sha256':sources}
    run.save(d/'result.json',result);return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for n in ('directory','snapshot','base-json','base','word-json','word','library','bindings','yosys','arisca'):
        p.add_argument('--'+n,type=Path,required=True)
    p.add_argument('--top',required=True);p.add_argument('--seconds',type=int,default=180)
    a=p.parse_args();print(prove(a.directory,a.snapshot,a.base_json,a.base,a.word_json,a.word,a.library,a.top,
          run.read_json(a.bindings),a.yosys,a.arisca,seconds=a.seconds)['verdict'])
