//! The instruction set, compiled functions and the disassembler.

use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Const(u32),
    Nil,
    True,
    False,
    Pop,
    PopN(u16),
    Dup,
    Dup2,
    GetLocal(u16),
    SetLocal(u16),
    /// Moves the value in a local slot into a fresh cell, because a closure captures it.
    Box(u16),
    GetBoxed(u16),
    SetBoxed(u16),
    GetCapture(u16),
    SetCapture(u16),
    GetGlobal(u32),
    SetGlobal(u32),
    DefineGlobal(u32),
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Neg,
    Not,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Jump(u32),
    /// Pops the condition.
    JumpIfFalse(u32),
    /// Leaves the condition on the stack when jumping, pops it otherwise (for `and`).
    JumpIfFalseKeep(u32),
    /// Leaves the condition on the stack when jumping, pops it otherwise (for `or`).
    JumpIfTrueKeep(u32),
    Call(u8),
    Return,
    Closure(u32),
    List(u32),
    Map(u32),
    Index,
    SetIndex,
    GetField(u32),
    SetField(u32),
    /// Concatenates the top n values as text.
    Concat(u32),
    /// Turns the value on top of the stack into something `ForNext` can walk: a list stays, a map becomes its keys,
    /// a string becomes its characters.
    IterStart,
    /// With the sequence in local slot `seq` and the position in `seq + 1`: pushes the next item, or jumps when done.
    ForNext {
        seq: u16,
        exit: u32,
    },
    /// Suspends the program and asks the host to perform an effect.
    Effect {
        name: u32,
        argc: u8,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Str(String),
}

/// Where a closure finds a captured variable when it is created: a boxed local of the enclosing function, or one of
/// the enclosing closure's own captures.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Capture {
    Local(u16),
    Outer(u16),
}

/// Debug information: a named local slot and the code range where it is in scope.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalInfo {
    pub name: String,
    pub slot: u16,
    pub start: u32,
    pub end: u32,
    pub boxed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub arity: u8,
    pub code: Vec<Op>,
    /// The source line of each instruction.
    pub lines: Vec<u32>,
    pub captures: Vec<Capture>,
    pub capture_names: Vec<String>,
    pub locals: Vec<LocalInfo>,
}

#[derive(Debug, Clone)]
pub struct Program {
    /// The script itself is function 0.
    pub functions: Vec<Function>,
    pub consts: Vec<Const>,
    /// Global variable names; the first ones are the built-ins.
    pub globals: Vec<String>,
    pub source: String,
    /// Identifies the source and the compiler that produced this program; a checkpoint only resumes on the same.
    pub fingerprint: u64,
}

impl Program {
    pub fn disassemble(&self) -> String {
        let mut out = String::new();
        for (i, f) in self.functions.iter().enumerate() {
            let _ = writeln!(
                out,
                "fn #{i} {} (arity {}, {} instructions)",
                f.name,
                f.arity,
                f.code.len()
            );
            if !f.capture_names.is_empty() {
                let _ = writeln!(out, "  captures: {}", f.capture_names.join(", "));
            }
            let mut last_line = 0;
            for (pc, op) in f.code.iter().enumerate() {
                let line = f.lines[pc];
                let line_col = if line != last_line {
                    format!("{line:>4}")
                } else {
                    "   |".to_string()
                };
                last_line = line;
                let _ = writeln!(out, "  {pc:04} {line_col}  {}", self.describe(f, *op));
            }
            out.push('\n');
        }
        out
    }

    pub fn describe(&self, f: &Function, op: Op) -> String {
        let local = |slot: u16| {
            f.locals
                .iter()
                .find(|l| l.slot == slot)
                .map(|l| format!("  ; {}", l.name))
                .unwrap_or_default()
        };
        let constant = |i: u32| match &self.consts[i as usize] {
            Const::Int(n) => n.to_string(),
            Const::Float(x) => crate::value::fmt_float(*x),
            Const::Str(s) => {
                let mut q = String::new();
                crate::value::quote(s, &mut q);
                q
            }
        };
        let name = |i: u32| match &self.consts[i as usize] {
            Const::Str(s) => s.clone(),
            _ => "?".into(),
        };
        match op {
            Op::Const(i) => format!("CONST {}", constant(i)),
            Op::GetLocal(s) => format!("GET_LOCAL {s}{}", local(s)),
            Op::SetLocal(s) => format!("SET_LOCAL {s}{}", local(s)),
            Op::Box(s) => format!("BOX {s}{}", local(s)),
            Op::GetBoxed(s) => format!("GET_BOXED {s}{}", local(s)),
            Op::SetBoxed(s) => format!("SET_BOXED {s}{}", local(s)),
            Op::GetCapture(i) => format!("GET_CAPTURE {i}  ; {}", f.capture_names[i as usize]),
            Op::SetCapture(i) => format!("SET_CAPTURE {i}  ; {}", f.capture_names[i as usize]),
            Op::GetGlobal(g) => format!("GET_GLOBAL {}", self.globals[g as usize]),
            Op::SetGlobal(g) => format!("SET_GLOBAL {}", self.globals[g as usize]),
            Op::DefineGlobal(g) => format!("DEFINE_GLOBAL {}", self.globals[g as usize]),
            Op::Closure(i) => format!("CLOSURE #{i} {}", self.functions[i as usize].name),
            Op::GetField(i) => format!("GET_FIELD {}", name(i)),
            Op::SetField(i) => format!("SET_FIELD {}", name(i)),
            Op::ForNext { seq, exit } => format!("FOR_NEXT {seq} -> {exit:04}"),
            Op::Effect { name: n, argc } => format!("EFFECT {} ({argc} args)", name(n)),
            Op::Jump(t) => format!("JUMP -> {t:04}"),
            Op::JumpIfFalse(t) => format!("JUMP_IF_FALSE -> {t:04}"),
            Op::JumpIfFalseKeep(t) => format!("JUMP_IF_FALSE_KEEP -> {t:04}"),
            Op::JumpIfTrueKeep(t) => format!("JUMP_IF_TRUE_KEEP -> {t:04}"),
            other => {
                // `PopN(2)` -> `POP_N 2`, `Add` -> `ADD`
                let s = format!("{other:?}");
                let (head, tail) = s
                    .split_once('(')
                    .map(|(h, t)| (h, t.trim_end_matches(')')))
                    .unwrap_or((&s, ""));
                let mut upper = String::new();
                for (i, c) in head.chars().enumerate() {
                    if c.is_uppercase() && i > 0 {
                        upper.push('_');
                    }
                    upper.push(c.to_ascii_uppercase());
                }
                if tail.is_empty() {
                    upper
                } else {
                    format!("{upper} {tail}")
                }
            }
        }
    }
}

/// FNV-1a, 64 bit: a stable hash that does not depend on the platform or the Rust version.
pub fn fnv1a(bytes: &[u8], mut h: u64) -> u64 {
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
