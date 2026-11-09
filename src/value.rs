//! Values, the heap they live on, and the garbage collector.
//!
//! Every object lives in one arena and is addressed by its index, a [`Ref`]. There are no pointers anywhere in the
//! VM: that is what makes its whole state trivial to write to disk and read back, on any machine.

use std::collections::HashMap;
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ref(pub u32);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Obj(Ref),
    /// A built-in function, by its index in [`crate::builtins::BUILTINS`].
    Native(u16),
}

impl Value {
    pub fn truthy(self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

#[derive(Debug, Clone)]
pub enum Obj {
    Str(Box<str>),
    List(Vec<Value>),
    Map(Map),
    Closure {
        func: u32,
        captures: Vec<Ref>,
    },
    /// A variable captured by a closure. Captured variables live in cells so every closure sees the same one.
    Cell(Value),
}

/// A map with string keys that remembers insertion order, so iterating it is deterministic.
#[derive(Debug, Clone, Default)]
pub struct Map {
    entries: Vec<(Box<str>, Value)>,
    index: HashMap<Box<str>, usize>,
}

impl Map {
    pub fn get(&self, key: &str) -> Option<Value> {
        self.index.get(key).map(|&i| self.entries[i].1)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        match self.index.get(key) {
            Some(&i) => self.entries[i].1 = value,
            None => {
                self.index.insert(key.into(), self.entries.len());
                self.entries.push((key.into(), value));
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let i = self.index.remove(key)?;
        let (_, v) = self.entries.remove(i);
        for slot in self.index.values_mut() {
            if *slot > i {
                *slot -= 1;
            }
        }
        Some(v)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, Value)> {
        self.entries.iter().map(|(k, v)| (&**k, *v))
    }
}

/// A value detached from any heap: what goes in and out of the VM through effects.
#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Data>),
    Map(Vec<(String, Data)>),
}

impl Data {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Data::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Data::Int(n) => Some(n as f64),
            Data::Float(x) => Some(x),
            _ => None,
        }
    }

    /// The value as Spola source, e.g. `{"a": [1, "x"]}`.
    pub fn to_source(&self) -> String {
        let mut out = String::new();
        self.write_source(&mut out);
        out
    }

    fn write_source(&self, out: &mut String) {
        match self {
            Data::Nil => out.push_str("nil"),
            Data::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Data::Int(n) => out.push_str(&n.to_string()),
            Data::Float(x) => out.push_str(&fmt_float(*x)),
            Data::Str(s) => quote(s, out),
            Data::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_source(out);
                }
                out.push(']');
            }
            Data::Map(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    quote(k, out);
                    out.push_str(": ");
                    v.write_source(out);
                }
                out.push('}');
            }
        }
    }
}

pub fn fmt_float(x: f64) -> String {
    if x.is_finite() && x == x.trunc() && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// Writes `s` as a double-quoted string literal that the lexer reads back unchanged.
pub fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            c => out.push(c),
        }
    }
    out.push('"');
}

const MIN_GC_THRESHOLD: usize = 4096;

#[derive(Debug, Clone, Default)]
pub struct Heap {
    pub(crate) objs: Vec<Option<Obj>>,
    free: Vec<u32>,
    live: usize,
    next_gc: usize,
}

impl Heap {
    pub fn new() -> Self {
        Heap {
            objs: Vec::new(),
            free: Vec::new(),
            live: 0,
            next_gc: MIN_GC_THRESHOLD,
        }
    }

    /// Rebuilds a heap from its slots, as read from a checkpoint.
    pub(crate) fn from_slots(objs: Vec<Option<Obj>>) -> Self {
        let free = objs
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, o)| o.is_none())
            .map(|(i, _)| i as u32)
            .collect();
        let live = objs.iter().filter(|o| o.is_some()).count();
        Heap {
            objs,
            free,
            live,
            next_gc: (live * 2).max(MIN_GC_THRESHOLD),
        }
    }

    pub fn alloc(&mut self, obj: Obj) -> Ref {
        self.live += 1;
        match self.free.pop() {
            Some(i) => {
                self.objs[i as usize] = Some(obj);
                Ref(i)
            }
            None => {
                self.objs.push(Some(obj));
                Ref(self.objs.len() as u32 - 1)
            }
        }
    }

    pub fn get(&self, r: Ref) -> &Obj {
        self.objs[r.0 as usize].as_ref().expect("dangling reference")
    }

    pub fn get_mut(&mut self, r: Ref) -> &mut Obj {
        self.objs[r.0 as usize].as_mut().expect("dangling reference")
    }

    pub fn live(&self) -> usize {
        self.live
    }

    pub fn should_collect(&self) -> bool {
        self.live >= self.next_gc
    }

    pub fn str(&mut self, s: &str) -> Value {
        Value::Obj(self.alloc(Obj::Str(s.into())))
    }

    /// The string behind `v`, if it is one.
    pub fn as_str(&self, v: Value) -> Option<&str> {
        match v {
            Value::Obj(r) => match self.get(r) {
                Obj::Str(s) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }

    /// Mark and sweep from `roots`. Afterwards only reachable objects remain.
    pub fn collect(&mut self, roots: impl Iterator<Item = Value>) {
        let mut marked = vec![false; self.objs.len()];
        let mut work: Vec<Ref> = roots
            .filter_map(|v| if let Value::Obj(r) = v { Some(r) } else { None })
            .collect();
        while let Some(r) = work.pop() {
            let i = r.0 as usize;
            if marked[i] {
                continue;
            }
            marked[i] = true;
            let push = |v: &Value, work: &mut Vec<Ref>| {
                if let Value::Obj(r) = v {
                    work.push(*r)
                }
            };
            match self.objs[i].as_ref().expect("dangling reference") {
                Obj::Str(_) => {}
                Obj::List(items) => items.iter().for_each(|v| push(v, &mut work)),
                Obj::Map(m) => m.entries.iter().for_each(|(_, v)| push(v, &mut work)),
                Obj::Closure { captures, .. } => work.extend(captures.iter().copied()),
                Obj::Cell(v) => push(v, &mut work),
            }
        }
        for (i, slot) in self.objs.iter_mut().enumerate() {
            if slot.is_some() && !marked[i] {
                *slot = None;
                self.free.push(i as u32);
                self.live -= 1;
            }
        }
        // keep handing out low indices first, so checkpoints stay compact
        self.free.sort_unstable_by(|a, b| b.cmp(a));
        while matches!(self.objs.last(), Some(None)) {
            self.objs.pop();
        }
        let len = self.objs.len() as u32;
        self.free.retain(|&i| i < len);
        self.next_gc = (self.live * 2).max(MIN_GC_THRESHOLD);
    }

    /// Structural equality: strings, lists and maps compare by content, functions by identity.
    pub fn equal(&self, a: Value, b: Value) -> bool {
        self.equal_depth(a, b, 0)
    }

    fn equal_depth(&self, a: Value, b: Value, depth: u32) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => x as f64 == y,
            (Value::Obj(x), Value::Obj(y)) => {
                if x == y {
                    return true;
                }
                if depth > 100 {
                    return false;
                }
                match (self.get(x), self.get(y)) {
                    (Obj::Str(p), Obj::Str(q)) => p == q,
                    (Obj::List(p), Obj::List(q)) => {
                        p.len() == q.len() && p.iter().zip(q).all(|(a, b)| self.equal_depth(*a, *b, depth + 1))
                    }
                    (Obj::Map(p), Obj::Map(q)) => {
                        p.len() == q.len()
                            && p.iter()
                                .all(|(k, v)| q.get(k).is_some_and(|w| self.equal_depth(v, w, depth + 1)))
                    }
                    _ => false,
                }
            }
            _ => a == b,
        }
    }

    pub fn type_name(&self, v: Value) -> &'static str {
        match v {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Native(_) => "function",
            Value::Obj(r) => match self.get(r) {
                Obj::Str(_) => "string",
                Obj::List(_) => "list",
                Obj::Map(_) => "map",
                Obj::Closure { .. } | Obj::Cell(_) => "function",
            },
        }
    }

    /// Copies a value out of the heap.
    pub fn to_data(&self, v: Value) -> Result<Data, String> {
        self.to_data_depth(v, 0)
    }

    fn to_data_depth(&self, v: Value, depth: u32) -> Result<Data, String> {
        if depth > 100 {
            return Err("value is nested too deeply (or contains itself)".into());
        }
        Ok(match v {
            Value::Nil => Data::Nil,
            Value::Bool(b) => Data::Bool(b),
            Value::Int(n) => Data::Int(n),
            Value::Float(x) => Data::Float(x),
            Value::Native(_) => return Err("a function cannot leave the program".into()),
            Value::Obj(r) => match self.get(r) {
                Obj::Str(s) => Data::Str(s.to_string()),
                Obj::List(items) => Data::List(
                    items
                        .iter()
                        .map(|v| self.to_data_depth(*v, depth + 1))
                        .collect::<Result<_, _>>()?,
                ),
                Obj::Map(m) => Data::Map(
                    m.iter()
                        .map(|(k, v)| Ok((k.to_string(), self.to_data_depth(v, depth + 1)?)))
                        .collect::<Result<_, String>>()?,
                ),
                Obj::Closure { .. } | Obj::Cell(_) => return Err("a function cannot leave the program".into()),
            },
        })
    }

    /// Copies a value into the heap.
    pub fn alloc_data(&mut self, d: &Data) -> Value {
        match d {
            Data::Nil => Value::Nil,
            Data::Bool(b) => Value::Bool(*b),
            Data::Int(n) => Value::Int(*n),
            Data::Float(x) => Value::Float(*x),
            Data::Str(s) => self.str(s),
            Data::List(items) => {
                let items = items.iter().map(|d| self.alloc_data(d)).collect();
                Value::Obj(self.alloc(Obj::List(items)))
            }
            Data::Map(entries) => {
                let mut m = Map::default();
                for (k, d) in entries {
                    let v = self.alloc_data(d);
                    m.set(k, v);
                }
                Value::Obj(self.alloc(Obj::Map(m)))
            }
        }
    }

    /// How `print` and string interpolation show a value: strings bare, everything else as source.
    pub fn display(&self, v: Value, fn_name: &dyn Fn(u32) -> String) -> String {
        if let Some(s) = self.as_str(v) {
            return s.to_string();
        }
        let mut out = String::new();
        self.repr(v, &mut out, 0, fn_name);
        out
    }

    pub fn repr(&self, v: Value, out: &mut String, depth: u32, fn_name: &dyn Fn(u32) -> String) {
        if depth > 32 {
            out.push_str("...");
            return;
        }
        match v {
            Value::Nil => out.push_str("nil"),
            Value::Bool(b) => out.push_str(if b { "true" } else { "false" }),
            Value::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Float(x) => out.push_str(&fmt_float(x)),
            Value::Native(i) => {
                let _ = write!(out, "<builtin {}>", crate::builtins::BUILTINS[i as usize].0);
            }
            Value::Obj(r) => match self.get(r) {
                Obj::Str(s) => quote(s, out),
                Obj::List(items) => {
                    out.push('[');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        self.repr(*item, out, depth + 1, fn_name);
                    }
                    out.push(']');
                }
                Obj::Map(m) => {
                    out.push('{');
                    for (i, (k, v)) in m.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        if is_identifier(k) {
                            out.push_str(k);
                        } else {
                            quote(k, out);
                        }
                        out.push_str(": ");
                        self.repr(v, out, depth + 1, fn_name);
                    }
                    out.push('}');
                }
                Obj::Closure { func, .. } => {
                    let _ = write!(out, "<fn {}>", fn_name(*func));
                }
                Obj::Cell(v) => self.repr(*v, out, depth, fn_name),
            },
        }
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}
