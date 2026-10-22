# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Public constrained ABC mapping atop the unchanged arbitrary-state v3 flow.

The Yosys public help documents -constr as enabling buffer/upsize/dnsize and
-D in picoseconds. FFs are mapped before ABC and -dff is never enabled. The
full output still requires arbitrary-initial-state LEC and independent STA.
"""
from pathlib import Path
from decimal import Decimal
import math
import subprocess
import sys

import equiv
import run
import yosys_initial


def pipeline(path):
    path=Path(path)
    if path.stat().st_size>1024**2:raise ValueError('reference config byte budget')
    cfg=run.read_json(path);directory=Path(cfg['directory'])
    if cfg.get('yosys_bounded_map') or cfg.get('yosys_abc_fast'):
        raise ValueError('timing comparison requires full default ABC mapping')
    spec=cfg['yosys_timing'];constraint=directory/'abc.constr'
    period=spec['period_ns']
    if type(period) not in (int,float) or not math.isfinite(period*1000) or period<=0:
        raise ValueError('invalid reference period')
    period_ps=format(Decimal(str(period))*1000,'f')
    constraint.write_text(f"set_driving_cell {spec['driving_cell']}\nset_load {spec['output_load_ff']}\n")
    text=yosys_initial.script(cfg,directory)
    old=f"abc -liberty {equiv.quote(cfg['library'])}\n"
    if text.count(old)!=1:raise ValueError('unexpected reference mapping script')
    new=f"abc -liberty {equiv.quote(cfg['library'])} -constr {equiv.quote(constraint)} -D {period_ps}\n"
    script=directory/'synth.ys';script.write_text(text.replace(old,new))
    run.save(directory/'reference-timing.json',{'mode':'public default constrained ABC mapping',**spec,
        'functional_scope':'arbitrary-state v3; no reset, no init completion, no FFs passed to ABC',
        'validation':'independent whole-netlist timing/LEC required'})
    return subprocess.call([cfg['yosys'],'-Q','-T','-s',str(script)],stdin=subprocess.DEVNULL,cwd=directory)


if __name__=='__main__':raise SystemExit(pipeline(sys.argv[1]))
