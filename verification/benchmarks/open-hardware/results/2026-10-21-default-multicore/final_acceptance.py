from pathlib import Path
import hashlib,json,math,sys
root=Path('/mnt/data2/cgao/git/RustSys');base=root/'.work/multicore-next-20261021'
sys.path.insert(0,str(root/'verification/benchmarks/open-hardware'))
import run,scaling_report,scaling_verify
read=lambda p:json.loads(p.read_text())
data=read(base/'unified/results.json');proofs=read(base/'validation/proofs.json');timing=read(base/'validation/timing.json')
old=read(root/'.work/multicore-20261021/common-v9/results.json');oldtiming=read(root/'.work/multicore-20261021/validation-v9/timing.json')
_,_,speeds,selfups=scaling_report.tables(data,proofs,timing)
index=lambda d:{(r['id'],r['library'],r['allocated_physical_cores']):r for r in d['rows']}
rows=index(data);prior=index(old);times={(r['id'],r['library']):r for r in timing['rows']};prevtime={(r['id'],r['library']):r for r in oldtiming['rows']}
proofmap={(r['id'],r['library']):r for r in proofs['rows']}
assert len(rows)==20 and all(t in r for r in rows.values() for t in ('rustsys','yosys'))
records=[]
for did in scaling_report.ORDER:
 for lib in scaling_report.LIBS:
  q=rows[did,lib,16];p=prior[did,lib,16];tm=times[did,lib];oldtm=prevtime[did,lib]
  a=q['rustsys']['qor']['area_um2'];b=p['rustsys']['qor']['area_um2']
  assert a<=b+.001,('area regression',did,lib,a,b)
  assert a<q['yosys']['qor']['area_um2']
  assert tm['rustsys']['fmax_mhz']>=oldtm['rustsys']['fmax_mhz']-1e-9,('Fmax regression',did,lib)
  assert tm['rustsys']['fmax_mhz']>tm['yosys']['fmax_mhz'],('Fmax reference gap',did,lib)
  for n in (1,16):
   r=rows[did,lib,n];assert r['rustsys']['median_wall_s']<r['yosys']['median_wall_s'],('runtime gap',did,lib,n)
   for tool in ('rustsys','yosys'):
    assert len(r[tool]['samples'])==3 and all(x['status']=='ok' for x in r[tool]['samples'])
    assert r[tool]['netlist_sha256']==q[tool]['netlist_sha256']
  for tool in ('rustsys','yosys'):
   proof=proofmap[did,lib][tool];assert scaling_verify.complete(proof)
   assert proof['scope'].get('reset_assumptions')=='none' and proof['scope'].get('input_assumptions')=='none'
  records.append({'id':did,'library':lib,'rustsys_area_before_um2':b,'rustsys_area_after_um2':a,
   'rustsys_fmax_before_mhz':oldtm['rustsys']['fmax_mhz'],'rustsys_fmax_after_mhz':tm['rustsys']['fmax_mhz'],
   'speedup16':q['yosys']['median_wall_s']/q['rustsys']['median_wall_s'],
   'rustsys_self_scaling':rows[did,lib,1]['rustsys']['median_wall_s']/q['rustsys']['median_wall_s']})
source=read(base/'final-binaries.json')
for path,h in source['source_sha256'].items():assert run.digest(root/path)==h,('measured source changed',path)
for path,h in source['binary_sha256'].items():assert run.digest(path)==h,('measured binary changed',path)
assert run.digest(source['source_archive'])==source['source_archive_sha256']
cec=read(base/'pass-cec-v9/results.json');seq=read(base/'enabled-controls-v9/results.json')
assert cec['status']=='complete' and len(cec['rows'])==155 and all(x['status']=='proven' for x in cec['rows'])
assert seq['status']=='passed' and len(seq['rows'])==6 and all(x['passed'] for x in seq['rows'])
result={'status':'passed','matrix':'40 actually measured allocations;1warmup+3formal runs each',
 'complete_distinct_netlist_LECs':20,'complete_STA_results':20,'initial_states':'arbitrary under checked correspondence',
 'reset_assumptions':'none','input_assumptions':'none','thread_outputs':'both tools byte-identical1/16',
 'rustsys_area_growth_pairs':0,'rustsys_fmax_regressions':0,'runtime_wins':20,'area_wins':10,'setup_fmax_wins':10,
 'max_rustsys_scaling':max(selfups),'speedup16_range':[min(speeds),max(speeds)],'rows':records,
 'bound_records':{str(path):run.digest(path) for path in [base/'unified/results.json',base/'validation/proofs.json',base/'validation/timing.json',base/'final-binaries.json',base/'pass-cec-v9/results.json',base/'enabled-controls-v9/results.json']}}
run.save(base/'acceptance.json',result)
print('Accepted40allocations,20LECs,20STA;timewins20/20;area/Fmaxwins10/10;no area growth or Fmax regression.')
print('MaximumRustSysscaling',max(selfups),'16corespeedup range',min(speeds),max(speeds))
