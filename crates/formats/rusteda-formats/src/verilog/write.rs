// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Structural netlist writer (VERILOG-REQ-0300, VERILOG-REQ-0302).

use rayon::prelude::*;
use std::fmt::Write as _;

use crate::verilog::token::keyword;

/// Port direction in a netlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NlDir {
    Input,
    Output,
}

/// A netlist port; `width == None` for scalars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlPort {
    pub name: String,
    pub dir: NlDir,
    pub width: Option<u32>,
}

/// A reference to a net bit or a constant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NlRef {
    /// A scalar net (or a whole port).
    Net(String),
    /// Bit `k` of a vector net.
    Bit(String, u32),
    Const(bool),
}

/// A cell instance with named connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlInstance {
    pub cell: String,
    pub name: String,
    pub conns: Vec<(String, NlRef)>,
}

/// A flat structural module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NlModule {
    pub name: String,
    pub ports: Vec<NlPort>,
    /// Internal wires; `None` width for scalars.
    pub wires: Vec<(String, Option<u32>)>,
    pub instances: Vec<NlInstance>,
    /// `assign lhs = rhs;`
    pub assigns: Vec<(NlRef, NlRef)>,
}

/// True if `name` can be written without escaping.
pub fn is_simple_ident(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$')
        && keyword(name).is_none()
}

/// Writes a name, escaping it when necessary (backslash, name, one space).
pub fn ident(name: &str) -> String {
    if is_simple_ident(name) {
        name.to_string()
    } else {
        format!("\\{name} ")
    }
}

fn write_ident(out: &mut impl std::fmt::Write, name: &str) -> std::fmt::Result {
    if is_simple_ident(name) {
        out.write_str(name)
    } else {
        write!(out, "\\{name} ")
    }
}

fn reference(out: &mut impl std::fmt::Write, r: &NlRef) -> std::fmt::Result {
    match r {
        NlRef::Net(n) => write_ident(out, n),
        NlRef::Bit(n, k) => {
            write_ident(out, n)?;
            write!(out, "[{k}]")
        }
        NlRef::Const(false) => out.write_str("1'b0"),
        NlRef::Const(true) => out.write_str("1'b1"),
    }
}

fn instance(out: &mut impl std::fmt::Write, inst: &NlInstance) -> std::fmt::Result {
    out.write_str("  ")?;
    write_ident(out, &inst.cell)?;
    out.write_char(' ')?;
    write_ident(out, &inst.name)?;
    out.write_str(" (")?;
    for (i, (pin, r)) in inst.conns.iter().enumerate() {
        if i != 0 {
            out.write_str(", ")?;
        }
        out.write_char('.')?;
        write_ident(out, pin)?;
        out.write_char('(')?;
        reference(out, r)?;
        out.write_char(')')?;
    }
    out.write_str(");\n")
}

/// At most16 temporary chunks of2MiB; oversized chunks use the ordered serial writer.
/// The final output already required by this API is O(the explicit netlist text), never paths.
struct Chunk(String);
impl std::fmt::Write for Chunk {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        const LIMIT: usize = 2 * 1024 * 1024;
        let needed = self
            .0
            .len()
            .checked_add(value.len())
            .ok_or(std::fmt::Error)?;
        if needed > LIMIT {
            return Err(std::fmt::Error);
        }
        if needed > self.0.capacity() {
            let capacity = needed.next_power_of_two().min(LIMIT);
            self.0.reserve_exact(capacity - self.0.len());
        }
        self.0.push_str(value);
        Ok(())
    }
}

/// Renders a module as Verilog text.
pub fn write_netlist(m: &NlModule) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "module {}(", ident(&m.name));
    for (i, p) in m.ports.iter().enumerate() {
        let dir = match p.dir {
            NlDir::Input => "input",
            NlDir::Output => "output",
        };
        let range = p
            .width
            .map(|w| format!(" [{}:0]", w.saturating_sub(1)))
            .unwrap_or_default();
        let sep = if i + 1 == m.ports.len() { "" } else { "," };
        let _ = writeln!(s, "  {dir} wire{range} {}{sep}", ident(&p.name));
    }
    let _ = writeln!(s, ");");
    for (w, width) in &m.wires {
        let range = width
            .map(|w| format!(" [{}:0]", w.saturating_sub(1)))
            .unwrap_or_default();
        let _ = writeln!(s, "  wire{range} {};", ident(w));
    }
    for batch in m.instances.chunks(8192) {
        if batch.len() >= 1024 && rayon::current_num_threads() > 1 {
            let chunks: Vec<_> = batch
                .par_chunks(512)
                .map(|part| {
                    let mut out = Chunk(String::with_capacity(65_536));
                    for inst in part {
                        instance(&mut out, inst).ok()?;
                    }
                    Some(out.0)
                })
                .collect();
            for (part, chunk) in batch.chunks(512).zip(chunks) {
                if let Some(chunk) = chunk {
                    s.push_str(&chunk);
                } else {
                    for inst in part {
                        let _ = instance(&mut s, inst);
                    }
                }
            }
        } else {
            for inst in batch {
                let _ = instance(&mut s, inst);
            }
        }
    }
    for (l, r) in &m.assigns {
        let _ = s.write_str("  assign ");
        let _ = reference(&mut s, l);
        let _ = s.write_str(" = ");
        let _ = reference(&mut s, r);
        let _ = s.write_str(";\n");
    }
    let _ = writeln!(s, "endmodule");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verilog::ast::{Item, ModuleItem, PortConn, PortList};

    //2048 explicit cells/<2MiB input, <=32MiB bounded parallel text scratch.
    #[test]
    fn parallel_instance_text_matches_serial_and_chunk_limit_is_hard() {
        let mut m = NlModule {
            name: "module".into(),
            ..Default::default()
        };
        for i in 0..2048 {
            m.instances.push(NlInstance {
                cell: "inv".into(),
                name: format!("unit.{i}"),
                conns: vec![
                    ("A".into(), NlRef::Bit("bus.a".into(), i)),
                    ("Y".into(), NlRef::Net(format!("net.{i}"))),
                ],
            });
        }
        let render = |threads| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| write_netlist(&m))
        };
        assert_eq!(render(1), render(16));
        let mut chunk = Chunk(String::new());
        assert!(chunk.write_str(&"a".repeat(2 * 1024 * 1024 + 1)).is_err());
        assert!(chunk.0.is_empty());
    }

    // VERILOG-REQ-0300 to VERILOG-REQ-0302
    #[test]
    fn escaping_and_round_trip() {
        assert_eq!(ident("a_1"), "a_1");
        assert_eq!(ident("u.q[3]"), "\\u.q[3] ");
        assert_eq!(ident("module"), "\\module ");
        let m = NlModule {
            name: "top".into(),
            ports: vec![
                NlPort {
                    name: "a".into(),
                    dir: NlDir::Input,
                    width: Some(2),
                },
                NlPort {
                    name: "y".into(),
                    dir: NlDir::Output,
                    width: None,
                },
            ],
            wires: vec![("u.q[0]".into(), None), ("n1".into(), None)],
            instances: vec![NlInstance {
                cell: "nd2".into(),
                name: "c0".into(),
                conns: vec![
                    ("A".into(), NlRef::Bit("a".into(), 0)),
                    ("B".into(), NlRef::Net("u.q[0]".into())),
                    ("Y".into(), NlRef::Net("n1".into())),
                ],
            }],
            assigns: vec![(NlRef::Net("y".into()), NlRef::Net("n1".into()))],
        };
        let text = write_netlist(&m);
        let unit = crate::verilog::test_util::parse_src(&text);
        let Item::Module(md) = &unit.items[0] else {
            panic!()
        };
        assert_eq!(md.name.name, "top");
        let PortList::Ansi(ports) = &md.ports else {
            panic!()
        };
        assert_eq!(ports.len(), 2);
        let inst = md.items.iter().find_map(|i| match i {
            ModuleItem::Instance(x) => Some(x),
            _ => None,
        });
        let inst = inst.unwrap();
        assert_eq!(inst.module.name, "nd2");
        let PortConn::Named(pin, Some(Some(e))) = &inst.instances[0].conns[1] else {
            panic!()
        };
        assert_eq!(pin.name, "B");
        assert_eq!(e.as_ident().unwrap().name, "u.q[0]");
        assert!(md.items.iter().any(|i| matches!(i, ModuleItem::Assign(_))));
    }
}
