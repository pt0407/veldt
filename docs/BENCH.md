# VM vs tree-walk — honest benchmark numbers

`veldt run prog --vm` runs the same source on a stack-based bytecode
VM with a Cranelift tier-2 JIT (pure-int and pure-float functions →
native after 64 calls). Numbers from `bench/*.veldt` on Apple Silicon:

`veldt compare --all` prints a fresh table like this on demand.

| benchmark          | tree-walk | --vm     | speedup |
|--------------------|-----------|----------|---------|
| dict_ops           | 7.31s     | 0.23s    | ~32x    |
| recursive fib(20)  | 1.77s     | 0.02s    | ~88x    |
| numeric_loop       | 1.45s     | 0.47s    | ~3x     |
| fn_calls           | 0.57s     | 0.16s    | ~3.6x   |
| list_ops_fast      | 0.82s     | 0.28s    | ~3x     |
| dict_ops_fast      | 1.28s     | 0.76s    | ~1.7x   |
| string_build       | 0.07s     | 0.05s    | ~1.4x   |

What's native: pure-int and pure-float fn bodies — arithmetic,
comparisons, let/assign, if/while loops, self+cross-function calls
(name#variant keyed so evolved variants recompile), `call_best`
dispatches to the fittest variant's native code after 64 calls.

What's still bytecode: strings, lists, dicts, closures, builtins,
match — those fall back to the VM interpreter (still faster than
tree-walk for the loop-heavy ones). The tree-walker stays as the
semantic reference; `tests/vm_parity.rs` locks both engines to
identical variable state on 8 representative programs.

Coverage: all `examples/*.veldt` run end-to-end under `--vm`.
