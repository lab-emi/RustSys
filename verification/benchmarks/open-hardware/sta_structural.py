# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Adapt unused signed wire declarations for OpenSTA's structural reader.

Two streamed passes, <=1024 names/64KiB each, <=1MiB per line, <=4GiB input.
Planning<128MiB, O(input bytes + identifier tokens), no graph/path allocation.
Only internal wires used in their declaration and an optional constant driver
qualify. Any cell, port, alias, widening expression or other use fails closed.
Erasing their signed marker cannot alter a port, gate pin, bit or connection.
"""
from pathlib import Path
import re

LINE=1024**2
TOTAL=4*1024**3
DECL=re.compile(rb'^(\s*wire\s+)(signed)(\s+\[[^\]\r\n]+\]\s+)(\\[^\s]+|[a-zA-Z_$][a-zA-Z0-9_$]*)(\s*;\s*)$')
TOKEN=re.compile(rb'\\[^\s]+|[a-zA-Z_$][a-zA-Z0-9_$]*')


def lines(path):
    if path.stat().st_size>TOTAL:raise ValueError('structural STA input byte budget')
    used=0
    with path.open('rb') as stream:
        while data:=stream.readline(LINE+1):
            used+=len(data)
            if len(data)>LINE or used>TOTAL:raise ValueError('structural STA line/byte budget')
            yield data


def adapt(source,destination):
    source,destination=map(Path,(source,destination));names={}
    for number,line in enumerate(lines(source),1):
        m=DECL.fullmatch(line)
        if not m:continue
        name=m[4]
        if len(names)>=1024 or len(name)>65536 or name in names:
            raise ValueError('signed unused-wire name budget or duplicate')
        driver=re.compile(rb'\s*assign\s+'+re.escape(name)+rb'\s*=\s*\d+\s*\x27[sS]?[bBoOdDhH][0-9a-fA-F_xXzZ?]+\s*;\s*')
        names[name]={'declaration_line':number,'driver_line':None,'driver':driver}
    if not names:return source,{'method':'unmodified structural Verilog','changed_declarations':[]}
    records=[]
    with destination.open('xb') as output:
        for number,line in enumerate(lines(source),1):
            m=DECL.fullmatch(line)
            for match in TOKEN.finditer(line):
                name=match[0]
                if name not in names:continue
                entry=names[name]
                if number==entry['declaration_line']:continue
                if entry['driver_line'] is not None or not entry['driver'].fullmatch(line):
                    raise ValueError('signed wire has a functional use; structural STA adapter refuses '+name.decode())
                entry['driver_line']=number
            output.write(line[:m.start(2)]+b' '*len(m[2])+line[m.end(2):] if m else line)
    for name,entry in names.items():
        records.append({'name':name.decode(),'declaration_line':entry['declaration_line'],
                        'constant_driver_line':entry['driver_line']})
    return destination,{'method':'erase signed marker of unused internal wires only',
        'functional_uses_checked_absent':True,'ports_cells_bits_and_connections_unchanged':True,
        'changed_declarations':records}
