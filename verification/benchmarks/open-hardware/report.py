#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Render measured data; refuse to turn an unverified result into a speedup claim."""
import json
from pathlib import Path
import sys

TABLE_HEADER = (
    "| Core | PDK | Time, s ↓<br>RustSys / Yosys | Peak RAM, MiB ↓<br>RustSys / Yosys | Cell area, µm² ↓<br>RustSys / Yosys | RTL LEC<br>RustSys / Yosys |",
    "|---|---|---:|---:|---:|---|",
)


def update_readme_table(text, rendered):
    """Update measured core/PDK rows while preserving the other campaigns.

    At most 64 rows and 1 MiB of README text; no proof/model data is loaded.
    """
    begin, end = '<!-- open-hardware-benchmark:start -->', '<!-- open-hardware-benchmark:end -->'
    if len(text) > 1024**2 or len(rendered) > 1024**2:
        raise ValueError('README table text budget reached')
    if text.count(begin) != 1 or text.count(end) != 1:
        raise ValueError('README requires one benchmark marker pair')
    left, right = text.index(begin) + len(begin), text.index(end)
    if left >= right:
        raise ValueError('README benchmark markers are out of order')

    def rows(block):
        lines = block.strip().splitlines()
        if not 2 <= len(lines) <= 66 or tuple(lines[:2]) != TABLE_HEADER:
            raise ValueError('unexpected README benchmark table or row budget reached')
        result = {}
        for line in lines[2:]:
            cells = [cell.strip() for cell in line.split('|')[1:-1]]
            if not line.startswith('|') or not line.endswith('|') or len(cells) != 6:
                raise ValueError('unexpected README benchmark row')
            key = tuple(cells[:2])
            if key in result:
                raise ValueError('duplicate README core/PDK row')
            result[key] = line
        return result

    merged = rows(text[left:right])
    merged.update(rows(rendered))
    if len(merged) > 64:
        raise ValueError('README benchmark row budget reached')
    table_text = '\n'.join([*TABLE_HEADER, *merged.values()])
    return text[:left] + '\n\n' + table_text + '\n\n' + text[right:]


def complete_external_proof(proof):
    method = proof.get("method")
    if (proof.get("status") != "ok" or proof.get("verdict") != "proven"
            or type(proof.get("partitions")) is not int or proof["partitions"] <= 0):
        return False
    if isinstance(method, str) and method.startswith("eqy-"):
        return True
    if method == 'boolean-mac-context-replacement-v1':
        c=proof.get('certificate',{})
        return (all(c.get(flag) is True for flag in ('complete','context_identity_checked',
                    'all_outputs_and_storage_preserved','binary_operand_totality_checked',
                    'arithmetic_theorem_proven','initial_relation_covers_all_gate_initial_states'))
                and type(c.get('width')) is int and 1<c['width']<=32
                and c.get('reset_assumptions')=='none'
                and proof.get('scope',{}).get('reset_assumptions')=='none')
    if method == 'output-partitioned-combinational-refinement-v1':
        c=proof.get('certificate',{});n=c.get('native',{});r=c.get('comparison',{})
        interface=c.get('interface',{});native_interface=n.get('certificate',{}).get('interface',{})
        outputs={name for name,p in interface.items() if p[0]=='output'}
        native_outputs={name for name,p in native_interface.items() if p[0]=='output'}
        selected=set(c.get('selected_outputs',[]))
        return (all(c.get(flag) is True for flag in ('initial_relation_covers_all_gate_initial_states',
                'all_state_mapped_or_universally_quantified','clock_reset_cones_defined',
                'all_output_bits_compared','exact_output_coverage_checked','identical_graphs_checked'))
            and n.get('method')=='yosys-combinational-output-projection-v1'
            and n.get('status')=='ok' and n.get('verdict')=='proven'
            and n.get('certificate',{}).get('zero_temporal_state_checked') is True
            and n.get('certificate',{}).get('undefinedness_checked') is True
            and bool(selected) and bool(native_outputs) and not selected & native_outputs
            and selected | native_outputs == outputs
            and r.get('equivalent') is True and r.get('implementation_definedness_checked') is True
            and proof['partitions']==n.get('partitions',-1)+r.get('outputs',-1)
            and proof.get('scope',{}).get('reset_assumptions')=='none')
    if method == 'yosys-combinational-refinement-v1':
        c = proof.get('certificate', {})
        if c.get('output_exclusions'):
            return False
        return (all(c.get(flag) is True for flag in (
                    'initial_relation_covers_all_gate_initial_states',
                    'all_state_mapped_or_universally_quantified', 'clock_reset_cones_defined',
                    'all_output_bits_compared', 'zero_temporal_state_checked', 'undefinedness_checked'))
                and type(c.get('equiv_cells')) is int and 0 < c['equiv_cells'] <= 500000
                and c.get('reference_case_semantics', {}).get('source_semantics_complete') is True
                and proof.get('limits', {}).get('induction_frames') == 1
                and proof.get('scope', {}).get('reset_assumptions') == 'none')
    if method == 'arithmetic-inductive-composition-v1':
        certificate = proof.get('certificate', {})
        children = certificate.get('subproofs', [])
        components = certificate.get('arithmetic', {}).get('components', [])
        return (certificate.get('complete') is True and certificate.get('transitivity_checked') is True
                and certificate.get('arithmetic_evidence_checked') is True and len(children) == 2
                and all(complete_external_proof(child) for child in children)
                and certificate.get('arithmetic', {}).get('complete') is True
                and bool(components) and all(type(c.get('width')) is int and 0<c['width']<=32 for c in components)
                and proof['partitions']==sum(c['partitions'] for c in children)+sum(2*c['width'] for c in components))
    if method == 'transitive-inductive-composition-v1':
        certificate=proof.get('certificate',{})
        children=certificate.get('subproofs',[])
        return (certificate.get('complete') is True and certificate.get('transitivity_checked') is True
                and len(children)==2 and all(complete_external_proof(child) for child in children)
                and proof['partitions']==sum(c['partitions'] for c in children))
    if method in ("abc-inductive-transition-v1", "abc-inductive-transition-xprop-v2", "abc-inductive-transition-priority-v4", "abc-inductive-transition-priority-v5", 'abc-inductive-transition-dual-xprop-v6'):
        certificate = proof.get("certificate", {})
        if certificate.get('reference_encoding', 'formal-xprop') not in ('formal-xprop', 'total-boolean-zero-mask'):
            return False
        if certificate.get('reference_encoding') == 'total-boolean-zero-mask':
            if certificate.get('reference_totality_checked') is not True or len(certificate.get('reference_totality_sha256', '')) != 64:
                return False
        flags = ("equivalent", "initial_relation_covers_all_gate_initial_states",
                 "all_state_mapped_or_universally_quantified", "clock_reset_cones_defined",
                 "all_output_bits_compared", "port_order_verified",
                 "projection_graphs_unchanged")
        if method == 'abc-inductive-transition-dual-xprop-v6':
            flags += ('implementation_unknowns_encoded', 'implementation_definedness_checked', 'definedness_refinement_checked')
            if len(certificate.get('implementation_xprop_sha256', '')) != 64:
                return False
        else:
            flags += ('implementation_function_total',)
        if method in ("abc-inductive-transition-xprop-v2", "abc-inductive-transition-priority-v4", "abc-inductive-transition-priority-v5", 'abc-inductive-transition-dual-xprop-v6'):
            flags += ("reference_unknowns_encoded", "refinement_masks_checked")
            if not isinstance(certificate.get("reference_xprop_sha256"), str) or len(certificate["reference_xprop_sha256"]) != 64:
                return False
        if method in ("abc-inductive-transition-priority-v4", "abc-inductive-transition-priority-v5", 'abc-inductive-transition-dual-xprop-v6'):
            if certificate.get("reference_case_semantics", {}).get("policy") != "procedural-priority-no-case-promises":
                return False
            if certificate.get("reference_case_semantics", {}).get("source_semantics_complete") is not True:
                return False
            if len(certificate.get("reference_case_normalization_sha256", "")) != 64:
                return False
        if method in ('abc-inductive-transition-priority-v5', 'abc-inductive-transition-dual-xprop-v6') and certificate.get('asynchronous_state_visibility') != 'reset value while reset is active; stored state otherwise arbitrary':
            return False
        return (all(certificate.get(flag) is True for flag in flags)
                and certificate.get("cec_partitioning") is False
                and certificate.get("outputs") == proof["partitions"]
                and certificate.get("outputs_proven") == proof["partitions"]
                and all(isinstance(certificate.get(field), str) and len(certificate[field]) == 64
                        for field in ("gold_aiger_sha256", "gate_aiger_sha256",
                                      "cec_log_sha256", "state_relation_sha256")))
    return False


def proven(row):
    return all(
        row.get(tool, {}).get("status") == "ok"
        and complete_external_proof(row[tool].get("equivalence", {}))
        for tool in ("rustsys", "yosys")
    ) and row["rustsys"].get("independent_checker", {}).get("status") == "ok" \
        and row["rustsys"].get("independent_checker", {}).get("verdict") == "equivalent"


def pair(row, field, fmt):
    values = []
    for tool in ("rustsys", "yosys"):
        r = row.get(tool, {})
        value = r.get("qor", {}).get(field) if field == "area_um2" else r.get(field)
        values.append(fmt(value) if r.get("status") == "ok" and value is not None else "—")
    return " / ".join(values)


def table(result):
    lines = list(TABLE_HEADER)
    for row in result["rows"]:
        time = pair(row, "median_wall_s", lambda x: f"{x:.2f}")
        memory = pair(row, "peak_rss_kib", lambda x: f"{x / 1024:.0f}")
        area = pair(row, "area_um2", lambda x: f"{x:,.0f}")
        pdk = {"sky130hd_tt": "Sky130 HD", "gf180mcu7t_tt": "GF180 7T"}[row["library"]]
        states = []
        for tool in ("rustsys", "yosys"):
            measured = row.get(tool, {})
            proof = measured.get("equivalence", {})
            states.append("Proven" if measured.get("status") == "ok"
                          and complete_external_proof(proof)
                          else "Unverified" if measured.get("status") == "ok" else "Incomplete")
        state = " / ".join(states)
        lines.append(f"| {row['design']} | {pdk} | {time} | {memory} | {area} | {state} |")
    return "\n".join(lines)


def speedups(result):
    # Both tools must finish, produce stable QoR, and pass the external oracle.
    return [r["yosys"]["median_wall_s"] / r["rustsys"]["median_wall_s"]
            for r in result["rows"] if proven(r) and r["rustsys"]["median_wall_s"] > 0]


if __name__ == "__main__":
    print(table(json.loads(Path(sys.argv[1]).read_text())))
