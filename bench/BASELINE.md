# Veldt perf baseline (post-Rc-bodies + by-ref dispatch)

Measured with `./bench/run.sh` on a debug build.

| workload     | size            | time    | bottleneck                      |
|--------------|-----------------|---------|----------------------------------|
| numeric_loop | 100k iterations | 930ms   | ~9µs/step — stmt dispatch        |
| string_build | 5k concats      | 69ms    | fine (small n)                   |
| fn_calls     | 30k calls       | ~450ms  | ~15µs/call — env alloc + binding |
| dict_ops     | 4k puts+gets    | 7246ms  | put() clones whole map = O(n²)   |
| list_ops     | 20k pushes      | 40022ms | push() clones whole list = O(n²) |

## The two quadratics (biggest wins available)

- `list.push(x)` clones the list every call → building a list of n items
  is O(n²). Fix: Rc<List> with copy-on-write, or an `append`/`+` that
  mutates. This one fix would take list_ops from 40s to <100ms.
- `dict.put(k,v)` clones the whole map → same O(n²). Same fix shape.

fn_calls at ~15µs/call is the second-order target (env HashMap alloc
per call + param save/restore in closure env).
