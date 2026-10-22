// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

use super::*;

#[derive(Serialize)]
struct Path {
    handle: String,
    slack_ns: f64,
    startpoint: String,
}

#[derive(Serialize)]
struct Scope {
    corner: &'static str,
    wire_model: &'static str,
    parasitics: &'static str,
}

// WS-REQ-0090, WS-REQ-0091, WS-REQ-0092: the expected output of a full envelope.
#[test]
fn key_order_and_layout() {
    let mut e = Envelope::new("ruststa run");
    e.workspace = Some("./alu_ws".into());
    e.base_rev = Some(Rev(1));
    e.new_rev = Some(Rev(2));
    e.set_scope(
        RawJson::new(&Scope {
            corner: "tt",
            wire_model: "wireload_estimated",
            parasitics: "none",
        })
        .unwrap(),
    )
    .unwrap();
    e.set_metrics(RawJson::parse(r#"{"wns_ns": -0.382, "tns_ns": -14.21}"#).unwrap())
        .unwrap();
    let path = Path {
        handle: "path1".into(),
        slack_ns: -0.382,
        startpoint: "u/q[3]".into(),
    };
    e.set("top_paths", &[path]).unwrap();
    e.set("truncated_paths", &36).unwrap();
    e.set("abc", &true).unwrap();
    e.set("top_paths", &Vec::<u8>::new()).unwrap();
    e.diagnostics.push(
        Diagnostic::new(
            Severity::Warning,
            DiagCode::of("T0007"),
            "3 endpoints are unconstrained",
        )
        .at("top.sdc", 4, 1)
        .with_help("add set_output_delay"),
    );
    e.push_next_action(&[
        "ruststa", "path", "-w", "./alu_ws", "--rev", "rev2", "--target", "path1",
    ]);
    let expected = r#"{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "ruststa run",
  "workspace": "./alu_ws",
  "base_rev": "rev1",
  "rev": null,
  "new_rev": "rev2",
  "scope": {
    "corner": "tt",
    "wire_model": "wireload_estimated",
    "parasitics": "none"
  },
  "completeness": {},
  "metrics": {
    "wns_ns": -0.382,
    "tns_ns": -14.21
  },
  "top_paths": [],
  "truncated_paths": 36,
  "abc": true,
  "diagnostics": [
    {
      "severity": "warning",
      "code": "T0007",
      "location": {
        "file": "top.sdc",
        "line": 4,
        "col": 1
      },
      "message": "3 endpoints are unconstrained",
      "help": "add set_output_delay"
    }
  ],
  "error": null,
  "next_actions": [
    "ruststa path -w ./alu_ws --rev rev2 --target path1"
  ]
}"#;
    assert_eq!(e.to_json(JsonStyle::Pretty), expected);
    assert_eq!(
        e.to_json(JsonStyle::Compact),
        reformat_compact(expected),
        "compact is the same document without whitespace"
    );
    let mut out = Vec::new();
    e.write_to(&mut out, JsonStyle::Compact).unwrap();
    assert!(out.ends_with(b"}\n"));
}

fn reformat_compact(text: &str) -> String {
    json::reformat(text, JsonStyle::Compact).unwrap()
}

// WS-REQ-0090: standard keys are present even when empty, also for usage errors.
#[test]
fn failure_envelope() {
    let e = Envelope::failure(
        "rusteda",
        ErrorInfo::new(ErrorCode::Usage, "unknown flag --x"),
    );
    assert!(!e.ok());
    assert_eq!(
        e.to_json(JsonStyle::Compact),
        r#"{"schema":"rusteda.v1","ok":false,"command":"rusteda","workspace":null,"base_rev":null,"rev":null,"new_rev":null,"scope":{},"completeness":{},"metrics":{},"diagnostics":[],"error":{"code":"USAGE","message":"unknown flag --x","details":null},"next_actions":[]}"#
    );
    let conflict = Error::RevisionConflict {
        base: Rev(3),
        head: Rev(4),
    };
    let e = Envelope::failure("rusteda revert", &conflict);
    let error = e.error.as_ref().unwrap();
    assert_eq!(error.exit_code(), 4);
    assert_eq!(
        error.details.as_str(),
        r#"{"base_rev":"rev3","head":"rev4"}"#
    );
    let text = e.to_json(JsonStyle::Pretty);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["error"]["code"], "REVISION_CONFLICT");
    // Every standard key, in order.
    let mut last = 0;
    for key in STANDARD_KEYS {
        let at = text
            .find(&format!("\n  \"{key}\": "))
            .unwrap_or_else(|| panic!("{key}"));
        assert!(at > last || key == "schema", "{key}");
        last = at;
    }
}

#[test]
fn field_keys() {
    let mut e = Envelope::new("x");
    for bad in [
        "",
        "ok",
        "metrics",
        "next_actions",
        "Top",
        "a-b",
        "1a",
        "a b",
    ] {
        assert!(e.set(bad, &1).is_err(), "{bad}");
    }
    e.set("a1_b", &1).unwrap();
    assert_eq!(e.field("a1_b").unwrap().as_str(), "1");
    assert_eq!(e.fields().len(), 1);
    assert!(RawJson::parse("{").is_err());
    assert!(RawJson::null().is_null());
}

// WS-REQ-0091 (review CW-10): a JsonObject keeps the caller's key order, also for keys
// known only at run time, where a map sorts them.
#[test]
fn objects_keep_insertion_order() {
    let mut scope = JsonObject::new();
    scope
        .insert("corner", "tt_025C_1v80")
        .unwrap()
        .insert("wire_model", "wireload_estimated")
        .unwrap()
        .insert("parasitics", "none")
        .unwrap()
        .insert("drc_status", "unchecked")
        .unwrap();
    assert_eq!(
        scope.to_raw().as_str(),
        r#"{"corner":"tt_025C_1v80","wire_model":"wireload_estimated","parasitics":"none","drc_status":"unchecked"}"#
    );
    let map: crate::JsonMap = [("wire_model", "y"), ("corner", "x")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::from(v)))
        .collect();
    assert_eq!(
        RawJson::new(&map).unwrap().as_str(),
        r#"{"corner":"x","wire_model":"y"}"#,
        "a map sorts its keys"
    );

    let mut per_corner = JsonObject::new();
    for (corner, wns) in [("tt", -0.1), ("ss", -0.5), ("ff", 0.2)] {
        per_corner.insert(corner, &wns).unwrap();
    }
    per_corner.insert("tt", &-0.15).unwrap();
    assert_eq!((per_corner.len(), per_corner.is_empty()), (3, false));
    assert_eq!(per_corner.get("ss").unwrap().as_str(), "-0.5");
    let mut e = Envelope::new("ruststa run");
    e.set_metrics(&per_corner).unwrap();
    assert!(
        e.to_json(JsonStyle::Compact)
            .contains(r#""metrics":{"tt":-0.15,"ss":-0.5,"ff":0.2}"#)
    );
    assert_eq!(JsonObject::new().to_raw().as_str(), "{}");
    assert!(JsonObject::new().is_empty());
}

// WS-REQ-0092 (review CW-11): plan Example 1 is illustrative for whitespace and the text of
// numbers. Made from its values, the envelope has its keys, in its order, and its values,
// plus the keys the contract adds; the layout is the documented one: every array element
// on its own line, and an f64 printed as serde_json prints it.
#[test]
fn plan_example_1() {
    #[derive(Serialize)]
    struct Completeness {
        constrained_endpoints: u64,
        unconstrained_endpoints: u64,
        clock_domains: Vec<&'static str>,
    }
    #[derive(Serialize)]
    struct TopPath {
        handle: &'static str,
        slack_ns: f64,
        startpoint: &'static str,
        endpoint: &'static str,
        logic_levels: u32,
        cell_delay_pct: f64,
        wire_delay_pct: f64,
    }
    let plan = r#"{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "ruststa run",
  "workspace": "./alu_ws",
  "base_rev": "rev1",
  "new_rev": "rev2",
  "scope": {
    "corner": "tt_025C_1v80",
    "wire_model": "wireload_estimated",
    "parasitics": "none",
    "drc_status": "unchecked"
  },
  "completeness": {
    "constrained_endpoints": 1024,
    "unconstrained_endpoints": 0,
    "clock_domains": ["core_clk"]
  },
  "metrics": {
    "wns_ns": -0.382,
    "tns_ns": -14.210,
    "violating_endpoints": 38
  },
  "top_paths": [
    {
      "handle": "path1",
      "slack_ns": -0.382,
      "startpoint": "u_alu/reg_a_q_reg[31]/CLK",
      "endpoint": "u_mul/acc_reg[63]/D",
      "logic_levels": 19,
      "cell_delay_pct": 74.2,
      "wire_delay_pct": 25.8
    },
    {
      "handle": "path2",
      "slack_ns": -0.315,
      "startpoint": "u_dec/op_reg[4]/CLK",
      "endpoint": "u_alu/cout_reg/D",
      "logic_levels": 14,
      "cell_delay_pct": 81.0,
      "wire_delay_pct": 19.0
    }
  ],
  "truncated_paths": 36,
  "next_actions": [
    "ruststa path -w ./alu_ws --rev rev2 --target path1 --format json",
    "rustsys opt-timing -w ./alu_ws --base-rev rev2 --target path1 --format json"
  ]
}"#;
    let mut e = Envelope::new("ruststa run");
    e.workspace = Some("./alu_ws".into());
    e.base_rev = Some(Rev(1));
    e.new_rev = Some(Rev(2));
    let mut scope = JsonObject::new();
    for (key, value) in [
        ("corner", "tt_025C_1v80"),
        ("wire_model", "wireload_estimated"),
        ("parasitics", "none"),
        ("drc_status", "unchecked"),
    ] {
        scope.insert(key, value).unwrap();
    }
    e.set_scope(scope).unwrap();
    e.set_completeness(
        RawJson::new(&Completeness {
            constrained_endpoints: 1024,
            unconstrained_endpoints: 0,
            clock_domains: vec!["core_clk"],
        })
        .unwrap(),
    )
    .unwrap();
    let mut metrics = JsonObject::new();
    metrics
        .insert("wns_ns", &-0.382)
        .unwrap()
        .insert("tns_ns", &-14.210)
        .unwrap()
        .insert("violating_endpoints", &38)
        .unwrap();
    e.set_metrics(metrics).unwrap();
    let paths = [
        TopPath {
            handle: "path1",
            slack_ns: -0.382,
            startpoint: "u_alu/reg_a_q_reg[31]/CLK",
            endpoint: "u_mul/acc_reg[63]/D",
            logic_levels: 19,
            cell_delay_pct: 74.2,
            wire_delay_pct: 25.8,
        },
        TopPath {
            handle: "path2",
            slack_ns: -0.315,
            startpoint: "u_dec/op_reg[4]/CLK",
            endpoint: "u_alu/cout_reg/D",
            logic_levels: 14,
            cell_delay_pct: 81.0,
            wire_delay_pct: 19.0,
        },
    ];
    e.set("top_paths", &paths).unwrap();
    e.set("truncated_paths", &36).unwrap();
    e.next_actions = vec![
        "ruststa path -w ./alu_ws --rev rev2 --target path1 --format json".into(),
        "rustsys opt-timing -w ./alu_ws --base-rev rev2 --target path1 --format json".into(),
    ];
    let ours = e.to_json(JsonStyle::Pretty);

    let mut expected: serde_json::Value = serde_json::from_str(plan).unwrap();
    for (key, value) in [
        ("rev", serde_json::Value::Null),
        ("diagnostics", serde_json::json!([])),
        ("error", serde_json::Value::Null),
    ] {
        expected[key] = value;
    }
    let value: serde_json::Value = serde_json::from_str(&ours).unwrap();
    assert_eq!(value, expected, "the values of Example 1");
    let order = [
        "schema",
        "ok",
        "command",
        "workspace",
        "base_rev",
        "rev",
        "new_rev",
        "scope",
        "completeness",
        "metrics",
        "top_paths",
        "truncated_paths",
        "diagnostics",
        "error",
        "next_actions",
    ];
    let at: Vec<usize> = order
        .iter()
        .map(|key| ours.find(&format!("\n  \"{key}\": ")).unwrap_or(0))
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{ours}");
    for nested in [
        "\"corner\": \"tt_025C_1v80\",\n    \"wire_model\": \"wireload_estimated\",\n    \
             \"parasitics\": \"none\",\n    \"drc_status\": \"unchecked\"\n",
        "\"wns_ns\": -0.382,\n    \"tns_ns\": -14.21,\n    \"violating_endpoints\": 38\n",
    ] {
        assert!(ours.contains(nested), "{nested}");
    }
    // The documented layout: arrays one element per line, f64 text as serialized.
    assert!(ours.contains("\"clock_domains\": [\n      \"core_clk\"\n    ]"));
    assert!(!ours.contains("-14.210"));
}

// WS-REQ-0094
#[test]
fn diagnostic_json() {
    let d = Diagnostic::from_parts(
        Severity::Error,
        DiagCode::parse("V0100").unwrap(),
        Some(("a.v".into(), 3, 7)),
        "expected ';'",
        None,
    );
    assert_eq!(d.to_json(), serde_json::to_string(&d).unwrap());
    assert_eq!(
        d.to_json(),
        r#"{"severity":"error","code":"V0100","location":{"file":"a.v","line":3,"col":7},"message":"expected ';'","help":null}"#
    );
    let d = Diagnostic::new(Severity::Note, DiagCode::of("W0101"), "copied \"x\"").with_help("h");
    assert_eq!(d.to_json(), serde_json::to_string(&d).unwrap());
    assert_eq!(Severity::parse("warning"), Some(Severity::Warning));
    assert_eq!(Severity::parse("fatal"), None);
}

// WS-REQ-0094 (review CR2-7), WS-REQ-0190: a diagnostic code is a letter of the contract's
// areas (BLIF and AIGER readers included) and four digits; nothing else becomes a DiagCode, so
// no envelope or text line carries one.
#[test]
fn diagnostic_codes_are_checked() {
    assert_eq!(DiagCode::LETTERS, "ABCDGLSTVW");
    for good in [
        "A0003", "B0001", "C0001", "D0002", "G0010", "L0003", "S0004", "T0007", "V0100", "W0101",
    ] {
        let code = DiagCode::parse(good).unwrap();
        assert_eq!(code.as_str(), good);
        assert_eq!(code.to_string(), good);
        assert_eq!(code.letter(), good.chars().next().unwrap());
        assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{good}\""));
    }
    for bad in [
        "timing", "", "W001", "W00011", "X0001", "w0001", "W00a1", "W 001", "0W001", "Wé01",
    ] {
        let err = DiagCode::parse(bad).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal, "{bad}");
    }
    // A diagnostic read back from JSON is checked too.
    let text =
        r#"{"severity":"warning","code":"timing","location":null,"message":"m","help":null}"#;
    assert!(serde_json::from_str::<Diagnostic>(text).is_err());
    let ok = text.replace("timing", "T0001");
    let d: Diagnostic = serde_json::from_str(&ok).unwrap();
    assert_eq!(d.code, DiagCode::of("T0001"));
}

// WS-REQ-0090, WS-REQ-0091 (review CR2-7): scope, completeness and metrics are JSON objects,
// and no value of the envelope has an object with two equal keys.
#[test]
fn contract_parts_are_checked() {
    let mut e = Envelope::new("x");
    for bad in ["[1,2]", "1", "\"tt\"", "null", "true"] {
        let raw = RawJson::parse(bad).unwrap();
        assert_eq!(
            e.set_scope(raw.clone()).unwrap_err().code(),
            ErrorCode::Internal
        );
        assert!(e.set_completeness(raw.clone()).is_err(), "{bad}");
        assert!(e.set_metrics(raw).is_err(), "{bad}");
    }
    assert_eq!(e.scope().as_str(), "{}");
    let err = RawJson::parse(r#"{"corner":"a","corner":"b"}"#).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Internal);
    assert!(err.to_string().contains("duplicate key"), "{err}");
    e.set_scope(RawJson::parse(r#"{"corner":"a"}"#).unwrap())
        .unwrap();
    e.set_metrics(JsonObject::new()).unwrap();
    assert_eq!(e.scope().as_str(), r#"{"corner":"a"}"#);
    assert_eq!(e.metrics().as_str(), "{}");
    assert_eq!(e.completeness().as_str(), "{}");

    // A hand-written Serialize that repeats a key is refused as well.
    struct Twice;
    impl Serialize for Twice {
        fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut map = s.serialize_map(Some(2))?;
            map.serialize_entry("k", &1)?;
            map.serialize_entry("k", &2)?;
            map.end()
        }
    }
    assert_eq!(
        RawJson::new(&Twice).unwrap_err().code(),
        ErrorCode::Internal
    );
    assert!(e.set("twice", &Twice).is_err());
}

// WS-REQ-0091 (review CR2-9): a JsonObject or RawJson nested in a serialized struct keeps
// its key order, so a per-corner value inside each path of a tool field keeps the corners in
// the order the tool gave them.
#[test]
fn ordered_objects_nested_in_structs() {
    #[derive(Serialize)]
    struct TopPath {
        handle: String,
        slack_ns: JsonObject,
        scope: RawJson,
    }
    let mut per_corner = JsonObject::new();
    for (corner, slack) in [("tt", -0.3), ("ss", -0.9), ("ff", 0.1)] {
        per_corner.insert(corner, &slack).unwrap();
    }
    let path = TopPath {
        handle: "path1".into(),
        slack_ns: per_corner.clone(),
        scope: RawJson::parse(r#"{"z":[1,{"y":null,"b":true}],"a":"x"}"#).unwrap(),
    };
    let mut e = Envelope::new("ruststa run");
    e.set("top_paths", &[path]).unwrap();
    assert_eq!(
        e.field("top_paths").unwrap().as_str(),
        r#"[{"handle":"path1","slack_ns":{"tt":-0.3,"ss":-0.9,"ff":0.1},"scope":{"z":[1,{"y":null,"b":true}],"a":"x"}}]"#
    );
    assert_eq!(
        serde_json::to_string(&per_corner).unwrap(),
        per_corner.to_raw().as_str()
    );
    // Numbers are written from their values.
    let raw = RawJson::parse(r#"{"b":1.50,"a":-0.0,"n":-3}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&raw).unwrap(),
        r#"{"b":1.5,"a":-0.0,"n":-3}"#
    );
}
