#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Combine unconditional output projections with exact graph/interface coverage.

One word-level native proof and one formal-X/ABC proof share the same bounded
state-cut models. No partial native result is accepted. The native projection
is deliberately not an independently publishable whole-design certificate.
Both projections retain every input and every cell equation. Only output ports
and optional names differ. <=64 selected ports, <=1024 selected bits, <=100k
cells/model; <512 MiB adapter state, one external worker at a time, 4 GiB scope.
"""
from pathlib import Path
import copy
import time
import run
import transition
import native_comb

METHOD='output-partitioned-combinational-refinement-v1'


def projection(full, part, wanted):
    if full['cells']!=part['cells'] or full.get('attributes')!=part.get('attributes'):
        raise ValueError('an output projection changed cell equations')
    expected={n:p for n,p in full['ports'].items() if p['direction']=='input' or n in wanted}
    if part['ports']!=expected:raise ValueError('output projection changed inputs or kept outputs')


def validate(directory,proof):
    directory=Path(directory);c=proof['certificate'];nd=directory/'word';np=run.read_json(nd/'result.json')
    if np!=c['native'] or np['method']!=native_comb.PROJECTION_METHOD or np['verdict']!='proven':
        raise ValueError('missing complete native output-projection proof')
    native_comb.validate(nd,np)
    for p,digest in proof['source_sha256'].items():
        if run.digest(p)!=digest:raise ValueError('partitioned source changed')
    full_outputs={n for n,p in c['interface'].items() if p[0]=='output'}
    native_outputs={n for n,p in np['certificate']['interface'].items() if p[0]=='output'}
    selected=set(c['selected_outputs'])
    if native_outputs & selected or native_outputs | selected != full_outputs:
        raise ValueError('output projections overlap or omit an obligation')
    if selected!=set(np['certificate']['output_exclusions']):raise ValueError('native projection differs')
    top=c['top'];lim=transition.Limits(max_cells=100000,max_connections=2000000,max_json_bytes=256*1024**2)
    for side in ('gold','gate'):
        full=transition.load(nd/f'transition/{side}-comb-full.json',top,lim)
        if {n:list(p) for n,p in native_comb.shapes(full).items()}!=c['interface']:
            raise ValueError('full projection interface changed')
        for path,wanted in [(nd/f'transition/{side}-comb.json',native_outputs),
                            (directory/f'control-models/{side}-comb.json',selected)]:
            part=transition.load(path,top,lim);projection(full,part,wanted)
    raw=c['comparison'];cec=directory/'control-cec'
    if (not raw['equivalent'] or not raw['all_output_bits_compared'] or not raw['port_order_verified']
            or not raw['implementation_definedness_checked'] or raw['cec_partitioning'] is not False
            or raw['outputs']!=2*sum(c['interface'][n][1] for n in selected)):
        raise ValueError('incomplete control-cone definedness proof')
    if raw['inputs']!=sum(p[1] for p in c['interface'].values() if p[0]=='input'):
        raise ValueError('control projection input coverage differs')
    if run.digest(cec/'cec.stdout')!=raw['cec_log_sha256']:
        raise ValueError('control aggregate CEC log changed')
    for name,field in [('gold.aig','gold_aiger_sha256'),('gate.aig','gate_aiger_sha256'),
                       ('reference-xprop.il','reference_xprop_sha256'),('implementation-xprop.il','implementation_xprop_sha256')]:
        if run.digest(cec/name)!=raw[field]:raise ValueError('control comparison model changed')
    if raw.get('reference_encoding')=='total-boolean-zero-mask':
        if run.digest(cec/'reference-totality.json')!=raw['reference_totality_sha256']:
            raise ValueError('control totality evidence changed')
    cursor=0
    for batch in raw['explicit_batches']:
        if batch['first']!=cursor or not batch['proven']:raise ValueError('incomplete control proof range')
        label='cec-whole' if batch.get('whole_graph') else f'cec-{cursor:06d}-{cursor+batch["count"]:06d}'
        text=(cec/(label+'.stdout')).read_text();command=run.read_json(cec/(label+'.command.json'))[-1]
        status=run.read_json(cec/(label+'.status.json'))
        if (not command.startswith(('&cec ','cec -n ')) or ' -p ' in command
                or status['status']!='ok' or status['exit_code']!=0
                or run.digest(cec/(label+'.stdout'))!=batch['log_sha256'] or not transition.cec_equivalent(text)):
            raise ValueError('control external CEC did not complete')
        cursor+=batch['count']
    if cursor!=raw['outputs'] or proof['partitions']!=np['partitions']+cursor:
        raise ValueError('partitioned obligation count differs')
    return proof


def prove(config,directory,yosys,eqy,abc,selected_outputs,*,seconds=1800,memory_gib=8,internal_exclusions=(),constant_exclusions=()):
    directory=Path(directory).resolve();directory.mkdir(parents=True,exist_ok=False);start=time.monotonic()
    run.save(directory/'input.json',config)
    try:
        if not 0<len(selected_outputs)<=64 or len(set(selected_outputs))!=len(selected_outputs):
            raise ValueError('invalid output-projection count')
        np=native_comb.prove(config,directory/'word',yosys,eqy,abc,seconds=min(900,seconds),memory_gib=memory_gib,
                             internal_exclusions=internal_exclusions,output_exclusions=selected_outputs,
                             constant_exclusions=constant_exclusions)
        if np['verdict']!='proven':raise ValueError('native output projection remains unverified')
        c=np['certificate'];top=c['top'];interface=c['original_interface']
        if sum(interface[n][1] for n in selected_outputs)>1024:raise ValueError('control projection bit budget reached')
        models=directory/'control-models';models.mkdir();lim=transition.Limits(max_cells=100000,max_connections=2000000,max_json_bytes=256*1024**2,max_batch_outputs=64,whole_attempt_s=180)
        for side in ('gold','gate'):
            m=transition.load(directory/f'word/transition/{side}-comb-full.json',top,lim)
            m['ports']={n:p for n,p in m['ports'].items() if p['direction']=='input' or n in selected_outputs}
            m['netnames']={n:{'bits':p['bits'],'attributes':{},'hide_name':0} for n,p in m['ports'].items()}
            run.save(models/f'{side}-comb.json',{'modules':{top:m}})
        raw=transition.compare(models,directory/'control-cec',top,yosys,abc,seconds-(time.monotonic()-start),memory_gib,lim,partial_gate=True)
        if not raw['equivalent']:raise ValueError('control output projection remains unverified')
        cert={k:v for k,v in c.items() if k in ('initial_relation_covers_all_gate_initial_states','all_state_mapped_or_universally_quantified','clock_reset_cones_defined','reference_case_semantics','asynchronous_state_visibility')}
        cert.update({'top':top,'interface':interface,'selected_outputs':list(selected_outputs),'native':np,'comparison':raw,
                     'all_output_bits_compared':True,'exact_output_coverage_checked':True,'identical_graphs_checked':True})
        result={'method':METHOD,'status':'ok','verdict':'proven','partitions':np['partitions']+raw['outputs'],
                'source_sha256':np['source_sha256'],'tools':np['tools'],'certificate':cert,'wall_s':time.monotonic()-start,
                'peak_rss_kib':max([np['peak_rss_kib'],*[s.get('peak_rss_kib',0) or 0 for s in raw['steps']]]),
                'scope':{'reset_assumptions':'none','initial_state':'all implementation initial states; checked correspondence',
                         'outputs':'all original ports and state-transition obligations; disjoint complete output projections',
                         'wire_model':'none','corner':Path(config['library']).stem,'unconstrained_endpoints':'not_applicable'}}
        validate(directory,result)
    except (ValueError,KeyError,TypeError,OSError,MemoryError) as error:
        result={'method':METHOD,'status':'unverified','verdict':'unverified','partitions':None,'wall_s':time.monotonic()-start,
                'diagnostic':{'code':'BENCH_OUTPUT_PARTITIONS','message':str(error)},'scope':{'reset_assumptions':'none'}}
    run.save(directory/'result.json',result)
    return result
