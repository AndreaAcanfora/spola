//! Spola: a small scripting language whose programs can be paused, saved to disk and resumed anywhere.
//!
//! ```
//! use spola::{Data, Outcome, Vm, compile};
//!
//! let program = compile(r#"
//!     let name = await ask("What is your name?")
//!     return "Hello, {name}!"
//! "#).unwrap();
//! let mut vm = Vm::new(program);
//!
//! // The program stops at `await` and asks the host for an effect.
//! let Outcome::Effect { name, .. } = vm.run(None).unwrap() else { panic!() };
//! assert_eq!(name, "ask");
//!
//! // Save it, and bring it back later: in another process, on another machine.
//! let checkpoint = vm.snapshot();
//! let mut vm = Vm::restore(&checkpoint).unwrap();
//! vm.resume(&Data::Str("Ada".into())).unwrap();
//! assert_eq!(vm.run(None).unwrap(), Outcome::Done(Data::Str("Hello, Ada!".into())));
//! ```

mod ast;
pub mod builtins;
pub mod bytecode;
pub mod compiler;
mod error;
pub mod json;
mod lexer;
mod parser;
mod snapshot;
mod value;
mod vm;

#[cfg(target_arch = "wasm32")]
mod wasm;

pub use bytecode::Program;
pub use compiler::compile;
pub use error::{Error, ErrorKind, Span, TraceFrame};
pub use value::Data;
pub use vm::{FrameInfo, Outcome, Status, Vm};
