//! Checkpoints: the whole state of a running program as bytes.
//!
//! A checkpoint holds the program's source, its heap, stack, call frames and globals. Restoring one recompiles the
//! source, checks that it produces the same program, and validates every reference, so a damaged or hand-made
//! checkpoint is rejected with an error instead of crashing the VM.
//!
//! Layout (integers are LEB128 varints unless noted):
//!
//! ```text
//! "SPOLA" format(u8) fingerprint(u64 LE) source
//! steps effects host:Data status globals consts stack frames heap
//! checksum(u64 LE, FNV-1a of everything before it)
//! ```

use std::sync::Arc;

use crate::builtins::BUILTINS;
use crate::bytecode::{FNV_OFFSET, fnv1a};
use crate::compiler::compile;
use crate::error::Error;
use crate::value::{Data, Heap, Map, Obj, Ref, Value};
use crate::vm::{Frame, MAX_FRAMES, Status, Vm};

const MAGIC: &[u8] = b"SPOLA";
const FORMAT: u8 = 1;

impl Vm {
    /// Writes the program's whole state to bytes. Collects garbage first, so only live objects are saved.
    pub fn snapshot(&mut self) -> Vec<u8> {
        self.compact();
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.0.push(FORMAT);
        w.0.extend_from_slice(&self.program.fingerprint.to_le_bytes());
        w.str(&self.program.source);
        w.uint(self.steps);
        w.uint(self.effects);
        w.data(&self.host);
        match &self.status {
            Status::Running => w.uint(0),
            Status::Waiting { effect, args } => {
                w.uint(1);
                w.str(effect);
                w.values(args);
            }
            Status::Done(v) => {
                w.uint(2);
                w.value(*v);
            }
            Status::Failed => w.uint(3),
        }
        w.uint(self.globals.len() as u64);
        for g in &self.globals {
            match g {
                None => w.uint(0),
                Some(v) => {
                    w.uint(1);
                    w.value(*v);
                }
            }
        }
        w.values(&self.consts);
        w.values(&self.stack);
        w.uint(self.frames.len() as u64);
        for f in &self.frames {
            w.uint(f.func as u64);
            w.uint(f.closure.map_or(0, |r| r.0 as u64 + 1));
            w.uint(f.ip as u64);
            w.uint(f.base as u64);
        }
        w.uint(self.heap.objs.len() as u64);
        for slot in &self.heap.objs {
            match slot {
                None => w.uint(0),
                Some(Obj::Str(s)) => {
                    w.uint(1);
                    w.str(s);
                }
                Some(Obj::List(items)) => {
                    w.uint(2);
                    w.values(items);
                }
                Some(Obj::Map(m)) => {
                    w.uint(3);
                    w.uint(m.len() as u64);
                    for (k, v) in m.iter() {
                        w.str(k);
                        w.value(v);
                    }
                }
                Some(Obj::Closure { func, captures }) => {
                    w.uint(4);
                    w.uint(*func as u64);
                    w.uint(captures.len() as u64);
                    for c in captures {
                        w.uint(c.0 as u64);
                    }
                }
                Some(Obj::Cell(v)) => {
                    w.uint(5);
                    w.value(*v);
                }
            }
        }
        let sum = fnv1a(&w.0, FNV_OFFSET);
        w.0.extend_from_slice(&sum.to_le_bytes());
        w.0
    }

    /// Rebuilds a program from a checkpoint made by [`Vm::snapshot`]. Any program output not yet taken is lost.
    pub fn restore(bytes: &[u8]) -> Result<Vm, Error> {
        if bytes.len() < MAGIC.len() + 1 + 8 + 8 || !bytes.starts_with(MAGIC) {
            return Err(Error::snapshot("not a Spola checkpoint"));
        }
        if bytes[MAGIC.len()] != FORMAT {
            return Err(Error::snapshot(format!(
                "unsupported checkpoint format {}",
                bytes[MAGIC.len()]
            )));
        }
        let (body, sum) = bytes.split_at(bytes.len() - 8);
        if fnv1a(body, FNV_OFFSET).to_le_bytes() != sum {
            return Err(Error::snapshot("checksum mismatch: the checkpoint is damaged"));
        }
        let mut r = Reader {
            b: body,
            i: MAGIC.len() + 1,
        };
        let fingerprint = u64::from_le_bytes(r.take(8)?.try_into().unwrap());
        let source = r.str()?;
        let program = compile(&source).map_err(|e| Error::snapshot(format!("its source no longer compiles: {e}")))?;
        if program.fingerprint != fingerprint {
            return Err(Error::snapshot("it was made by a different version of Spola"));
        }
        let steps = r.uint()?;
        let effects = r.uint()?;
        let host = r.data(0)?;
        let status_tag = r.uint()?;
        let status = match status_tag {
            0 => Status::Running,
            1 => Status::Waiting {
                effect: r.str()?,
                args: r.values()?,
            },
            2 => Status::Done(r.value()?),
            3 => Status::Failed,
            _ => return Err(Error::snapshot("bad status")),
        };
        let n = r.len()?;
        if n != program.globals.len() {
            return Err(Error::snapshot("global count does not match the program"));
        }
        let mut globals = Vec::with_capacity(n);
        for _ in 0..n {
            globals.push(match r.uint()? {
                0 => None,
                1 => Some(r.value()?),
                _ => return Err(Error::snapshot("bad global")),
            });
        }
        let consts = r.values()?;
        if consts.len() != program.consts.len() {
            return Err(Error::snapshot("constant count does not match the program"));
        }
        let stack = r.values()?;
        let n = r.len()?;
        if n > MAX_FRAMES {
            return Err(Error::snapshot("too many frames"));
        }
        let mut frames = Vec::with_capacity(n);
        for _ in 0..n {
            let func = r.u32()?;
            let closure = match r.u32()? {
                0 => None,
                c => Some(Ref(c - 1)),
            };
            frames.push(Frame {
                func,
                closure,
                ip: r.u32()?,
                base: r.u32()?,
            });
        }
        let n = r.len()?;
        let mut objs = Vec::with_capacity(n);
        for _ in 0..n {
            objs.push(match r.uint()? {
                0 => None,
                1 => Some(Obj::Str(r.str()?.into())),
                2 => Some(Obj::List(r.values()?)),
                3 => {
                    let n = r.len()?;
                    let mut m = Map::default();
                    for _ in 0..n {
                        let k = r.str()?;
                        let v = r.value()?;
                        m.set(&k, v);
                    }
                    Some(Obj::Map(m))
                }
                4 => {
                    let func = r.u32()?;
                    let n = r.len()?;
                    let mut captures = Vec::with_capacity(n);
                    for _ in 0..n {
                        captures.push(Ref(r.u32()?));
                    }
                    Some(Obj::Closure { func, captures })
                }
                5 => Some(Obj::Cell(r.value()?)),
                _ => return Err(Error::snapshot("bad object")),
            });
        }
        if r.i != r.b.len() {
            return Err(Error::snapshot("trailing bytes"));
        }
        let vm = Vm {
            program: Arc::new(program),
            heap: Heap::from_slots(objs),
            stack,
            frames,
            globals,
            consts,
            status,
            steps,
            effects,
            output: String::new(),
            host,
        };
        validate(&vm)?;
        Ok(vm)
    }
}

/// Checks every reference in a restored VM, so the interpreter never meets a dangling one.
fn validate(vm: &Vm) -> Result<(), Error> {
    let bad = |what: &str| Err(Error::snapshot(format!("invalid {what}")));
    let p = &vm.program;
    let heap_ok = |r: Ref| vm.heap.objs.get(r.0 as usize).is_some_and(|o| o.is_some());
    let value_ok = |v: &Value| match *v {
        Value::Obj(r) => heap_ok(r),
        Value::Native(n) => (n as usize) < BUILTINS.len(),
        _ => true,
    };
    for obj in vm.heap.objs.iter().flatten() {
        let ok = match obj {
            Obj::Str(_) => true,
            Obj::List(items) => items.iter().all(value_ok),
            Obj::Map(m) => m.iter().all(|(_, v)| value_ok(&v)),
            Obj::Closure { func, captures } => p.functions.get(*func as usize).is_some_and(|f| {
                f.captures.len() == captures.len()
                    && captures
                        .iter()
                        .all(|c| heap_ok(*c) && matches!(vm.heap.get(*c), Obj::Cell(_)))
            }),
            Obj::Cell(v) => value_ok(v),
        };
        if !ok {
            return bad("heap object");
        }
    }
    if !vm.stack.iter().all(value_ok) || !vm.globals.iter().flatten().all(value_ok) {
        return bad("value");
    }
    for (c, v) in p.consts.iter().zip(&vm.consts) {
        let same = match (c, v) {
            (crate::bytecode::Const::Int(n), Value::Int(m)) => n == m,
            (crate::bytecode::Const::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
            (crate::bytecode::Const::Str(s), v) => value_ok(v) && vm.heap.as_str(*v) == Some(s.as_str()),
            _ => false,
        };
        if !same {
            return bad("constant");
        }
    }
    let mut last_base = 0;
    for (i, f) in vm.frames.iter().enumerate() {
        let Some(func) = p.functions.get(f.func as usize) else {
            return bad("frame");
        };
        let closure_ok = match f.closure {
            None => i == 0 && f.func == 0,
            Some(r) => {
                heap_ok(r) && matches!(vm.heap.get(r), Obj::Closure { func, .. } if *func == f.func) && f.func != 0
            }
        };
        if !closure_ok
            || f.ip as usize > func.code.len()
            || (f.base as usize) >= vm.stack.len().max(1)
            || f.base < last_base
        {
            return bad("frame");
        }
        last_base = f.base;
    }
    let status_ok = match &vm.status {
        Status::Running => !vm.frames.is_empty(),
        Status::Waiting { args, .. } => !vm.frames.is_empty() && args.iter().all(value_ok),
        Status::Done(v) => vm.frames.is_empty() && value_ok(v),
        Status::Failed => true,
    };
    if !status_ok {
        return bad("status");
    }
    Ok(())
}

struct Writer(Vec<u8>);

impl Writer {
    fn uint(&mut self, mut n: u64) {
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    fn int(&mut self, n: i64) {
        self.uint(((n << 1) ^ (n >> 63)) as u64);
    }

    fn str(&mut self, s: &str) {
        self.uint(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }

    fn value(&mut self, v: Value) {
        match v {
            Value::Nil => self.uint(0),
            Value::Bool(false) => self.uint(1),
            Value::Bool(true) => self.uint(2),
            Value::Int(n) => {
                self.uint(3);
                self.int(n);
            }
            Value::Float(x) => {
                self.uint(4);
                self.0.extend_from_slice(&x.to_bits().to_le_bytes());
            }
            Value::Obj(r) => {
                self.uint(5);
                self.uint(r.0 as u64);
            }
            Value::Native(n) => {
                self.uint(6);
                self.uint(n as u64);
            }
        }
    }

    fn values(&mut self, vs: &[Value]) {
        self.uint(vs.len() as u64);
        for v in vs {
            self.value(*v);
        }
    }

    fn data(&mut self, d: &Data) {
        match d {
            Data::Nil => self.uint(0),
            Data::Bool(false) => self.uint(1),
            Data::Bool(true) => self.uint(2),
            Data::Int(n) => {
                self.uint(3);
                self.int(*n);
            }
            Data::Float(x) => {
                self.uint(4);
                self.0.extend_from_slice(&x.to_bits().to_le_bytes());
            }
            Data::Str(s) => {
                self.uint(5);
                self.str(s);
            }
            Data::List(items) => {
                self.uint(6);
                self.uint(items.len() as u64);
                items.iter().for_each(|d| self.data(d));
            }
            Data::Map(entries) => {
                self.uint(7);
                self.uint(entries.len() as u64);
                for (k, v) in entries {
                    self.str(k);
                    self.data(v);
                }
            }
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        let end = self
            .i
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| Error::snapshot("truncated"))?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }

    fn uint(&mut self) -> Result<u64, Error> {
        let mut n: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = self.take(1)?[0];
            n |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(Error::snapshot("bad integer"))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        u32::try_from(self.uint()?).map_err(|_| Error::snapshot("number out of range"))
    }

    /// A count of items that follow; each takes at least one byte, which bounds allocations.
    fn len(&mut self) -> Result<usize, Error> {
        let n = self.uint()?;
        if n > (self.b.len() - self.i) as u64 {
            return Err(Error::snapshot("truncated"));
        }
        Ok(n as usize)
    }

    fn int(&mut self) -> Result<i64, Error> {
        let u = self.uint()?;
        Ok((u >> 1) as i64 ^ -((u & 1) as i64))
    }

    fn f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_bits(u64::from_le_bytes(self.take(8)?.try_into().unwrap())))
    }

    fn str(&mut self) -> Result<String, Error> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| Error::snapshot("invalid UTF-8"))
    }

    fn value(&mut self) -> Result<Value, Error> {
        Ok(match self.uint()? {
            0 => Value::Nil,
            1 => Value::Bool(false),
            2 => Value::Bool(true),
            3 => Value::Int(self.int()?),
            4 => Value::Float(self.f64()?),
            5 => Value::Obj(Ref(self.u32()?)),
            6 => Value::Native(u16::try_from(self.uint()?).map_err(|_| Error::snapshot("bad builtin"))?),
            _ => return Err(Error::snapshot("bad value")),
        })
    }

    fn values(&mut self) -> Result<Vec<Value>, Error> {
        let n = self.len()?;
        (0..n).map(|_| self.value()).collect()
    }

    fn data(&mut self, depth: u32) -> Result<Data, Error> {
        if depth > 200 {
            return Err(Error::snapshot("nested too deeply"));
        }
        Ok(match self.uint()? {
            0 => Data::Nil,
            1 => Data::Bool(false),
            2 => Data::Bool(true),
            3 => Data::Int(self.int()?),
            4 => Data::Float(self.f64()?),
            5 => Data::Str(self.str()?),
            6 => {
                let n = self.len()?;
                Data::List((0..n).map(|_| self.data(depth + 1)).collect::<Result<_, _>>()?)
            }
            7 => {
                let n = self.len()?;
                Data::Map(
                    (0..n)
                        .map(|_| Ok((self.str()?, self.data(depth + 1)?)))
                        .collect::<Result<_, Error>>()?,
                )
            }
            _ => return Err(Error::snapshot("bad data")),
        })
    }
}
