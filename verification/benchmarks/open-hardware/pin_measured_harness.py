#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Retain the actual per-tool driver snapshots of a joined measurement matrix.

<=40 rows, <=8 source snapshots, <=256 files/snapshot, <=64MiB copied bytes;
streamed hashes and <512MiB estimated reporting RAM. No graph or EDA tool runs.
"""
from pathlib import Path
import argparse
import hashlib
import shutil

import run


def pin(workspace):
    workspace=Path(workspace).resolve();data=run.read_json(workspace/'results.json')
    if len(data['rows'])>40:raise ValueError('measurement row budget')
    snapshots={};total=0
    for row in data['rows']:
        for tool in ('rustsys','yosys'):
            if tool not in row:continue
            for rep in (0,1,2,3):
                folder=workspace/row['id']/row['library']/f"{row['allocated_physical_cores']}c"/f'{tool}-{rep}'
                cmd=folder/'synth.command.json'
                if cmd.stat().st_size>65536:raise ValueError('command byte budget')
                drivers=[Path(x).resolve() for x in run.read_json(cmd) if isinstance(x,str) and x.endswith('.py')]
                if (len(drivers)!=1 or drivers[0].parent.name!='harness'
                        or not drivers[0].is_relative_to(run.ROOT)):
                    raise ValueError('expected one repository-owned measured snapshot driver')
                source=drivers[0].parent;key=(tool,source)
                if key in snapshots:continue
                if len(snapshots)>=8:raise ValueError('snapshot count budget')
                original=source.parent/'results.json'
                if original.stat().st_size>64*1024**2:raise ValueError('source manifest byte budget')
                pins=run.read_json(original)['harness_sha256']
                if len(pins)>256 or drivers[0].name not in pins:raise ValueError('snapshot manifest incomplete')
                target=workspace/'measurement-harness'/tool/hashlib.sha256(str(source).encode()).hexdigest()[:16]
                target.mkdir(parents=True,exist_ok=False)
                for name,digest in sorted(pins.items()):
                    if Path(name).name!=name:raise ValueError('non-flat snapshot entry')
                    path=source/name
                    if path.is_symlink() or path.stat().st_size>1024**2:raise ValueError('snapshot file budget')
                    total+=path.stat().st_size
                    if total>64*1024**2:raise ValueError('snapshot copy budget')
                    if run.digest(path)!=digest:raise ValueError('measured snapshot changed: '+str(path))
                    shutil.copy2(path,target/name)
                snapshots[key]={'tool':tool,'source_snapshot':str(source),
                    'source_manifest':str(original),'source_manifest_sha256':run.digest(original),
                    'directory':target.relative_to(workspace).as_posix(),'sha256':pins}
    manifest={'schema':1,'snapshots':list(snapshots.values()),'copied_bytes':total}
    run.save(workspace/'measurement-harness.json',manifest)
    data['selected_harness_snapshots']={'path':'measurement-harness.json',
        'sha256':run.digest(workspace/'measurement-harness.json')}
    run.save(workspace/'results-with-harness.json',data)
    (workspace/'results-with-harness.json').replace(workspace/'results.json')
    print('Pinned actual tool snapshots:',len(snapshots))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--workspace',type=Path,required=True)
    pin(p.parse_args().workspace)
