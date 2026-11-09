//! Built-in functions. They are all deterministic: anything that reads the outside world (time, randomness, files,
//! the network) is an effect, reached with `await`, so a run can always be replayed exactly.
//!
//! The order of this table is part of the checkpoint format: add new built-ins at the end.

use crate::value::{Data, Obj, Value};
use crate::vm::{Vm, num};

pub type Native = fn(&mut Vm, &[Value]) -> Result<Value, String>;

pub static BUILTINS: &[(&str, Native)] = &[
    ("print", print),
    ("len", len),
    ("push", push),
    ("pop", pop),
    ("insert", insert),
    ("remove", remove),
    ("keys", keys),
    ("values", values),
    ("has", has),
    ("str", str_),
    ("int", int),
    ("float", float),
    ("type", type_),
    ("range", range),
    ("join", join),
    ("split", split),
    ("slice", slice),
    ("contains", contains),
    ("upper", upper),
    ("lower", lower),
    ("trim", trim),
    ("abs", abs),
    ("min", min),
    ("max", max),
    ("floor", floor),
    ("round", round),
    ("sqrt", sqrt),
    ("sort", sort),
    ("reverse", reverse),
    ("assert", assert),
    ("error", error),
    ("json", json),
    ("parse_json", parse_json),
    ("repr", repr),
];

fn arity(args: &[Value], n: usize) -> Result<(), String> {
    if args.len() == n {
        Ok(())
    } else {
        Err(format!(
            "expected {n} argument{}, got {}",
            if n == 1 { "" } else { "s" },
            args.len()
        ))
    }
}

fn arity_range(args: &[Value], lo: usize, hi: usize) -> Result<(), String> {
    if (lo..=hi).contains(&args.len()) {
        Ok(())
    } else {
        Err(format!("expected {lo} to {hi} arguments, got {}", args.len()))
    }
}

fn string(vm: &Vm, v: Value) -> Result<&str, String> {
    vm.heap
        .as_str(v)
        .ok_or_else(|| format!("expected a string, got {}", vm.heap.type_name(v)))
}

fn int_arg(vm: &Vm, v: Value) -> Result<i64, String> {
    match v {
        Value::Int(n) => Ok(n),
        _ => Err(format!("expected an int, got {}", vm.heap.type_name(v))),
    }
}

fn number(vm: &Vm, v: Value) -> Result<f64, String> {
    num(v).ok_or_else(|| format!("expected a number, got {}", vm.heap.type_name(v)))
}

fn list_ref(vm: &Vm, v: Value) -> Result<crate::value::Ref, String> {
    match v {
        Value::Obj(r) if matches!(vm.heap.get(r), Obj::List(_)) => Ok(r),
        _ => Err(format!("expected a list, got {}", vm.heap.type_name(v))),
    }
}

fn list(vm: &Vm, v: Value) -> Result<&Vec<Value>, String> {
    let r = list_ref(vm, v)?;
    let Obj::List(items) = vm.heap.get(r) else {
        unreachable!()
    };
    Ok(items)
}

fn list_mut(vm: &mut Vm, v: Value) -> Result<&mut Vec<Value>, String> {
    let r = list_ref(vm, v)?;
    let Obj::List(items) = vm.heap.get_mut(r) else {
        unreachable!()
    };
    Ok(items)
}

fn new_list(vm: &mut Vm, items: Vec<Value>) -> Value {
    Value::Obj(vm.heap.alloc(Obj::List(items)))
}

fn print(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    let line: Vec<String> = args.iter().map(|a| vm.display(*a)).collect();
    vm.output.push_str(&line.join(" "));
    vm.output.push('\n');
    Ok(Value::Nil)
}

fn len(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let n = match args[0] {
        Value::Obj(r) => match vm.heap.get(r) {
            Obj::Str(s) => s.chars().count(),
            Obj::List(items) => items.len(),
            Obj::Map(m) => m.len(),
            _ => return Err("expected a string, list or map".into()),
        },
        v => return Err(format!("expected a string, list or map, got {}", vm.heap.type_name(v))),
    };
    Ok(Value::Int(n as i64))
}

fn push(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 2)?;
    list_mut(vm, args[0])?.push(args[1]);
    Ok(args[0])
}

fn pop(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    list_mut(vm, args[0])?.pop().ok_or_else(|| "the list is empty".into())
}

fn insert(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 3)?;
    let i = int_arg(vm, args[1])?;
    let items = list_mut(vm, args[0])?;
    if i < 0 || i as usize > items.len() {
        return Err(format!("index {i} out of range for length {}", items.len()));
    }
    items.insert(i as usize, args[2]);
    Ok(args[0])
}

fn remove(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 2)?;
    let Value::Obj(r) = args[0] else {
        return Err("expected a list or map".into());
    };
    if let Some(k) = vm.heap.as_str(args[1]).map(str::to_string) {
        let Obj::Map(m) = vm.heap.get_mut(r) else {
            return Err("string keys are for maps".into());
        };
        return Ok(m.remove(&k).unwrap_or(Value::Nil));
    }
    let i = int_arg(vm, args[1])?;
    let items = list_mut(vm, args[0])?;
    let len = items.len() as i64;
    let j = if i < 0 { i + len } else { i };
    if j < 0 || j >= len {
        return Err(format!("index {i} out of range for length {len}"));
    }
    Ok(items.remove(j as usize))
}

fn map_entries(vm: &Vm, v: Value) -> Result<Vec<(String, Value)>, String> {
    match v {
        Value::Obj(r) => match vm.heap.get(r) {
            Obj::Map(m) => Ok(m.iter().map(|(k, v)| (k.to_string(), v)).collect()),
            _ => Err(format!("expected a map, got {}", vm.heap.type_name(v))),
        },
        _ => Err(format!("expected a map, got {}", vm.heap.type_name(v))),
    }
}

fn keys(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let entries = map_entries(vm, args[0])?;
    let items = entries.iter().map(|(k, _)| vm.heap.str(k)).collect();
    Ok(new_list(vm, items))
}

fn values(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let items = map_entries(vm, args[0])?.into_iter().map(|(_, v)| v).collect();
    Ok(new_list(vm, items))
}

fn has(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 2)?;
    let k = string(vm, args[1])?;
    match args[0] {
        Value::Obj(r) => match vm.heap.get(r) {
            Obj::Map(m) => Ok(Value::Bool(m.get(k).is_some())),
            _ => Err("expected a map".into()),
        },
        v => Err(format!("expected a map, got {}", vm.heap.type_name(v))),
    }
}

fn str_(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let s = vm.display(args[0]);
    Ok(vm.heap.str(&s))
}

fn repr(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let s = vm.repr(args[0]);
    Ok(vm.heap.str(&s))
}

fn int(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    match args[0] {
        Value::Int(n) => Ok(Value::Int(n)),
        Value::Float(x) if x.is_finite() && x.abs() < 9.2e18 => Ok(Value::Int(x.trunc() as i64)),
        Value::Float(_) => Err("number out of range".into()),
        Value::Bool(b) => Ok(Value::Int(b as i64)),
        v => {
            let s = string(vm, v)?;
            s.trim()
                .parse()
                .map(Value::Int)
                .map_err(|_| format!("not an integer: {s:?}"))
        }
    }
}

fn float(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    match args[0] {
        Value::Int(n) => Ok(Value::Float(n as f64)),
        Value::Float(x) => Ok(Value::Float(x)),
        v => {
            let s = string(vm, v)?;
            s.trim()
                .parse()
                .map(Value::Float)
                .map_err(|_| format!("not a number: {s:?}"))
        }
    }
}

fn type_(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let t = vm.heap.type_name(args[0]);
    Ok(vm.heap.str(t))
}

fn range(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity_range(args, 1, 3)?;
    let (start, end) = if args.len() == 1 {
        (0, int_arg(vm, args[0])?)
    } else {
        (int_arg(vm, args[0])?, int_arg(vm, args[1])?)
    };
    let step = if args.len() == 3 { int_arg(vm, args[2])? } else { 1 };
    if step == 0 {
        return Err("step cannot be zero".into());
    }
    let count = if step > 0 {
        (end - start).max(0) / step
    } else {
        (start - end).max(0) / -step
    } as u64;
    if count > 50_000_000 {
        return Err("range too large".into());
    }
    let mut items = Vec::with_capacity(count as usize + 1);
    let mut i = start;
    while (step > 0 && i < end) || (step < 0 && i > end) {
        items.push(Value::Int(i));
        i += step;
    }
    Ok(new_list(vm, items))
}

fn join(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity_range(args, 1, 2)?;
    let sep = if args.len() == 2 {
        string(vm, args[1])?.to_string()
    } else {
        String::new()
    };
    let parts: Vec<String> = list(vm, args[0])?.iter().map(|v| vm.display(*v)).collect();
    let s = parts.join(&sep);
    Ok(vm.heap.str(&s))
}

fn split(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity_range(args, 1, 2)?;
    let s = string(vm, args[0])?.to_string();
    let parts: Vec<String> = if args.len() == 2 {
        let sep = string(vm, args[1])?;
        if sep.is_empty() {
            return Err("separator cannot be empty".into());
        }
        s.split(sep).map(String::from).collect()
    } else {
        s.split_whitespace().map(String::from).collect()
    };
    let items = parts.iter().map(|p| vm.heap.str(p)).collect();
    Ok(new_list(vm, items))
}

/// Clamps Python-style slice bounds (negative counts from the end) to `0..=len`.
fn bounds(start: i64, end: i64, len: usize) -> (usize, usize) {
    let fix = |i: i64| {
        if i < 0 {
            (i + len as i64).max(0) as usize
        } else {
            (i as usize).min(len)
        }
    };
    let (s, e) = (fix(start), fix(end));
    (s, e.max(s))
}

fn slice(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity_range(args, 2, 3)?;
    let start = int_arg(vm, args[1])?;
    if let Some(s) = vm.heap.as_str(args[0]) {
        let chars: Vec<char> = s.chars().collect();
        let end = if args.len() == 3 {
            int_arg(vm, args[2])?
        } else {
            chars.len() as i64
        };
        let (a, b) = bounds(start, end, chars.len());
        let out: String = chars[a..b].iter().collect();
        return Ok(vm.heap.str(&out));
    }
    let items = list(vm, args[0])?;
    let end = if args.len() == 3 {
        int_arg(vm, args[2])?
    } else {
        items.len() as i64
    };
    let (a, b) = bounds(start, end, items.len());
    let out = items[a..b].to_vec();
    Ok(new_list(vm, out))
}

fn contains(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 2)?;
    if let Some(s) = vm.heap.as_str(args[0]) {
        return Ok(Value::Bool(s.contains(string(vm, args[1])?)));
    }
    let items = list(vm, args[0])?;
    Ok(Value::Bool(items.iter().any(|v| vm.heap.equal(*v, args[1]))))
}

fn upper(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let s = string(vm, args[0])?.to_uppercase();
    Ok(vm.heap.str(&s))
}

fn lower(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let s = string(vm, args[0])?.to_lowercase();
    Ok(vm.heap.str(&s))
}

fn trim(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let s = string(vm, args[0])?.trim().to_string();
    Ok(vm.heap.str(&s))
}

fn abs(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    match args[0] {
        Value::Int(n) => n.checked_abs().map(Value::Int).ok_or_else(|| "integer overflow".into()),
        v => Ok(Value::Float(number(vm, v)?.abs())),
    }
}

fn extreme(vm: &mut Vm, args: &[Value], want: std::cmp::Ordering) -> Result<Value, String> {
    let items: Vec<Value> = if args.len() == 1 {
        list(vm, args[0])?.clone()
    } else {
        args.to_vec()
    };
    let mut best = *items.first().ok_or("needs at least one value")?;
    for v in &items[1..] {
        if vm.compare(*v, best).map_err(|e| e.message)? == want {
            best = *v;
        }
    }
    Ok(best)
}

fn min(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    extreme(vm, args, std::cmp::Ordering::Less)
}

fn max(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    extreme(vm, args, std::cmp::Ordering::Greater)
}

fn to_int(x: f64) -> Result<Value, String> {
    if x.is_finite() && x.abs() < 9.2e18 {
        Ok(Value::Int(x as i64))
    } else {
        Err("number out of range".into())
    }
}

fn floor(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    match args[0] {
        Value::Int(n) => Ok(Value::Int(n)),
        v => to_int(number(vm, v)?.floor()),
    }
}

fn round(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    match args[0] {
        Value::Int(n) => Ok(Value::Int(n)),
        v => to_int(number(vm, v)?.round()),
    }
}

fn sqrt(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    Ok(Value::Float(number(vm, args[0])?.sqrt()))
}

fn sort(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let mut items = list(vm, args[0])?.clone();
    let mut err = None;
    items.sort_by(|a, b| {
        vm.compare(*a, *b).unwrap_or_else(|e| {
            err.get_or_insert(e.message);
            std::cmp::Ordering::Equal
        })
    });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(new_list(vm, items))
}

fn reverse(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    if let Some(s) = vm.heap.as_str(args[0]) {
        let r: String = s.chars().rev().collect();
        return Ok(vm.heap.str(&r));
    }
    let items = list(vm, args[0])?.iter().rev().copied().collect();
    Ok(new_list(vm, items))
}

fn assert(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity_range(args, 1, 2)?;
    if args[0].truthy() {
        return Ok(Value::Nil);
    }
    Err(if args.len() == 2 {
        vm.display(args[1])
    } else {
        "assertion failed".into()
    })
}

fn error(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    Err(vm.display(args[0]))
}

fn json(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let d = vm.heap.to_data(args[0])?;
    let s = crate::json::encode(&d);
    Ok(vm.heap.str(&s))
}

fn parse_json(vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    arity(args, 1)?;
    let d: Data = crate::json::decode(string(vm, args[0])?)?;
    Ok(vm.heap.alloc_data(&d))
}
