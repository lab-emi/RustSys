#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Fetch pinned optional Liberty data, never tool implementation sources.

One download <=16 MiB and one decoded member <=32 MiB: <128 MiB estimated
RSS, one worker; <=1024 ZIP entries. Run under a 2 GiB / 120 s OS guard.
"""
from pathlib import Path
import argparse
import hashlib
import io
import urllib.request
import zipfile

import run


def checked_member(raw, spec):
    if len(raw)!=spec['archive_bytes'] or hashlib.sha256(raw).hexdigest()!=spec['archive_sha256']:
        raise ValueError('additional PDK archive size/checksum differs')
    if len(raw)>16*1024**2 or not 0<spec['size']<=32*1024**2:
        raise ValueError('additional PDK byte budget')
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        entries=archive.infolist()
        if len(entries)>1024 or len({entry.filename for entry in entries})!=len(entries):
            raise ValueError('additional PDK ZIP entry budget or duplicate member')
        entry=archive.getinfo(spec['member'])
        if entry.file_size!=spec['size']:raise ValueError('Liberty decoded size differs')
        data=archive.read(entry)
    if hashlib.sha256(data).hexdigest()!=spec['sha256']:
        raise ValueError('additional Liberty checksum differs')
    return data


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--manifest',type=Path,default=run.HERE/'additional-pdks.json')
    p.add_argument('--directory',type=Path,default=run.ROOT/'target/pdk')
    a=p.parse_args();specs=run.read_json(a.manifest)['library']
    if not 1<=len(specs)<=8:raise ValueError('additional library count budget')
    a.directory.mkdir(parents=True,exist_ok=True)
    for spec in specs:
        if Path(spec['filename']).name!=spec['filename']:raise ValueError('non-flat Liberty filename')
        target=a.directory/spec['filename']
        if target.exists():
            if target.stat().st_size!=spec['size'] or run.digest(target)!=spec['sha256']:
                raise ValueError('existing Liberty differs; preserve it for investigation')
        else:
            with urllib.request.urlopen(spec['archive_url'],timeout=30) as response:
                raw=response.read(16*1024**2+1)
            data=checked_member(raw,spec)
            temporary=target.with_suffix('.download');temporary.write_bytes(data);temporary.replace(target)
        print('Verified',spec['version'],spec['corner'],spec['sha256'])


if __name__=='__main__':main()
