// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

use super::*;
fn source() -> String {
    "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER [ ]\n*T_UNIT 1 PS\n*C_UNIT 2 FF\n*R_UNIT 2 OHM\n*NAME_MAP\n*1 a\n*2 u\n*D_NET *1 3\n*CONN\n*P *1 I\n*I *2:A I *C -1 2 *D BUF\n*CAP\n1 a 1\n2 *2:A 2\n*RES\n1 a *2:A 5\n*END\n".into()
}
// RCLI-REQ-0071, RCLI-REQ-0072: scalar units and name mapping, no nominal-unit assumptions.
#[test]
fn mapped_units_and_metadata() {
    let s = read(&source(), Limits::default()).unwrap();
    assert_eq!(s.nets[0].nodes[1].name, "u:A");
    assert_eq!(s.nets[0].total_cap_pf, 0.006);
    assert_eq!(s.nets[0].resistors[0].kohm, 0.01);
    assert_eq!(s.node_count, 2);
}
// RCLI-REQ-0071: syntax and unsupported semantics fail explicitly.
#[test]
fn reject_corrupt_or_semantic_extensions() {
    for (from, to) in [
        ("*END", ""),
        ("*D_NET", "*R_NET"),
        ("1 a 1", "1 a NaN"),
        ("1 a 1", "1 a -1"),
        ("1 a 1", "1 a 1:2:3"),
        ("*2:A I", "*99:A I"),
        ("*CONN", "*INDUC"),
        ("*C -1 2", "*L 0.2"),
        ("*1 a", "*1 a\n*1 a"),
        ("*SPEF", "*UNKNOWN"),
        ("*D_NET *1 3", "*D_NET *1 9"),
        (
            "*D_NET *1 3",
            "*DESIGN_FLOW \"PIN_CAP INCLUDED\"\n*D_NET *1 3",
        ),
    ] {
        assert!(
            read(&source().replace(from, to), Limits::default()).is_err(),
            "{from} -> {to}"
        );
    }
}
// RCLI-REQ-0072: every independent resource budget fails before it can grow without bound.
#[test]
fn enforce_all_budgets() {
    for limits in [
        Limits {
            max_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_nodes: 1,
            ..Limits::default()
        },
        Limits {
            max_elements: 1,
            ..Limits::default()
        },
        Limits {
            max_name_bytes: 1,
            ..Limits::default()
        },
    ] {
        assert_eq!(read(&source(), limits).unwrap_err().code, "T0072");
    }
    assert!(
        read(
            &source(),
            Limits {
                max_nets: 0,
                ..Limits::default()
            }
        )
        .is_err()
    );
}
// RCLI-REQ-0072: only the local endpoint receives an inter-net coupling term.
#[test]
fn explicit_grounding_and_total() {
    let text = source()
        .replace("*D_NET *1 3", "*D_NET *1 4")
        .replace("2 *2:A 2", "2 *2:A 2\n3 *2:A remote:1 1");
    let s = read(&text, Limits::default()).unwrap();
    assert_eq!(s.coupling_records, 1);
    assert_eq!(s.nets[0].nodes.len(), 2);
    assert_eq!(s.nets[0].nodes[1].cap_pf, 0.006);
    assert!(read(&text.replace("remote:1", "a"), Limits::default()).is_err());
}

// RCLI-REQ-0071, RCLI-REQ-0072: source statement order cannot change RC reductions.
#[test]
fn canonical_order_is_independent_of_record_order() {
    let a = source();
    let b = a
        .replace(
            "*P *1 I\n*I *2:A I *C -1 2 *D BUF",
            "*I *2:A I *C -1 2 *D BUF\n*P *1 I",
        )
        .replace("1 a 1\n2 *2:A 2", "2 *2:A 2\n1 a 1");
    assert_eq!(
        read(&a, Limits::default()).unwrap(),
        read(&b, Limits::default()).unwrap()
    );
}

// RCLI-REQ-0074: public typed data must not bypass bounds before engine indexing.
#[test]
fn rejects_invalid_public_rc_models() {
    let good = read(&source(), Limits::default()).unwrap();
    good.validate(Limits::default()).unwrap();
    let mut bad = good.clone();
    bad.nets[0].connections[0].node = u32::MAX;
    assert!(bad.validate(Limits::default()).is_err());
    let mut bad = good.clone();
    bad.nets[0].resistors[0].b = u32::MAX;
    assert!(bad.validate(Limits::default()).is_err());
    let mut bad = good.clone();
    bad.node_count = 0;
    assert!(bad.validate(Limits::default()).is_err());
    assert_eq!(bad.estimated_state_bytes(), good.estimated_state_bytes());
    let mut bad = good;
    bad.nets[0].nodes[0].cap_pf = f64::INFINITY;
    assert!(bad.validate(Limits::default()).is_err());
}
