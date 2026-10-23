# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Public Yosys commands for a separately labelled arbitrary-initial-state flow.

No implementation source is used. opt_merge -keepdc retains distinct
uninitialized FFs. The v3 flow completes data X bits before opt_dff so an
X-next-state FF cannot become an undriven X wire and then a constant output.
Memory lowering is structural; no opt_mem or read-FF absorption. setundef is
NEVER called with -init. Initial register states are not completed to zero.
"""
from pathlib import Path
import subprocess
import sys
import equiv
import run


def script(cfg, directory):
    d=Path(directory);q=equiv.quote
    lib=q(cfg['library']);top=equiv.object_name(cfg['design']['top'])
    latch=run.latch_map(cfg)
    # Constant-next FFs still have arbitrary values before their first clock.
    # opt_dff may fold them even with -keepdc. The v2 profile does not run it.
    ff='' if cfg.get('yosys_preserve_all_ffs',False) else 'opt_dff -keepdc\n'
    safe='opt_expr -keepdc\nopt_merge -keepdc\n'+ff+'opt_clean\n'
    early=safe.replace('opt_dff -keepdc\n','') if cfg.get('yosys_delay_ff_optimization',False) else safe
    speed=' -fast' if cfg.get('yosys_abc_fast',False) else ''
    mapping=f'abc{speed} -liberty {lib}\n'
    if cfg.get('yosys_bounded_map',False):
        # Public ABC &nf limits: four priority cuts/node, one area-flow round.
        # Avoid the legacy map path's large cut storage on this full core.
        mapping=f'abc -liberty {lib} -script "+strash;&get,-n;&nf,-C,4,-F,1;&put"\n'
    files=cfg['design']['files']
    if cfg.get('normalize_case_promises',False):
        files=[];sources=[]
        for index,name in enumerate(cfg['design']['files']):
            path=d/f'priority-source-{index}.v'
            sources.append(run.rtl_semantics.verilog_priority(name,path))
            files.append(path.name)
        run.save(d/'case-normalization.json',{'policy':'procedural-priority-no-case-promises',
            'source_semantics_complete':True,'verilog_sources':sources})
    return (f'read_liberty -lib {lib}\nread_verilog -sv '
        +' '.join(q(p) for p in files)
        +f'\nhierarchy -check -top {top}\nproc\nflatten\n'
        +early+'alumacc\nmemory_collect\nmemory_map\nsetundef -zero\n'+safe
        +'techmap\n'+safe
        +f'techmap -map {q(latch)}\ntechmap\n'
        +f'dfflibmap -liberty {lib}\n'+mapping+'clean\n'
        +'setundef -zero -undriven\ncheck -assert\n'
        +f'tee -o {q(d/"report.json")} stat -json -liberty {lib}\n'
        +f'write_verilog -noattr {q(d/"netlist.v")}\n')


if __name__=='__main__':
    cfg=run.read_json(Path(sys.argv[1]));directory=Path(cfg['directory'])
    path=directory/'synth.ys';path.write_text(script(cfg,directory))
    raise SystemExit(subprocess.call([cfg['yosys'],'-Q','-T','-s',str(path)],stdin=subprocess.DEVNULL,cwd=directory))
