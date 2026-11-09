//! The browser playground's interface: plain exported functions, no bindings generator.
//!
//! Strings go in as UTF-8 written into memory from [`alloc`]; answers come back as JSON (or raw bytes for
//! checkpoints) in a result buffer read through [`result_ptr`] and [`result_len`].
//!
//! The session records the run as it goes: a checkpoint every [`SPACING`] instructions and at every effect, plus
//! every effect's result. Seeking to any instruction restores the nearest earlier checkpoint and replays from
//! there, which works because the VM is deterministic once effects are fixed. That is the time-travel slider.

use std::cell::RefCell;

use crate::bytecode::Op;
use crate::json;
use crate::value::Data;
use crate::vm::{Outcome, Status, Vm};
use crate::{Error, compile};

const SPACING: u64 = 1000;
/// Above this, every other checkpoint is dropped (spacing doubles), so long runs stay within memory.
const MAX_CHECKPOINT_BYTES: usize = 48 << 20;

struct Checkpoint {
    step: u64,
    bytes: Vec<u8>,
    /// Length of the transcript when it was taken.
    out_len: usize,
}

#[derive(Default)]
struct Session {
    head: Option<Vm>,
    /// Everything the program printed, at the head.
    transcript: String,
    checkpoints: Vec<Checkpoint>,
    checkpoint_bytes: usize,
    spacing: u64,
    /// Results of the effects, in order; replays read them instead of asking the host again.
    journal: Vec<Data>,
    /// Steps at which effects happened, for the timeline.
    effect_steps: Vec<u64>,
    /// A position in the past being shown; `None` shows the head.
    view: Option<(Vm, String)>,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn set_result(bytes: Vec<u8>) {
    RESULT.with(|r| *r.borrow_mut() = bytes);
}

fn reply(d: Data) {
    set_result(json::encode(&d).into_bytes());
}

fn input(ptr: *mut u8, len: usize) -> Vec<u8> {
    // SAFETY: JavaScript filled `len` bytes at `ptr`, a buffer it got from `alloc(len)`.
    unsafe { Vec::from_raw_parts(ptr, len, len) }
}

fn s(x: &str) -> Data {
    Data::Str(x.to_string())
}

fn n(x: u64) -> Data {
    Data::Int(x as i64)
}

fn obj(entries: Vec<(&str, Data)>) -> Data {
    Data::Map(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn error_data(e: &Error) -> Data {
    let mut fields = vec![("message", s(&e.message)), ("text", s(&e.to_string()))];
    if let Some(span) = e.span {
        fields.push(("line", Data::Int(span.line as i64)));
        fields.push(("col", Data::Int(span.col as i64)));
    }
    if let Some(top) = e.trace.first() {
        fields.push(("line", Data::Int(top.line as i64)));
    }
    obj(fields)
}

#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let ptr = v.as_mut_ptr();
    std::mem::forget(v);
    ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn result_len() -> usize {
    RESULT.with(|r| r.borrow().len())
}

impl Session {
    fn start(&mut self, vm: Vm) {
        *self = Session {
            spacing: SPACING,
            ..Session::default()
        };
        self.head = Some(vm);
        self.checkpoint();
    }

    fn checkpoint(&mut self) {
        let Some(vm) = self.head.as_mut() else { return };
        let step = vm.steps;
        if self
            .checkpoints
            .last()
            .is_some_and(|c| c.step == step && c.out_len == self.transcript.len())
        {
            return;
        }
        let bytes = vm.snapshot();
        self.checkpoint_bytes += bytes.len();
        self.checkpoints.push(Checkpoint {
            step,
            bytes,
            out_len: self.transcript.len(),
        });
        if self.checkpoint_bytes > MAX_CHECKPOINT_BYTES {
            self.spacing *= 2;
            let spacing = self.spacing;
            let effects = self.effect_steps.clone();
            let mut kept = Vec::new();
            for (i, c) in std::mem::take(&mut self.checkpoints).into_iter().enumerate() {
                if i == 0 || c.step % spacing == 0 || effects.contains(&c.step) {
                    kept.push(c);
                }
            }
            self.checkpoint_bytes = kept.iter().map(|c| c.bytes.len()).sum();
            self.checkpoints = kept;
        }
    }

    /// Runs the head forward by up to `budget` instructions, stopping at checkpoint boundaries.
    fn run(&mut self, budget: u64) -> Data {
        self.view = None;
        let Some(vm) = self.head.as_mut() else {
            return obj(vec![("status", s("crashed"))]);
        };
        if !matches!(vm.status(), Status::Running) {
            return self.view_data();
        }
        let mut left = budget;
        let mut outcome = Ok(Outcome::Paused);
        while left > 0 {
            let vm = self.head.as_mut().unwrap();
            let slice = left.min(self.spacing - vm.steps % self.spacing);
            let before = vm.steps;
            outcome = vm.run(Some(slice));
            let (ran, steps) = (vm.steps - before, vm.steps);
            let out = vm.take_output();
            self.transcript.push_str(&out);
            left = left.saturating_sub(ran.max(1));
            match &outcome {
                Ok(Outcome::Paused) => {
                    if steps % self.spacing == 0 {
                        self.checkpoint();
                    }
                }
                Ok(Outcome::Effect { .. }) => {
                    self.effect_steps.push(steps);
                    self.checkpoint();
                    break;
                }
                _ => break,
            }
        }
        let mut d = self.view_data();
        if let (Data::Map(fields), Err(e)) = (&mut d, &outcome) {
            fields.push(("error".into(), error_data(e)));
        }
        d
    }

    fn resume(&mut self, value: Data) -> Data {
        let Some(vm) = self.head.as_mut() else {
            return obj(vec![("status", s("crashed"))]);
        };
        self.view = None;
        if vm.resume(&value).is_ok() {
            self.journal.push(value);
            self.checkpoint();
        }
        self.view_data()
    }

    fn fail(&mut self, message: &str) -> Data {
        let Some(vm) = self.head.as_mut() else {
            return obj(vec![("status", s("crashed"))]);
        };
        self.view = None;
        let e = vm.fail_effect(message);
        let mut d = self.view_data();
        if let Data::Map(fields) = &mut d {
            fields.push(("error".into(), error_data(&e)));
        }
        d
    }

    /// Rebuilds the VM as it was after `step` instructions.
    fn seek(&mut self, step: u64) -> Data {
        let head_steps = self.head.as_ref().map_or(0, |vm| vm.steps);
        if step >= head_steps {
            self.view = None;
            return self.view_data();
        }
        let Some(c) = self.checkpoints.iter().rev().find(|c| c.step <= step) else {
            return self.view_data();
        };
        let mut vm = Vm::restore(&c.bytes).expect("own checkpoint");
        let mut out = self.transcript[..c.out_len].to_string();
        while vm.steps < step {
            if matches!(vm.status(), Status::Waiting { .. }) {
                let Some(v) = self.journal.get(vm.effects as usize) else {
                    break;
                };
                vm.resume(v).expect("waiting");
            }
            let r = vm.run(Some(step - vm.steps));
            out.push_str(&vm.take_output());
            if !matches!(r, Ok(Outcome::Paused) | Ok(Outcome::Effect { .. })) {
                break;
            }
        }
        self.view = Some((vm, out));
        self.view_data()
    }

    fn view_data(&mut self) -> Data {
        let head = self.head.as_ref();
        let (vm, output, past) = match (&self.view, head) {
            (Some((vm, out)), _) => (vm, out.as_str(), true),
            (None, Some(vm)) => (vm, self.transcript.as_str(), false),
            (None, None) => return obj(vec![("status", s("crashed"))]),
        };
        let (status, effect) = match vm.status() {
            Status::Running => ("running", Data::Nil),
            Status::Waiting { .. } => {
                let (name, args) = vm.pending().unwrap();
                ("waiting", obj(vec![("name", s(&name)), ("args", Data::List(args))]))
            }
            Status::Done(v) => ("done", s(&vm.repr(*v))),
            Status::Failed => ("failed", Data::Nil),
        };
        let stack = vm
            .call_stack()
            .into_iter()
            .map(|f| {
                obj(vec![
                    ("function", s(&f.function)),
                    ("line", n(f.line as u64)),
                    (
                        "locals",
                        Data::List(
                            f.locals
                                .into_iter()
                                .map(|(k, v)| Data::List(vec![s(&k), s(&v)]))
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        let (func, pc) = vm
            .frames
            .last()
            .map(|f| {
                let pc = if matches!(vm.status(), Status::Running) {
                    f.ip
                } else {
                    f.ip.saturating_sub(1)
                };
                (f.func as u64, pc as u64)
            })
            .unwrap_or((0, 0));
        let last = self.checkpoints.last();
        obj(vec![
            ("status", s(status)),
            ("effect", effect),
            ("step", n(vm.steps)),
            ("headStep", n(head.map_or(0, |h| h.steps))),
            ("past", Data::Bool(past)),
            ("effects", n(vm.effects)),
            ("line", n(vm.line() as u64)),
            ("func", n(func)),
            ("pc", n(pc)),
            ("stack", Data::List(stack)),
            ("output", s(output)),
            ("heap", n(vm.heap_size() as u64)),
            ("checkpoints", n(self.checkpoints.len() as u64)),
            ("checkpointSize", n(last.map_or(0, |c| c.bytes.len() as u64))),
            ("checkpointStep", n(last.map_or(0, |c| c.step))),
            (
                "effectSteps",
                Data::List(self.effect_steps.iter().map(|&x| n(x)).collect()),
            ),
        ])
    }
}

fn program_data(vm: &Vm) -> Data {
    let p = vm.program();
    let functions = p
        .functions
        .iter()
        .map(|f| {
            let ops = f
                .code
                .iter()
                .enumerate()
                .map(|(pc, op)| {
                    let effect = matches!(op, Op::Effect { .. });
                    Data::List(vec![n(f.lines[pc] as u64), s(&p.describe(f, *op)), Data::Bool(effect)])
                })
                .collect();
            obj(vec![
                ("name", s(&f.name)),
                ("arity", n(f.arity as u64)),
                ("ops", Data::List(ops)),
            ])
        })
        .collect();
    obj(vec![("source", s(&p.source)), ("functions", Data::List(functions))])
}

/// Compiles a program and starts a session at its first instruction.
#[unsafe(no_mangle)]
pub extern "C" fn load(ptr: *mut u8, len: usize) -> u32 {
    let src = String::from_utf8_lossy(&input(ptr, len)).into_owned();
    match compile(&src) {
        Ok(program) => {
            let vm = Vm::new(program);
            let d = program_data(&vm);
            SESSION.with(|ss| ss.borrow_mut().start(vm));
            reply(d);
            0
        }
        Err(e) => {
            reply(error_data(&e));
            1
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn run(budget: f64) {
    reply(SESSION.with(|ss| ss.borrow_mut().run(budget.max(1.0) as u64)));
}

/// Answers the awaited effect with a JSON value.
#[unsafe(no_mangle)]
pub extern "C" fn resume(ptr: *mut u8, len: usize) {
    let text = String::from_utf8_lossy(&input(ptr, len)).into_owned();
    let value = json::decode(&text).unwrap_or(Data::Str(text));
    reply(SESSION.with(|ss| ss.borrow_mut().resume(value)));
}

/// Makes the awaited effect fail with a message.
#[unsafe(no_mangle)]
pub extern "C" fn fail(ptr: *mut u8, len: usize) {
    let message = String::from_utf8_lossy(&input(ptr, len)).into_owned();
    reply(SESSION.with(|ss| ss.borrow_mut().fail(&message)));
}

#[unsafe(no_mangle)]
pub extern "C" fn seek(step: f64) {
    reply(SESSION.with(|ss| ss.borrow_mut().seek(step.max(0.0) as u64)));
}

#[unsafe(no_mangle)]
pub extern "C" fn view() {
    reply(SESSION.with(|ss| ss.borrow_mut().view_data()));
}

/// The latest checkpoint, as bytes: the same format the `spola` command reads.
#[unsafe(no_mangle)]
pub extern "C" fn checkpoint() {
    let bytes = SESSION.with(|ss| {
        ss.borrow()
            .checkpoints
            .last()
            .map(|c| c.bytes.clone())
            .unwrap_or_default()
    });
    set_result(bytes);
}

/// Simulates a crash: everything in memory is gone.
#[unsafe(no_mangle)]
pub extern "C" fn crash() {
    SESSION.with(|ss| *ss.borrow_mut() = Session::default());
}

/// Starts a session from checkpoint bytes, made here or by the `spola` command.
#[unsafe(no_mangle)]
pub extern "C" fn restore(ptr: *mut u8, len: usize) -> u32 {
    match Vm::restore(&input(ptr, len)) {
        Ok(vm) => {
            let d = program_data(&vm);
            SESSION.with(|ss| {
                let mut ss = ss.borrow_mut();
                let effects = vm.effects as usize;
                ss.start(vm);
                // effects before the checkpoint are not in this journal; pad it so indexes line up
                ss.journal = vec![Data::Nil; effects];
            });
            reply(d);
            0
        }
        Err(e) => {
            reply(error_data(&e));
            1
        }
    }
}
