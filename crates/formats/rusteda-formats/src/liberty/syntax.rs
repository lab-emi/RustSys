// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Generic Liberty syntax tree (LIBERTY-REQ-0001 to LIBERTY-REQ-0004). Every group and attribute
//! records the 1-based line and column of its name.

use super::Error;

/// Maximum group nesting depth (LIBERTY-REQ-0004).
pub const MAX_DEPTH: usize = 64;

/// A group statement: `kind ( args ) { ... }`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group {
    pub kind: String,
    pub args: Vec<String>,
    pub attrs: Vec<Attr>,
    pub groups: Vec<Group>,
    pub line: u32,
    pub col: u32,
}

/// An attribute statement.
#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    pub name: String,
    pub value: AttrValue,
    pub line: u32,
    pub col: u32,
}

/// Simple (`name : value ;`) or complex (`name ( v, ... ) ;`) attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Simple(String),
    Complex(Vec<String>),
}

impl Group {
    /// First simple attribute with the given name.
    pub fn simple(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find_map(|a| match &a.value {
            AttrValue::Simple(v) if a.name == name => Some(v.as_str()),
            _ => None,
        })
    }

    /// First complex attribute with the given name.
    pub fn complex(&self, name: &str) -> Option<&[String]> {
        self.attrs.iter().find_map(|a| match &a.value {
            AttrValue::Complex(v) if a.name == name => Some(v.as_slice()),
            _ => None,
        })
    }

    /// First attribute with the given name, simple or complex.
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }

    /// Child groups of one kind.
    pub fn children<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Group> + 'a {
        self.groups.iter().filter(move |g| g.kind == kind)
    }
}

struct Scanner<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    file: &'a str,
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'_' | b'.'
                | b'['
                | b']'
                | b'!'
                | b'\''
                | b'+'
                | b'-'
                | b'*'
                | b'/'
                | b'<'
                | b'>'
                | b'&'
                | b'|'
                | b'^'
                | b'$'
                | b'#'
                | b'@'
                | b'%'
                | b'='
                | b'~'
        )
}

impl<'a> Scanner<'a> {
    fn err(&self, msg: impl Into<String>) -> Error {
        Error::Syntax {
            file: self.file.to_string(),
            line: self.line,
            col: self.col,
            msg: msg.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<u8> {
        self.src.get(self.pos + off).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    /// Skips blanks, comments and line continuations. Newlines are skipped only if `newlines`.
    /// Returns true if a newline was crossed.
    fn skip_ws(&mut self, newlines: bool) -> Result<bool, Error> {
        let mut crossed = false;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | 0x0c) => {
                    self.bump();
                }
                Some(b'\n') => {
                    if !newlines {
                        return Ok(crossed);
                    }
                    crossed = true;
                    self.bump();
                }
                Some(b'\\') => {
                    // Line continuation: backslash, optional blanks, newline.
                    let mut k = 1;
                    while matches!(self.peek_at(k), Some(b' ' | b'\t' | b'\r')) {
                        k += 1;
                    }
                    if self.peek_at(k) == Some(b'\n') {
                        for _ in 0..=k {
                            self.bump();
                        }
                    } else {
                        return Ok(crossed);
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.bump();
                    self.bump();
                    loop {
                        match self.peek() {
                            None => return Err(self.err("unterminated comment")),
                            Some(b'*') if self.peek_at(1) == Some(b'/') => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            Some(b'\n') => {
                                crossed = true;
                                self.bump();
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.bump();
                    }
                }
                _ => return Ok(crossed),
            }
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        // Opening quote already peeked.
        self.bump();
        let mut out = Vec::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => break,
                Some(b'\\') => match self.peek() {
                    Some(b'\n') => {
                        self.bump();
                    }
                    Some(b'\r') if self.peek_at(1) == Some(b'\n') => {
                        self.bump();
                        self.bump();
                    }
                    Some(b'"') => {
                        self.bump();
                        out.push(b'"');
                    }
                    Some(b'\\') => {
                        self.bump();
                        out.push(b'\\');
                    }
                    _ => out.push(b'\\'),
                },
                Some(b) => out.push(b),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn word(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_word_byte(b) {
                // Stop before a comment start.
                if b == b'/' && matches!(self.peek_at(1), Some(b'*' | b'/')) {
                    break;
                }
                self.bump();
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.src[start..self.pos]).into_owned()
    }

    /// One argument inside `( ... )`: a string, or the text up to `,` or `)`.
    fn arg(&mut self) -> Result<String, Error> {
        self.skip_ws(true)?;
        if self.peek() == Some(b'"') {
            return self.string();
        }
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unexpected end of file in argument list")),
                Some(b',' | b')') => break,
                Some(b'"') => text.push_str(&self.string()?),
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'\\') => {
                    self.skip_ws(true)?;
                    if !matches!(self.peek(), Some(b',' | b')')) && !text.is_empty() {
                        text.push(' ');
                    }
                    if self.peek() == Some(b'\\') {
                        // A lone backslash that is not a continuation.
                        text.push('\\');
                        self.bump();
                    }
                }
                Some(b'/') if matches!(self.peek_at(1), Some(b'*' | b'/')) => {
                    self.skip_ws(true)?;
                }
                Some(b) if is_word_byte(b) => text.push_str(&self.word()),
                Some(b) => {
                    return Err(
                        self.err(format!("unexpected character '{}' in argument", b as char))
                    );
                }
            }
        }
        Ok(text.trim().to_string())
    }

    /// Value of a simple attribute: text up to `;` or the end of the line.
    fn simple_value(&mut self) -> Result<String, Error> {
        self.skip_ws(false)?;
        let mut text = String::new();
        let mut only_string: Option<String> = None;
        let mut parts = 0;
        loop {
            match self.peek() {
                None => break,
                Some(b';') => {
                    self.bump();
                    break;
                }
                Some(b'\n') => break,
                Some(b'}') => break,
                Some(b'"') => {
                    let s = self.string()?;
                    text.push_str(&s);
                    parts += 1;
                    only_string = if parts == 1 { Some(s) } else { None };
                }
                Some(b' ' | b'\t' | b'\r' | b'\\') => {
                    let before = self.pos;
                    let crossed = self.skip_ws(false)?;
                    if crossed {
                        break;
                    }
                    if self.pos == before {
                        // Lone backslash.
                        text.push('\\');
                        self.bump();
                    } else if !matches!(self.peek(), Some(b';' | b'\n') | None) {
                        text.push(' ');
                    }
                }
                Some(b'/') if matches!(self.peek_at(1), Some(b'*' | b'/')) => {
                    if self.skip_ws(false)? {
                        break;
                    }
                }
                Some(b) if is_word_byte(b) => {
                    let w = self.word();
                    text.push_str(&w);
                    parts += 1;
                    only_string = None;
                }
                Some(b) => {
                    // Punctuation inside values (for example parentheses in expressions).
                    self.bump();
                    text.push(b as char);
                    parts += 1;
                    only_string = None;
                }
            }
        }
        Ok(only_string.unwrap_or_else(|| text.trim().to_string()))
    }

    fn statements(&mut self, parent: &mut Group, depth: usize) -> Result<(), Error> {
        loop {
            self.skip_ws(true)?;
            match self.peek() {
                None => {
                    if depth == 0 {
                        return Ok(());
                    }
                    return Err(self.err("unexpected end of file: missing '}'"));
                }
                Some(b'}') => {
                    if depth == 0 {
                        return Err(self.err("unexpected '}'"));
                    }
                    self.bump();
                    return Ok(());
                }
                Some(b';') => {
                    self.bump();
                }
                Some(b) if is_word_byte(b) || b == b'"' => {
                    let (line, col) = (self.line, self.col);
                    let name = if b == b'"' {
                        self.string()?
                    } else {
                        self.word()
                    };
                    self.skip_ws(true)?;
                    match self.peek() {
                        Some(b':') => {
                            self.bump();
                            let value = self.simple_value()?;
                            parent.attrs.push(Attr {
                                name,
                                value: AttrValue::Simple(value),
                                line,
                                col,
                            });
                        }
                        Some(b'(') => {
                            self.bump();
                            let mut args = Vec::new();
                            self.skip_ws(true)?;
                            if self.peek() == Some(b')') {
                                self.bump();
                            } else {
                                loop {
                                    args.push(self.arg()?);
                                    self.skip_ws(true)?;
                                    match self.bump() {
                                        Some(b',') => continue,
                                        Some(b')') => break,
                                        _ => return Err(self.err("expected ',' or ')'")),
                                    }
                                }
                            }
                            // Group or complex attribute.
                            let _ = self.skip_ws(false)?;
                            let save = (self.pos, self.line, self.col);
                            self.skip_ws(true)?;
                            if self.peek() == Some(b'{') {
                                self.bump();
                                if depth + 1 > MAX_DEPTH {
                                    return Err(self.err("groups nested too deeply"));
                                }
                                let mut g = Group {
                                    kind: name,
                                    args,
                                    line,
                                    col,
                                    ..Group::default()
                                };
                                self.statements(&mut g, depth + 1)?;
                                parent.groups.push(g);
                            } else {
                                (self.pos, self.line, self.col) = save;
                                if self.peek() == Some(b';') {
                                    self.bump();
                                }
                                parent.attrs.push(Attr {
                                    name,
                                    value: AttrValue::Complex(args),
                                    line,
                                    col,
                                });
                            }
                        }
                        _ => return Err(self.err(format!("expected ':' or '(' after '{name}'"))),
                    }
                }
                Some(b) => return Err(self.err(format!("unexpected character '{}'", b as char))),
            }
        }
    }
}

/// Parses Liberty text into a root group whose children are the file's top-level statements.
pub fn parse(text: &str, file: &str) -> Result<Group, Error> {
    let mut sc = Scanner {
        src: text.as_bytes(),
        pos: 0,
        line: 1,
        col: 1,
        file,
    };
    let mut root = Group {
        kind: String::from("<file>"),
        ..Group::default()
    };
    sc.statements(&mut root, 0)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    // LIBERTY-REQ-0001, LIBERTY-REQ-0002, LIBERTY-REQ-0003
    #[test]
    fn groups_and_attributes() {
        let text = r#"
/* header */ library (lib1) {
  time_unit : "1ns" ;
  capacitive_load_unit (1, pf);
  nom_voltage : 1.2
  cell (INV) { // comment
    area : 1.5;
    pin (A) { direction : input; capacitance : 0.002; }
    pin (Y) { direction : output; function : "!A";
      timing () { related_pin : "A";
        cell_rise (tmpl) { index_1 ("0.1, 0.2"); values ("1, 2", \
                                                        "3, 4"); }
      }
    }
  }
}
"#;
        let root = parse(text, "t.lib").unwrap();
        let lib = &root.groups[0];
        assert_eq!(lib.kind, "library");
        assert_eq!(lib.args, vec!["lib1"]);
        assert_eq!(lib.simple("time_unit"), Some("1ns"));
        assert_eq!(lib.simple("nom_voltage"), Some("1.2"));
        assert_eq!(
            lib.complex("capacitive_load_unit").unwrap(),
            &["1".to_string(), "pf".to_string()]
        );
        let cell = lib.children("cell").next().unwrap();
        assert_eq!(cell.simple("area"), Some("1.5"));
        let y = cell.children("pin").nth(1).unwrap();
        assert_eq!(y.simple("function"), Some("!A"));
        let t = y.children("timing").next().unwrap();
        assert!(t.args.is_empty());
        let cr = t.children("cell_rise").next().unwrap();
        assert_eq!(cr.complex("values").unwrap().len(), 2);
    }

    // LIBERTY-REQ-0004
    #[test]
    fn malformed_inputs_are_errors() {
        for bad in [
            "library (x) {",
            "library (x) { a : 1; } }",
            "cell (\"x) {}",
            "a ( b c",
            "/* x",
        ] {
            assert!(parse(bad, "bad.lib").is_err(), "{bad}");
        }
        let deep = "g () {".repeat(70) + &"}".repeat(70);
        assert!(parse(&deep, "deep.lib").is_err());
    }

    #[test]
    fn unquoted_multiword_simple_value() {
        let root = parse("x : a & b ;\ny : (c + d)' ;", "f").unwrap();
        assert_eq!(root.simple("x"), Some("a & b"));
        assert_eq!(root.simple("y"), Some("(c + d)'"));
    }
}
