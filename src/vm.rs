//! The virtual machine: a stack machine that runs [`Program`]s and stops at every `await`.

use std::sync::Arc;

use crate::builtins::BUILTINS;
use crate::bytecode::{Capture, Const, Op, Program};
use crate::error::{Error, TraceFrame};
use crate::value::{Data, Heap, Map, Obj, Ref, Value};

/// Deep enough for real recursion, shallow enough to fail cleanly instead of eating all memory.
pub const MAX_FRAMES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub func: u32,
    /// The closure being run; `None` for the top-level script.
    pub closure: Option<Ref>,
    pub ip: u32,
    /// Stack index of slot 0 of this call.
    pub base: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Ready to run (or in the middle of running).
    Running,
    /// Stopped at `await`, until the host resumes it with the effect's result.
    Waiting {
        effect: String,
        args: Vec<Value>,
    },
    Done(Value),
    Failed,
}

/// Why [`Vm::run`] returned.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The program finished with this value.
    Done(Data),
    /// The program awaits an effect. Perform it (or not: checkpoint, exit, come back next week) and call
    /// [`Vm::resume`] with the result.
    Effect { name: String, args: Vec<Data> },
    /// The step budget ran out. Call [`Vm::run`] again to continue.
    Paused,
}

/// A frame of a running program, for debuggers and `spola inspect`.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameInfo {
    pub function: String,
    pub line: u32,
    /// Local variables in scope, shown as source.
    pub locals: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Vm {
    pub(crate) program: Arc<Program>,
    pub(crate) heap: Heap,
    pub(crate) stack: Vec<Value>,
    pub(crate) frames: Vec<Frame>,
    /// `None` until the variable is defined.
    pub(crate) globals: Vec<Option<Value>>,
    /// The program's constants, with strings already on the heap.
    pub(crate) consts: Vec<Value>,
    pub(crate) status: Status,
    /// Instructions executed so far.
    pub steps: u64,
    /// Effects completed so far.
    pub effects: u64,
    /// Text printed by the program and not yet taken by the host.
    pub(crate) output: String,
    /// Anything the host wants to keep in the checkpoint, such as when a `sleep` started.
    pub host: Data,
}

impl Vm {
    pub fn new(program: Program) -> Vm {
        let program = Arc::new(program);
        let mut heap = Heap::new();
        let consts = program
            .consts
            .iter()
            .map(|c| match c {
                Const::Int(n) => Value::Int(*n),
                Const::Float(x) => Value::Float(*x),
                Const::Str(s) => heap.str(s),
            })
            .collect();
        let mut globals = vec![None; program.globals.len()];
        for (i, slot) in globals.iter_mut().enumerate().take(BUILTINS.len()) {
            *slot = Some(Value::Native(i as u16));
        }
        Vm {
            program,
            heap,
            stack: vec![Value::Nil],
            frames: vec![Frame {
                func: 0,
                closure: None,
                ip: 0,
                base: 0,
            }],
            globals,
            consts,
            status: Status::Running,
            steps: 0,
            effects: 0,
            output: String::new(),
            host: Data::Nil,
        }
    }

    pub fn program(&self) -> &Program {
        &self.program
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn is_done(&self) -> bool {
        matches!(self.status, Status::Done(_))
    }

    /// Takes the text the program printed since the last call.
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }

    /// The effect the program is waiting for, if it is waiting.
    pub fn pending(&self) -> Option<(String, Vec<Data>)> {
        match &self.status {
            Status::Waiting { effect, args } => Some((
                effect.clone(),
                args.iter()
                    .map(|a| self.heap.to_data(*a).unwrap_or(Data::Nil))
                    .collect(),
            )),
            _ => None,
        }
    }

    /// Objects alive on the heap.
    pub fn heap_size(&self) -> usize {
        self.heap.live()
    }

    /// Hands the result of the awaited effect back to the program. Call [`Vm::run`] next.
    pub fn resume(&mut self, result: &Data) -> Result<(), Error> {
        if !matches!(self.status, Status::Waiting { .. }) {
            return Err(Error::runtime("the program is not waiting for an effect"));
        }
        let v = self.heap.alloc_data(result);
        self.stack.push(v);
        self.status = Status::Running;
        self.effects += 1;
        Ok(())
    }

    /// Makes the awaited effect fail: the program stops with this error, raised at the `await`.
    pub fn fail_effect(&mut self, message: &str) -> Error {
        let effect = match &self.status {
            Status::Waiting { effect, .. } => effect.clone(),
            _ => "effect".into(),
        };
        let e = self.error(format!("{effect}: {message}"));
        self.status = Status::Failed;
        e
    }

    pub fn display(&self, v: Value) -> String {
        self.heap
            .display(v, &|f| self.program.functions[f as usize].name.clone())
    }

    pub fn repr(&self, v: Value) -> String {
        let mut out = String::new();
        self.heap
            .repr(v, &mut out, 0, &|f| self.program.functions[f as usize].name.clone());
        out
    }

    /// The source line about to run (or, while waiting, the line of the `await`).
    pub fn line(&self) -> u32 {
        self.frames.last().map(|fr| self.frame_line(fr)).unwrap_or(0)
    }

    fn frame_line(&self, fr: &Frame) -> u32 {
        let f = &self.program.functions[fr.func as usize];
        let pc = if matches!(self.status, Status::Running) && fr == self.frames.last().unwrap() {
            fr.ip as usize
        } else {
            (fr.ip as usize).saturating_sub(1)
        };
        f.lines.get(pc).or(f.lines.last()).copied().unwrap_or(0)
    }

    /// The call stack, innermost first, with each frame's local variables.
    pub fn call_stack(&self) -> Vec<FrameInfo> {
        let mut out = Vec::new();
        for fr in self.frames.iter().rev() {
            let f = &self.program.functions[fr.func as usize];
            let pc = fr.ip.saturating_sub(1);
            let mut locals: Vec<(String, String)> = Vec::new();
            for l in f.locals.iter().filter(|l| l.start <= fr.ip && pc < l.end) {
                let Some(v) = self.stack.get(fr.base as usize + l.slot as usize).copied() else {
                    continue;
                };
                let v = match v {
                    Value::Obj(r) if l.boxed => match self.heap.get(r) {
                        Obj::Cell(inner) => *inner,
                        _ => v,
                    },
                    _ => v,
                };
                locals.retain(|(n, _)| *n != l.name);
                locals.push((l.name.clone(), self.repr(v)));
            }
            if let Some(Value::Obj(r)) = fr.closure.map(Value::Obj)
                && let Obj::Closure { captures, .. } = self.heap.get(r)
            {
                for (name, cell) in f.capture_names.iter().zip(captures) {
                    if let Obj::Cell(v) = self.heap.get(*cell) {
                        locals.push((name.clone(), self.repr(*v)));
                    }
                }
            }
            if fr.func == 0 {
                for (i, g) in self.globals.iter().enumerate().skip(BUILTINS.len()) {
                    if let Some(v) = g {
                        locals.push((self.program.globals[i].clone(), self.repr(*v)));
                    }
                }
            }
            out.push(FrameInfo {
                function: f.name.clone(),
                line: self.frame_line(fr),
                locals,
            });
        }
        out
    }

    fn error(&self, message: impl Into<String>) -> Error {
        let mut e = Error::runtime(message);
        e.trace = self
            .frames
            .iter()
            .rev()
            .map(|fr| TraceFrame {
                function: self.program.functions[fr.func as usize].name.clone(),
                line: {
                    let f = &self.program.functions[fr.func as usize];
                    f.lines.get((fr.ip as usize).saturating_sub(1)).copied().unwrap_or(0)
                },
            })
            .collect();
        e
    }

    fn collect_garbage(&mut self) {
        let mut roots: Vec<Value> = Vec::with_capacity(self.stack.len() + self.globals.len() + self.consts.len());
        roots.extend(self.stack.iter().copied());
        roots.extend(self.globals.iter().flatten().copied());
        roots.extend(self.consts.iter().copied());
        roots.extend(self.frames.iter().filter_map(|f| f.closure.map(Value::Obj)));
        match &self.status {
            Status::Waiting { args, .. } => roots.extend(args.iter().copied()),
            Status::Done(v) => roots.push(*v),
            _ => {}
        }
        self.heap.collect(roots.into_iter());
    }

    /// Collects garbage now. Checkpoints call this, so they hold only live objects.
    pub fn compact(&mut self) {
        self.collect_garbage();
    }

    fn pop(&mut self) -> Result<Value, Error> {
        self.stack
            .pop()
            .ok_or_else(|| Error::runtime("stack underflow (corrupt program state)"))
    }

    fn peek(&self, depth: usize) -> Result<Value, Error> {
        self.stack
            .len()
            .checked_sub(depth + 1)
            .map(|i| self.stack[i])
            .ok_or_else(|| Error::runtime("stack underflow (corrupt program state)"))
    }

    fn slot(&self, base: u32, slot: u16) -> Result<usize, Error> {
        let i = base as usize + slot as usize;
        if i < self.stack.len() {
            Ok(i)
        } else {
            Err(Error::runtime("bad local slot (corrupt program state)"))
        }
    }

    fn cell(&self, v: Value) -> Result<Ref, Error> {
        match v {
            Value::Obj(r) if matches!(self.heap.get(r), Obj::Cell(_)) => Ok(r),
            _ => Err(Error::runtime("expected a captured variable (corrupt program state)")),
        }
    }

    fn capture(&self, frame: &Frame, i: u16) -> Result<Ref, Error> {
        let Some(c) = frame.closure else {
            return Err(Error::runtime("no closure (corrupt program state)"));
        };
        match self.heap.get(c) {
            Obj::Closure { captures, .. } => captures
                .get(i as usize)
                .copied()
                .ok_or_else(|| Error::runtime("bad capture (corrupt program state)")),
            _ => Err(Error::runtime("not a closure (corrupt program state)")),
        }
    }

    fn type_name(&self, v: Value) -> &'static str {
        self.heap.type_name(v)
    }

    fn str_value(&mut self, s: &str) -> Value {
        self.heap.str(s)
    }

    /// Runs until the program finishes, awaits an effect, or executes `budget` instructions.
    pub fn run(&mut self, budget: Option<u64>) -> Result<Outcome, Error> {
        match &self.status {
            Status::Running => {}
            Status::Waiting { effect, .. } => {
                return Err(Error::runtime(format!(
                    "the program is waiting for `{effect}`; resume it first"
                )));
            }
            Status::Done(_) => return Err(Error::runtime("the program has already finished")),
            Status::Failed => return Err(Error::runtime("the program has failed")),
        }
        let result = self.execute(budget);
        if let Err(e) = &result {
            self.status = Status::Failed;
            if e.trace.is_empty() {
                let mut e = e.clone();
                e.trace = self.error("").trace;
                return Err(e);
            }
        }
        result
    }

    fn execute(&mut self, budget: Option<u64>) -> Result<Outcome, Error> {
        let program = self.program.clone();
        let limit = budget.map(|b| self.steps.saturating_add(b));
        loop {
            if limit.is_some_and(|l| self.steps >= l) {
                return Ok(Outcome::Paused);
            }
            if self.heap.should_collect() {
                self.collect_garbage();
            }
            let fi = self.frames.len() - 1;
            let frame = self.frames[fi];
            let func = &program.functions[frame.func as usize];
            let Some(&op) = func.code.get(frame.ip as usize) else {
                return Err(self.error("ran past the end of a function (corrupt program state)"));
            };
            self.frames[fi].ip += 1;
            self.steps += 1;
            match op {
                Op::Const(k) => {
                    let v = *self.consts.get(k as usize).ok_or_else(|| self.error("bad constant"))?;
                    self.stack.push(v);
                }
                Op::Nil => self.stack.push(Value::Nil),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),
                Op::Pop => {
                    self.pop()?;
                }
                Op::PopN(n) => {
                    let len = self
                        .stack
                        .len()
                        .checked_sub(n as usize)
                        .ok_or_else(|| self.error("stack underflow"))?;
                    self.stack.truncate(len);
                }
                Op::Dup => {
                    let v = self.peek(0)?;
                    self.stack.push(v);
                }
                Op::Dup2 => {
                    let (a, b) = (self.peek(1)?, self.peek(0)?);
                    self.stack.push(a);
                    self.stack.push(b);
                }
                Op::GetLocal(s) => {
                    let i = self.slot(frame.base, s)?;
                    self.stack.push(self.stack[i]);
                }
                Op::SetLocal(s) => {
                    let v = self.pop()?;
                    let i = self.slot(frame.base, s)?;
                    self.stack[i] = v;
                }
                Op::Box(s) => {
                    let i = self.slot(frame.base, s)?;
                    let cell = self.heap.alloc(Obj::Cell(self.stack[i]));
                    self.stack[i] = Value::Obj(cell);
                }
                Op::GetBoxed(s) => {
                    let i = self.slot(frame.base, s)?;
                    let r = self.cell(self.stack[i])?;
                    let Obj::Cell(v) = self.heap.get(r) else { unreachable!() };
                    self.stack.push(*v);
                }
                Op::SetBoxed(s) => {
                    let v = self.pop()?;
                    let i = self.slot(frame.base, s)?;
                    let r = self.cell(self.stack[i])?;
                    *self.heap.get_mut(r) = Obj::Cell(v);
                }
                Op::GetCapture(c) => {
                    let r = self.capture(&frame, c)?;
                    let v = match self.heap.get(r) {
                        Obj::Cell(v) => *v,
                        _ => return Err(self.error("bad capture (corrupt program state)")),
                    };
                    self.stack.push(v);
                }
                Op::SetCapture(c) => {
                    let v = self.pop()?;
                    let r = self.capture(&frame, c)?;
                    *self.heap.get_mut(r) = Obj::Cell(v);
                }
                Op::GetGlobal(g) => match self.globals.get(g as usize).copied().flatten() {
                    Some(v) => self.stack.push(v),
                    None => {
                        let name = &program.globals[g as usize];
                        return Err(self.error(format!("undefined variable `{name}`")));
                    }
                },
                Op::SetGlobal(g) => {
                    let v = self.pop()?;
                    match self.globals.get_mut(g as usize) {
                        Some(slot @ Some(_)) => *slot = Some(v),
                        _ => {
                            let name = &program.globals[g as usize];
                            return Err(self.error(format!("assignment to undeclared variable `{name}`; use `let`")));
                        }
                    }
                }
                Op::DefineGlobal(g) => {
                    let v = self.pop()?;
                    if g as usize >= self.globals.len() {
                        return Err(self.error("bad global"));
                    }
                    self.globals[g as usize] = Some(v);
                }
                Op::Add => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    let v = self.add(a, b)?;
                    self.stack.push(v);
                }
                Op::Sub | Op::Mul | Op::Div | Op::Mod => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    let v = self.arith(op, a, b)?;
                    self.stack.push(v);
                }
                Op::Neg => {
                    let v = match self.pop()? {
                        Value::Int(n) => Value::Int(n.checked_neg().ok_or_else(|| self.error("integer overflow"))?),
                        Value::Float(x) => Value::Float(-x),
                        other => return Err(self.error(format!("cannot negate {}", self.type_name(other)))),
                    };
                    self.stack.push(v);
                }
                Op::Not => {
                    let v = self.pop()?;
                    self.stack.push(Value::Bool(!v.truthy()));
                }
                Op::Eq | Op::Ne => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    let eq = self.heap.equal(a, b);
                    self.stack.push(Value::Bool(if op == Op::Eq { eq } else { !eq }));
                }
                Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    let ord = self.compare(a, b)?;
                    let r = match op {
                        Op::Lt => ord.is_lt(),
                        Op::Le => ord.is_le(),
                        Op::Gt => ord.is_gt(),
                        _ => ord.is_ge(),
                    };
                    self.stack.push(Value::Bool(r));
                }
                Op::Jump(t) => self.frames[fi].ip = t,
                Op::JumpIfFalse(t) => {
                    if !self.pop()?.truthy() {
                        self.frames[fi].ip = t;
                    }
                }
                Op::JumpIfFalseKeep(t) => {
                    if self.peek(0)?.truthy() {
                        self.pop()?;
                    } else {
                        self.frames[fi].ip = t;
                    }
                }
                Op::JumpIfTrueKeep(t) => {
                    if self.peek(0)?.truthy() {
                        self.frames[fi].ip = t;
                    } else {
                        self.pop()?;
                    }
                }
                Op::Call(argc) => self.call(argc)?,
                Op::Return => {
                    let result = self.pop()?;
                    let done = self.frames.pop().unwrap();
                    self.stack.truncate(done.base as usize);
                    if self.frames.is_empty() {
                        self.status = Status::Done(result);
                        let data = self
                            .heap
                            .to_data(result)
                            .unwrap_or_else(|_| Data::Str(self.display(result)));
                        return Ok(Outcome::Done(data));
                    }
                    self.stack.push(result);
                }
                Op::Closure(fidx) => {
                    let f = program
                        .functions
                        .get(fidx as usize)
                        .ok_or_else(|| self.error("bad function"))?;
                    let mut captures = Vec::with_capacity(f.captures.len());
                    for c in &f.captures {
                        captures.push(match *c {
                            Capture::Local(s) => {
                                let i = self.slot(frame.base, s)?;
                                self.cell(self.stack[i])?
                            }
                            Capture::Outer(o) => self.capture(&frame, o)?,
                        });
                    }
                    let r = self.heap.alloc(Obj::Closure { func: fidx, captures });
                    self.stack.push(Value::Obj(r));
                }
                Op::List(n) => {
                    let start = self
                        .stack
                        .len()
                        .checked_sub(n as usize)
                        .ok_or_else(|| self.error("stack underflow"))?;
                    let items = self.stack.split_off(start);
                    let r = self.heap.alloc(Obj::List(items));
                    self.stack.push(Value::Obj(r));
                }
                Op::Map(n) => {
                    let start = self
                        .stack
                        .len()
                        .checked_sub(2 * n as usize)
                        .ok_or_else(|| self.error("stack underflow"))?;
                    let items = self.stack.split_off(start);
                    let mut m = Map::default();
                    for pair in items.chunks(2) {
                        let k = self
                            .heap
                            .as_str(pair[0])
                            .ok_or_else(|| self.error("map key is not a string"))?;
                        let k = k.to_string();
                        m.set(&k, pair[1]);
                    }
                    let r = self.heap.alloc(Obj::Map(m));
                    self.stack.push(Value::Obj(r));
                }
                Op::Index => {
                    let index = self.pop()?;
                    let obj = self.pop()?;
                    let v = self.index(obj, index)?;
                    self.stack.push(v);
                }
                Op::SetIndex => {
                    let v = self.pop()?;
                    let index = self.pop()?;
                    let obj = self.pop()?;
                    self.set_index(obj, index, v)?;
                }
                Op::GetField(k) => {
                    let obj = self.pop()?;
                    let name = self
                        .consts
                        .get(k as usize)
                        .copied()
                        .ok_or_else(|| self.error("bad constant"))?;
                    let v = self.field(obj, name)?;
                    self.stack.push(v);
                }
                Op::SetField(k) => {
                    let v = self.pop()?;
                    let obj = self.pop()?;
                    let name = self
                        .consts
                        .get(k as usize)
                        .copied()
                        .ok_or_else(|| self.error("bad constant"))?;
                    self.set_index(obj, name, v)?;
                }
                Op::Concat(n) => {
                    let start = self
                        .stack
                        .len()
                        .checked_sub(n as usize)
                        .ok_or_else(|| self.error("stack underflow"))?;
                    let mut s = String::new();
                    for v in &self.stack[start..] {
                        s.push_str(&self.display(*v));
                    }
                    self.stack.truncate(start);
                    let v = self.str_value(&s);
                    self.stack.push(v);
                }
                Op::IterStart => {
                    let v = self.pop()?;
                    let seq = match v {
                        Value::Obj(r) => match self.heap.get(r) {
                            Obj::List(_) => v,
                            Obj::Map(m) => {
                                let keys: Vec<String> = m.iter().map(|(k, _)| k.to_string()).collect();
                                let items = keys.iter().map(|k| self.heap.str(k)).collect();
                                Value::Obj(self.heap.alloc(Obj::List(items)))
                            }
                            Obj::Str(s) => {
                                let chars: Vec<String> = s.chars().map(String::from).collect();
                                let items = chars.iter().map(|c| self.heap.str(c)).collect();
                                Value::Obj(self.heap.alloc(Obj::List(items)))
                            }
                            _ => return Err(self.error(format!("cannot loop over a {}", self.type_name(v)))),
                        },
                        _ => return Err(self.error(format!("cannot loop over {}", self.type_name(v)))),
                    };
                    self.stack.push(seq);
                }
                Op::ForNext { seq, exit } => {
                    let si = self.slot(frame.base, seq)?;
                    let ii = self.slot(frame.base, seq + 1)?;
                    let Value::Int(i) = self.stack[ii] else {
                        return Err(self.error("bad loop index"));
                    };
                    let next = match self.stack[si] {
                        Value::Obj(r) => match self.heap.get(r) {
                            Obj::List(items) => items.get(i as usize).copied(),
                            _ => return Err(self.error("bad loop sequence")),
                        },
                        _ => return Err(self.error("bad loop sequence")),
                    };
                    match next {
                        Some(v) => {
                            self.stack[ii] = Value::Int(i + 1);
                            self.stack.push(v);
                        }
                        None => self.frames[fi].ip = exit,
                    }
                }
                Op::Effect { name, argc } => {
                    let start = self
                        .stack
                        .len()
                        .checked_sub(argc as usize)
                        .ok_or_else(|| self.error("stack underflow"))?;
                    let args = self.stack.split_off(start);
                    let effect = self
                        .heap
                        .as_str(
                            *self
                                .consts
                                .get(name as usize)
                                .ok_or_else(|| self.error("bad constant"))?,
                        )
                        .ok_or_else(|| self.error("bad effect name"))?
                        .to_string();
                    let mut data = Vec::with_capacity(args.len());
                    for a in &args {
                        data.push(
                            self.heap
                                .to_data(*a)
                                .map_err(|e| self.error(format!("`await {effect}`: {e}")))?,
                        );
                    }
                    self.status = Status::Waiting {
                        effect: effect.clone(),
                        args,
                    };
                    return Ok(Outcome::Effect {
                        name: effect,
                        args: data,
                    });
                }
            }
        }
    }

    fn call(&mut self, argc: u8) -> Result<(), Error> {
        let callee_at = self
            .stack
            .len()
            .checked_sub(argc as usize + 1)
            .ok_or_else(|| self.error("stack underflow"))?;
        let callee = self.stack[callee_at];
        match callee {
            Value::Obj(r) => {
                let Obj::Closure { func, .. } = self.heap.get(r) else {
                    return Err(self.error(format!("cannot call a {}", self.type_name(callee))));
                };
                let func = *func;
                let f = &self.program.functions[func as usize];
                if f.arity != argc {
                    return Err(self.error(format!(
                        "{} takes {} argument{} but got {argc}",
                        f.name,
                        f.arity,
                        if f.arity == 1 { "" } else { "s" }
                    )));
                }
                if self.frames.len() >= MAX_FRAMES {
                    return Err(self.error("stack overflow: too much recursion"));
                }
                self.frames.push(Frame {
                    func,
                    closure: Some(r),
                    ip: 0,
                    base: callee_at as u32,
                });
                Ok(())
            }
            Value::Native(n) => {
                let (name, native) = BUILTINS[n as usize];
                let args: Vec<Value> = self.stack[callee_at + 1..].to_vec();
                let v = native(self, &args).map_err(|e| self.error(format!("{name}: {e}")))?;
                self.stack.truncate(callee_at);
                self.stack.push(v);
                Ok(())
            }
            other => Err(self.error(format!("cannot call {}", self.type_name(other)))),
        }
    }

    fn add(&mut self, a: Value, b: Value) -> Result<Value, Error> {
        Ok(match (a, b) {
            (Value::Int(x), Value::Int(y)) => {
                Value::Int(x.checked_add(y).ok_or_else(|| self.error("integer overflow"))?)
            }
            (Value::Obj(x), Value::Obj(y)) => match (self.heap.get(x), self.heap.get(y)) {
                (Obj::Str(p), Obj::Str(q)) => {
                    let s = format!("{p}{q}");
                    self.str_value(&s)
                }
                (Obj::List(p), Obj::List(q)) => {
                    let items = p.iter().chain(q).copied().collect();
                    Value::Obj(self.heap.alloc(Obj::List(items)))
                }
                _ => return Err(self.type_error("add", a, b)),
            },
            _ => match (num(a), num(b)) {
                (Some(x), Some(y)) => Value::Float(x + y),
                _ => return Err(self.type_error("add", a, b)),
            },
        })
    }

    fn arith(&mut self, op: Op, a: Value, b: Value) -> Result<Value, Error> {
        let verb = match op {
            Op::Sub => "subtract",
            Op::Mul => "multiply",
            Op::Div => "divide",
            _ => "take the remainder of",
        };
        if let (Value::Int(x), Value::Int(y)) = (a, b) {
            if matches!(op, Op::Div | Op::Mod) && y == 0 {
                return Err(self.error("division by zero"));
            }
            let r = match op {
                Op::Sub => x.checked_sub(y),
                Op::Mul => x.checked_mul(y),
                Op::Div => x.checked_div(y),
                _ => x.checked_rem_euclid(y),
            };
            return r.map(Value::Int).ok_or_else(|| self.error("integer overflow"));
        }
        match (num(a), num(b)) {
            (Some(x), Some(y)) => Ok(Value::Float(match op {
                Op::Sub => x - y,
                Op::Mul => x * y,
                Op::Div => x / y,
                _ => x.rem_euclid(y),
            })),
            _ => Err(self.type_error(verb, a, b)),
        }
    }

    fn type_error(&self, verb: &str, a: Value, b: Value) -> Error {
        self.error(format!("cannot {verb} {} and {}", self.type_name(a), self.type_name(b)))
    }

    pub(crate) fn compare(&self, a: Value, b: Value) -> Result<std::cmp::Ordering, Error> {
        if let (Value::Int(x), Value::Int(y)) = (a, b) {
            return Ok(x.cmp(&y));
        }
        if let (Some(x), Some(y)) = (num(a), num(b)) {
            return x.partial_cmp(&y).ok_or_else(|| self.error("cannot compare NaN"));
        }
        if let (Some(x), Some(y)) = (self.heap.as_str(a), self.heap.as_str(b)) {
            return Ok(x.cmp(y));
        }
        Err(self.error(format!(
            "cannot compare {} and {}",
            self.type_name(a),
            self.type_name(b)
        )))
    }

    fn index(&mut self, obj: Value, index: Value) -> Result<Value, Error> {
        let Value::Obj(r) = obj else {
            return Err(self.error(format!("cannot index {}", self.type_name(obj))));
        };
        match self.heap.get(r) {
            Obj::List(items) => {
                let i = self.position(index, items.len())?;
                Ok(items[i])
            }
            Obj::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                let i = self.position(index, chars.len())?;
                let c = chars[i].to_string();
                Ok(self.str_value(&c))
            }
            Obj::Map(m) => match self.heap.as_str(index) {
                Some(k) => Ok(m.get(k).unwrap_or(Value::Nil)),
                None => Err(self.error(format!("map keys are strings, not {}", self.type_name(index)))),
            },
            _ => Err(self.error(format!("cannot index a {}", self.type_name(obj)))),
        }
    }

    /// Resolves a list index, counting from the end when negative.
    fn position(&self, index: Value, len: usize) -> Result<usize, Error> {
        let Value::Int(i) = index else {
            return Err(self.error(format!("index must be an int, not {}", self.type_name(index))));
        };
        let j = if i < 0 { i + len as i64 } else { i };
        if j < 0 || j >= len as i64 {
            return Err(self.error(format!("index {i} out of range for length {len}")));
        }
        Ok(j as usize)
    }

    fn set_index(&mut self, obj: Value, index: Value, v: Value) -> Result<(), Error> {
        let Value::Obj(r) = obj else {
            return Err(self.error(format!("cannot set an index on {}", self.type_name(obj))));
        };
        match self.heap.get(r) {
            Obj::List(items) => {
                let i = self.position(index, items.len())?;
                let Obj::List(items) = self.heap.get_mut(r) else {
                    unreachable!()
                };
                items[i] = v;
                Ok(())
            }
            Obj::Map(_) => {
                let Some(k) = self.heap.as_str(index).map(str::to_string) else {
                    return Err(self.error(format!("map keys are strings, not {}", self.type_name(index))));
                };
                let Obj::Map(m) = self.heap.get_mut(r) else {
                    unreachable!()
                };
                m.set(&k, v);
                Ok(())
            }
            Obj::Str(_) => Err(self.error("strings cannot be changed; build a new one")),
            _ => Err(self.error(format!("cannot set an index on a {}", self.type_name(obj)))),
        }
    }

    fn field(&self, obj: Value, name: Value) -> Result<Value, Error> {
        if let Value::Obj(r) = obj
            && let Obj::Map(m) = self.heap.get(r)
        {
            let k = self.heap.as_str(name).unwrap_or_default();
            return Ok(m.get(k).unwrap_or(Value::Nil));
        }
        let k = self.heap.as_str(name).unwrap_or_default();
        Err(self.error(format!("cannot read field `{k}` of {}", self.type_name(obj))))
    }
}

pub(crate) fn num(v: Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(n as f64),
        Value::Float(x) => Some(x),
        _ => None,
    }
}
