# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Replay a simple externally refuted output directly in both original AIGERs.

At most 2M input ids *4 B, 16384 output ids *4 B, bounded header/line reads;
<256 MiB including the reused state-ledger check's capped 128M-byte presence
table, one worker. No graph/SAT search. A differing pair of free
input literals is a complete Boolean witness, valid for any other input values.
Never classify solver timeout or a nontrivial cone as a replayed counterexample.
"""
from pathlib import Path
from array import array
import copy
import sys
import run
import large_lec
import slice_lec

METHOD='abc-independent-rtl-event-counterexample-v1'


def words(path,maximum):
    if path.stat().st_size>maximum*4 or path.stat().st_size%4:
        raise ValueError('counterexample index-map budget')
    out=array('I');out.frombytes(path.read_bytes())
    if sys.byteorder!='little':out.byteswap()
    return out


def output(path,index):
    if path.stat().st_size>2*1024**3:raise ValueError('counterexample AIGER byte budget')
    with path.open('rb') as stream:
        h=stream.readline(256).decode().split()
        if len(h)!=6 or h[0]!='aig':raise ValueError('counterexample AIGER header')
        m,i,l,o,a=map(int,h[1:])
        if l or m!=i+a or i>2_000_000 or o>8_000_000 or not 0<=index<o:
            raise ValueError('counterexample AIGER interface budget')
        for _ in range(index+1):
            line=stream.readline(32)
            if not line.endswith(b'\n'):raise ValueError('counterexample output-line budget')
        literal=int(line)
        if literal//2>i or literal<2:raise ValueError('counterexample is not a free input literal')
        return literal,i


def replay(proof,source,projection,check):
    if proof.get('method')!=slice_lec.METHOD:raise ValueError('requires exact sliced model')
    base=proof['certificate']['source_export'];manifest=base['manifest']
    if not base['artifact_hashes_checked'] or not base['state_relation_coverage_checked']:
        raise ValueError('unverified source export')
    relation=large_lec.verify_relation(source,manifest)
    if any(base[k]!=v for k,v in relation.items()):raise ValueError('state relation changed')
    if (manifest['reset_assumptions'] or manifest['initial_state_choices'] or manifest['case_assumptions']
            or not manifest['all_rtl_inputs_and_states_quantified'] or not manifest['all_gate_initial_states_covered']):
        raise ValueError('restricted initial-state model')
    cert=run.read_json(check/'certificate.json')
    if cert['measurement']['status']!='ok' or cert['proven']:raise ValueError('no completed external refutation')
    log=check/'abc.stdout'
    if log.stat().st_size>4*1024**2 or 'Networks are NOT EQUIVALENT' not in log.read_text():
        raise ValueError('no external inequivalence verdict')
    for name,sha in cert['sha256'].items():
        if run.digest(projection/name)!=sha:raise ValueError('changed projection')
    ids=words(projection/'output_ids.u32le',16384);inputs=words(projection/'input_ids.u32le',2_000_000)
    original=ids[0]
    if original>=manifest['output_bits']:raise ValueError('counterexample is not an original observable output')
    literals=[]
    for name in ('gold','gate'):
        path=source/(name+'.aig')
        if run.digest(path)!=base[name+'_sha256']:raise ValueError('changed original AIGER')
        local,local_inputs=output(projection/(name+'.aig'),0)
        native,native_inputs=output(path,original)
        if local_inputs!=len(inputs) or native_inputs!=manifest['sides'][0]['inputs']:
            raise ValueError('changed input interface')
        if native!=2*inputs[local//2-1]+(local&1):raise ValueError('projected literal differs from original')
        literals.append(native)
    a,b=literals
    if a//2==b//2:raise ValueError('requires two distinct arbitrary variables')
    # An observable invariant rules out *every* alternative state relation:
    # the implementation duplicates one bit, while the RTL has independent bits.
    # A failure of an arbitrary internal matching heuristic alone is insufficient.
    if original==0:raise ValueError('no adjacent observable invariant')
    previous=[output(source/(name+'.aig'),original-1)[0] for name in ('gold','gate')]
    if previous!=[b,b]:raise ValueError('no relation-independent duplicated-output witness')
    assignment={str(a//2):1^(a&1),str(b//2):b&1}
    values=[assignment[str(x//2)]^(x&1) for x in literals]
    if values!=[1,0]:raise ValueError('counterexample replay failed')
    witness={'original_output_index':original,'original_literals':literals,
        'observable_invariant_replayed':True,
        'original_output_indices':[original-1,original],
        'reference_vector_literals':[b,a],'implementation_vector_literals':[b,b],
        'reference_vector_values':[0,1],'implementation_vector_values':[0,0],
        'implementation_invariant':'both original output bits share the same literal; the reference 01 vector is impossible for every implementation initial state',
        'original_aiger_files':{name:str((source/(name+'.aig')).resolve()) for name in ('gold','gate')},
        'input_assignment_one_based':assignment,'other_inputs':'arbitrary; absent from both output functions',
        'evaluated_outputs':values,'original_aiger_replay':True,
        'external_abc_refuted':True,'external_log_sha256':run.digest(log),
        'projection_sha256':cert['sha256'],'source_export':base,
        'artifact_hashes_checked':True,'reset_assumptions':[],'initial_state_choices':[]}
    result=copy.deepcopy(proof)
    result.update(method=METHOD,status='ok',verdict='not-equivalent',partitions=1,
        certificate=witness,diagnostic={'code':'BENCH_LEC_COUNTEREXAMPLE',
        'message':'External ABC refuted an original output; its free-state witness was replayed in both complete AIGER interfaces.'})
    if not complete(result):raise ValueError('incomplete counterexample certificate')
    return result


def complete(proof):
    c=proof.get('certificate',{});m=c.get('source_export',{}).get('manifest',{})
    return (proof.get('method')==METHOD and proof.get('status')=='ok' and proof.get('verdict')=='not-equivalent'
        and c.get('original_aiger_replay') is True and c.get('external_abc_refuted') is True
        and c.get('artifact_hashes_checked') is True and c.get('evaluated_outputs')==[1,0]
        and c.get('observable_invariant_replayed') is True
        and c.get('reference_vector_values')==[0,1] and c.get('implementation_vector_values')==[0,0]
        and c.get('reset_assumptions')==[] and c.get('initial_state_choices')==[]
        and m.get('all_rtl_inputs_and_states_quantified') is True and m.get('all_gate_initial_states_covered') is True
        and m.get('reset_assumptions')==[] and m.get('initial_state_choices')==[]
        and type(c.get('original_output_index')) is int and 0<=c['original_output_index']<m.get('output_bits',0)
        and len(c.get('external_log_sha256',''))==64)
