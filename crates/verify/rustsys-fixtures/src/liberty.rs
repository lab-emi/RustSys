// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSys contributors

//! Synthetic Liberty libraries. Cell names, functions, areas and timing values are invented for
//! testing; they model no real process.

use std::fmt::Write as _;

/// Library variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// Many combinational cells, flip-flops of every kind, latches and tie cells.
    Rich,
    /// Few gates; only flip-flops with an active-low clear, no set; no tie cells.
    ResetOnly,
    /// Inverter, NAND2 and a clear flip-flop only.
    Minimal,
    /// Production-style file: power pins, power tables, timing checks, drive strengths, and
    /// cells that synthesis must not use (`liberty_full`).
    Full,
}

impl Variant {
    pub const ALL: [Variant; 4] = [
        Variant::Rich,
        Variant::ResetOnly,
        Variant::Minimal,
        Variant::Full,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Variant::Rich => "wt_rich",
            Variant::ResetOnly => "wt_reset_only",
            Variant::Minimal => "wt_minimal",
            Variant::Full => "wt_full",
        }
    }
}

struct Comb {
    name: &'static str,
    area: f64,
    inputs: &'static [&'static str],
    out: &'static str,
    func: &'static str,
    /// Intrinsic delay and load slope, in ns and ns/pF.
    d0: f64,
    k: f64,
}

const fn c(
    name: &'static str,
    area: f64,
    inputs: &'static [&'static str],
    out: &'static str,
    func: &'static str,
    d0: f64,
    k: f64,
) -> Comb {
    Comb {
        name,
        area,
        inputs,
        out,
        func,
        d0,
        k,
    }
}

const RICH: &[Comb] = &[
    c("wt_inv_x1", 1.0, &["A"], "Y", "!A", 0.010, 4.0),
    c("wt_inv_x2", 1.5, &["A"], "Y", "A'", 0.008, 2.0),
    c("wt_buf_x1", 1.5, &["A"], "Y", "A", 0.020, 3.0),
    c("wt_nand2_x1", 1.25, &["A", "B"], "Y", "!(A&B)", 0.014, 5.0),
    c("wt_nand2_x2", 2.0, &["A", "B"], "Y", "(A B)'", 0.012, 2.5),
    c(
        "wt_nand3_x1",
        1.75,
        &["A", "B", "C"],
        "Y",
        "!(A&B&C)",
        0.018,
        6.0,
    ),
    c(
        "wt_nand4_x1",
        2.25,
        &["A", "B", "C", "D"],
        "Y",
        "!(A*B*C*D)",
        0.022,
        7.0,
    ),
    c("wt_nor2_x1", 1.25, &["A", "B"], "Y", "!(A|B)", 0.016, 6.0),
    c(
        "wt_nor3_x1",
        2.0,
        &["A", "B", "C"],
        "Y",
        "(A+B+C)'",
        0.022,
        8.0,
    ),
    c(
        "wt_nor4_x1",
        2.5,
        &["A", "B", "C", "D"],
        "Y",
        "!(A|B|C|D)",
        0.028,
        10.0,
    ),
    c("wt_and2_x1", 1.75, &["A", "B"], "Y", "A&B", 0.024, 3.0),
    c(
        "wt_and3_x1",
        2.25,
        &["A", "B", "C"],
        "Y",
        "A&B&C",
        0.028,
        3.0,
    ),
    c(
        "wt_and4_x1",
        2.75,
        &["A", "B", "C", "D"],
        "Y",
        "A&B&C&D",
        0.032,
        3.0,
    ),
    c("wt_or2_x1", 1.75, &["A", "B"], "Y", "A|B", 0.026, 3.0),
    c(
        "wt_or3_x1",
        2.25,
        &["A", "B", "C"],
        "Y",
        "A+B+C",
        0.030,
        3.0,
    ),
    c(
        "wt_or4_x1",
        2.75,
        &["A", "B", "C", "D"],
        "Y",
        "A|B|C|D",
        0.036,
        3.0,
    ),
    c("wt_xor2_x1", 2.75, &["A", "B"], "Y", "A^B", 0.030, 5.0),
    c("wt_xnor2_x1", 2.75, &["A", "B"], "Y", "!(A^B)", 0.030, 5.0),
    c(
        "wt_xor3_x1",
        4.5,
        &["A", "B", "C"],
        "Y",
        "A^B^C",
        0.045,
        5.0,
    ),
    c(
        "wt_aoi21_x1",
        1.75,
        &["A1", "A2", "B"],
        "Y",
        "!((A1&A2)|B)",
        0.020,
        6.0,
    ),
    c(
        "wt_aoi22_x1",
        2.25,
        &["A1", "A2", "B1", "B2"],
        "Y",
        "!((A1&A2)|(B1&B2))",
        0.024,
        7.0,
    ),
    c(
        "wt_oai21_x1",
        1.75,
        &["A1", "A2", "B"],
        "Y",
        "!((A1|A2)&B)",
        0.020,
        6.0,
    ),
    c(
        "wt_oai22_x1",
        2.25,
        &["A1", "A2", "B1", "B2"],
        "Y",
        "((A1+A2)(B1+B2))'",
        0.024,
        7.0,
    ),
    c(
        "wt_aoi211_x1",
        2.25,
        &["A1", "A2", "B", "C"],
        "Y",
        "!((A1&A2)|B|C)",
        0.026,
        8.0,
    ),
    c(
        "wt_oai211_x1",
        2.25,
        &["A1", "A2", "B", "C"],
        "Y",
        "!((A1|A2)&B&C)",
        0.026,
        8.0,
    ),
    c(
        "wt_ao21_x1",
        2.25,
        &["A1", "A2", "B"],
        "Y",
        "(A1&A2)|B",
        0.028,
        3.0,
    ),
    c(
        "wt_oa21_x1",
        2.25,
        &["A1", "A2", "B"],
        "Y",
        "(A1|A2)&B",
        0.028,
        3.0,
    ),
    c(
        "wt_nand2b_x1",
        1.75,
        &["AN", "B"],
        "Y",
        "!(!AN&B)",
        0.020,
        5.0,
    ),
    c(
        "wt_mux2_x1",
        2.75,
        &["A0", "A1", "S"],
        "Y",
        "(A0&!S)|(A1&S)",
        0.030,
        4.0,
    ),
    c(
        "wt_mux2i_x1",
        2.5,
        &["A0", "A1", "S"],
        "Y",
        "!((A0&!S)|(A1&S))",
        0.026,
        6.0,
    ),
    c(
        "wt_mux4_x1",
        5.5,
        &["A0", "A1", "A2", "A3", "S0", "S1"],
        "Y",
        "(!S1&!S0&A0)|(!S1&S0&A1)|(S1&!S0&A2)|(S1&S0&A3)",
        0.050,
        4.0,
    ),
    c(
        "wt_maj3_x1",
        3.0,
        &["A", "B", "C"],
        "Y",
        "(A&B)|(B&C)|(A&C)",
        0.034,
        4.0,
    ),
];

const RESET_ONLY: &[Comb] = &[
    c("wt_inv_x1", 1.0, &["A"], "Y", "!A", 0.010, 4.0),
    c("wt_nand2_x1", 1.25, &["A", "B"], "Y", "!(A&B)", 0.014, 5.0),
    c("wt_nor2_x1", 1.25, &["A", "B"], "Y", "!(A|B)", 0.016, 6.0),
    c("wt_and2_x1", 1.75, &["A", "B"], "Y", "A&B", 0.024, 3.0),
    c("wt_or2_x1", 1.75, &["A", "B"], "Y", "A|B", 0.026, 3.0),
    c("wt_xor2_x1", 2.75, &["A", "B"], "Y", "A^B", 0.030, 5.0),
    c(
        "wt_aoi21_x1",
        1.75,
        &["A1", "A2", "B"],
        "Y",
        "!((A1&A2)|B)",
        0.020,
        6.0,
    ),
    c(
        "wt_oai21_x1",
        1.75,
        &["A1", "A2", "B"],
        "Y",
        "!((A1|A2)&B)",
        0.020,
        6.0,
    ),
    c(
        "wt_mux2_x1",
        2.75,
        &["A0", "A1", "S"],
        "Y",
        "(A0&!S)|(A1&S)",
        0.030,
        4.0,
    ),
];

const MINIMAL: &[Comb] = &[
    c("wt_inv_x1", 1.0, &["A"], "Y", "!A", 0.010, 4.0),
    c("wt_nand2_x1", 1.25, &["A", "B"], "Y", "!(A&B)", 0.014, 5.0),
];

fn table(out: &mut String, kind: &str, d0: f64, k: f64) {
    let loads = [0.001, 0.01, 0.05];
    let slews = [0.01, 0.1, 0.5];
    let mut rows = Vec::new();
    for s in slews {
        let row: Vec<String> = loads
            .iter()
            .map(|l| format!("{:.5}", d0 + k * l + 0.1 * s))
            .collect();
        rows.push(format!("\"{}\"", row.join(", ")));
    }
    let _ = writeln!(out, "        {kind} (wt_tbl) {{");
    let _ = writeln!(
        out,
        "          values ({});",
        rows.join(", \\\n                  ")
    );
    let _ = writeln!(out, "        }}");
}

fn comb_cell(out: &mut String, cl: &Comb) {
    let _ = writeln!(out, "  cell ({}) {{", cl.name);
    let _ = writeln!(out, "    area : {};", cl.area);
    for i in cl.inputs {
        let _ = writeln!(
            out,
            "    pin ({i}) {{ direction : input; capacitance : 0.0015; }}"
        );
    }
    let _ = writeln!(out, "    pin ({}) {{", cl.out);
    let _ = writeln!(out, "      direction : output;");
    let _ = writeln!(out, "      function : \"{}\";", cl.func);
    for (j, i) in cl.inputs.iter().enumerate() {
        let _ = writeln!(out, "      timing () {{");
        let _ = writeln!(out, "        related_pin : \"{i}\";");
        let extra = 0.002 * j as f64;
        table(out, "cell_rise", cl.d0 + extra, cl.k);
        table(out, "cell_fall", cl.d0 * 0.9 + extra, cl.k * 0.9);
        let _ = writeln!(out, "      }}");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "  }}");
}

struct Seq {
    name: &'static str,
    area: f64,
    group: &'static str,
    /// (pin, is_clock)
    inputs: &'static [(&'static str, bool)],
    /// (pin, function)
    outputs: &'static [(&'static str, &'static str)],
    extra: &'static str,
}

const RICH_SEQ: &[Seq] = &[
    Seq {
        name: "wt_dff_x1",
        area: 5.0,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; }",
        inputs: &[("D", false), ("CK", true)],
        outputs: &[("Q", "IQ"), ("QN", "IQN")],
        extra: "",
    },
    Seq {
        name: "wt_dffq_x1",
        area: 4.5,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; }",
        inputs: &[("D", false), ("CK", true)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
    Seq {
        name: "wt_dffn_x1",
        area: 5.0,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"!CKN\"; }",
        inputs: &[("D", false), ("CKN", true)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
    Seq {
        name: "wt_dffr_x1",
        area: 6.0,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }",
        inputs: &[("D", false), ("CK", true), ("RN", false)],
        outputs: &[("Q", "IQ"), ("QN", "IQN")],
        extra: "",
    },
    Seq {
        name: "wt_dffs_x1",
        area: 6.0,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; preset : \"!SN\"; }",
        inputs: &[("D", false), ("CK", true), ("SN", false)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
    Seq {
        name: "wt_dffrs_x1",
        area: 7.0,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; preset : \"!SN\"; clear_preset_var1 : L; clear_preset_var2 : H; }",
        inputs: &[("D", false), ("CK", true), ("RN", false), ("SN", false)],
        outputs: &[("Q", "IQ"), ("QN", "IQN")],
        extra: "",
    },
    Seq {
        name: "wt_dffrp_x1",
        area: 6.2,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; clear : \"R\"; }",
        inputs: &[("D", false), ("CK", true), ("R", false)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
    Seq {
        name: "wt_lat_x1",
        area: 4.0,
        group: "latch (IQ, IQN) { data_in : \"D\"; enable : \"G\"; }",
        inputs: &[("D", false), ("G", true)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
    Seq {
        name: "wt_latn_x1",
        area: 4.0,
        group: "latch (IQ, IQN) { data_in : \"D\"; enable : \"!GN\"; }",
        inputs: &[("D", false), ("GN", true)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
    Seq {
        name: "wt_sdff_x1",
        area: 7.0,
        group: "ff (IQ, IQN) { next_state : \"(D&!SE)|(SI&SE)\"; clocked_on : \"CK\"; }",
        inputs: &[("D", false), ("SI", false), ("SE", false), ("CK", true)],
        outputs: &[("Q", "IQ")],
        extra: "  test_cell () { }\n",
    },
    Seq {
        name: "wt_edff_x1",
        area: 6.5,
        group: "ff (IQ, IQN) { next_state : \"(D&E)|(IQ&!E)\"; clocked_on : \"CK\"; }",
        inputs: &[("D", false), ("E", false), ("CK", true)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
];

const RESET_ONLY_SEQ: &[Seq] = &[
    Seq {
        name: "wt_dffr_x1",
        area: 6.0,
        group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }",
        inputs: &[("D", false), ("CK", true), ("RN", false)],
        outputs: &[("Q", "IQ"), ("QN", "IQN")],
        extra: "",
    },
    Seq {
        name: "wt_latn_x1",
        area: 4.0,
        group: "latch (IQ, IQN) { data_in : \"D\"; enable : \"!GN\"; }",
        inputs: &[("D", false), ("GN", true)],
        outputs: &[("Q", "IQ")],
        extra: "",
    },
];

const MINIMAL_SEQ: &[Seq] = &[Seq {
    name: "wt_dffr_x1",
    area: 6.0,
    group: "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }",
    inputs: &[("D", false), ("CK", true), ("RN", false)],
    outputs: &[("Q", "IQ")],
    extra: "",
}];

fn seq_cell(out: &mut String, s: &Seq) {
    let _ = writeln!(out, "  cell ({}) {{", s.name);
    let _ = writeln!(out, "    area : {};", s.area);
    let _ = writeln!(out, "    {}", s.group);
    out.push_str(s.extra);
    let clk = s.inputs.iter().find(|x| x.1).map_or("CK", |x| x.0);
    for (p, is_clk) in s.inputs {
        let ck = if *is_clk { " clock : true;" } else { "" };
        let _ = writeln!(
            out,
            "    pin ({p}) {{ direction : input; capacitance : 0.0018;{ck} }}"
        );
    }
    for (p, f) in s.outputs {
        let _ = writeln!(out, "    pin ({p}) {{");
        let _ = writeln!(out, "      direction : output;");
        let _ = writeln!(out, "      function : \"{f}\";");
        let _ = writeln!(out, "      timing () {{");
        let _ = writeln!(out, "        related_pin : \"{clk}\";");
        let _ = writeln!(out, "        timing_type : rising_edge;");
        table(out, "cell_rise", 0.060, 3.0);
        table(out, "cell_fall", 0.055, 3.0);
        let _ = writeln!(out, "      }}");
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "  }}");
}

/// Liberty text of a synthetic library.
pub fn library(v: Variant) -> String {
    if v == Variant::Full {
        return crate::liberty_full::library();
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "/* Synthetic library for tests. Values are invented. */"
    );
    let _ = writeln!(out, "library ({}) {{", v.name());
    let _ = writeln!(out, "  delay_model : table_lookup;");
    let _ = writeln!(out, "  time_unit : \"1ns\";");
    let _ = writeln!(out, "  voltage_unit : \"1V\";");
    let _ = writeln!(out, "  capacitive_load_unit (1, pf);");
    let _ = writeln!(out, "  lu_table_template (wt_tbl) {{");
    let _ = writeln!(out, "    variable_1 : input_net_transition;");
    let _ = writeln!(out, "    variable_2 : total_output_net_capacitance;");
    let _ = writeln!(out, "    index_1 (\"0.01, 0.1, 0.5\");");
    let _ = writeln!(out, "    index_2 (\"0.001, 0.01, 0.05\");");
    let _ = writeln!(out, "  }}");
    let (comb, seq) = match v {
        Variant::Rich => (RICH, RICH_SEQ),
        Variant::ResetOnly => (RESET_ONLY, RESET_ONLY_SEQ),
        Variant::Minimal => (MINIMAL, MINIMAL_SEQ),
        Variant::Full => unreachable!("handled above"),
    };
    for cl in comb {
        comb_cell(&mut out, cl);
    }
    for s in seq {
        seq_cell(&mut out, s);
    }
    if v == Variant::Rich {
        // Tie cells, a dont_use cell, and a two-output cell that synthesis must not use.
        out.push_str("  cell (wt_tiehi_x1) { area : 1.0; pin (H) { direction : output; function : \"1\"; } }\n");
        out.push_str("  cell (wt_tielo_x1) { area : 1.0; pin (L) { direction : output; function : \"0\"; } }\n");
        out.push_str("  cell (wt_nand2_x0) { area : 0.1; dont_use : true;\n    pin (A) { direction : input; }\n    pin (B) { direction : input; }\n    pin (Y) { direction : output; function : \"!(A&B)\"; }\n  }\n");
        out.push_str("  cell (wt_fa_x1) { area : 8.0;\n    pin (A) { direction : input; }\n    pin (B) { direction : input; }\n    pin (CI) { direction : input; }\n    pin (S) { direction : output; function : \"A^B^CI\"; }\n    pin (CO) { direction : output; function : \"(A&B)|(CI&(A^B))\"; }\n  }\n");
        out.push_str("  cell (wt_fill_x1) { area : 1.0; }\n");
    }
    out.push_str("}\n");
    out
}
