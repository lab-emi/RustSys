#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Bounded external word-level LEC after checked removal of all storage.

The external Yosys SAT engine handles arithmetic primitives not supported by
xprop. State, events, reset visibility and initial coverage are prepared by the
same guarded transition adapter. No temporal state remains in the SAT problem.
Planning: <=100,000 cells in each model (~100 MiB Python state); <=500,000
comparison points. Ibex uses about 6.3 million SAT clauses / 2.4 million variables
and <4 GiB RSS. One SAT worker, one induction frame, 900 s default deadline;
run under a 4 GiB physical-memory scope and 8 GiB virtual-memory cap.
"""
from pathlib import Path
import json
import re
import resource
import sys
import time
import lec
import run
import transition
import equiv

METHOD = 'yosys-combinational-refinement-v1'
PROJECTION_METHOD = 'yosys-combinational-output-projection-v1'
MAX_EQUIV = 500000
FORBIDDEN = ('ff','latch','mem','any','assume','assert','cover','initstate')


def model(path, top):
    m = transition.load(path,top,transition.Limits(max_cells=MAX_EQUIV,max_json_bytes=256*1024**2))
    if any(any(x in c['type'].lower() for x in FORBIDDEN) for c in m['cells'].values()):
        raise ValueError('storage or formal constraints remain in combinational proof')
    if any('init' in n.get('attributes',{}) for n in m['netnames'].values()):
        raise ValueError('initial constraints remain in combinational proof')
    if any(p['direction'] not in ('input','output') for p in m['ports'].values()):
        raise ValueError('non-combinational port interface')
    return m


def shapes(m):
    return {n:(p['direction'],len(p['bits'])) for n,p in m['ports'].items()}


def live_hint_names(m, max_bits=2000000, max_expansions=2000000):
    """Retain optional names only in the checked output cones.

    One visited bit set and one expanded-cell set, O(V+E); no paths are
    enumerated. At the configured bounds driver/reachability tables add <256
    MiB to the <=100,000-cell model. A word cell is expanded only once.
    External SCC/driver checks still precede the actual SAT proof.
    """
    drivers = {}
    for name,cell in m['cells'].items():
        for port,bits in cell['connections'].items():
            if cell['port_directions'][port]=='output':
                for bit in bits:
                    if type(bit) is int:drivers[bit]=name
        if len(drivers)>max_bits:raise ValueError('native hint driver budget reached')
    seen = {bit for p in m['ports'].values() if p['direction']=='output'
            for bit in p['bits'] if type(bit) is int}
    todo=list(seen);expanded=set();work=0
    while todo:
        bit=todo.pop();name=drivers.get(bit)
        if name is None or name in expanded:continue
        expanded.add(name);cell=m['cells'][name]
        for port,bits in cell['connections'].items():
            if cell['port_directions'][port]!='input':continue
            for value in bits:
                work+=1
                if work>max_expansions:raise ValueError('native hint expansion budget reached')
                if type(value) is int and value not in seen:
                    seen.add(value);todo.append(value)
                    if len(seen)>max_bits:raise ValueError('native hint visited-state budget reached')
    return {name for name,wire in m['netnames'].items()
            if wire['bits'] and all(type(bit) is int and bit in seen for bit in wire['bits'])}


def validate(directory, proof):
    directory=Path(directory);c=proof['certificate'];top=c['top']
    for name,digest in proof['source_sha256'].items():
        if run.digest(name)!=digest:raise ValueError('native proof source changed')
    for name,digest in c['files'].items():
        if run.digest(directory/name)!=digest:raise ValueError('native proof model/log changed')
    status=run.read_json(directory/'native/prove.status.json')
    log=(directory/'native/prove.stdout').read_text()
    if (status.get('status')!='ok' or status.get('exit_code')!=0
            or 'Equivalence successfully proven!' not in log):
        raise ValueError('missing successful native completion')
    before=model(directory/'native/before.json','equiv')
    after=model(directory/'native/after.json','equiv')
    expected=c['interface']
    if any({n:list(v) for n,v in shapes(m).items()}!=expected for m in (before,after)):
        raise ValueError('native proof omitted or changed a port')
    count=0
    for cell in after['cells'].values():
        if cell['type']=='$equiv':
            count+=1
            if cell['connections']['A']!=cell['connections']['B']:
                raise ValueError('unproved native comparison point')
    if count!=c['equiv_cells'] or not 0<count<=MAX_EQUIV:
        raise ValueError('incomplete native comparison coverage')
    if sum(cell['type']=='$equiv' for cell in before['cells'].values())!=count:
        raise ValueError('native comparison points disappeared')
    if proof['partitions']!=sum(p[1] for p in expected.values() if p[0]=='output'):
        raise ValueError('native output obligation count differs')
    for tool in proof.get('tools',{}).values():
        if run.digest(tool['path'])!=tool['sha256']:raise ValueError('native proof executable changed')
    command=(directory/'native/prove.ys').read_text()
    if 'equiv_induct -undef -seq 1\n' not in command or 'equiv_status -assert\n' not in command or '-ignore-unknown-cells' in command:
        raise ValueError('unverified native proof command')
    if c.get('library_models') is not None:
        if run.read_json(directory/'models/dual-flops.json')!=c['library_models']:
            raise ValueError('Liberty physical-state model changed')
        if c['library_models']['models']:
            import library_models
            library_models.validate_conformance(directory/'models')
    return proof


def failed_hints(directory, max_hints=256):
    """Suggest only optional names; all actual proof ports stay mandatory."""
    directory=Path(directory);log=directory/'native/prove.stdout';path=directory/'native/before.json'
    if not log.exists() or not path.exists():return ()
    if log.stat().st_size>256*1024**2:raise ValueError('native hint log byte budget reached')
    m=model(path,'equiv');ids=re.findall(r'^\s*Unproven \$equiv (\S+):',log.read_text(),re.M)
    if len(ids)>MAX_EQUIV:raise ValueError('native rejected-hint point budget reached')
    bits={b for n in ids if n in m['cells'] and m['cells'][n]['type']=='$equiv'
          for b in m['cells'][n]['connections']['Y']}
    names=tuple(sorted(n for n,w in m['netnames'].items() if n not in m['ports']
        and not n.startswith('__lec_') and not n.endswith(('_gold','_gate')) and bits.intersection(w['bits'])))
    if len(names)>max_hints:raise ValueError('native rejected-hint count budget reached')
    return names


def failed_outputs(directory, max_outputs=64):
    """Return actual unproved output names for an independent projection."""
    directory=Path(directory);log=directory/'native/prove.stdout';path=directory/'native/before.json'
    if not log.exists() or not path.exists():return ()
    if log.stat().st_size>256*1024**2:raise ValueError('native output log byte budget reached')
    m=model(path,'equiv');ids=re.findall(r'^\s*Unproven \$equiv (\S+):',log.read_text(),re.M)
    if len(ids)>MAX_EQUIV:raise ValueError('native output point budget reached')
    bits={b for n in ids if n in m['cells'] for b in m['cells'][n]['connections']['Y']}
    names=tuple(sorted(n for n,p in m['ports'].items() if p['direction']=='output' and bits.intersection(p['bits'])))
    if len(names)>max_outputs:raise ValueError('native output projection count budget reached')
    return names


def prove(config,directory,yosys,eqy,abc,seconds=900,memory_gib=8,internal_names=None,constant_exclusions=(),internal_exclusions=(),output_exclusions=()):
    directory=Path(directory).resolve();directory.mkdir(parents=True,exist_ok=False)
    start=time.monotonic();steps=[];cert={};inputs={}
    run.save(directory/'input.json',config)
    def remaining():
        left=seconds-(time.monotonic()-start)
        if left<=0:raise ValueError('native combinational proof deadline reached')
        return left
    def execute(argv,folder,label):
        r=run.execute(argv,folder,label,seconds=remaining(),memory_gib=memory_gib,max_file_bytes=256*1024**2)
        steps.append(r)
        if r['status']!='ok':raise ValueError('native combinational phase did not finish: '+label)
    try:
        if (seconds<=0 or memory_gib<=0 or internal_names is not None and len(internal_names)>128
                or len(internal_exclusions)>256 or len(output_exclusions)>64):
            raise ValueError('invalid native combinational proof budget')
        files=[*map(Path,config['design']['files']),Path(config['netlist']),Path(config['library'])]
        inputs={str(p.resolve()):run.digest(p) for p in files}
        tool={n:{'path':str(Path(p).resolve()),'sha256':run.digest(p)} for n,p in [('yosys',yosys),('eqy',eqy),('abc',abc)]}
        top=config['design']['top'];md=directory/'models';md.mkdir()
        prep=lec.prove(config,md,yosys,eqy,abc,seconds=remaining(),memory_gib=memory_gib,
                       multiclock=True,partition_clocks=True,model_only=True,native_storage=True,
                       compact_library_wires=True,max_output_gib=8)
        steps.extend(prep.get('steps',[]))
        policy=prep.get('case_normalization',{})
        if prep['status']!='prepared' or not policy.get('source_semantics_complete') or policy.get('policy')!='procedural-priority-no-case-promises':
            raise ValueError('native proof source preparation incomplete')
        td=directory/'transition';limits=transition.Limits(max_cells=100000,max_connections=2000000,max_json_bytes=256*1024**2,
                                                        constant_exclusions=tuple(constant_exclusions))
        execute([sys.executable,str(Path(transition.__file__).resolve()),'--prepare-models-named',str(md),str(td),top,str(yosys),str(remaining()),str(memory_gib),json.dumps(limits.__dict__)],directory,'prepare-models')
        plan=run.read_json(td/'state-relation.json');parts=[];interface=None
        native=directory/'native';native.mkdir()
        import fnmatch
        original_interface=None
        if output_exclusions:
            import shutil
            for side in ('gold','gate'):
                path=td/f'{side}-comb.json';m=model(path,top)
                if original_interface is None:original_interface=shapes(m)
                for name in output_exclusions:
                    if name not in m['ports'] or m['ports'][name]['direction']!='output':
                        raise ValueError('output projection must name existing output ports')
                shutil.copyfile(path,td/f'{side}-comb-full.json')
                m['ports']={n:p for n,p in m['ports'].items() if n not in output_exclusions}
                if not any(p['direction']=='output' for p in m['ports'].values()):
                    raise ValueError('native output projection must remain nonempty')
                m['netnames']={n:w for n,w in m['netnames'].items() if n not in output_exclusions}
                run.save(path,{'modules':{top:m}})
        # A deliberately unpaired state is universally quantified, not a new
        # correspondence candidate merely because an unused alias shares a
        # name. Keep every actual port/obligation while removing that hint.
        blocked = set(internal_exclusions)
        for side in ('gold','gate'):
            candidate = model(td/f'{side}-comb.json', top)
            free = {bit for name,port in candidate['ports'].items()
                    if name.startswith('__lec_unmatched_') for bit in port['bits']}
            blocked.update(name for name,wire in candidate['netnames'].items()
                           if name not in candidate['ports'] and free.intersection(wire['bits']))
            blocked.update(candidate['netnames'].keys()-candidate['ports'].keys()-live_hint_names(candidate))
            del candidate
        for side in ('gold','gate'):
            p=td/f'{side}-comb.json';m=model(p,top)
            if interface is None:interface=shapes(m)
            elif shapes(m)!=interface:raise ValueError('native gold/gate ports differ')
            if blocked.intersection(m['ports']):raise ValueError('an actual proof port cannot be excluded')
            if side=='gate' and not config.get('gate_refinement'):
                transition.guard_total_implementation(m)
            if blocked or internal_names is not None:
                m['netnames']={n:w for n,w in m['netnames'].items() if n in m['ports'] or n not in blocked and
                              (internal_names is None or any(fnmatch.fnmatchcase(n,p) for p in internal_names))}
                run.save(p,{'modules':{top:m}})
            del m
            parts += [f'read_json {equiv.quote(p)}',f'hierarchy -top {equiv.object_name(top)}']
            if side=='gate' and not config.get('gate_refinement'):
                parts+=['scc -expect 0','check -assert']
            parts += ['setundef -undriven -undef','scc -expect 0','check -assert',
                      'rename -hide c:* w:*$*',f'rename {equiv.object_name(top)} {side}',f'design -stash {side}']
        parts+=['design -copy-from gold gold','design -copy-from gate gate','equiv_make gold gate equiv',
                'hierarchy -top equiv','select -assert-none t:$*ff* t:$*FF* t:$*latch* t:$*LATCH* t:$mem* t:$any* t:$assume',
                f'write_json {equiv.quote(native/"before.json")}','equiv_induct -undef -seq 1','equiv_status -assert',
                f'write_json {equiv.quote(native/"after.json")}']
        script=native/'prove.ys';script.write_text('\n'.join(parts)+'\n')
        execute([str(yosys),'-Q','-T','-s',str(script)],native,'prove')
        after=model(native/'after.json','equiv');count=sum(c['type']=='$equiv' for c in after['cells'].values());del after
        cert={k:v for k,v in plan.items() if k not in ('groups','forgotten_state')}
        cert.update({'top':top,'equiv_cells':count,'interface':{n:list(v) for n,v in interface.items()},
                     'optional_internal_exclusions':list(internal_exclusions),
                     'output_exclusions':list(output_exclusions),
                     'original_interface':{n:list(v) for n,v in (original_interface or interface).items()},
                     'all_output_bits_compared':True,'zero_temporal_state_checked':True,'undefinedness_checked':True,
                     'reference_case_semantics':policy,'library_models':prep.get('library_models'),
                     'files':{str(p.relative_to(directory)):run.digest(p) for p in [script,native/'before.json',native/'after.json',native/'prove.stdout',native/'prove.status.json',td/'state-relation.json',td/'gold-comb.json',td/'gate-comb.json',md/'case-normalization.json']}})
        if prep.get('library_models') is not None:
            cert['files'].update({str(p.relative_to(directory)):run.digest(p) for p in (md/'dual-flops.json',md/'dual-flops.v')})
        if output_exclusions:
            cert['files'].update({str(p.relative_to(directory)):run.digest(p) for p in (td/'gold-comb-full.json',td/'gate-comb-full.json')})
        result={'method':PROJECTION_METHOD if output_exclusions else METHOD,'status':'ok','verdict':'proven','partitions':sum(w for k,w in interface.values() if k=='output'),
                'source_sha256':inputs,'tools':tool,'certificate':cert,'wall_s':time.monotonic()-start,
                'peak_rss_kib':max([resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,*[s.get('peak_rss_kib',0) or 0 for s in steps]]),
                'scope':{'reset_assumptions':'none','initial_state':'all implementation initial states; checked state correspondence',
                         'data':'external native undef-aware defined-bit refinement',
                         'outputs':'selected output projection; not a whole-design certificate' if output_exclusions else 'all original ports and state-transition obligations',
                         'wire_model':'none','corner':Path(config['library']).stem,'unconstrained_endpoints':'not_applicable'},
                'limits':{'wall_s':seconds,'virtual_memory_gib':memory_gib,'cells_per_model':100000,'equiv_cells':MAX_EQUIV,'induction_frames':1},'steps':steps}
        validate(directory,result)
    except (ValueError,KeyError,TypeError,OSError,MemoryError) as error:
        result={'method':PROJECTION_METHOD if output_exclusions else METHOD,'status':'unverified','verdict':'unverified','partitions':None,
                'source_sha256':inputs,'certificate':cert,'steps':steps,'wall_s':time.monotonic()-start,
                'diagnostic':{'code':'BENCH_NATIVE_COMB','message':str(error)},'scope':{'reset_assumptions':'none'}}
    run.save(directory/'result.json',result)
    return result
