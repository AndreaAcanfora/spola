//! Where checkpoints are kept: a file, or a key on a Redis-compatible server (Redis, Valkey, loomkv...).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

pub enum Store {
    File(PathBuf),
    Redis { addr: String, key: String },
}

impl Store {
    /// `redis://host:port/key` for a server, anything else is a file path.
    pub fn parse(spec: &str) -> Result<Store, String> {
        let Some(rest) = spec.strip_prefix("redis://") else {
            return Ok(Store::File(spec.into()));
        };
        let (addr, key) = rest.split_once('/').ok_or("expected redis://host:port/key")?;
        if key.is_empty() {
            return Err("expected redis://host:port/key".into());
        }
        let addr = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("{addr}:6379")
        };
        Ok(Store::Redis {
            addr,
            key: key.to_string(),
        })
    }

    pub fn describe(&self) -> String {
        match self {
            Store::File(p) => p.display().to_string(),
            Store::Redis { addr, key } => format!("redis://{addr}/{key}"),
        }
    }

    pub fn load(&self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Store::File(p) => match std::fs::read(p) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("cannot read {}: {e}", p.display())),
            },
            Store::Redis { addr, key } => match Redis::connect(addr)?.command(&[b"GET", key.as_bytes()])? {
                Reply::Bulk(b) => Ok(b),
                other => Err(format!("unexpected reply to GET: {other:?}")),
            },
        }
    }

    /// Saves atomically: a crash in the middle leaves the previous checkpoint intact.
    pub fn save(&self, bytes: &[u8]) -> Result<(), String> {
        match self {
            Store::File(p) => {
                let tmp = p.with_extension("tmp");
                let write = || -> std::io::Result<()> {
                    let mut f = std::fs::File::create(&tmp)?;
                    f.write_all(bytes)?;
                    f.sync_all()?;
                    std::fs::rename(&tmp, p)
                };
                write().map_err(|e| format!("cannot write {}: {e}", p.display()))
            }
            Store::Redis { addr, key } => match Redis::connect(addr)?.command(&[b"SET", key.as_bytes(), bytes])? {
                Reply::Simple(s) if s == "OK" => Ok(()),
                other => Err(format!("unexpected reply to SET: {other:?}")),
            },
        }
    }

    pub fn delete(&self) -> Result<(), String> {
        match self {
            Store::File(p) => match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("cannot delete {}: {e}", p.display()))
                }
                _ => Ok(()),
            },
            Store::Redis { addr, key } => Redis::connect(addr)?.command(&[b"DEL", key.as_bytes()]).map(|_| ()),
        }
    }
}

#[derive(Debug)]
enum Reply {
    Simple(String),
    Int,
    Bulk(Option<Vec<u8>>),
}

/// Just enough of the Redis protocol (RESP2) for GET, SET and DEL.
struct Redis {
    reader: BufReader<TcpStream>,
}

impl Redis {
    fn connect(addr: &str) -> Result<Redis, String> {
        let stream = TcpStream::connect(addr).map_err(|e| format!("cannot connect to {addr}: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        Ok(Redis {
            reader: BufReader::new(stream),
        })
    }

    fn command(&mut self, args: &[&[u8]]) -> Result<Reply, String> {
        let mut msg = format!("*{}\r\n", args.len()).into_bytes();
        for a in args {
            msg.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
            msg.extend_from_slice(a);
            msg.extend_from_slice(b"\r\n");
        }
        let io = |e: std::io::Error| format!("redis: {e}");
        self.reader.get_mut().write_all(&msg).map_err(io)?;
        let mut line = String::new();
        self.reader.read_line(&mut line).map_err(io)?;
        let line = line.trim_end();
        let (kind, rest) = line.split_at(line.len().min(1));
        match kind {
            "+" => Ok(Reply::Simple(rest.to_string())),
            "-" => Err(format!("redis: {rest}")),
            ":" => Ok(Reply::Int),
            "$" => {
                let n: i64 = rest.parse().map_err(|_| "redis: bad bulk length")?;
                if n < 0 {
                    return Ok(Reply::Bulk(None));
                }
                let mut buf = vec![0; n as usize + 2];
                self.reader.read_exact(&mut buf).map_err(io)?;
                buf.truncate(n as usize);
                Ok(Reply::Bulk(Some(buf)))
            }
            _ => Err(format!("redis: unexpected reply {line:?}")),
        }
    }
}
