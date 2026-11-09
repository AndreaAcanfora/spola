//! The effects the `spola` command provides to programs.

use std::io::{BufRead, IsTerminal, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use spola::Data;

pub enum Effect {
    Done(Data),
    Failed(String),
    /// Stop here; the program continues when someone runs `spola resume --with`.
    Park,
}

pub struct Host {
    detach: bool,
    rng: u64,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Host {
    pub fn new(detach: bool) -> Host {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Host { detach, rng: seed | 1 }
    }

    /// Runs before the checkpoint that precedes an effect: records what must survive a crash during it.
    pub fn before(&self, name: &str, args: &[Data], host: &mut Data) {
        if name == "sleep" && *host == Data::Nil {
            let secs = args.first().and_then(Data::as_f64).unwrap_or(0.0).max(0.0);
            let until = now_ms() + (secs * 1000.0) as i64;
            *host = Data::Map(vec![("sleep_until".into(), Data::Int(until))]);
        }
    }

    pub fn perform(&mut self, name: &str, args: &[Data], host: &Data) -> Effect {
        match self.effect(name, args, host) {
            Ok(e) => e,
            Err(msg) => Effect::Failed(msg),
        }
    }

    fn effect(&mut self, name: &str, args: &[Data], host: &Data) -> Result<Effect, String> {
        let arg = |i: usize| args.get(i).ok_or_else(|| format!("missing argument {}", i + 1));
        let text = |i: usize| {
            arg(i)?
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("argument {} must be a string", i + 1))
        };
        Ok(Effect::Done(match name {
            "sleep" => {
                let until = match host {
                    Data::Map(e) => e.iter().find_map(|(k, v)| {
                        if let (true, Data::Int(t)) = (k == "sleep_until", v) {
                            Some(*t)
                        } else {
                            None
                        }
                    }),
                    _ => None,
                };
                let until = until.ok_or("sleep(seconds) needs a number")?;
                let left = until - now_ms();
                if left > 0 {
                    std::thread::sleep(Duration::from_millis(left as u64));
                }
                Data::Nil
            }
            "now" => Data::Int(now_ms()),
            "random" => {
                // xorshift64*
                self.rng ^= self.rng >> 12;
                self.rng ^= self.rng << 25;
                self.rng ^= self.rng >> 27;
                let x = self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d);
                Data::Float((x >> 11) as f64 / (1u64 << 53) as f64)
            }
            "env" => std::env::var(text(0)?).map(Data::Str).unwrap_or(Data::Nil),
            "ask" => {
                let prompt = args
                    .first()
                    .map(|a| a.as_str().map(str::to_string).unwrap_or_else(|| a.to_source()));
                if self.detach {
                    return Ok(Effect::Park);
                }
                if let Some(p) = prompt {
                    eprint!("{p} ");
                    let _ = std::io::stderr().flush();
                }
                let mut line = String::new();
                match std::io::stdin().lock().read_line(&mut line) {
                    Ok(0) => return Ok(Effect::Park),
                    Ok(_) => {
                        if !std::io::stdin().is_terminal() {
                            eprintln!();
                        }
                        Data::Str(line.trim_end_matches(['\n', '\r']).to_string())
                    }
                    Err(e) => return Err(format!("cannot read input: {e}")),
                }
            }
            "exec" => {
                let cmd = text(0)?;
                let out = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .stdin(std::process::Stdio::null())
                    .output()
                    .map_err(|e| format!("cannot run {cmd:?}: {e}"))?;
                Data::Map(vec![
                    ("code".into(), Data::Int(out.status.code().unwrap_or(-1) as i64)),
                    (
                        "stdout".into(),
                        Data::Str(String::from_utf8_lossy(&out.stdout).into_owned()),
                    ),
                    (
                        "stderr".into(),
                        Data::Str(String::from_utf8_lossy(&out.stderr).into_owned()),
                    ),
                ])
            }
            "read_file" => {
                let path = text(0)?;
                Data::Str(std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?)
            }
            "write_file" => {
                let path = text(0)?;
                std::fs::write(&path, text(1)?).map_err(|e| format!("{path}: {e}"))?;
                Data::Nil
            }
            other => {
                return Err(format!(
                    "unknown effect `{other}`; the spola command provides sleep, ask, now, random, env, exec, read_file, write_file"
                ));
            }
        }))
    }
}
