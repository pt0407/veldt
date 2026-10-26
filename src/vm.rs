// src/vm.rs — Phase 11: stack-based bytecode VM.
//
// The tree-walking interpreter remains the semantic reference; this is
// the fast executor for the computational core. Anything outside the
// subset (match, closures, structs, comprehensions, imports, grow-
// planting) is rejected by the compiler with a clear "VM subset" error —
// `veldt run` without --vm always uses the tree-walker, so nothing
// regresses. Builtins and method calls delegate to a helper
// Interpreter instance so stdlib semantics are never duplicated.

use crate::ast::*;
use crate::interpreter::{Interpreter, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy)]
enum Instr {
    // u32 fields index into Chunk.consts / Chunk.names — Instr stays
    // Copy so dispatch is a plain match on a copied instr, no heap.
    Const(u32),             // const pool index
    Load(u32),              // local slot
    Store(u32),             // local slot
    LoadG(u32),             // name-table index -> globals map
    StoreG(u32),            // name-table index
    Pop,
    Add, Sub, Mul, Div, Mod,
    Neg, Not,
    Eq, Neq, Lt, Gt, Le, Ge,
    AndOp, OrOp, InOp,
    Jump(u32),
    JumpIfFalse(u32),        // pops the condition
    PeekJumpIfNull(u32),     // for ??: jump keeping value if null
    Call(u32, u32),          // name idx, argc
    MethodCall(u32, u32),    // name idx, argc
    MakeList(u32),
    IndexGet,
    IndexSetVar(u32),
    ToStr,
    ForIter,
    ForNext(u32, u32),       // var slot, jump target
    CaptureEnv,              // top of stack: Closure proto -> captured env
    CallVar(u32, u32),       // slot holding Closure, argc
    MethodCallVar(u32, u32, u32), // receiver slot, method name idx, argc
    PrintOut,
    Return,
    Halt,
}

struct Chunk {
    code: Vec<Instr>,
    consts: Vec<Value>,
    names: Vec<String>,
    slot_names: Vec<String>,   // slot idx -> var name (for closure capture)
}

struct CompiledFn {
    id: u32,
    fitness: f64,
    params: Vec<String>,
    code: Rc<Chunk>,   // shared — call pushes an Rc bump, not a clone
}

struct Frame {
    code: Rc<Chunk>,
    pc: usize,
    env: Vec<Option<Value>>,            // slot-indexed locals
    base: usize,                        // value-stack base for this frame
    iters: Vec<(Vec<Value>, usize)>,    // for-loop iterator stack
}

pub struct Vm {
    fns: HashMap<String, Vec<CompiledFn>>,
    globals: HashMap<String, Value>,
    helper: Interpreter,                // delegation for builtins/methods
    pub steps: u64,
    max_steps: u64,
}

// ---------------- compiler ----------------

struct Compiler {
    code: Vec<Instr>,
    consts: Vec<Value>,
    names: Vec<String>,
    slot_names: Vec<String>,   // slot idx -> name, for closure capture
    // loop stack: (start_pc, [pending break patch positions])
    loops: Vec<(usize, Vec<usize>)>,
    // var name -> local slot (allocated lazily; env Vec grows to fit)
    slots: HashMap<String, u32>,
    next_slot: u32,
}

impl Compiler {
    fn slot(&mut self, name: &str) -> u32 {
        if let Some(&s) = self.slots.get(name) { return s; }
        let s = self.next_slot; self.next_slot += 1;
        self.slots.insert(name.to_string(), s);
        self.slot_names.push(name.to_string());
        s
    }

    fn emit(&mut self, i: Instr) -> usize {
        self.code.push(i);
        self.code.len() - 1
    }
    fn patch(&mut self, at: usize, target: usize) {
        match &mut self.code[at] {
            Instr::Jump(t) | Instr::JumpIfFalse(t)
            | Instr::PeekJumpIfNull(t) | Instr::ForNext(_, t) =>
                *t = target as u32,
            _ => panic!("patch on non-jump"),
        }
    }

    fn konst(&mut self, v: Value) -> u32 {
        self.consts.push(v);
        (self.consts.len() - 1) as u32
    }
    fn name_idx(&mut self, n: &str) -> u32 {
        if let Some(i) = self.names.iter().position(|s| s == n) {
            return i as u32;
        }
        self.names.push(n.to_string());
        (self.names.len() - 1) as u32
    }

    fn compile_stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let(name, e) | Stmt::Assign(name, e) => {
                self.compile_expr(e)?;
                let sl = self.slot(name);
                self.emit(Instr::Store(sl));
            }
            Stmt::Print(e) => {
                self.compile_expr(e)?;
                self.emit(Instr::PrintOut);
            }
            Stmt::ExprStmt(e) => { self.compile_expr(e)?; self.emit(Instr::Pop); }
            Stmt::Return(e) => {
                match e {
                    Some(x) => self.compile_expr(x)?,
                    None => { let k = self.konst(Value::Null); self.emit(Instr::Const(k)); }
                };
                self.emit(Instr::Return);
            }
            Stmt::If(cond, then, els) => {
                self.compile_expr(cond)?;
                let jf = self.emit(Instr::JumpIfFalse(u32::MAX));
                for st in then { self.compile_stmt(st)?; }
                let jend = self.emit(Instr::Jump(u32::MAX));
                let else_pc = self.code.len();
                self.patch(jf, else_pc);
                if let Some(b) = els { for st in b { self.compile_stmt(st)?; } }
                let end = self.code.len();
                self.patch(jend, end);
            }
            Stmt::While(cond, body) => {
                let start = self.code.len();
                self.compile_expr(cond)?;
                let jf = self.emit(Instr::JumpIfFalse(u32::MAX));
                self.loops.push((start, vec![]));
                for st in body { self.compile_stmt(st)?; }
                self.emit(Instr::Jump(start as u32));
                let end = self.code.len();
                self.patch(jf, end);
                let (_, breaks) = self.loops.pop().unwrap();
                for b in breaks { self.patch(b, end); }
            }
            Stmt::For(var, iter, body) => {
                self.compile_expr(iter)?;
                self.emit(Instr::ForIter);
                let sl = self.slot(var);
                let start = self.code.len();
                let next = self.emit(Instr::ForNext(sl, u32::MAX));
                self.loops.push((start, vec![]));
                for st in body { self.compile_stmt(st)?; }
                self.emit(Instr::Jump(start as u32));
                let end = self.code.len();
                self.patch(next, end);
                let (_, breaks) = self.loops.pop().unwrap();
                for b in breaks { self.patch(b, end); }
            }
            Stmt::Break => {
                if self.loops.is_empty() { return Err("break outside loop".into()); }
                let j = self.emit(Instr::Jump(u32::MAX));
                self.loops.last_mut().unwrap().1.push(j);
            }
            Stmt::Continue => {
                let (start, _) = self.loops.last()
                    .ok_or("continue outside loop")?;
                let t = *start as u32;
                self.emit(Instr::Jump(t));
            }
            Stmt::IndexAssign(obj, idx, val) => {
                // Var receiver: write goes back through the env so the
                // persistent structure's new head replaces the var.
                if let Expr::Var(name) = obj {
                    self.compile_expr(idx)?;
                    self.compile_expr(val)?;
                    let sl = self.slot(name);
                    self.emit(Instr::IndexSetVar(sl));
                } else {
                    return Err("VM subset: index-assign only on variables".into());
                }
            }
            _ => return Err(format!("VM subset: unsupported stmt {:?}", stmt_tag(s))),
        }
        Ok(())
    }

    fn compile_expr(&mut self, e: &Expr) -> Result<(), String> {
        match e {
            Expr::Int(n) => { let k = self.konst(Value::Int(*n)); self.emit(Instr::Const(k)); },
            Expr::Float(f) => { let k = self.konst(Value::Float(*f)); self.emit(Instr::Const(k)); },
            Expr::Str(s) => { let k = self.konst(Value::Str(s.clone().into())); self.emit(Instr::Const(k)); },
            Expr::Bool(b) => { let k = self.konst(Value::Bool(*b)); self.emit(Instr::Const(k)); },
            Expr::Null => { let k = self.konst(Value::Null); self.emit(Instr::Const(k)); },
            Expr::Var(n) => {
                match self.slots.get(n) {
                    Some(&sl) => self.emit(Instr::Load(sl)),
                    None => { let i = self.name_idx(n); self.emit(Instr::LoadG(i)) }
                };
            }
            Expr::List(items) => {
                for it in items { self.compile_expr(it)?; }
                self.emit(Instr::MakeList(items.len() as u32));
            }
            Expr::BinOp(l, op, r) => {
                match op {
                    BinOp::NullCoal => {
                        self.compile_expr(l)?;
                        let j = self.emit(Instr::PeekJumpIfNull(u32::MAX));
                        // non-null path: keep lhs, jump to end
                        let jend = self.emit(Instr::Jump(u32::MAX));
                        let rhs_pc = self.code.len();
                        self.patch(j, rhs_pc);
                        self.emit(Instr::Pop); // discard null lhs
                        self.compile_expr(r)?;
                        let end = self.code.len();
                        self.patch(jend, end);
                    }
                    BinOp::And => { self.compile_expr(l)?; self.compile_expr(r)?; self.emit(Instr::AndOp); }
                    BinOp::Or => { self.compile_expr(l)?; self.compile_expr(r)?; self.emit(Instr::OrOp); }
                    BinOp::In => { self.compile_expr(l)?; self.compile_expr(r)?; self.emit(Instr::InOp); }
                    _ => {
                        self.compile_expr(l)?;
                        self.compile_expr(r)?;
                        self.emit(match op {
                            BinOp::Add => Instr::Add, BinOp::Sub => Instr::Sub,
                            BinOp::Mul => Instr::Mul, BinOp::Div => Instr::Div,
                            BinOp::Mod => Instr::Mod, BinOp::Eq => Instr::Eq,
                            BinOp::Neq => Instr::Neq, BinOp::Lt => Instr::Lt,
                            BinOp::Gt => Instr::Gt, BinOp::Le => Instr::Le,
                            BinOp::Ge => Instr::Ge,
                            _ => unreachable!(),
                        });
                    }
                }
            }
            Expr::UnaryOp(UnaryOp::Neg, inner) => {
                self.compile_expr(inner)?; self.emit(Instr::Neg);
            }
            Expr::UnaryOp(UnaryOp::Not, inner) => {
                self.compile_expr(inner)?; self.emit(Instr::Not);
            }
            Expr::Call(name, variant, args) => {
                if let Some(v) = variant {
                    return Err(format!("VM subset: explicit variant {}#{} unsupported", name, v));
                }
                for a in args { self.compile_expr(a)?; }
                if let Some(&sl) = self.slots.get(name) {
                    // name is a local var — may hold a Closure
                    self.emit(Instr::CallVar(sl, args.len() as u32));
                } else {
                    let ni = self.name_idx(name);
                    self.emit(Instr::Call(ni, args.len() as u32));
                }
            }
            Expr::MethodCall(obj, m, args) => {
                // Var receiver: delegate then write the (possibly mutated)
                // receiver back into the slot — mutating methods like
                // append/insert must land on OUR env, not the helper's.
                let var_slot = match obj.as_ref() {
                    Expr::Var(n) => self.slots.get(n).copied(),
                    _ => None,
                };
                self.compile_expr(obj)?;
                for a in args { self.compile_expr(a)?; }
                let ni = self.name_idx(m);
                match var_slot {
                    Some(sl) => self.emit(Instr::MethodCallVar(
                        sl, ni, args.len() as u32)),
                    None => self.emit(Instr::MethodCall(
                        ni, args.len() as u32)),
                };
            }
            Expr::Index(o, i) => {
                self.compile_expr(o)?; self.compile_expr(i)?;
                self.emit(Instr::IndexGet);
            }
            Expr::Lambda(params, body) => {
                // Closure proto in const pool; CaptureEnv fills its env
                // with a snapshot of the frame's named slots at runtime.
                let proto = Value::Closure(
                    params.clone(), body.clone(),
                    std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())));
                let k = self.konst(proto);
                self.emit(Instr::Const(k));
                self.emit(Instr::CaptureEnv);
            }
            Expr::Interp(parts) => {
                for (i, p) in parts.iter().enumerate() {
                    self.compile_expr(p)?;
                    if !matches!(p, Expr::Str(_)) { self.emit(Instr::ToStr); }
                    if i > 0 { self.emit(Instr::Add); }
                }
                if parts.is_empty() {
                    let k = self.konst(Value::Str("".into()));
                    self.emit(Instr::Const(k));
                }
            }
            _ => return Err(format!("VM subset: unsupported expr {:?}", expr_tag(e))),
        }
        Ok(())
    }
}

fn stmt_tag(s: &Stmt) -> &'static str {
    match s {
        Stmt::Let(..) => "let", Stmt::Assign(..) => "assign",
        Stmt::Print(..) => "print", Stmt::ExprStmt(..) => "exprstmt",
        Stmt::Return(..) => "return", Stmt::If(..) => "if",
        Stmt::While(..) => "while", Stmt::For(..) => "for",
        Stmt::Break => "break", Stmt::Continue => "continue",
        Stmt::IndexAssign(..) => "index-assign", Stmt::FieldAssign(..) => "field-assign",
        Stmt::LetDestructure(..) => "let-destructure", Stmt::FnDef(..) => "fn",
        Stmt::StructDef(..) => "struct", Stmt::Match(..) => "match",
        Stmt::Try(..) => "try", Stmt::Grow(..) => "grow",
        Stmt::Import(..) => "import",
    }
}

fn expr_tag(e: &Expr) -> &'static str {
    match e {
        Expr::Int(..) => "int", Expr::Float(..) => "float",
        Expr::Str(..) => "str", Expr::Interp(..) => "interp",
        Expr::Bool(..) => "bool", Expr::Null => "null",
        Expr::List(..) => "list", Expr::Var(..) => "var",
        Expr::BinOp(..) => "binop", Expr::UnaryOp(..) => "unary",
        Expr::Call(..) => "call", Expr::MethodCall(..) => "method",
        Expr::FieldAccess(..) => "field", Expr::StructLit(..) => "structlit",
        Expr::Index(..) => "index", Expr::Slice(..) => "slice",
        Expr::Lambda(..) => "lambda", Expr::ListComp(..) => "listcomp",
        Expr::SafeGet(..) => "safeget",
    }
}

// ---------------- VM ----------------

impl Vm {
    pub fn new() -> Self {
        Vm {
            fns: HashMap::new(),
            globals: HashMap::new(),
            helper: Interpreter::new(),
            steps: 0,
            max_steps: 100_000_000,
        }
    }

    /// Compile + run a parsed program. Garden functions must already be
    /// planted via load_garden() if call sites reference them.
    pub fn run(&mut self, stmts: &[Stmt], max_steps: u64) -> Result<(), String> {
        self.max_steps = max_steps;
        // First pass: register fn defs (compiled).
        for s in stmts {
            match s {
                Stmt::FnDef(name, params, body)
                | Stmt::Grow(GrowItem::Fn(name, params, body, _)) => {
                    let pnames: Vec<String> =
                        params.iter().map(|p| p.name.clone()).collect();
                    let code = self.compile_fn(&pnames, body)?;
                    let id = self.fns.get(name).map(|v| v.len() as u32 + 1).unwrap_or(1);
                    self.fns.entry(name.clone()).or_default().push(CompiledFn {
                        id, fitness: 1.0,
                        params: params.iter().map(|p| p.name.clone()).collect(),
                        code: Rc::new(code),
                    });
                }
                _ => {}
            }
        }
        // Second pass: compile the rest as main.
        let rest: Vec<Stmt> = stmts.iter()
            .filter(|s| !matches!(s,
                Stmt::FnDef(..) | Stmt::Grow(..)
                | Stmt::StructDef(..) | Stmt::Import(..)))
            .cloned().collect();
        let main_code = self.compile_body(&rest)?;
        self.exec(Rc::new(main_code), &mut Vec::new())
    }

    /// Compile garden variants into the VM (same filter as the tree-walker).
    pub fn load_garden(&mut self, veldt: &crate::veldt::Veldt) -> Result<(), String> {
        for (name, variants) in &veldt.functions {
            let mut list = Vec::new();
            for entry in variants {
                if matches!(entry.health, crate::veldt::Health::Healthy
                            | crate::veldt::Health::Sick(_)) {
                    let pnames: Vec<String> =
                        entry.params.iter().map(|p| p.name.clone()).collect();
                    let code = self.compile_fn(&pnames, &entry.body)
                        .map_err(|e| format!("vm: can't compile {}#{}: {}", name, entry.id, e))?;
                    list.push(CompiledFn {
                        id: entry.id,
                        fitness: entry.fitness,
                        params: entry.params.iter().map(|p| p.name.clone()).collect(),
                        code: Rc::new(code),
                    });
                }
            }
            self.fns.insert(name.clone(), list);
        }
        for (name, entry) in &veldt.variables {
            if let Ok(v) = crate::veldt::deserialize_value(&entry.value) {
                self.globals.insert(name.clone(), v);
            }
        }
        Ok(())
    }

    fn compile_body(&self, stmts: &[Stmt]) -> Result<Chunk, String> {
        self.compile_fn(&[], stmts)
    }

    fn compile_fn(&self, params: &[String], stmts: &[Stmt])
        -> Result<Chunk, String> {
        let mut c = Compiler {
            code: Vec::new(), consts: Vec::new(), names: Vec::new(),
            slot_names: Vec::new(),
            loops: Vec::new(), slots: HashMap::new(), next_slot: 0,
        };
        // params occupy slots 0..n — the frame binds args by position
        for p in params { c.slot(p); }
        for s in stmts { c.compile_stmt(s)?; }
        c.emit(Instr::Halt);
        Ok(Chunk { code: c.code, consts: c.consts,
                   names: c.names, slot_names: c.slot_names })
    }

    fn exec(&mut self, code: Rc<Chunk>, args_out: &mut Vec<Value>) -> Result<(), String> {
        let mut stack: Vec<Value> = Vec::with_capacity(64);
        let mut frames: Vec<Frame> = vec![Frame {
            code, pc: 0, env: Vec::new(), base: 0, iters: Vec::new(),
        }];
        let mut code = frames.last().unwrap().code.clone(); // Rc bump — once

        loop {
            self.steps += 1;
            if self.steps > self.max_steps {
                return Err(format!("VM step limit exceeded ({})", self.max_steps));
            }
            let instr = {
                let frame = frames.last_mut().unwrap();
                if frame.pc >= code.code.len() {
                    if frames.len() == 1 { return Ok(()); }
                    frames.pop();
                    stack.push(Value::Null);
                    code = frames.last().unwrap().code.clone();
                    continue;
                }
                let i = frame.pc;
                frame.pc += 1;
                code.code[i]
            };
            match instr {
                Instr::Const(k) => stack.push(code.consts[k as usize].clone()),
                Instr::Load(sl) => {
                    let frame = frames.last_mut().unwrap();
                    let v = frame.env.get(sl as usize)
                        .and_then(|o| o.clone())
                        .ok_or_else(|| format!("Undefined variable (slot {})", sl))?;
                    stack.push(v);
                }
                Instr::LoadG(ni) => {
                    let n = &code.names[ni as usize];
                    let v = self.globals.get(n).cloned()
                        .ok_or_else(|| format!("Undefined variable: {}", n))?;
                    stack.push(v);
                }
                Instr::Store(sl) => {
                    let v = stack.pop().ok_or("stack underflow")?;
                    let frame = frames.last_mut().unwrap();
                    if frame.env.len() <= sl as usize {
                        frame.env.resize(sl as usize + 1, None);
                    }
                    frame.env[sl as usize] = Some(v);
                }
                Instr::StoreG(ni) => {
                    let v = stack.pop().ok_or("stack underflow")?;
                    self.globals.insert(code.names[ni as usize].clone(), v);
                }
                Instr::Pop => { stack.pop(); }
                Instr::Add => self.binop(&mut stack, BinOp::Add)?,
                Instr::Sub => self.binop(&mut stack, BinOp::Sub)?,
                Instr::Mul => self.binop(&mut stack, BinOp::Mul)?,
                Instr::Div => self.binop(&mut stack, BinOp::Div)?,
                Instr::Mod => self.binop(&mut stack, BinOp::Mod)?,
                Instr::Eq => self.binop(&mut stack, BinOp::Eq)?,
                Instr::Neq => self.binop(&mut stack, BinOp::Neq)?,
                Instr::Lt => self.binop(&mut stack, BinOp::Lt)?,
                Instr::Gt => self.binop(&mut stack, BinOp::Gt)?,
                Instr::Le => self.binop(&mut stack, BinOp::Le)?,
                Instr::Ge => self.binop(&mut stack, BinOp::Ge)?,
                Instr::AndOp => {
                    let r = stack.pop().unwrap(); let l = stack.pop().unwrap();
                    stack.push(Value::Bool(truthy(&l) && truthy(&r)));
                }
                Instr::OrOp => {
                    let r = stack.pop().unwrap(); let l = stack.pop().unwrap();
                    stack.push(Value::Bool(truthy(&l) || truthy(&r)));
                }
                Instr::InOp => {
                    let r = stack.pop().unwrap(); let l = stack.pop().unwrap();
                    let hit = match (&l, &r) {
                        (v, Value::List(items)) =>
                            items.iter().any(|i| values_eq(v, i)),
                        (Value::Str(a), Value::Str(b)) => b.contains(&**a),
                        (Value::Str(k), Value::Dict(d)) => d.contains_key(&**k),
                        _ => return Err("'in' requires list, dict, or string".into()),
                    };
                    stack.push(Value::Bool(hit));
                }
                Instr::Neg => {
                    let v = stack.pop().unwrap();
                    stack.push(match v {
                        Value::Int(n) => Value::Int(-n),
                        Value::Float(f) => Value::Float(-f),
                        _ => return Err("cannot negate".into()),
                    });
                }
                Instr::Not => {
                    let v = stack.pop().unwrap();
                    stack.push(Value::Bool(!truthy(&v)));
                }
                Instr::Jump(t) => frames.last_mut().unwrap().pc = t as usize,
                Instr::JumpIfFalse(t) => {
                    let v = stack.pop().unwrap();
                    if !truthy(&v) { frames.last_mut().unwrap().pc = t as usize; }
                }
                Instr::PeekJumpIfNull(t) => {
                    if matches!(stack.last(), Some(Value::Null)) {
                        frames.last_mut().unwrap().pc = t as usize;
                    }
                }
                Instr::ToStr => {
                    let v = stack.pop().unwrap();
                    stack.push(Value::Str(format!("{}", v).into()));
                }
                Instr::MakeList(n) => {
                    let items: Vec<Value> =
                        stack.drain(stack.len() - n as usize..).collect();
                    stack.push(Value::List(items.into_iter().collect()));
                }
                Instr::IndexGet => {
                    let idx = stack.pop().unwrap();
                    let obj = stack.pop().unwrap();
                    stack.push(index_get(&obj, &idx)?);
                }
                Instr::IndexSetVar(sl) => {
                    let val = stack.pop().unwrap();
                    let idx = stack.pop().unwrap();
                    let frame = frames.last_mut().unwrap();
                    let cur = frame.env.get(sl as usize).cloned()
                        .flatten()
                        .ok_or("index-assign on unset variable")?;
                    let nv = index_set(&cur, &idx, &val)?;
                    if frame.env.len() <= sl as usize {
                        frame.env.resize(sl as usize + 1, None);
                    }
                    frame.env[sl as usize] = Some(nv);
                }
                Instr::ForIter => {
                    let v = stack.pop().unwrap();
                    let items = match v {
                        Value::List(l) => l.iter().cloned().collect(),
                        Value::Str(s) => s.chars()
                            .map(|c| Value::Str(c.to_string().into())).collect(),
                        Value::Dict(d) => d.keys()
                            .map(|k| Value::Str(k.clone().into())).collect(),
                        _ => return Err("for needs a list, string, or dict".into()),
                    };
                    frames.last_mut().unwrap().iters.push((items, 0));
                }
                Instr::ForNext(sl, end) => {
                    let next = {
                        let frame = frames.last_mut().unwrap();
                        match frame.iters.last_mut() {
                            Some((items, i)) if *i < items.len() => {
                                let v = items[*i].clone(); *i += 1;
                                Some(v)
                            }
                            _ => None,
                        }
                    };
                    let frame = frames.last_mut().unwrap();
                    match next {
                        Some(v) => {
                            if frame.env.len() <= sl as usize {
                                frame.env.resize(sl as usize + 1, None);
                            }
                            frame.env[sl as usize] = Some(v);
                        }
                        None => { frame.iters.pop(); frame.pc = end as usize; }
                    }
                }
                Instr::Call(ni, argc) => {
                    let args: Vec<Value> =
                        stack.drain(stack.len() - argc as usize..).collect();
                    let name = code.names[ni as usize].clone();
                    // builtin first (same as tree-walk)
                    if let Some(v) = self.helper.try_builtin(&name, &args)? {
                        stack.push(v);
                        continue;
                    }
                    let variants = self.fns.get(&name)
                        .ok_or_else(|| format!("Undefined function: {}", name))?;
                    let chosen = variants.iter()
                        .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                            .unwrap_or(std::cmp::Ordering::Equal))
                        .ok_or_else(|| format!("No variants of {}", name))?;
                    if args.len() != chosen.params.len() {
                        return Err(format!("{} expects {} args, got {}",
                            name, chosen.params.len(), args.len()));
                    }
                    let env: Vec<Option<Value>> =
                        args.into_iter().map(Some).collect();
                    frames.push(Frame {
                        code: chosen.code.clone(),   // Rc bump — no alloc
                        pc: 0, env, base: stack.len(), iters: Vec::new(),
                    });
                    code = frames.last().unwrap().code.clone();
                }
                Instr::MethodCall(ni, argc) => {
                    let args: Vec<Value> =
                        stack.drain(stack.len() - argc as usize..).collect();
                    let recv = stack.pop().unwrap();
                    let m = &code.names[ni as usize];
                    let v = self.delegate_method(recv, m, args)?;
                    stack.push(v);
                }
                Instr::MethodCallVar(sl, ni, argc) => {
                    let args: Vec<Value> =
                        stack.drain(stack.len() - argc as usize..).collect();
                    let recv = stack.pop().unwrap();
                    let m = code.names[ni as usize].clone();
                    let v = self.delegate_method(recv, &m, args)?;
                    // write back — mutating methods changed the receiver
                    let newv = self.helper.vars.borrow()
                        .get("__vm_recv").cloned();
                    if let Some(nv) = newv {
                        let frame = frames.last_mut().unwrap();
                        if frame.env.len() <= sl as usize {
                            frame.env.resize(sl as usize + 1, None);
                        }
                        frame.env[sl as usize] = Some(nv);
                    }
                    stack.push(v);
                }
                Instr::CaptureEnv => {
                    let proto = stack.pop().unwrap();
                    let Value::Closure(params, body, _) = proto else {
                        return Err("CaptureEnv on non-closure".into());
                    };
                    // snapshot named slots into a HashMap env (capture-all,
                    // matching the tree-walker's vars.clone() semantics)
                    let frame = frames.last().unwrap();
                    let mut env = HashMap::new();
                    for (i, v) in frame.env.iter().enumerate() {
                        if let (Some(vv), Some(n)) =
                            (v, code.slot_names.get(i)) {
                            env.insert(n.clone(), vv.clone());
                        }
                    }
                    stack.push(Value::Closure(
                        params, body,
                        std::rc::Rc::new(std::cell::RefCell::new(env))));
                }
                Instr::CallVar(sl, argc) => {
                    let args: Vec<Value> =
                        stack.drain(stack.len() - argc as usize..).collect();
                    let frame = frames.last().unwrap();
                    let v = frame.env.get(sl as usize)
                        .and_then(|o| o.clone())
                        .ok_or("call on unset variable")?;
                    match v {
                        Value::Closure(..) => {
                            let r = self.helper.call_closure_pub(&v, args)?;
                            stack.push(r);
                        }
                        _ => return Err("variable is not callable".into()),
                    }
                }
                Instr::PrintOut => {
                    let v = stack.pop().unwrap();
                    println!("{}", v);
                }
                Instr::Return => {
                    let v = stack.pop().unwrap_or(Value::Null);
                    let f = frames.pop().unwrap();
                    stack.truncate(f.base);
                    if frames.is_empty() {
                        let _ = args_out;
                        return Ok(());
                    }
                    code = frames.last().unwrap().code.clone();
                    stack.push(v);
                }
                Instr::Halt => {
                    if frames.len() == 1 { return Ok(()); }
                    let f = frames.pop().unwrap();
                    stack.truncate(f.base);
                    code = frames.last().unwrap().code.clone();
                    stack.push(Value::Null);
                }
            }
        }
    }

    /// Delegate a method call to the shared interpreter by evaluating a
    /// synthetic MethodCall expr — all method semantics (including the
    /// Phase 6 fast paths) come from the one implementation.
    fn delegate_method(&mut self, recv: Value, m: &str, args: Vec<Value>)
        -> Result<Value, String> {
        let mut vars = self.helper.vars.borrow_mut();
        vars.insert("__vm_recv".to_string(), recv);
        for (i, a) in args.iter().enumerate() {
            vars.insert(format!("__vm_arg{}", i), a.clone());
        }
        drop(vars);
        let arg_exprs: Vec<Expr> = (0..args.len())
            .map(|i| Expr::Var(format!("__vm_arg{}", i))).collect();
        let e = Expr::MethodCall(
            Box::new(Expr::Var("__vm_recv".into())), m.to_string(), arg_exprs);
        self.helper.eval_expr_pub(&e)
    }

    fn binop(&mut self, stack: &mut Vec<Value>, op: BinOp) -> Result<(), String> {
        let r = stack.pop().ok_or("stack underflow")?;
        let l = stack.pop().ok_or("stack underflow")?;
        stack.push(binop(&l, &op, &r)?);
        Ok(())
    }
}

// ---------- helpers (pure, no interpreter state) ----------

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(n) => *n != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Null => false,
        Value::List(l) => !l.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Set(s) => !s.is_empty(),
        _ => true,
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        (Value::List(x), Value::List(y)) =>
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_eq(p, q)),
        _ => false,
    }
}

fn index_get(obj: &Value, idx: &Value) -> Result<Value, String> {
    match (obj, idx) {
        (Value::List(l), Value::Int(i)) => {
            let len = l.len() as i64;
            let ix = if *i < 0 { len + i } else { *i };
            if ix < 0 || ix >= len {
                return Err(format!("index {} out of bounds (len {})", i, len));
            }
            Ok(l.get(ix as usize).cloned().unwrap_or(Value::Null))
        }
        (Value::Str(s), Value::Int(i)) => {
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as i64;
            let ix = if *i < 0 { len + i } else { *i };
            if ix < 0 || ix >= len {
                return Err(format!("index {} out of bounds", i));
            }
            Ok(Value::Str(chars[ix as usize].to_string().into()))
        }
        (Value::Dict(d), Value::Str(k)) =>
            Ok(d.get(&**k).cloned().unwrap_or(Value::Null)),
        _ => Err(format!("cannot index {:?} with {:?}", obj_tag(obj), obj_tag(idx))),
    }
}

fn index_set(obj: &Value, idx: &Value, val: &Value) -> Result<Value, String> {
    match (obj, idx) {
        (Value::List(l), Value::Int(i)) => {
            let len = l.len() as i64;
            let ix = if *i < 0 { len + i } else { *i };
            if ix < 0 || ix >= len {
                return Err(format!("index {} out of bounds", i));
            }
            let mut l2 = l.clone();
            l2.set(ix as usize, val.clone());
            Ok(Value::List(l2))
        }
        (Value::Dict(d), Value::Str(k)) => {
            let mut d2 = d.clone();
            d2.insert((**k).to_string(), val.clone());
            Ok(Value::Dict(d2))
        }
        _ => Err("VM subset: index-assign supports lists and dicts".into()),
    }
}

fn obj_tag(v: &Value) -> &'static str {
    match v {
        Value::Int(..) => "int", Value::Float(..) => "float",
        Value::Str(..) => "str", Value::Bool(..) => "bool",
        Value::List(..) => "list", Value::Dict(..) => "dict",
        Value::Set(..) => "set", Value::Struct(..) => "struct",
        Value::Closure(..) => "closure", Value::Null => "null",
    }
}

fn binop(l: &Value, op: &BinOp, r: &Value) -> Result<Value, String> {
    use Value::*;
    Ok(match (l, op, r) {
        (Int(a), BinOp::Add, Int(b)) => Int(a + b),
        (Float(a), BinOp::Add, Float(b)) => Float(a + b),
        (Int(a), BinOp::Add, Float(b)) => Float((*a as f64) + b),
        (Float(a), BinOp::Add, Int(b)) => Float(a + (*b as f64)),
        (Str(a), BinOp::Add, Str(b)) => Str(format!("{}{}", a, b).into()),
        (Str(a), BinOp::Add, other) => Str(format!("{}{}", a, other).into()),
        (other, BinOp::Add, Str(b)) => Str(format!("{}{}", other, b).into()),
        (Int(a), BinOp::Sub, Int(b)) => Int(a - b),
        (Float(a), BinOp::Sub, Float(b)) => Float(a - b),
        (Int(a), BinOp::Sub, Float(b)) => Float((*a as f64) - b),
        (Float(a), BinOp::Sub, Int(b)) => Float(a - (*b as f64)),
        (Int(a), BinOp::Mul, Int(b)) => Int(a * b),
        (Float(a), BinOp::Mul, Float(b)) => Float(a * b),
        (Int(a), BinOp::Mul, Float(b)) => Float((*a as f64) * b),
        (Float(a), BinOp::Mul, Int(b)) => Float(a * (*b as f64)),
        (Int(a), BinOp::Div, Int(b)) => {
            if *b == 0 { return Err("division by zero".into()); }
            Int(a / b)
        },
        (Float(a), BinOp::Div, Float(b)) => Float(a / b),
        (Int(a), BinOp::Div, Float(b)) => Float((*a as f64) / b),
        (Float(a), BinOp::Div, Int(b)) => Float(a / (*b as f64)),
        (Int(a), BinOp::Mod, Int(b)) => {
            if *b == 0 { return Err("modulo by zero".into()); }
            Int(a % b)
        },
        (Float(a), BinOp::Mod, Float(b)) => Float(a % b),
        (Int(a), BinOp::Mod, Float(b)) => Float((*a as f64) % b),
        (Float(a), BinOp::Mod, Int(b)) => Float(a % (*b as f64)),
        (_, BinOp::Eq, _) => Bool(values_eq(l, r)),
        (_, BinOp::Neq, _) => Bool(!values_eq(l, r)),
        (Int(a), BinOp::Lt, Int(b)) => Bool(a < b),
        (Float(a), BinOp::Lt, Float(b)) => Bool(a < b),
        (Int(a), BinOp::Lt, Float(b)) => Bool((*a as f64) < *b),
        (Float(a), BinOp::Lt, Int(b)) => Bool(*a < (*b as f64)),
        (Str(a), BinOp::Lt, Str(b)) => Bool(a < b),
        (Int(a), BinOp::Gt, Int(b)) => Bool(a > b),
        (Float(a), BinOp::Gt, Float(b)) => Bool(a > b),
        (Int(a), BinOp::Gt, Float(b)) => Bool((*a as f64) > *b),
        (Float(a), BinOp::Gt, Int(b)) => Bool(*a > (*b as f64)),
        (Int(a), BinOp::Le, Int(b)) => Bool(a <= b),
        (Float(a), BinOp::Le, Float(b)) => Bool(a <= b),
        (Int(a), BinOp::Le, Float(b)) => Bool((*a as f64) <= *b),
        (Float(a), BinOp::Le, Int(b)) => Bool(*a <= (*b as f64)),
        (Int(a), BinOp::Ge, Int(b)) => Bool(a >= b),
        (Float(a), BinOp::Ge, Float(b)) => Bool(a >= b),
        (Int(a), BinOp::Ge, Float(b)) => Bool((*a as f64) >= *b),
        (Float(a), BinOp::Ge, Int(b)) => Bool(*a >= (*b as f64)),
        (Bool(a), BinOp::And, Bool(b)) => Bool(*a && *b),
        (Bool(a), BinOp::Or, Bool(b)) => Bool(*a || *b),
        _ => return Err(format!(
            "cannot apply {:?} to {:?} and {:?}", op, obj_tag(l), obj_tag(r))),
    })
}
