//! A small JSON encoder and decoder for [`Data`].

use crate::value::Data;

pub fn encode(d: &Data) -> String {
    let mut out = String::new();
    write(d, &mut out);
    out
}

fn write(d: &Data, out: &mut String) {
    match d {
        Data::Nil => out.push_str("null"),
        Data::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Data::Int(n) => out.push_str(&n.to_string()),
        Data::Float(x) if x.is_finite() => out.push_str(&crate::value::fmt_float(*x)),
        Data::Float(_) => out.push_str("null"),
        Data::Str(s) => string(s, out),
        Data::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        Data::Map(entries) => {
            out.push('{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(k, out);
                out.push(':');
                write(v, out);
            }
            out.push('}');
        }
    }
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn decode(s: &str) -> Result<Data, String> {
    let mut p = Parser {
        b: s.as_bytes(),
        i: 0,
        depth: 0,
    };
    let d = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(d)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("invalid JSON at byte {}: {what}", self.i)
    }

    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &str, d: Data) -> Result<Data, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(d)
        } else {
            Err(self.err("unexpected token"))
        }
    }

    fn value(&mut self) -> Result<Data, String> {
        self.ws();
        match self.b.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'n') => self.lit("null", Data::Nil),
            Some(b't') => self.lit("true", Data::Bool(true)),
            Some(b'f') => self.lit("false", Data::Bool(false)),
            Some(b'"') => Ok(Data::Str(self.string()?)),
            Some(b'[') => {
                self.nest()?;
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        items.push(self.value()?);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err("expected `,` or `]`")),
                        }
                    }
                }
                self.depth -= 1;
                Ok(Data::List(items))
            }
            Some(b'{') => {
                self.nest()?;
                self.i += 1;
                let mut entries: Vec<(String, Data)> = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        if self.b.get(self.i) != Some(&b'"') {
                            return Err(self.err("expected a key"));
                        }
                        let k = self.string()?;
                        self.ws();
                        if self.b.get(self.i) != Some(&b':') {
                            return Err(self.err("expected `:`"));
                        }
                        self.i += 1;
                        let v = self.value()?;
                        match entries.iter_mut().find(|(e, _)| *e == k) {
                            Some(e) => e.1 = v,
                            None => entries.push((k, v)),
                        }
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err("expected `,` or `}`")),
                        }
                    }
                }
                self.depth -= 1;
                Ok(Data::Map(entries))
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn nest(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > 200 {
            Err(self.err("nested too deeply"))
        } else {
            Ok(())
        }
    }

    fn number(&mut self) -> Result<Data, String> {
        let start = self.i;
        let mut float = false;
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b'0'..=b'9' | b'-' | b'+' => {}
                b'.' | b'e' | b'E' => float = true,
                _ => break,
            }
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        if !float && let Ok(n) = text.parse::<i64>() {
            return Ok(Data::Int(n));
        }
        text.parse::<f64>().map(Data::Float).map_err(|_| self.err("bad number"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.b.get(self.i).copied() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return String::from_utf8(out).map_err(|_| self.err("invalid UTF-8"));
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = match self.b.get(self.i).copied() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'n') => '\n',
                        Some(b't') => '\t',
                        Some(b'r') => '\r',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'u') => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) {
                                if self.b.get(self.i + 1) != Some(&b'\\') || self.b.get(self.i + 2) != Some(&b'u') {
                                    return Err(self.err("unpaired surrogate"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                let code = 0x10000 + ((hi - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                                char::from_u32(code).ok_or_else(|| self.err("bad surrogate pair"))?
                            } else {
                                char::from_u32(hi).ok_or_else(|| self.err("bad escape"))?
                            }
                        }
                        _ => return Err(self.err("bad escape")),
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    self.i += 1;
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// Reads the 4 hex digits after `\u`; leaves `i` on the last one.
    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .b
            .get(self.i + 1..self.i + 5)
            .ok_or_else(|| self.err("bad \\u escape"))?;
        let s = std::str::from_utf8(digits).map_err(|_| self.err("bad \\u escape"))?;
        let n = u32::from_str_radix(s, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let src = r#"{"a":[1,2.5,"x\né😀"],"b":null,"c":true,"d":{}}"#;
        let d = decode(src).unwrap();
        assert_eq!(decode(&encode(&d)).unwrap(), d);
        assert_eq!(
            d,
            Data::Map(vec![
                (
                    "a".into(),
                    Data::List(vec![Data::Int(1), Data::Float(2.5), Data::Str("x\né😀".into())])
                ),
                ("b".into(), Data::Nil),
                ("c".into(), Data::Bool(true)),
                ("d".into(), Data::Map(vec![])),
            ])
        );
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "[1,", "{\"a\" 1}", "tru", "[1] x", "\"abc", &"[".repeat(500)] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }
}
