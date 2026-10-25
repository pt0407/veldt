# Veldt perf baseline (post-persistent-structures)

Measured with `./bench/run.sh` on a debug build (main @ Phase 6).

| workload     | size            | baseline | now    | notes                          |
|--------------|-----------------|----------|--------|--------------------------------|
| numeric_loop | 100k iterations | 930ms    | 491ms  | stmt dispatch ~5µs/step        |
| string_build | 5k concats      | 69ms     | 29ms   | Arc<str> — no string clone    |
| fn_calls     | 30k calls       | ~450ms   | 584ms  | env alloc + binding (next wall)|
| dict_ops     | 4k puts+gets    | 7246ms   | 170ms  | im::HashMap — clone O(1)      |
| list_ops     | 20k pushes      | 40022ms  | ~620ms | im::Vector push_back O(log n) |

## Phase 6 results

- The two quadratics are dead: `im::Vector`/`im::HashMap` give O(1)
  clone + structural sharing. list_ops 40s→<1s, dict_ops 7.2s→170ms.
- `xs.append(v)` mutates in place (var receiver); `d["k"]=v` works;
  `s.len()` added.
- fn_calls (~19µs/call) is now the dominant wall: env HashMap alloc +
  param save/restore per call. Options: small-map frames, interned
  param names, or a frame pool.
- numeric_loop ~5µs/step: stmt dispatch + step_count tick. Options:
  hot-loop fast paths, cheaper RefCell vars.
