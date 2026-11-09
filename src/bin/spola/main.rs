//! The `spola` command: runs programs, keeps them durable, and inspects their checkpoints.

mod effects;
mod store;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use spola::{Data, Outcome, Status, Vm, compile};

use effects::{Effect, Host};
use store::Store;

const USAGE: &str = "\
spola: a small language whose programs can be paused, saved and resumed anywhere

usage:
  spola run <file.sp> [--state <checkpoint>] [--detach] [--fresh]
      Runs a program. With --state, the program is durable: its whole state is saved at every `await`, and running
      the same command again after a crash (or Ctrl-C) resumes it exactly where it stopped. <checkpoint> is a file
      path or redis://host:port/key (Redis, Valkey, loomkv, any Redis-compatible server).
      --detach   when the program asks for input (`await ask(...)`), save and exit instead of waiting
      --fresh    discard an existing checkpoint and start from the beginning

  spola resume <checkpoint> [--with <value>]
      Continues a saved program. --with answers the effect it is waiting for (JSON, or plain text).

  spola inspect <checkpoint>
      Shows where a saved program is: the line, the call stack, the local variables, what it is waiting for.

  spola disasm <file.sp>      Prints the bytecode.
  spola check <file.sp>       Compiles without running.

effects (use them with `await`, e.g. `let answer = await ask(\"Deploy?\")`):
  sleep(seconds)   ask(prompt)   now()   random()   env(name)
  exec(command)    read_file(path)   write_file(path, text)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("resume") => cmd_resume(&args[1..]),
        Some("inspect") => cmd_inspect(&args[1..]),
        Some("disasm") => cmd_disasm(&args[1..]),
        Some("check") => cmd_check(&args[1..]),
        Some("--version" | "-V") => {
            println!("spola {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Some("help" | "--help" | "-h") | None => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => Err(format!("unknown command `{other}`; try `spola help`")),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}", paint("1;31", &format!("error: {e}")));
            ExitCode::FAILURE
        }
    }
}

/// Program exit code when it stops to wait for input (`--detach`).
const EXIT_WAITING: u8 = 3;

struct Flags {
    positional: Vec<String>,
    state: Option<String>,
    with: Option<String>,
    detach: bool,
    fresh: bool,
}

fn flags(args: &[String]) -> Result<Flags, String> {
    let mut f = Flags {
        positional: Vec::new(),
        state: None,
        with: None,
        detach: false,
        fresh: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--state" => f.state = Some(it.next().ok_or("--state needs a value")?.clone()),
            "--with" => f.with = Some(it.next().ok_or("--with needs a value")?.clone()),
            "--detach" => f.detach = true,
            "--fresh" => f.fresh = true,
            s if s.starts_with("--") => return Err(format!("unknown option `{s}`")),
            _ => f.positional.push(a.clone()),
        }
    }
    Ok(f)
}

fn one_file(f: &Flags, what: &str) -> Result<String, String> {
    match f.positional.as_slice() {
        [file] => Ok(file.clone()),
        _ => Err(format!("expected one {what}; try `spola help`")),
    }
}

fn read_source(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))
}

fn cmd_check(args: &[String]) -> Result<ExitCode, String> {
    let path = one_file(&flags(args)?, "file")?;
    compile(&read_source(&path)?).map_err(|e| format!("{path}: {e}"))?;
    eprintln!("{path}: ok");
    Ok(ExitCode::SUCCESS)
}

fn cmd_disasm(args: &[String]) -> Result<ExitCode, String> {
    let path = one_file(&flags(args)?, "file")?;
    let program = compile(&read_source(&path)?).map_err(|e| format!("{path}: {e}"))?;
    print!("{}", program.disassemble());
    Ok(ExitCode::SUCCESS)
}

fn cmd_run(args: &[String]) -> Result<ExitCode, String> {
    let f = flags(args)?;
    let path = one_file(&f, "file")?;
    let source = read_source(&path)?;
    let program = compile(&source).map_err(|e| format!("{path}: {e}"))?;
    let store = f.state.as_deref().map(Store::parse).transpose()?;

    let mut vm = None;
    if let Some(store) = &store {
        if f.fresh {
            store.delete()?;
        } else if let Some(bytes) = store.load()? {
            let saved = Vm::restore(&bytes).map_err(|e| format!("{}: {e}", store.describe()))?;
            if saved.program().source != source {
                return Err(format!(
                    "{} holds a run of a different version of {path}; use --fresh to start over",
                    store.describe()
                ));
            }
            match saved.status() {
                Status::Done(_) => {
                    note(&format!(
                        "{path} already finished in this checkpoint; use --fresh to run it again"
                    ));
                    return Ok(ExitCode::SUCCESS);
                }
                Status::Failed => {
                    note(&format!(
                        "{path} failed in this checkpoint; use --fresh to run it again"
                    ));
                    return Ok(ExitCode::FAILURE);
                }
                _ => {}
            }
            note(&format!(
                "↻ resuming from {} (line {}, {} effects done, {} steps)",
                store.describe(),
                saved.line(),
                saved.effects,
                saved.steps
            ));
            vm = Some(saved);
        }
    }
    let vm = vm.unwrap_or_else(|| Vm::new(program));
    drive(vm, store.as_ref(), f.detach)
}

fn cmd_resume(args: &[String]) -> Result<ExitCode, String> {
    let f = flags(args)?;
    let store = Store::parse(&one_file(&f, "checkpoint")?)?;
    let bytes = store
        .load()?
        .ok_or_else(|| format!("{} does not exist", store.describe()))?;
    let mut vm = Vm::restore(&bytes).map_err(|e| format!("{}: {e}", store.describe()))?;
    match vm.status() {
        Status::Done(_) => return Err("the program has already finished".into()),
        Status::Failed => return Err("the program has failed".into()),
        Status::Waiting { effect, .. } => {
            if let Some(value) = &f.with {
                let data = spola::json::decode(value).unwrap_or_else(|_| Data::Str(value.clone()));
                note(&format!("↻ resuming: `{effect}` returns {}", data.to_source()));
                vm.host = Data::Nil;
                vm.resume(&data).map_err(|e| e.to_string())?;
                store.save(&vm.snapshot())?;
            } else {
                note(&format!("↻ resuming at line {}: performing `{effect}`", vm.line()));
            }
        }
        Status::Running => {
            if f.with.is_some() {
                return Err("the program is not waiting for an effect; drop --with".into());
            }
            note(&format!("↻ resuming at line {}", vm.line()));
        }
    }
    drive(vm, Some(&store), false)
}

/// Runs the VM to completion, performing effects and saving a checkpoint around each one.
fn drive(mut vm: Vm, store: Option<&Store>, detach: bool) -> Result<ExitCode, String> {
    let mut host = Host::new(detach);
    let save = |vm: &mut Vm| -> Result<(), String> {
        match store {
            Some(s) => s.save(&vm.snapshot()),
            None => Ok(()),
        }
    };
    loop {
        let outcome = match vm.status() {
            Status::Waiting { .. } => {
                let (name, args) = vm.pending().unwrap();
                Ok(Outcome::Effect { name, args })
            }
            _ => vm.run(Some(100_000)),
        };
        flush(&mut vm);
        match outcome {
            Ok(Outcome::Paused) => {}
            Ok(Outcome::Done(value)) => {
                save(&mut vm)?;
                if value != Data::Nil {
                    println!("{}", value.to_source());
                }
                return Ok(ExitCode::SUCCESS);
            }
            Ok(Outcome::Effect { name, args }) => {
                host.before(&name, &args, &mut vm.host);
                save(&mut vm)?;
                match host.perform(&name, &args, &vm.host) {
                    Effect::Done(result) => {
                        vm.host = Data::Nil;
                        vm.resume(&result).map_err(|e| e.to_string())?;
                        save(&mut vm)?;
                    }
                    Effect::Failed(message) => {
                        let e = vm.fail_effect(&message);
                        save(&mut vm)?;
                        return Err(e.to_string());
                    }
                    Effect::Park => {
                        let size = vm.snapshot().len();
                        let call = format!("{name}({})", join_args(&args));
                        note(&format!("⏸ waiting for {call} at line {}", vm.line()));
                        match store {
                            Some(s) => {
                                note(&format!("  checkpoint: {} ({})", s.describe(), human_size(size)));
                                note(&format!(
                                    "  resume with: spola resume {} --with '<answer>'",
                                    s.describe()
                                ));
                            }
                            None => note("  (no --state given, so this run cannot be resumed)"),
                        }
                        return Ok(ExitCode::from(EXIT_WAITING));
                    }
                }
            }
            Err(e) => {
                save(&mut vm)?;
                return Err(e.to_string());
            }
        }
    }
}

fn join_args(args: &[Data]) -> String {
    args.iter().map(Data::to_source).collect::<Vec<_>>().join(", ")
}

fn flush(vm: &mut Vm) {
    let out = vm.take_output();
    if !out.is_empty() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
}

fn cmd_inspect(args: &[String]) -> Result<ExitCode, String> {
    let f = flags(args)?;
    let store = Store::parse(&one_file(&f, "checkpoint")?)?;
    let bytes = store
        .load()?
        .ok_or_else(|| format!("{} does not exist", store.describe()))?;
    let vm = Vm::restore(&bytes).map_err(|e| format!("{}: {e}", store.describe()))?;
    let status = match vm.status() {
        Status::Running => "running (between effects)".to_string(),
        Status::Waiting { effect, .. } => {
            let (_, args) = vm.pending().unwrap();
            format!("waiting for {effect}({})", join_args(&args))
        }
        Status::Done(v) => format!("finished, returned {}", vm.repr(*v)),
        Status::Failed => "failed".to_string(),
    };
    println!("{} {}", paint("1", "checkpoint"), store.describe());
    println!("  status     {status}");
    if let Data::Map(entries) = &vm.host
        && let Some((_, Data::Int(until))) = entries.iter().find(|(k, _)| k == "sleep_until")
    {
        let left = (*until - effects::now_ms()).max(0);
        println!("  wakes in   {:.1}s", left as f64 / 1000.0);
    }
    println!("  progress   {} steps, {} effects done", vm.steps, vm.effects);
    println!(
        "  size       {} ({} heap objects)",
        human_size(bytes.len()),
        vm.heap_size()
    );
    let lines: Vec<&str> = vm.program().source.lines().collect();
    let stack = vm.call_stack();
    if let Some(top) = stack.first().filter(|_| !vm.is_done()) {
        println!();
        let at = top.line as usize;
        for n in at.saturating_sub(2).max(1)..=(at + 2).min(lines.len()) {
            let text = lines[n - 1];
            if n == at {
                println!("{}", paint("1;33", &format!("  → {n:>4} │ {text}")));
            } else {
                println!("    {n:>4} │ {text}");
            }
        }
    }
    println!();
    for frame in &stack {
        println!("{} {} (line {})", paint("1", "in"), frame.function, frame.line);
        for (name, value) in &frame.locals {
            let mut value = value.clone();
            if value.chars().count() > 70 {
                value = value.chars().take(67).collect::<String>() + "...";
            }
            println!("    {name} = {value}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else {
        format!("{:.1} KB", n as f64 / 1024.0)
    }
}

fn note(msg: &str) {
    eprintln!("{}", paint("2", msg));
}

fn paint(code: &str, s: &str) -> String {
    if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
