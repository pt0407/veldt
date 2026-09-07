# Veldt — a language where code is alive

## Overview

Veldt is a programming language where programs grow new code, and that code lives in a shared, persistent ecosystem called the veldt. Grown code has a lifespan: healthy code persists, sick code gets healed or dies. The codebase is not a file you edit — it is a living veldt that grows, self-cleans, and evolves.

Implemented in Rust. Interpreter design (lexer → parser → interpreter → ecosystem).

## Core metaphor

- Programs are **seeds** — they run and produce output.
- Code is **alive** — it enters the veldt, lives, can get sick, can heal, ages, and dies.
- The veldt is **shared** — all programs grow into and read from one global ecosystem.
- The codebase **evolves** — the language gets richer as the veldt grows.

## Output channels

Programs have two output channels:

1. **`print`** — outputs data to the user. Ephemeral. Normal program output.
2. **`grow`** — outputs code to the veldt. Permanent (until the code dies). This is how programs extend the language.

Example:
```
fn greet(name) {
    print("hello " + name)
}

greet("world")

grow fn shout(name) {
    print("HELLO " + name.upper())
}
```

After running, `shout` enters the veldt. Any future program can call `shout`. If `shout` has a bug, it gets sick, the runtime attempts healing, and if it cannot be saved, it dies with an obituary.

## The veldt (ecosystem)

The veldt is a shared, persistent store of living code. It is loaded on startup and written to after every run.

### Storage

The veldt persists to disk (a directory containing living code entries, each with metadata: source, health, age, lifespan, usage count). Location: `~/.veldt/garden/` by default.

### What lives in the veldt

Functions, variables, and type definitions that have been grown by any program. Each entry stores:
- **Source**: the code itself
- **Born**: timestamp when it was grown
- **Age**: how many runs it has survived
- **Health**: `healthy`, `sick`, `dying`
- **Lifespan**: max age before death (depends on health)
- **Usage count**: how many times it's been called by running programs
- **Healing attempts**: history of repair attempts

## Life cycle of code

### Birth

When a program executes a `grow` statement, the grown code enters the veldt. The runtime immediately assesses its health:

- **Parse check**: does the code parse?
- **Type check**: does it type-check against the current veldt?
- **Run check**: can it execute without immediate errors?

If all pass → **healthy**, long lifespan (default: 100 runs).
If parse fails → **stillborn** (dies immediately, obituary printed).
If parse OK but type/run fails → **sick**, short lifespan (default: 5 runs).

### Life

During each program run, the veldt is loaded. Living code is available to all programs. When a running program calls veldt code, that code's usage count increments and its lifespan renews (extends by a renewal amount, e.g., +50 runs, capped at a max). Used healthy code thrives.

### Aging

After each run, all veldt entries age by 1. Sick code ages by 3 (ages faster). Code that was used during the run gets renewed. Code that was not used just ages normally.

### Healing (immune system)

When code is sick, the runtime attempts to heal it before it dies. Healing is **garden-inspired**: the runtime searches the veldt for healthy code with similar signatures (same name pattern, same number of parameters, similar structure) and tries to adapt the sick code using the healthy code as a template.

Healing strategies, attempted in order:
1. **Type grafting**: borrow type annotations from a similar healthy function.
2. **Structure grafting**: replace broken body with a healthy function's body, adjusting parameter names.
3. **Signature matching**: if a healthy function with the same name and compatible signature exists, replace the sick code with it.

If healing succeeds → code becomes healthy, lifespan restored.
If healing fails → code remains sick, continues aging toward death.

### Death

When code's age exceeds its lifespan, it dies. The runtime prints an **obituary**:

```
[OBITUARY] shout(name) — died at age 7 (sick)
  Born: 2026-09-07 14:32:01
  Health: sick (type error: unknown method 'upper' on str)
  Healing attempts: 2 (failed — no similar healthy function found)
  Last used: never
  Cause of death: lifespan exceeded
```

Dead code is removed from the veldt. The ecosystem self-cleans.

## Language specification

### Types

- `int` — integers
- `str` — strings
- `bool` — booleans
- `list` — lists (homogeneous)
- `fn` — functions

### Syntax

```
# Comments start with #

# Variable binding
let x = 5
let name = "world"

# Functions
fn greet(name: str) {
    print("hello " + name)
}

# Calling functions
greet("world")

# Conditionals
if x > 3 {
    print("big")
} else {
    print("small")
}

# Loops
for i in range(10) {
    print(i)
}

# While loops
while x > 0 {
    x = x - 1
}

# Growing code (the core feature)
grow fn shout(name: str) {
    print("HELLO " + name)
}

grow let default_greeting = "hello"

# Return
fn add(a: int, b: int) {
    return a + b
}
```

### Operators

- Arithmetic: `+`, `-`, `*`, `/`, `%`
- Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Boolean: `and`, `or`, `not`
- String concatenation: `+`

### Builtins

- `print(x)` — output to user
- `range(n)` — produce a list [0, 1, ..., n-1]
- `len(x)` — length of string or list
- `str.upper()`, `str.lower()` — string methods
- `list.push(x)` — append to list

## Implementation architecture

### Modules

1. **`lexer.rs`** — tokenize source code into tokens
2. **`parser.rs`** — parse tokens into an AST
3. **`ast.rs`** — AST type definitions
4. **`interpreter.rs`** — walk the AST, execute programs
5. **`veldt.rs`** — the ecosystem: load, save, age, heal, kill code
6. **`health.rs`** — health checking and healing logic
7. **`main.rs`** — CLI entry point, orchestrate lex → parse → load veldt → run → grow → age → save

### Execution flow

```
source file
    → lexer (tokens)
    → parser (AST)
    → load veldt from disk
    → interpreter runs AST
        → `print` outputs to stdout
        → `grow` collects grown code
        → calls to veldt code increment usage
    → after run:
        → new grown code enters veldt (health checked)
        → sick code gets healing attempts
        → all code ages
        → dead code is pruned (obituaries printed)
        → veldt saved to disk
```

### CLI

```
veldt run program.veldt       # run a program
veldt garden                  # list all living code in the veldt
veldt garden --sick           # list only sick code
veldt garden --obituaries     # show recent deaths
veldt prune <name>            # manually kill a piece of code
veldt clear                   # clear the entire veldt
```

## What we are NOT building today

- The visual IDE (next project — the veldt as a visual ecosystem)
- LLM-based healing (v2 — mechanical/garden-inspired healing first)
- A compiler or bytecode VM (interpreter only)
- Modules or imports (the veldt IS the module system)
- Concurrency

## Success criteria

1. You can write and run a Veldt program with variables, functions, conditionals, and loops.
2. Programs can `grow` code that persists to the veldt and is available in future runs.
3. Grown code is health-checked on birth.
4. Sick code gets garden-inspired healing attempts.
5. Code ages and dies with obituaries when its lifespan is exceeded.
6. `veldt garden` shows the living ecosystem.
7. The whole thing feels alive — not like editing files, like tending an ecosystem.
