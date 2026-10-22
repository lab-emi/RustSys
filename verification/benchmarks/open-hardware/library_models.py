# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Canonical single-state models for Liberty complementary-output flip-flops.

The two variables of ff(IQ, IQ_N) denote one physical state and its complement,
not independently initialized storage. Derive only bare, one-bit FFs with simple
clock/data expressions; reject unsupported dual-output cells when instantiated.
Memory ceiling: 128 MiB library text, one <=4 MiB cell, <=8192 cell records;
planning peak <512 MiB. No graph traversal, synthesis pass or reference source.
"""
from pathlib import Path
import re
import run

ID = r'[A-Za-z_][A-Za-z0-9_]*'
NAME = r'"?(' + ID + r')"?'


def generate(library, netlist, directory):
    library, netlist, directory = map(Path, (library, netlist, directory))
    if library.stat().st_size > 128 * 1024**2:
        raise ValueError('Liberty model source byte budget reached')
    text = library.read_text()
    text = re.sub(r'/\*.*?\*/|//[^\n]*', '', text, flags=re.S)
    heads = list(re.finditer(r'\bcell\s*\(\s*' + NAME + r'\s*\)\s*\{', text))
    if len(heads) > 8192:
        raise ValueError('Liberty model cell budget reached')
    models, records, unsupported = [], [], []
    for index, head in enumerate(heads):
        cell = head[1]
        body = text[head.end():heads[index+1].start() if index+1 < len(heads) else len(text)]
        if len(body) > 4 * 1024**2:
            raise ValueError('Liberty cell byte budget reached')
        ff = list(re.finditer(r'\bff\s*\(\s*' + NAME + r'\s*,\s*' + NAME + r'\s*\)\s*\{([^{}]*)\}', body))
        if not ff:
            continue
        attrs = lambda s: dict(re.findall(r'\b(' + ID + r')\s*:\s*"([^"\n]*)"\s*;', s))
        pins = list(re.finditer(r'\bpin\s*\(\s*' + NAME + r'\s*\)\s*\{', body))
        pinattrs = {p[1]: attrs(body[p.end():pins[i+1].start() if i+1 < len(pins) else len(body)])
                    for i,p in enumerate(pins)}
        a, b = ff[0][1], ff[0][2]
        outputs = {n:p.get('function') for n,p in pinattrs.items() if p.get('direction') == 'output'}
        if not {a,b} <= set(outputs.values()):
            continue
        f = attrs(ff[0][3])
        inputs = [n for n,p in pinattrs.items() if p.get('direction') == 'input']
        clock, data = f.get('clocked_on','').replace(' ',''), f.get('next_state','').replace(' ','')
        valid = (len(ff)==1 and set(f)=={'clocked_on','next_state'}
                 and all(v in (a,b) for v in outputs.values())
                 and re.fullmatch('!?'+ID,clock) and re.fullmatch('!?'+ID,data)
                 and clock.lstrip('!') in inputs and data.lstrip('!') in inputs
                 and len(pinattrs)==len(inputs)+len(outputs) and len(pinattrs)<=32)
        if not valid:
            unsupported.append(cell)
            continue
        declarations = [*(f'input {n};' for n in inputs),*(f'output {n};' for n in outputs)]
        state = '__lec_physical_state'
        if state in pinattrs:
            raise ValueError('reserved physical-state name collision')
        lines = [f'module {cell}('+','.join(pinattrs)+');',*declarations,f'reg {state};',
                 f'always @({"negedge" if clock.startswith("!") else "posedge"} {clock.lstrip("!")}) '
                 f'{state} <= {"~" if data.startswith("!") else ""}{data.lstrip("!")};',
                 *(f'assign {n} = {"~" if value==b else ""}{state};' for n,value in outputs.items()),'endmodule']
        models.append('\n'.join(lines))
        records.append({'cell':cell,'state_variables':[a,b],'clocked_on':clock,'next_state':data,'outputs':outputs,
                        'initial_state':'one arbitrary bit; complementary outputs have no independent state'})
    if unsupported:
        pattern = re.compile(r'(?<![A-Za-z0-9_])('+'|'.join(map(re.escape,unsupported))+r')\s+'+ID+r'\s*\(')
        with netlist.open() as stream:
            for line in stream:
                if len(line)>4*1024**2:
                    raise ValueError('netlist model scan line budget reached')
                if match:=pattern.search(line):
                    raise ValueError('unsupported complementary-output Liberty FF: '+match[1])
    path = directory/'dual-flops.v'
    path.write_text('// Generated from the pinned Liberty ff/pin declarations.\n'+'\n'.join(models)+'\n')
    manifest = {'library_sha256':run.digest(library),'model_sha256':run.digest(path),
                'models':records,'unsupported_unused_dual_cells':unsupported,'reset_assumptions':'none'}
    run.save(directory/'dual-flops.json',manifest)
    return path, manifest


def certify(library, directory, yosys, abc, seconds=120, memory_gib=4):
    """CEC against the external Liberty import under its physical state encoding.

    <=64 bare FF models, <=1000 cells/10000 connections per model, one worker.
    Peak planning <512 MiB plus the external Liberty frontend (<1 GiB).
    Both values of the single physical state are free inputs. No reset, data or
    clock input is constrained. Both next-state rails and clock functions are
    checked, in addition to every cell output.
    """
    import copy, time
    import equiv, transition
    directory=Path(directory);manifest=run.read_json(directory/'dual-flops.json')
    if (run.digest(library)!=manifest['library_sha256']
            or run.digest(directory/'dual-flops.v')!=manifest['model_sha256']):
        raise ValueError('physical-cell conformance inputs changed')
    if len(manifest['models'])>64:raise ValueError('dual-FF proof count budget reached')
    output=directory/'dual-flops-proof';output.mkdir(exist_ok=False);start=time.monotonic();items=[]
    def execute(argv,path,label):
        left=min(60,seconds-(time.monotonic()-start))
        if left<=0:raise ValueError('dual-FF conformance deadline reached')
        r=run.execute(argv,path,label,seconds=left,memory_gib=memory_gib,max_file_bytes=16*1024**2)
        if r['status']!='ok':raise ValueError('dual-FF conformance phase failed: '+label)
    lim=transition.Limits(max_cells=1000,max_bits=10000,max_state=8,max_connections=10000,max_json_bytes=16*1024**2)
    for record in manifest['models']:
        name=record['cell'];p=output/name;p.mkdir();v=directory/'dual-flops.v'
        script=p/'read.ys';script.write_text(
            f'read_liberty -ignore_miss_func -ignore_miss_dir {equiv.quote(library)}\nhierarchy -check -top {name}\nflatten\nproc\nopt_clean\nwrite_json {equiv.quote(p/"gold.json")}\ndesign -reset\n'
            f'read_verilog {equiv.quote(v)}\nhierarchy -check -top {name}\nflatten\nproc\nopt_clean\nwrite_json {equiv.quote(p/"gate.json")}\n')
        execute([str(yosys),'-Q','-T','-s',str(script)],p,'read')
        images=[];bindings=[]
        for side in ('gold','gate'):
            m=transition.load(p/f'{side}.json',name,lim);states,_,initial=transition.info(m,lim)
            if initial or not 0<len(states)<=2 or any(q['type'] not in ('$dff','$_DFF_P_','$_DFF_N_') for q in states.values()):
                raise ValueError('unexpected bare-FF state semantics')
            aliases=transition.state_output_aliases(m,set(states),lim);orient={};representatives={}
            for pin,var in record['outputs'].items():
                if pin not in aliases:raise ValueError('dual-FF output is not a state rail')
                q,inv=aliases[pin];sign=int(var==record['state_variables'][1])^inv
                if q in orient and orient[q]!=sign:raise ValueError('inconsistent physical-state encoding')
                orient[q]=sign;representatives.setdefault(var,(q,inv))
            if set(orient)!=set(states) or len(representatives)!=2:raise ValueError('incomplete physical-state coverage')
            fresh=transition.next_bit_id(m);physical=fresh;fresh+=1;inversions={}
            def invert(bit):
                nonlocal fresh
                if bit in ('0','1'):return str(1-int(bit))
                if bit not in inversions:
                    target=fresh;fresh+=1;inversions[bit]=target;m['cells']['$lec_dual_inv_'+str(target)]={'type':'$not','parameters':{'A_SIGNED':'0','A_WIDTH':'1','Y_WIDTH':'1'},'attributes':{},'port_directions':{'A':'input','Y':'output'},'connections':{'A':[bit],'Y':[target]}}
                return inversions[bit]
            replacement={q:invert(physical) if sign else physical for q,sign in orient.items()}
            bit=lambda b:replacement.get(b,b)
            for cell in list(m['cells'].values()):
                if cell['type'] not in transition.SEQ:
                    cell['connections']={port:[bit(b) for b in bs] for port,bs in cell['connections'].items()}
            m['cells']={n:c for n,c in m['cells'].items() if c['type'] not in transition.SEQ}
            for port in m['ports'].values():port['bits']=[bit(b) for b in port['bits']]
            m['ports']['__lec_physical_state']={'direction':'input','bits':[physical]}
            for var,(q,inv) in representatives.items():
                state=states[q];data=bit(state['d']);clock=bit(state['clock'])
                if inv:data=invert(data)
                if not state['pol']:clock=invert(clock)
                m['ports']['__lec_next_'+var]={'direction':'output','bits':[data]}
                m['ports']['__lec_clock_'+var]={'direction':'output','bits':[clock]}
            m['ports']=dict(sorted(m['ports'].items()));m['netnames']={n:{'bits':v['bits'],'attributes':{},'hide_name':0} for n,v in m['ports'].items()};m['attributes']={'top':'1'}
            transition.guard_total_implementation(m)
            path=p/f'{side}-comb.json';run.save(path,{'modules':{name:m}})
            script=p/f'{side}.ys';script.write_text(f'read_json {equiv.quote(path)}\nhierarchy -top {name}\nscc -expect 0\ncheck -assert\ntechmap\nopt -fast\naigmap\nopt_clean\nwrite_aiger -symbols {equiv.quote(p/(side+".aig"))}\n')
            execute([str(yosys),'-Q','-T','-s',str(script)],p,side)
            images.append(transition.aig_interface(p/f'{side}.aig',lim));bindings.append({'states':len(states),'orientation':{str(q):sign for q,sign in orient.items()}})
        if any(images[0][k]!=images[1][k] for k in ('inputs','outputs')):raise ValueError('dual-FF conformance interfaces differ')
        execute([str(abc),'-c',transition.cec_command(lim,30,p/'gold.aig',p/'gate.aig')],p,'cec')
        if not transition.cec_equivalent((p/'cec.stdout').read_text()):raise ValueError('dual-FF physical-state conformance was not proved')
        files={str(f.relative_to(output)):run.digest(f) for f in p.iterdir() if f.is_file()}
        items.append({'cell':name,'bindings':bindings,'inputs':images[0]['inputs'],'outputs':images[0]['outputs'],'files':files,'proven':True})
    if run.digest(library)!=manifest['library_sha256']:raise ValueError('conformance library changed')
    result={'complete':True,'library_sha256':manifest['library_sha256'],'model_sha256':manifest['model_sha256'],'cells':items,
            'state_scope':'one arbitrary physical bit; ff co-variable is its complement',
            'reset_assumptions':'none','tools':{n:{'path':str(Path(t).resolve()),'sha256':run.digest(t)} for n,t in [('yosys',yosys),('abc',abc)]}}
    run.save(output/'certificate.json',result);validate_conformance(directory)
    return result


def validate_conformance(directory):
    import transition
    directory=Path(directory);m=run.read_json(directory/'dual-flops.json');out=directory/'dual-flops-proof';c=run.read_json(out/'certificate.json')
    if (not c['complete'] or c['library_sha256']!=m['library_sha256'] or c['model_sha256']!=m['model_sha256']
            or run.digest(directory/'dual-flops.v')!=m['model_sha256'] or c['reset_assumptions']!='none'
            or [r['cell'] for r in c['cells']]!=[r['cell'] for r in m['models']]):
        raise ValueError('incomplete physical-cell conformance certificate')
    for item in c['cells']:
        for name,digest in item['files'].items():
            if run.digest(out/name)!=digest:raise ValueError('physical-cell conformance evidence changed')
        p=out/item['cell'];r=run.read_json(p/'cec.status.json')
        if r['status']!='ok' or r['exit_code']!=0 or not transition.cec_equivalent((p/'cec.stdout').read_text()):
            raise ValueError('physical-cell external CEC did not complete')
    return c
