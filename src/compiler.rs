//! Compiles the syntax tree to bytecode.
//!
//! Compilation has two passes. The first finds every local variable that some closure captures; the second emits
//! code, putting exactly those variables in heap cells. Everything else stays in a stack slot, which is faster.

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Expr, FnDecl, Stmt};
use crate::builtins::BUILTINS;
use crate::bytecode::{Capture, Const, FNV_OFFSET, Function, LocalInfo, Op, Program, fnv1a};
use crate::error::{Error, Span};
use crate::parser::parse;

/// Bumped whenever the bytecode or the checkpoint format changes meaning, so old checkpoints are refused.
pub const COMPILER_VERSION: &str = "spola-1";

pub fn compile(source: &str) -> Result<Program, Error> {
    let ast = parse(source)?;
    let captured = Analyzer::run(&ast);
    let mut c = Compiler {
        program: Program {
            functions: Vec::new(),
            consts: Vec::new(),
            globals: BUILTINS.iter().map(|(name, _)| name.to_string()).collect(),
            source: source.to_string(),
            fingerprint: fnv1a(source.as_bytes(), fnv1a(COMPILER_VERSION.as_bytes(), FNV_OFFSET)),
        },
        global_index: BUILTINS
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (name.to_string(), i as u32))
            .collect(),
        str_consts: HashMap::new(),
        captured,
        fns: Vec::new(),
        line: 1,
    };
    c.program.functions.push(empty_function("<script>", 0));
    c.fns.push(FnState::new(0));
    for stmt in &ast {
        c.statement(stmt)?;
    }
    c.emit(Op::Nil);
    c.emit(Op::Return);
    c.finish_function();
    Ok(c.program)
}

fn empty_function(name: &str, arity: u8) -> Function {
    Function {
        name: name.to_string(),
        arity,
        code: Vec::new(),
        lines: Vec::new(),
        captures: Vec::new(),
        capture_names: Vec::new(),
        locals: Vec::new(),
    }
}

/// Identifies a variable declaration: where it is, what kind it is, and its name.
type DeclKey = (u32, u32, u8, String);

const LET: u8 = 0;
const FN: u8 = 1;
const PARAM: u8 = 2;
const FOR: u8 = 3;

fn key(span: Span, kind: u8, name: &str) -> DeclKey {
    (span.line, span.col, kind, name.to_string())
}

/// The first pass: which local declarations are captured by an inner function.
struct Analyzer {
    /// For each function being analyzed, its scopes, each a list of declared names.
    fns: Vec<Vec<Vec<(String, DeclKey)>>>,
    captured: HashSet<DeclKey>,
}

impl Analyzer {
    fn run(ast: &[Stmt]) -> HashSet<DeclKey> {
        let mut a = Analyzer {
            fns: vec![vec![Vec::new()]],
            captured: HashSet::new(),
        };
        a.statements(ast);
        a.captured
    }

    fn at_global_scope(&self) -> bool {
        self.fns.len() == 1 && self.fns[0].len() == 1
    }

    fn declare(&mut self, name: &str, key: DeclKey) {
        if !self.at_global_scope() {
            self.fns
                .last_mut()
                .unwrap()
                .last_mut()
                .unwrap()
                .push((name.to_string(), key));
        }
    }

    fn resolve(&mut self, name: &str) {
        let current = self.fns.len() - 1;
        for fi in (0..self.fns.len()).rev() {
            for scope in self.fns[fi].iter().rev() {
                if let Some((_, key)) = scope.iter().rev().find(|(n, _)| n == name) {
                    if fi != current {
                        self.captured.insert(key.clone());
                    }
                    return;
                }
            }
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.fns.last_mut().unwrap().push(Vec::new());
        f(self);
        self.fns.last_mut().unwrap().pop();
    }

    fn statements(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.statement(s);
        }
    }

    fn block(&mut self, stmts: &[Stmt]) {
        self.scoped(|a| a.statements(stmts));
    }

    fn function(&mut self, f: &FnDecl) {
        let params = f.params.iter().map(|p| (p.clone(), key(f.span, PARAM, p))).collect();
        self.fns.push(vec![params]);
        self.statements(&f.body);
        self.fns.pop();
    }

    fn statement(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { name, value, span } => {
                self.expr(value);
                self.declare(name, key(*span, LET, name));
            }
            Stmt::Fn(f) => {
                self.declare(&f.name, key(f.span, FN, &f.name));
                self.function(f);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::If { cond, then, otherwise } => {
                self.expr(cond);
                self.block(then);
                if let Some(o) = otherwise {
                    self.block(o);
                }
            }
            Stmt::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::For { var, iter, body, span } => {
                self.expr(iter);
                self.scoped(|a| {
                    // the loop variable lives in its own scope, never at global level
                    a.fns
                        .last_mut()
                        .unwrap()
                        .last_mut()
                        .unwrap()
                        .push((var.clone(), key(*span, FOR, var)));
                    a.statements(body);
                });
            }
            Stmt::Block(b) => self.block(b),
            Stmt::Return { value: Some(e), .. } => self.expr(e),
            Stmt::Return { value: None, .. } | Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Var(name, _) => self.resolve(name),
            Expr::Nil(_) | Expr::Bool(..) | Expr::Int(..) | Expr::Float(..) | Expr::Str(..) => {}
            Expr::Interp(parts, _) | Expr::List(parts, _) => parts.iter().for_each(|p| self.expr(p)),
            Expr::Map(entries, _) => entries.iter().for_each(|(_, v)| self.expr(v)),
            Expr::Neg(x, _) | Expr::Not(x, _) | Expr::Field(x, _, _) => self.expr(x),
            Expr::Binary(_, l, r, _) | Expr::And(l, r, _) | Expr::Or(l, r, _) | Expr::Index(l, r, _) => {
                self.expr(l);
                self.expr(r);
            }
            Expr::Call(callee, args, _) => {
                self.expr(callee);
                args.iter().for_each(|a| self.expr(a));
            }
            Expr::Lambda(f) => self.function(f),
            Expr::Await { args, .. } => args.iter().for_each(|a| self.expr(a)),
        }
    }
}

struct Local {
    name: String,
    slot: u16,
    boxed: bool,
    /// Index into the function's debug `locals`, to close its range at scope end. `None` for hidden locals.
    info: Option<usize>,
}

struct Loop {
    /// Number of locals alive when the loop started; `break` and `continue` pop down to this.
    locals: usize,
    continue_target: u32,
    breaks: Vec<usize>,
}

struct FnState {
    index: u32,
    scopes: Vec<Vec<Local>>,
    slots: usize,
    captures: Vec<(Capture, String)>,
    loops: Vec<Loop>,
}

impl FnState {
    fn new(index: u32) -> Self {
        FnState {
            index,
            scopes: vec![Vec::new()],
            slots: 1,
            captures: Vec::new(),
            loops: Vec::new(),
        }
    }
}

enum Var {
    Local(u16, bool),
    Capture(u16),
    Global(u32),
}

struct Compiler {
    program: Program,
    global_index: HashMap<String, u32>,
    str_consts: HashMap<String, u32>,
    captured: HashSet<DeclKey>,
    fns: Vec<FnState>,
    line: u32,
}

impl Compiler {
    fn state(&mut self) -> &mut FnState {
        self.fns.last_mut().unwrap()
    }

    fn func(&mut self) -> &mut Function {
        let i = self.fns.last().unwrap().index as usize;
        &mut self.program.functions[i]
    }

    fn pc(&mut self) -> u32 {
        self.func().code.len() as u32
    }

    fn emit(&mut self, op: Op) -> usize {
        let line = self.line;
        let f = self.func();
        f.code.push(op);
        f.lines.push(line);
        f.code.len() - 1
    }

    fn patch(&mut self, at: usize) {
        let target = self.pc();
        let op = &mut self.func().code[at];
        *op = match *op {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfFalseKeep(_) => Op::JumpIfFalseKeep(target),
            Op::JumpIfTrueKeep(_) => Op::JumpIfTrueKeep(target),
            Op::ForNext { seq, .. } => Op::ForNext { seq, exit: target },
            other => unreachable!("patching {other:?}"),
        };
    }

    fn constant(&mut self, c: Const) -> u32 {
        if let Const::Str(s) = &c
            && let Some(&i) = self.str_consts.get(s)
        {
            return i;
        }
        let i = self.program.consts.len() as u32;
        if let Const::Str(s) = &c {
            self.str_consts.insert(s.clone(), i);
        }
        self.program.consts.push(c);
        i
    }

    fn global(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.global_index.get(name) {
            return i;
        }
        let i = self.program.globals.len() as u32;
        self.program.globals.push(name.to_string());
        self.global_index.insert(name.to_string(), i);
        i
    }

    fn at_global_scope(&self) -> bool {
        self.fns.len() == 1 && self.fns[0].scopes.len() == 1
    }

    /// Declares a local in the slot of the value on top of the stack.
    fn declare_local(&mut self, name: &str, key: Option<DeclKey>, span: Span) -> Result<u16, Error> {
        let boxed = key.is_some_and(|k| self.captured.contains(&k));
        let st = self.fns.last().unwrap();
        if st.slots >= u16::MAX as usize {
            return Err(Error::syntax("too many local variables in one function", span));
        }
        let slot = st.slots as u16;
        let start = self.pc();
        let info = if name.starts_with('$') {
            None
        } else {
            let f = self.func();
            f.locals.push(LocalInfo {
                name: name.to_string(),
                slot,
                start,
                end: u32::MAX,
                boxed,
            });
            Some(f.locals.len() - 1)
        };
        let st = self.state();
        st.slots += 1;
        st.scopes.last_mut().unwrap().push(Local {
            name: name.to_string(),
            slot,
            boxed,
            info,
        });
        if boxed {
            self.emit(Op::Box(slot));
        }
        Ok(slot)
    }

    fn begin_scope(&mut self) {
        self.state().scopes.push(Vec::new());
    }

    fn end_scope(&mut self) {
        let scope = self.state().scopes.pop().unwrap();
        self.state().slots -= scope.len();
        let end = self.pc();
        for local in &scope {
            if let Some(i) = local.info {
                self.func().locals[i].end = end;
            }
        }
        self.pop_n(scope.len());
    }

    fn pop_n(&mut self, n: usize) {
        match n {
            0 => {}
            1 => {
                self.emit(Op::Pop);
            }
            n => {
                self.emit(Op::PopN(n as u16));
            }
        }
    }

    fn resolve(&mut self, name: &str) -> Var {
        let depth = self.fns.len() - 1;
        match self.resolve_in(depth, name) {
            Some(v) => v,
            None => Var::Global(self.global(name)),
        }
    }

    fn resolve_in(&mut self, depth: usize, name: &str) -> Option<Var> {
        for scope in self.fns[depth].scopes.iter().rev() {
            if let Some(l) = scope.iter().rev().find(|l| l.name == name) {
                return Some(Var::Local(l.slot, l.boxed));
            }
        }
        if depth == 0 {
            return None;
        }
        let capture = match self.resolve_in(depth - 1, name)? {
            Var::Local(slot, boxed) => {
                debug_assert!(boxed, "captured local `{name}` was not boxed");
                Capture::Local(slot)
            }
            Var::Capture(i) => Capture::Outer(i),
            Var::Global(_) => return None,
        };
        let st = &mut self.fns[depth];
        if let Some(i) = st.captures.iter().position(|(c, _)| *c == capture) {
            return Some(Var::Capture(i as u16));
        }
        st.captures.push((capture, name.to_string()));
        Some(Var::Capture(st.captures.len() as u16 - 1))
    }

    fn get_var(&mut self, name: &str) {
        let op = match self.resolve(name) {
            Var::Local(s, false) => Op::GetLocal(s),
            Var::Local(s, true) => Op::GetBoxed(s),
            Var::Capture(i) => Op::GetCapture(i),
            Var::Global(g) => Op::GetGlobal(g),
        };
        self.emit(op);
    }

    fn set_var(&mut self, name: &str) {
        let op = match self.resolve(name) {
            Var::Local(s, false) => Op::SetLocal(s),
            Var::Local(s, true) => Op::SetBoxed(s),
            Var::Capture(i) => Op::SetCapture(i),
            Var::Global(g) => Op::SetGlobal(g),
        };
        self.emit(op);
    }

    fn finish_function(&mut self) -> FnState {
        let st = self.fns.pop().unwrap();
        let f = &mut self.program.functions[st.index as usize];
        f.captures = st.captures.iter().map(|(c, _)| *c).collect();
        f.capture_names = st.captures.iter().map(|(_, n)| n.clone()).collect();
        st
    }

    fn block(&mut self, stmts: &[Stmt]) -> Result<(), Error> {
        self.begin_scope();
        for s in stmts {
            self.statement(s)?;
        }
        self.end_scope();
        Ok(())
    }

    fn statement(&mut self, s: &Stmt) -> Result<(), Error> {
        match s {
            Stmt::Let { name, value, span } => {
                self.line = span.line;
                self.expr(value)?;
                self.line = span.line;
                if self.at_global_scope() {
                    let g = self.global(name);
                    self.emit(Op::DefineGlobal(g));
                } else {
                    self.declare_local(name, Some(key(*span, LET, name)), *span)?;
                }
            }
            Stmt::Fn(f) => {
                self.line = f.span.line;
                if self.at_global_scope() {
                    self.function(f)?;
                    let g = self.global(&f.name);
                    self.emit(Op::DefineGlobal(g));
                } else {
                    // declare first, so the function can call itself
                    self.emit(Op::Nil);
                    let slot = self.declare_local(&f.name, Some(key(f.span, FN, &f.name)), f.span)?;
                    self.function(f)?;
                    self.line = f.span.line;
                    let boxed = self.captured.contains(&key(f.span, FN, &f.name));
                    self.emit(if boxed { Op::SetBoxed(slot) } else { Op::SetLocal(slot) });
                }
            }
            Stmt::Expr(e) => {
                self.expr(e)?;
                self.emit(Op::Pop);
            }
            Stmt::Assign {
                target,
                op,
                value,
                span,
            } => self.assign(target, *op, value, *span)?,
            Stmt::If { cond, then, otherwise } => {
                self.expr(cond)?;
                let to_else = self.emit(Op::JumpIfFalse(0));
                self.block(then)?;
                match otherwise {
                    Some(o) => {
                        let to_end = self.emit(Op::Jump(0));
                        self.patch(to_else);
                        self.block(o)?;
                        self.patch(to_end);
                    }
                    None => self.patch(to_else),
                }
            }
            Stmt::While { cond, body } => {
                let start = self.pc();
                self.expr(cond)?;
                let exit = self.emit(Op::JumpIfFalse(0));
                let locals = self.fns.last().unwrap().slots;
                self.state().loops.push(Loop {
                    locals,
                    continue_target: start,
                    breaks: Vec::new(),
                });
                self.block(body)?;
                self.emit(Op::Jump(start));
                self.patch(exit);
                let lp = self.state().loops.pop().unwrap();
                for b in lp.breaks {
                    self.patch(b);
                }
            }
            Stmt::For { var, iter, body, span } => {
                self.expr(iter)?;
                self.line = span.line;
                self.emit(Op::IterStart);
                self.begin_scope();
                let seq = self.declare_local("$seq", None, *span)?;
                let k = self.constant(Const::Int(0));
                self.emit(Op::Const(k));
                self.declare_local("$index", None, *span)?;
                let start = self.pc();
                let next = self.emit(Op::ForNext { seq, exit: 0 });
                let locals = self.fns.last().unwrap().slots;
                self.state().loops.push(Loop {
                    locals,
                    continue_target: start,
                    breaks: Vec::new(),
                });
                self.begin_scope();
                self.declare_local(var, Some(key(*span, FOR, var)), *span)?;
                for s in body {
                    self.statement(s)?;
                }
                self.end_scope();
                self.emit(Op::Jump(start));
                self.patch(next);
                let lp = self.state().loops.pop().unwrap();
                for b in lp.breaks {
                    self.patch(b);
                }
                self.end_scope();
            }
            Stmt::Block(b) => self.block(b)?,
            Stmt::Return { value, span } => {
                match value {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Nil);
                    }
                }
                self.line = span.line;
                self.emit(Op::Return);
            }
            Stmt::Break(span) | Stmt::Continue(span) => {
                self.line = span.line;
                let Some(lp) = self.fns.last().unwrap().loops.last() else {
                    let what = if matches!(s, Stmt::Break(_)) {
                        "break"
                    } else {
                        "continue"
                    };
                    return Err(Error::syntax(format!("`{what}` outside a loop"), *span));
                };
                let (locals, target) = (lp.locals, lp.continue_target);
                let n = self.fns.last().unwrap().slots - locals;
                self.pop_n(n);
                if matches!(s, Stmt::Break(_)) {
                    let at = self.emit(Op::Jump(0));
                    self.state().loops.last_mut().unwrap().breaks.push(at);
                } else {
                    self.emit(Op::Jump(target));
                }
            }
        }
        Ok(())
    }

    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr, span: Span) -> Result<(), Error> {
        match target {
            Expr::Var(name, _) => {
                if let Some(op) = op {
                    self.get_var(name);
                    self.expr(value)?;
                    self.line = span.line;
                    self.binop(op);
                } else {
                    self.expr(value)?;
                }
                self.line = span.line;
                self.set_var(name);
            }
            Expr::Index(obj, index, _) => {
                self.expr(obj)?;
                self.expr(index)?;
                if let Some(op) = op {
                    self.emit(Op::Dup2);
                    self.emit(Op::Index);
                    self.expr(value)?;
                    self.line = span.line;
                    self.binop(op);
                } else {
                    self.expr(value)?;
                }
                self.line = span.line;
                self.emit(Op::SetIndex);
            }
            Expr::Field(obj, name, _) => {
                self.expr(obj)?;
                let k = self.constant(Const::Str(name.clone()));
                if let Some(op) = op {
                    self.emit(Op::Dup);
                    self.emit(Op::GetField(k));
                    self.expr(value)?;
                    self.line = span.line;
                    self.binop(op);
                } else {
                    self.expr(value)?;
                }
                self.line = span.line;
                self.emit(Op::SetField(k));
            }
            _ => unreachable!("the parser only builds assignments to variables, indexes and fields"),
        }
        Ok(())
    }

    fn binop(&mut self, op: BinOp) {
        self.emit(match op {
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Div => Op::Div,
            BinOp::Mod => Op::Mod,
            BinOp::Eq => Op::Eq,
            BinOp::Ne => Op::Ne,
            BinOp::Lt => Op::Lt,
            BinOp::Le => Op::Le,
            BinOp::Gt => Op::Gt,
            BinOp::Ge => Op::Ge,
        });
    }

    /// Compiles a function and leaves a closure over it on the stack.
    fn function(&mut self, f: &FnDecl) -> Result<(), Error> {
        let index = self.program.functions.len() as u32;
        self.program
            .functions
            .push(empty_function(&f.name, f.params.len() as u8));
        self.fns.push(FnState::new(index));
        self.line = f.span.line;
        for p in &f.params {
            self.declare_local(p, Some(key(f.span, PARAM, p)), f.span)?;
        }
        for s in &f.body {
            self.statement(s)?;
        }
        self.emit(Op::Nil);
        self.emit(Op::Return);
        self.finish_function();
        self.line = f.span.line;
        self.emit(Op::Closure(index));
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<(), Error> {
        self.line = e.span().line;
        match e {
            Expr::Nil(_) => {
                self.emit(Op::Nil);
            }
            Expr::Bool(b, _) => {
                self.emit(if *b { Op::True } else { Op::False });
            }
            Expr::Int(n, _) => {
                let k = self.constant(Const::Int(*n));
                self.emit(Op::Const(k));
            }
            Expr::Float(x, _) => {
                let k = self.constant(Const::Float(*x));
                self.emit(Op::Const(k));
            }
            Expr::Str(s, _) => {
                let k = self.constant(Const::Str(s.clone()));
                self.emit(Op::Const(k));
            }
            Expr::Interp(parts, _) => {
                for p in parts {
                    self.expr(p)?;
                }
                self.emit(Op::Concat(parts.len() as u32));
            }
            Expr::Var(name, _) => self.get_var(name),
            Expr::List(items, _) => {
                for i in items {
                    self.expr(i)?;
                }
                self.emit(Op::List(items.len() as u32));
            }
            Expr::Map(entries, _) => {
                for (k, v) in entries {
                    let k = self.constant(Const::Str(k.clone()));
                    self.emit(Op::Const(k));
                    self.expr(v)?;
                }
                self.emit(Op::Map(entries.len() as u32));
            }
            Expr::Neg(x, s) => {
                self.expr(x)?;
                self.line = s.line;
                self.emit(Op::Neg);
            }
            Expr::Not(x, s) => {
                self.expr(x)?;
                self.line = s.line;
                self.emit(Op::Not);
            }
            Expr::Binary(op, l, r, s) => {
                self.expr(l)?;
                self.expr(r)?;
                self.line = s.line;
                self.binop(*op);
            }
            Expr::And(l, r, _) => {
                self.expr(l)?;
                let end = self.emit(Op::JumpIfFalseKeep(0));
                self.expr(r)?;
                self.patch(end);
            }
            Expr::Or(l, r, _) => {
                self.expr(l)?;
                let end = self.emit(Op::JumpIfTrueKeep(0));
                self.expr(r)?;
                self.patch(end);
            }
            Expr::Call(callee, args, s) => {
                self.expr(callee)?;
                for a in args {
                    self.expr(a)?;
                }
                self.line = s.line;
                self.emit(Op::Call(args.len() as u8));
            }
            Expr::Index(obj, index, s) => {
                self.expr(obj)?;
                self.expr(index)?;
                self.line = s.line;
                self.emit(Op::Index);
            }
            Expr::Field(obj, name, s) => {
                self.expr(obj)?;
                let k = self.constant(Const::Str(name.clone()));
                self.line = s.line;
                self.emit(Op::GetField(k));
            }
            Expr::Lambda(f) => self.function(f)?,
            Expr::Await { effect, args, span } => {
                for a in args {
                    self.expr(a)?;
                }
                let name = self.constant(Const::Str(effect.clone()));
                self.line = span.line;
                self.emit(Op::Effect {
                    name,
                    argc: args.len() as u8,
                });
            }
        }
        Ok(())
    }
}
