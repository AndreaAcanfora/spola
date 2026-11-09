# Changelog

## 0.1.0 (2026-11-09)

First release.

- The language: integers, floats, strings with interpolation, lists, ordered maps, closures, recursion, `for`/`while`,
  optional semicolons, 34 built-in functions.
- Bytecode compiler and stack VM with a mark-and-sweep garbage collector.
- Effects with `await`, and checkpoints of the whole VM state that resume byte for byte, validated on load.
- The `spola` command: `run --state` (file or Redis-compatible server), `resume --with`, `inspect`, `disasm`,
  `check`; effects `sleep`, `ask`, `now`, `random`, `env`, `exec`, `read_file`, `write_file`.
- The playground: WebAssembly VM, kill and restore, checkpoints that survive a page reload, time travel.
