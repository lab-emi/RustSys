// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Reader rules beyond the syntax tree: attribute instances, macros, `` `default_nettype`` and
//! ports, literal values, and the bounds on what a text may make the reader allocate.

use super::tests::{cells, connections, errors, read};

// Attribute instances are read strictly, since the lexer skips their text: another form, or a
// macro that could assemble one, is rejected; 'src' attributes are accepted.
#[test]
fn reads_attribute_instances_strictly() {
    let module = "module m(input a, output y);\n  inv u (.A(a), .Y(y));\nendmodule\n";
    for attr in [
        "(* src *)",
        "(*src*)",
        "(* src = \"a.v:1\", src = \"b *) c\" *)",
        "(* \\src = \"x\" *)",
        "(* src =\n   \"x\"\n*)",
    ] {
        let g = read(&format!("{attr}\n{module}"));
        assert_eq!(connections(&g), ["u.A=a", "u.Y=y"], "{attr}");
    }
    let cases = [
        ("(* \\keep *)", "attribute 'keep' is not supported"),
        ("(* src, keep *)", "attribute 'keep' is not supported"),
        (
            "(* src = \"x\" keep *)",
            "a missing comma between attributes is not supported in a structural netlist (only \
             'src' attributes, without a value or with a string value, are accepted)",
        ),
        ("(* `K *)", "a macro inside an attribute instance"),
        ("(* src = `F *)", "a macro inside an attribute instance"),
        (
            "(* src = 1 *)",
            "an attribute value other than a string literal",
        ),
        (
            "(* src = (\"x\") *)",
            "an attribute value other than a string literal",
        ),
        ("(* 3 *)", "an attribute name that is not an identifier"),
        ("(* src /* c */ *)", "a missing comma between attributes"),
        ("(* *)", "a missing attribute name"),
        ("(* src, *)", "a missing attribute name"),
    ];
    for (attr, want) in cases {
        let text =
            format!("module m(input a, output y);\n  {attr} inv u (.A(a), .Y(y));\nendmodule\n");
        let e = errors(&text);
        assert!(
            e[0].starts_with("error[V0300]: t.v:2:3: ") && e[0].contains(want),
            "{attr}\n  want {want}\n  got  {e:?}"
        );
    }
    // A macro with arguments can assemble an attribute instance by substitution or token
    // pasting, which no text scan sees; object-like macros are read.
    for text in [
        "`define P(x) (x keep x)\nmodule m(); `P(*) endmodule\n",
        "`define P(x) (``x keep x``)\nmodule m(); `P(*) endmodule\n",
        "`define ID(n)n\nmodule m(); endmodule\n",
    ] {
        let e = errors(text);
        assert!(
            e[0].starts_with("error[V0300]: t.v:1:1: the macro '")
                && e[0].contains("with arguments is not supported"),
            "{text}\n{e:?}"
        );
    }
    let g = read(
        "`define CELL inv\n`define PIN (.A(a), .Y(y))\n\
         module m(input a, output y); `CELL u `PIN; endmodule\n",
    );
    assert_eq!(connections(&g), ["u.A=a", "u.Y=y"]);
}

// Under a `default_nettype other than wire, a port needs its net type written: ANSI ports in
// their declaration or the one they repeat, non-ANSI ports there or in a wire declaration.
// The directive inside a module is rejected.
#[test]
fn default_nettype_and_ports() {
    for text in [
        "`default_nettype tri1\nmodule m(input wire a, b, output wire y); endmodule\n",
        "`default_nettype none\nmodule m(input wire [1:0] a, \\b ); endmodule\n",
        "`default_nettype wand\nmodule m(a, y); input a; output wire y; wire a; endmodule\n",
        "`default_nettype none\nmodule m(a); input wire a; endmodule\n",
        "`default_nettype tri0\n`default_nettype wire\nmodule m(input a, b); endmodule\n",
        "`define W wire\n`default_nettype none\nmodule m(input `W a); endmodule\n",
        "module m(input a); endmodule\n`default_nettype none\n",
    ] {
        read(text);
    }
    let cases = [
        (
            "`default_nettype tri1\nmodule m(input a, output y); endmodule\n",
            "error[V0301]: t.v:2:16: port 'a' would have net type 'tri1'; only wire nets are \
             supported",
        ),
        (
            "`default_nettype none\nmodule m(input logic [1:0] a); endmodule\n",
            "error[V0301]: t.v:2:28: port 'a' has no net type, and `default_nettype none \
             requires one",
        ),
        (
            "`default_nettype none\nmodule m(input wire a, input b); endmodule\n",
            "error[V0301]: t.v:2:30: port 'b' has no net type",
        ),
        (
            "`default_nettype wand\nmodule m(a, y); input a; output wire y; endmodule\n",
            "error[V0301]: t.v:2:23: port 'a' would have net type 'wand'",
        ),
        (
            "`default_nettype none\nmodule m(a); input a; endmodule\n",
            "error[V0301]: t.v:2:20: port 'a' has no net type",
        ),
        (
            "module m(input a, output y);\n`default_nettype none\n  inv u (.A(a), .Y(x));\n  \
             assign y = x;\nendmodule\n",
            "error[V0300]: t.v:2:1: `default_nettype inside a module is not supported",
        ),
    ];
    for (text, want) in cases {
        let e = errors(text);
        assert!(
            e[0].starts_with(want),
            "{text}\n  want {want}\n  got  {e:?}"
        );
    }
    // Every port without a net type is reported.
    let e = errors("`default_nettype tri\nmodule m(input a, output [1:0] y); endmodule\n");
    assert_eq!(e.len(), 2, "{e:?}");
}

// Range bounds and bit indices: negated plain decimals are read, negated based literals are
// not (they wrap at their width); a range has at most MAX_RANGE_WIDTH bits, checked before
// any net is made.
#[test]
fn literal_and_width_rules() {
    let g = read("module m(input [3:-1] a, output y); inv u (.A(a[-1]), .Y(y)); endmodule\n");
    assert_eq!(connections(&g), ["u.A=a[-1]", "u.Y=y"]);
    let g = read("module m(input ['d0:2'd3] a); endmodule\n");
    assert_eq!(g.ports()[0].range, Some((0, 3)));
    let cases = [
        (
            "module m(input [3:-1] a, output y); inv u (.A(a[-2'd1]), .Y(y)); endmodule\n",
            "error[V0300]: t.v:1:49: the bit index '-2'd1'",
        ),
        (
            "module m(input [-2'd1:0] a); endmodule\n",
            "error[V0300]: t.v:1:17: the range bound '-2'd1'",
        ),
        (
            "module m(input [2147483646:0] a); endmodule\n",
            "error[V0303]: t.v:1:17: range [2147483646:0] is wider than 16777216 bits",
        ),
        (
            "module m(a); input [16777216:0] a; endmodule\n",
            "error[V0303]: t.v:1:21: range [16777216:0] is wider than 16777216 bits",
        ),
        (
            "module m(); wire [-2147483647:2147483647] w; endmodule\n",
            "error[V0303]: t.v:1:19: range [-2147483647:2147483647] is wider",
        ),
    ];
    for (text, want) in cases {
        let e = errors(text);
        assert!(
            e[0].starts_with(want),
            "{text}\n  want {want}\n  got  {e:?}"
        );
    }
    // The rejected declaration declares nothing, so its uses are reported as undeclared.
    let e = errors("module m(); wire [16777216:0] w; assign w[0] = 1'b0; endmodule\n");
    assert_eq!(e.len(), 2, "{e:?}");
    // All ranges together share a budget of bits (one per byte of text, at least
    // MAX_RANGE_WIDTH), here 12; a wire that repeats a port makes no nets and takes none.
    let small = |text: &str| super::read::read_with_budget(text, "t.v", &cells, 12);
    let e = small("module m(input [3:0] a, input [7:0] b); wire [3:0] c; endmodule\n")
        .unwrap_err()
        .to_string();
    assert_eq!(
        e,
        "error[V0303]: t.v:1:52: range [3:0] of 'c' exceeds the 12 bits that the ranges of this \
         text may declare together (one bit per byte of text, at least 16777216)"
    );
    let g = small("module m(a, b); input [7:0] a; input [3:0] b; wire [7:0] a; endmodule\n");
    assert_eq!(
        g.map(|g| g.net_count()).unwrap_or_else(|e| panic!("{e}")),
        12
    );
    let e = small("module m(a, b); input [7:0] a; input [7:0] b; endmodule\n").unwrap_err();
    assert!(e.to_string().contains("of 'b' exceeds the 12 bits"), "{e}");
}

// The preprocessor bounds the nesting of macro expansion but not its size: a macro may not use
// macros (each level could double the text), and the uses of macros may add at most the length
// of the text, or 1 MiB.
#[test]
fn bounds_macro_expansion() {
    let mut bomb = String::from("`define A0 x\n");
    for k in 1..=40 {
        bomb += &format!("`define A{k} `A{0} `A{0}\n", k - 1);
    }
    bomb += "module m(); wire `A40 ; endmodule\n";
    let e = errors(&bomb);
    assert!(
        e[0].starts_with(
            "error[V0300]: t.v:2:1: the macro 'A1' uses a macro or directive, which is not \
             supported in a structural netlist"
        ),
        "{e:?}"
    );
    assert_eq!(e.len(), 40, "{e:?}");
    let mut text = format!("`define BIG {}\nmodule m();\n", "x".repeat(1000));
    for _ in 0..1100 {
        text += "  `BIG\n";
    }
    text += "endmodule\n";
    let e = errors(&text);
    assert_eq!(
        e,
        [
            "error[V0300]: t.v:1050:3: macro uses that add more than 1048576 bytes are not \
          supported in a structural netlist (the uses of macros may add the length of the text, \
          or 1048576 bytes)"
        ]
    );
}

// The parser reads a min:typ:max expression as its typical value and drops the parentheses
// around an operand, so the reader finds both in the tokens and rejects them: no operand (an
// x/z literal, an undeclared name) is dropped unseen, and a parenthesized name is not taken for
// a net. A conditional operator is not min:typ:max, and a macro cannot hide a parenthesis.
#[test]
fn rejects_what_the_parser_reads_away() {
    let header = "module top(input a, input [1:0] b, input [3:-1] c, output y);\n  wire w;\n";
    let mtm = "a min:typ:max expression is not supported in a structural netlist";
    let paren = "a parenthesized expression is not supported in a structural netlist";
    let cases = [
        ("inv u (.A((1'bx:a:1'bz)), .Y(y));", "3:13", mtm),
        ("inv u (.A(a), .Y((q:y:r)));", "3:20", mtm),
        ("assign y = (1'b0:1'b1:1'b0);", "3:14", mtm),
        ("assign y = (a & b[0]:a:$random);", "3:14", mtm),
        ("assign y = b[(0:1:0)];", "3:16", mtm),
        ("inv u (.A(a), .Y((y)));", "3:20", paren),
        ("inv u (.A(((a))), .Y(y));", "3:13", paren),
        ("assign y = ((a));", "3:14", paren),
        ("assign w = (b[1]);", "3:14", paren),
        ("inv u (.A((b)[0]), .Y(y));", "3:13", paren),
        ("assign y = b[(1)];", "3:16", paren),
        ("assign y = c[-(1)];", "3:17", paren),
        ("inv u (.A((a ? b[1] : w)), .Y(y));", "3:13", paren),
    ];
    for (body, at, want) in cases {
        let text = format!("{header}  {body}\nendmodule\n");
        assert_eq!(
            errors(&text),
            [format!("error[V0300]: t.v:{at}: {want}")],
            "{body}"
        );
    }
    for (text, want) in [
        (
            "module m(input [(7:1:9):0] a); endmodule\n",
            format!("error[V0300]: t.v:1:17: {mtm}"),
        ),
        (
            "module m((p:a:q), y); input a; output y; endmodule\n",
            format!("error[V0300]: t.v:1:10: {mtm}"),
        ),
        (
            "module m((a), y); input a; output y; endmodule\n",
            format!("error[V0300]: t.v:1:10: {paren}"),
        ),
        (
            "`define P (a)\nmodule m(input a, output y); inv u (.A(`P), .Y(y)); endmodule\n",
            format!("error[V0300]: t.v:2:40: {paren}"),
        ),
    ] {
        assert_eq!(errors(text), [want], "{text}");
    }
    // Every one is reported, in source order.
    let e = errors(
        "module m(input a, output y); inv u (.A((1'bx:a:1'bz)), .Y((q:y:r)));\n  \
         assign y = (1'b0:1'b1:1'b0);\nendmodule\n",
    );
    assert_eq!(e.len(), 3, "{e:?}");
    assert!(e[0].contains("t.v:1:40") && e[1].contains("t.v:1:59") && e[2].contains("t.v:2:14"));
    // A conditional operator is an expression, without parentheses too.
    let e = errors("module m(input a, s, output y); inv u (.A(s ? a : y), .Y(y)); endmodule\n");
    assert_eq!(
        e,
        [
            "error[V0300]: t.v:1:43: the expression 's ? a : y' is not supported in a structural \
          netlist"
        ]
    );
}
