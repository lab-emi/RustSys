#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Check a compositional RTL -> arithmetic model -> mapped netlist certificate.

The arithmetic model differs from the pre-map model only at independently
certified multiplier outputs. Both adjoining LEC proofs must be complete, so
their refinement relations compose without an initial/reset assumption.
"""
from pathlib import Path
import argparse
import copy
import re
import tempfile
import time

import arithmetic
import report
import run
import transition

METHOD = 'arithmetic-inductive-composition-v1'
TRANSITIVE_METHOD = 'transitive-inductive-composition-v1'


def external(directory):
    return _external(Path(directory).resolve(), 0, set(), {})


def _external(directory, depth, active, cache):
    directory = Path(directory).resolve()
    if directory in active or depth > 32 or len(active) + len(cache) > 256:
        raise ValueError('external certificate dependency budget or cycle')
    if directory in cache:
        return cache[directory]
    directory = Path(directory)
    proof = run.read_json(directory/'result.json')
    if not report.complete_external_proof(proof):
        raise ValueError('composition requires complete external transition proofs')
    if len(proof.get('source_sha256', {})) > 8192:
        raise ValueError('external certificate source-count budget reached')
    for name, digest in proof['source_sha256'].items():
        if run.digest(name) != digest:
            raise ValueError('a composed source changed')
    if proof.get('method') == 'boolean-mac-context-replacement-v1':
        import mac_context
        mac_context.validate(directory,proof)
        cache[directory]=proof
        return proof
    if proof.get('method') == 'yosys-combinational-refinement-v1':
        import native_comb
        native_comb.validate(directory, proof)
        cache[directory] = proof
        return proof
    if proof.get('method') == 'output-partitioned-combinational-refinement-v1':
        import partition_comb
        partition_comb.validate(directory, proof)
        cache[directory] = proof
        return proof
    if proof.get('method') == TRANSITIVE_METHOD:
        c = proof['certificate']
        active = active | {directory}
        children = [_external(Path(p), depth + 1, active, cache) for p in c['directories']]
        if children != c['subproofs'] or len(children) != 2:
            raise ValueError('transitive child evidence differs')
        first, last = children
        intermediate = str(Path(c['intermediate']).resolve())
        for child in children:
            if child['source_sha256'].get(intermediate) != run.digest(intermediate):
                raise ValueError('transitive intermediate differs')
        if not c['original_sources']:
            raise ValueError('transitive original sources are missing')
        for name in c['original_sources']:
            if (name not in first['source_sha256'] or name not in proof['source_sha256']
                    or first['source_sha256'][name] != proof['source_sha256'][name]):
                raise ValueError('transitive original source differs')
        for name in (c['netlist'], c['library']):
            if (name not in last['source_sha256'] or name not in proof['source_sha256']
                    or last['source_sha256'][name] != proof['source_sha256'][name]):
                raise ValueError('transitive final input differs')
        cache[directory] = proof
        return proof
    if proof.get('method') not in transition.METHODS:
        raise ValueError('unsupported external child certificate method')
    c = proof['certificate']
    if c.get('library_models') is not None:
        if (run.read_json(directory/'models/dual-flops.json') != c['library_models']
                or run.digest(directory/'models/dual-flops.json') != c['library_models_manifest_sha256']
                or run.digest(directory/'models/dual-flops.v') != c['library_models']['model_sha256']):
            raise ValueError('physical Liberty state model changed')
        if c['library_models']['models']:
            import library_models
            library_models.validate_conformance(directory/'models')
    if c.get('reference_encoding') == 'total-boolean-zero-mask':
        total = run.read_json(directory/'cec/reference-totality.json')
        if (not c.get('reference_totality_checked')
                or run.digest(directory/'cec/reference-totality.json') != c['reference_totality_sha256']
                or total.get('model_sha256') != run.digest(directory/'cec/gold-ordered.json')
                or total.get('encoding_log_sha256') != run.digest(directory/'cec/reference-xprop.stdout')
                or not total.get('fully_driven_boolean_cells')
                or not total.get('external_scc_and_driver_checks')):
            raise ValueError('reference Boolean totality evidence changed')
    if proof['method'] == transition.DUAL_METHOD:
        if run.digest(directory/'cec/implementation-xprop.il') != c['implementation_xprop_sha256']:
            raise ValueError('target definedness model changed')
    for relative, field in [('transition/state-relation.json','state_relation_sha256'),
                            ('cec/gold.aig','gold_aiger_sha256'),('cec/gate.aig','gate_aiger_sha256'),
                            ('cec/reference-xprop.il','reference_xprop_sha256')]:
        if run.digest(directory/relative) != c[field]:
            raise ValueError('a composed model changed')
    cursor = 0
    for batch in c['explicit_batches']:
        if batch['first'] != cursor or not batch['proven']:
            raise ValueError('incomplete composed proof range')
        label = 'cec-whole' if batch.get('whole_graph') else f'cec-{cursor:06d}-{cursor+batch["count"]:06d}'
        log = directory/'cec'/(label+'.stdout')
        command = run.read_json(directory/'cec'/(label+'.command.json'))[-1]
        if (' -p ' in command or not command.startswith(('cec -n ', '&cec '))
                or run.digest(log) != batch['log_sha256'] or not transition.cec_equivalent(log.read_text())):
            raise ValueError('invalid composed CEC evidence')
        cursor += batch['count']
    if cursor != c['outputs']:
        raise ValueError('composed proof did not cover all outputs')
    cache[directory] = proof
    return proof


def check_arithmetic(directory, snapshot, source, ideal):
    directory, snapshot, source, ideal = map(Path, (directory, snapshot, source, ideal))
    proof = run.read_json(directory/'certificate.json')
    model = arithmetic.load_snapshot(snapshot)
    if (not proof['complete'] or proof['snapshot_sha256'] != run.digest(snapshot)
            or len(proof['components']) != len(model['multipliers'])):
        raise ValueError('incomplete arithmetic observation coverage')
    with tempfile.TemporaryDirectory() as temporary:
        temporary = Path(temporary)
        for index, item in enumerate(proof['components']):
            folder = directory/str(index)
            fresh = arithmetic.extract(model,index,temporary/f'{index}.aag')
            if fresh['aag_sha256'] != item['aag_sha256'] or run.digest(folder/'component.aag') != item['aag_sha256']:
                raise ValueError('arithmetic extraction changed')
            if any(fresh[key] != item.get(key) for key in fresh):
                raise ValueError('arithmetic operand or specialization metadata changed')
            aag = folder/'component.aag'
            if item.get('sign_completion'):
                extra = item['sign_completion']
                for name, key in [('complete-sign.aag','lifted_sha256'),
                                  ('complete-sign-zero.aag','zero_cofactor_sha256'),
                                  ('original.aig','binary_original_sha256'),
                                  ('zero-cofactor.aig','binary_cofactor_sha256')]:
                    if run.digest(folder/name) != extra[key]:
                        raise ValueError('arithmetic sign-completion model changed')
                arithmetic._zero_cofactor(folder/'complete-sign.aag',temporary/'zero.aag',item['fixed_zero_inputs'][0])
                if run.digest(temporary/'zero.aag') != extra['zero_cofactor_sha256']:
                    raise ValueError('arithmetic cofactor is not the declared constant specialization')
                if (run.digest(folder/'complete-sign-check.stdout') != extra['cec_log_sha256']
                        or not transition.cec_equivalent((folder/'complete-sign-check.stdout').read_text())):
                    raise ValueError('arithmetic specialization was not proved')
                aag = folder/'complete-sign.aag'
            arithmetic.check_encoding(aag, folder/'component.polys',folder/'component.spec',item['width'],item['signed'])
            for name, key in [('component.polys','polys_sha256'),('component.spec','spec_sha256'),
                              ('component.proof','proof_sha256'),('checked.proof','checked_proof_sha256'),
                              ('amulet.stdout','amulet_log_sha256'),('pacheck.stdout','pacheck_log_sha256')]:
                if run.digest(folder/name) != item[key]:
                    raise ValueError('arithmetic proof evidence changed')
            if not re.search(r'^\[pck2\] c TARGET CHECKED\s*$',(folder/'pacheck.stdout').read_text(),re.M):
                raise ValueError('arithmetic target was not independently checked')
        recreated = temporary/'ideal.v'
        arithmetic.idealize(snapshot,source,recreated,proof)
        if run.digest(recreated) != run.digest(ideal):
            raise ValueError('ideal model changed more than its certified multiplier outputs')
    return proof


def compose(output, source_proof, arithmetic_directory, snapshot, premap, ideal,
            mapped_proof, original_sources, netlist, library):
    source, target = external(source_proof), external(mapped_proof)
    original_sources = [Path(p) for p in original_sources]
    netlist, library, premap, ideal = map(Path, (netlist,library,premap,ideal))
    for p in [*original_sources, ideal]:
        if source['source_sha256'].get(str(p.resolve())) != run.digest(p):
            raise ValueError('source proof is not about this original RTL and ideal model')
    for p in [premap, netlist, library]:
        if target['source_sha256'].get(str(p.resolve())) != run.digest(p):
            raise ValueError('target proof is not about this pre-map model and mapped netlist')
    arithmetic_proof = check_arithmetic(arithmetic_directory,snapshot,premap,ideal)
    files = [*original_sources,netlist,library,premap,ideal,Path(snapshot),
             Path(source_proof)/'result.json',Path(mapped_proof)/'result.json',
             Path(arithmetic_directory)/'certificate.json']
    result = {'method':METHOD,'status':'ok','verdict':'proven',
              'partitions':source['partitions']+target['partitions']+sum(2*s['width'] for s in arithmetic_proof['components']),
              'wall_s':source['wall_s']+target['wall_s']+sum(s['amulet']['guard_wall_s']+s['pacheck']['guard_wall_s'] for s in arithmetic_proof['components']),
              'peak_rss_kib':max(source['peak_rss_kib'],target['peak_rss_kib']),
              'source_sha256':{str(p.resolve()):run.digest(p) for p in files},
              'scope':{'reference':'original RTL','outputs':'all original outputs and state-transition obligations',
                       'reset_assumptions':'none','initial_state':'all mapped implementation initial states; composed refinement relations',
                       'wire_model':'none'},
              'certificate':{'complete':True,'transitivity_checked':True,'arithmetic_evidence_checked':True,
                             'subproofs':[source,target],'arithmetic':arithmetic_proof,
                             'directories':{'source':str(Path(source_proof).resolve()),'arithmetic':str(Path(arithmetic_directory).resolve()),
                                            'target':str(Path(mapped_proof).resolve())},
                             'snapshot':str(Path(snapshot).resolve()),'premap':str(premap.resolve()),'ideal':str(ideal.resolve())}}
    output = Path(output)
    output.mkdir(parents=True,exist_ok=True)
    if (output/'result.json').exists():
        raise ValueError('composition output already contains a result')
    run.save(output/'result.json',result)
    return result


def compose_transitive(output, source_proof, target_proof, original_sources,
                       intermediate, netlist, library):
    """Compose two complete external statements at the exact same model bytes."""
    first, last = external(source_proof), external(target_proof)
    original_sources = [Path(p) for p in original_sources]
    intermediate, netlist, library = map(Path,(intermediate,netlist,library))
    for path in [*original_sources,intermediate]:
        if first['source_sha256'].get(str(path.resolve())) != run.digest(path):
            raise ValueError('first proof does not match the transitive interface')
    for path in [intermediate,netlist,library]:
        if last['source_sha256'].get(str(path.resolve())) != run.digest(path):
            raise ValueError('last proof does not match the transitive interface')
    files=[*original_sources,intermediate,netlist,library,Path(source_proof)/'result.json',Path(target_proof)/'result.json']
    result={'method':TRANSITIVE_METHOD,'status':'ok','verdict':'proven',
            'partitions':first['partitions']+last['partitions'],
            'wall_s':first['wall_s']+last['wall_s'],
            'peak_rss_kib':max(first['peak_rss_kib'],last['peak_rss_kib']),
            'source_sha256':{str(p.resolve()):run.digest(p) for p in files},
            'scope':{'reference':'original RTL','reset_assumptions':'none',
                     'initial_state':'all implementation initial states; transitive refinement'},
            'certificate':{'complete':True,'transitivity_checked':True,
                           'subproofs':[first,last],'intermediate':str(intermediate.resolve()),
                           'original_sources':[str(p.resolve()) for p in original_sources],
                           'netlist':str(netlist.resolve()),'library':str(library.resolve()),
                           'directories':[str(Path(source_proof).resolve()),str(Path(target_proof).resolve())]}}
    output=Path(output);output.mkdir(parents=True,exist_ok=False);run.save(output/'result.json',result)
    return result


def prove(config, directory, exporter, yosys, eqy, abc, amulet, pacheck,
          seconds=14400, memory_gib=16, max_cells=1500000):
    directory = Path(directory).resolve()
    directory.mkdir(parents=True,exist_ok=False)
    started = time.monotonic()
    files = [Path(p).resolve() for p in config['design']['files']]
    if len(files) != 1:
        raise ValueError('arithmetic composition currently requires one self-contained prepared RTL file')
    netlist = Path(config.get('netlist',Path(config['directory'])/'netlist.v')).resolve()
    library = Path(config['library']).resolve()
    source_hashes = {str(p):run.digest(p) for p in [*files,netlist,library]}
    binaries = {name:{'path':str(Path(path).resolve()),'sha256':run.digest(path)} for name,path in
                [('exporter',exporter),('yosys',yosys),('eqy',eqy),('abc',abc),('amulet',amulet),('pacheck',pacheck)]}
    run.save(directory/'input.json',config)
    run.save(directory/'tools.json',binaries)
    def remaining():
        value = seconds - (time.monotonic()-started)
        if value <= 0:
            raise ValueError('arithmetic composition deadline reached')
        return value
    try:
        premap, ideal = directory/'premap.v', directory/'ideal.v'
        snapshot = directory/'premap.trace.json'
        exported = run.execute([str(exporter),config['design']['top'],str(files[0]),str(premap),'trace-cells'],
                               directory,'export',seconds=min(300,remaining()),memory_gib=memory_gib,
                               max_file_bytes=256*1024**2)
        if exported['status'] != 'ok':
            raise ValueError('bounded arithmetic observation export failed')
        components = directory/'arithmetic'
        arithmetic_proof = arithmetic.certify(snapshot,components,amulet,pacheck,yosys,abc,deadline=started+seconds)
        run.save(directory/'replacement.json',arithmetic.idealize(snapshot,premap,ideal,arithmetic_proof))
        reference = copy.deepcopy(config)
        reference.update(netlist=str(ideal),gate_rtl=True,gate_icells=True)
        limits = transition.Limits(max_cells=max_cells,max_batch_outputs=64,whole_attempt_s=3600)
        first = directory/'rtl-to-ideal'
        first_proof = transition.prove(reference,first,str(yosys),str(eqy),str(abc),
                                       seconds=remaining(),memory_gib=memory_gib,limits=limits)
        if not report.complete_external_proof(first_proof):
            raise ValueError('original RTL to ideal arithmetic model remains unverified')
        mapped = copy.deepcopy(config)
        mapped.update(netlist=str(netlist))
        mapped['rtl_icells'] = True
        mapped['design']['files'] = [str(premap)]
        mapped['design']['sha256'] = {premap.name:run.digest(premap)}
        last = directory/'premap-to-netlist'
        last_proof = transition.prove(mapped,last,str(yosys),str(eqy),str(abc),
                                      seconds=remaining(),memory_gib=memory_gib,limits=limits)
        if not report.complete_external_proof(last_proof):
            raise ValueError('pre-mapping model to measured netlist remains unverified')
        for name,digest in source_hashes.items():
            if run.digest(name) != digest:
                raise ValueError('original input changed during arithmetic composition')
        for binary in binaries.values():
            if run.digest(binary['path']) != binary['sha256']:
                raise ValueError('a verification binary changed during arithmetic composition')
        return compose(directory,first,components,snapshot,premap,ideal,last,files,netlist,library)
    except (ValueError,KeyError,OSError,MemoryError) as error:
        result={'method':METHOD,'status':'unverified','verdict':'unverified','wall_s':time.monotonic()-started,
                'diagnostic':{'code':'BENCH_ARITHMETIC_COMPOSITION','message':str(error)},
                'source_sha256':source_hashes,'tools':binaries,'scope':{'reset_assumptions':'none'}}
        run.save(directory/'result.json',result)
        return result


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('--workspace',type=Path,required=True)
    for tool in ['exporter','yosys','eqy','abc','amulet','pacheck']:
        parser.add_argument('--'+tool,type=Path,required=True)
    parser.add_argument('--timeout',type=int,default=14400)
    parser.add_argument('--memory-gib',type=int,default=16)
    args=parser.parse_args()
    if args.timeout < 1 or args.memory_gib < 1:
        parser.error('time and memory limits must be positive')
    result=prove(run.read_json(args.config),args.workspace,*[getattr(args,name).resolve() for name in
                 ['exporter','yosys','eqy','abc','amulet','pacheck']],seconds=args.timeout,memory_gib=args.memory_gib)
    print(result['verdict'],result.get('diagnostic',''),flush=True)
    raise SystemExit(0 if result['verdict']=='proven' else 2)
