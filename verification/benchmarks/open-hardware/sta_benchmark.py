#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Black-box OpenSTA setup/Fmax measurement of an exact exported netlist.

Memory plan: large case~6M gates*4pins*(160B graph+2*96B tags)~=8GiB,
plus strings/network/library/scratch<=8GiB: planning ceiling16GiB, one STA
process/corner. Explicit32GiB RAM+1GiB swap/40GiB AS,3600s,128MiB/log guards.
Large runs hold scale.lock; no reference source or unbounded paths are read.
This is ideal-clock, zero-wire, same-edge register setup Fmax, not routed Fmax.
"""
from pathlib import Path
import argparse
from decimal import Decimal, InvalidOperation
import fcntl
import math
import os
import re
import subprocess

import run
import sta_structural


def completed_numeric_result(output, errors, sample):
    """Read the completed public report, allowing only its printed rounding.

    Two bounded numeric tokens; constant memory/work. OpenSTA prints min-period
    frequency to two decimals even when sta_report_default_digits is larger.
    Use the period reciprocal and require overlap of the two rounding intervals.
    A fixed relative tolerance incorrectly rejects slow, fully lowered memories.
    """
    if sample.get('status')!='ok' or re.search(r'(^|\n)(?:Error|BENCH_STA_ERROR)',output+'\n'+errors):
        return None
    if not (re.search(r'^\s*time\s+1ns\s*$',output,re.M)
            and re.search(r'^\s*capacitance\s+1pF\s*$',output,re.M)
            and 'BENCH_CLOCK_MIN_PERIOD_BEGIN\n' in output
            and 'BENCH_CLOCK_MIN_PERIOD_END' in output
            and 'BENCH_CONSTRAINT_CHECK_BEGIN' in output
            and 'BENCH_CONSTRAINT_CHECK_END' in output):
        return None
    segment=output.split('BENCH_CLOCK_MIN_PERIOD_BEGIN\n',1)[1].split('BENCH_CLOCK_MIN_PERIOD_END',1)[0]
    match=re.search(r'bench_clk\s+period_min\s*=\s*([0-9.eE+-]+)\s+fmax\s*=\s*([0-9.eE+-]+)',segment)
    if not match or any(len(token)>64 for token in match.groups()):return None
    try:
        period,printed=map(Decimal,match.groups())
        if (not period.is_finite() or not printed.is_finite()
                or period<=0 or printed<0
                or any(abs(value.as_tuple().exponent)>24 for value in (period,printed))):
            return None
        half_period=Decimal(1).scaleb(period.as_tuple().exponent)/2
        half_frequency=Decimal(1).scaleb(printed.as_tuple().exponent)/2
        if period<=half_period:return None
        low=Decimal(1000)/(period+half_period)
        high=Decimal(1000)/(period-half_period)
        if high<printed-half_frequency or low>printed+half_frequency:return None
        frequency=float(Decimal(1000)/period)
        if not math.isfinite(frequency) or frequency<=0:return None
        return {'minimum_period_ns':float(period),'fmax_mhz':frequency,
                'reported_fmax_mhz':float(printed),
                'numeric_method':'reciprocal of printed minimum period; both printed rounding intervals checked'}
    except (InvalidOperation,ValueError,OverflowError):
        return None


def tcl(value):
    value=str(value)
    if '\n' in value or '\r' in value:raise ValueError('multiline Tcl path or identifier')
    return '"'+value.replace('\\','\\\\').replace('"','\\"').replace('$','\\$').replace('[','\\[').replace(']','\\]')+'"'


def measure(netlist,lib,top,clock,sta,directory,memory_gib=32,seconds=3600,cpu=0):
    netlist,lib,sta=map(lambda p:Path(p).resolve(),(netlist,lib,sta))
    directory=Path(directory).resolve();directory.mkdir(parents=True,exist_ok=False)
    pins={str(p):run.digest(p) for p in (netlist,lib,sta)}
    analysis_netlist,adapter=sta_structural.adapt(netlist,directory/'analysis-netlist.v')
    pins[str(analysis_netlist)]=run.digest(analysis_netlist)
    run.save(directory/'netlist-format.json',adapter)
    script=directory/'timing.tcl'
    script.write_text(f'''set sta_continue_on_error 0
set sta_report_default_digits 6
if {{[catch {{
  read_liberty {tcl(lib)}
  read_verilog {tcl(analysis_netlist)}
  link_design {tcl(top)}
  if {{[llength [get_ports {tcl(clock)}]] != 1}} {{error "expected one declared root clock port"}}
  create_clock -name bench_clk -period 1000 [get_ports {tcl(clock)}]
  set_input_transition 0.1 [all_inputs]
  set_clock_transition 0.1 [all_clocks]
  set_input_delay 0 -clock bench_clk [delete_from_list [all_inputs] [get_ports {tcl(clock)}]]
  set_output_delay 0 -clock bench_clk [all_outputs]
  set_load 0.01 [all_outputs]
  report_units
  report_clock_properties
  puts "BENCH_CLOCK_MIN_PERIOD_BEGIN"
  report_clock_min_period -clocks [get_clocks bench_clk]
  puts "BENCH_CLOCK_MIN_PERIOD_END"
  report_checks -path_delay max -group_path_count 1 -endpoint_path_count 1 -format json > {tcl(directory/'worst-path.json')}
  puts "BENCH_CONSTRAINT_CHECK_BEGIN"
  check_setup -no_clock -unconstrained_endpoints -no_input_delay -no_output_delay -loops
  puts "BENCH_CONSTRAINT_CHECK_END"
}} message]}} {{
  puts stderr "BENCH_STA_ERROR: $message"
  exit 2
}}
exit
''')
    pins[str(script)]=run.digest(script)
    scope={'corner':lib.stem.split('__',1)[-1],'wire_model':'none; no estimated or routed parasitics',
           'clock':'bench_clk on '+clock,'clock_model':'ideal;100ps transition;zero uncertainty',
           'input_transition_ns':0.1,'output_load_pf':0.01,'input_output_delays_ns':0,
           'unconstrained_endpoints':'explicit OpenSTA check_setup receipt; not presumed zero',
           'fmax_scope':'same-clock same-edge register-to-register setup paths; excludes port paths, cross-edge paths and level-sensitive latches',
           'excluded_checks':['routed timing','hold closure','clock tree','physical DRC'],
           'root_clock_period_ns':1000,'period_purpose':'loose reference period; report_clock_min_period derives the actual minimum',
           'reset_assumptions':'none for functional proof; this is a separate timing model'}
    command=[str(sta),'-no_splash','-exit',str(script)]
    unit='rustsys-bench-sta-'+str(os.getpid())+'.scope'
    lock=(run.ROOT/'.work/scale.lock').open('a')
    try:
        if memory_gib>4:fcntl.flock(lock,fcntl.LOCK_EX)
        available=next(int(s.split()[1]) for s in Path('/proc/meminfo').read_text().splitlines() if s.startswith('MemAvailable:'))
        if available<(memory_gib+2)*1024**2:raise ValueError('insufficient available RAM for STA guard')
        sample=run.execute(['systemd-run','--user','--scope','--quiet','--unit='+unit,
            '-p',f'MemoryMax={memory_gib}G','-p','MemorySwapMax=1G','taskset','-c',str(cpu),*command],
            directory,'sta',seconds=seconds,memory_gib=memory_gib+8,max_file_bytes=128*1024**2)
    finally:
        subprocess.run(['systemctl','--user','kill','--signal=KILL',unit],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        lock.close()
    output=(directory/'sta.stdout').read_text();errors=(directory/'sta.stderr').read_text()
    result={'status':'unverified','scope':scope,'sha256':pins,'measurement':sample,
            'netlist_format_adapter':adapter,
            'opensta_version':run.version(sta,'-version'),'fmax_mhz':None,'minimum_period_ns':None,
            'numeric_parser_sha256':run.digest(Path(__file__))}
    result['receipts_sha256']={str(p):run.digest(p) for p in
        (directory/'sta.stdout',directory/'sta.stderr',directory/'sta.time',directory/'sta.command.json',directory/'worst-path.json',directory/'netlist-format.json')
        if p.exists()}
    # Retain exact raw output. Accept only a completed numeric min-period receipt;
    # syntax/model errors, timeouts and no constrained register path stay unverified.
    run.save(directory/'result.json',result)
    if any(run.digest(p)!=h for p,h in pins.items()):raise ValueError('STA input changed during analysis')
    numeric=completed_numeric_result(output,errors,sample)
    if numeric:result.update(status='ok',**numeric)
    run.save(directory/'result.json',result)
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('netlist','lib','sta','directory'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--top',required=True);p.add_argument('--clock',required=True)
    p.add_argument('--memory-gib',type=int,default=32);p.add_argument('--seconds',type=int,default=3600);p.add_argument('--cpu',type=int,default=0)
    a=p.parse_args()
    if not 1<=a.memory_gib<=32 or not 1<=a.seconds<=10800:p.error('invalid STA resource budget')
    r=measure(a.netlist,a.lib,a.top,a.clock,a.sta,a.directory,a.memory_gib,a.seconds,a.cpu)
    print(r['status'],r['minimum_period_ns'],r['fmax_mhz'],flush=True)
