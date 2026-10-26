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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Debug, Clone, Copy)]
pub enum Instr {
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
    CallId(u32, u32, u32),   // name idx, variant id, argc
    MethodCall(u32, u32),    // name idx, argc
    MakeList(u32),
    IndexGet,
    IndexSetVar(u32),
    ToStr,
    ForIter,
    ForNext(u32, u32),       // var slot, jump target
    CaptureEnv,              // top of stack: Closure proto -> captured env
    CallVar(u32, u32),       // slot holding Closure, argc
    GetField(u32),           // name-table idx — obj.field
    SliceGet,                // obj lo hi -> obj[lo:hi] (null bounds = open)
    ListPush(u32),           // append to list in env slot (in-place)
    AppendVar(u32),          // env[slot].append(stack.pop()) — no delegate
    InsertVar(u32, u32),     // dict[slot].insert(str, val) — name idx + slot
    InsertDynVar(u32),       // dict[slot].insert(stack key, stack val)
    FieldSetVar(u32, u32),   // obj slot, field name idx — obj.f = v
    MakeStruct(u32, u32),    // name idx, field count (name,val pairs below)                // obj lo hi -> obj[lo:hi] (null bounds = open)
    MethodCallVar(u32, u32, u32), // receiver slot, method name idx, argc
    TryPush(u32, u32),           // catch pc, err-var slot
    TryPop,
    PrintOut,
    Return,
    Halt,
}

pub struct Chunk {
    pub(crate) code: Vec<Instr>,
    pub(crate) consts: Vec<Value>,
    pub(crate) names: Vec<String>,
    pub(crate) slot_names: Vec<String>,   // slot idx -> var name (for closure capture)
}

pub struct CompiledFn {
    pub id: u32,
    pub fitness: f64,
    pub params: Vec<String>,
    pub code: Rc<Chunk>,   // shared — call pushes an Rc bump, not a clone
    pub calls: std::cell::Cell<u64>,
    pub jit_tried: std::cell::Cell<bool>,
    pub jit: std::cell::RefCell<Option<crate::jit::JitFn>>,
}

struct Frame {
    code: Rc<Chunk>,
    pc: usize,
    env: Vec<Option<Value>>,            // slot-indexed locals
    base: usize,                        // value-stack base for this frame
    iters: Vec<(Vec<Value>, usize)>,    // for-loop iterator stack
    try_stack: Vec<(u32, u32)>,         // (catch_pc, err_var slot)
}

pub struct Vm {
    pub fns: HashMap<String, Vec<CompiledFn>>,
    pub globals: HashMap<String, Value>,
    jit_engine: Option<crate::jit::JitEngine>,
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
    known_fns: HashSet<String>, // for `alias::method` resolution
    global_mode: bool,      // module top-level: let/assign -> globals
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
            Instr::TryPush(t, _) => *t = target as u32,
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
            Stmt::Match(scrut, arms, els) => {
                // scrutinee into a scratch slot, then a comparison chain
                let tmpn = format!("__m{}", self.next_slot);
                let sl = self.slot(&tmpn);
                self.compile_expr(scrut)?;
                self.emit(Instr::Store(sl));
                let mut end_jumps = Vec::new();
                for (pat, body) in arms {
                    self.emit(Instr::Load(sl));
                    self.compile_expr(pat)?;
                    self.emit(Instr::Eq);
                    let jf = self.emit(Instr::JumpIfFalse(u32::MAX));
                    for st in body { self.compile_stmt(st)?; }
                    end_jumps.push(self.emit(Instr::Jump(u32::MAX)));
                    let nxt = self.code.len();
                    self.patch(jf, nxt);
                }
                if let Some(b) = els {
                    for st in b { self.compile_stmt(st)?; }
                }
                let end = self.code.len();
                for j in end_jumps { self.patch(j, end); }
            }
            Stmt::Try(body, err_var, handler) => {
                let es = self.slot(err_var);
                let tp = self.emit(Instr::TryPush(u32::MAX, es));
                for st in body { self.compile_stmt(st)?; }
                self.emit(Instr::TryPop);
                let jend = self.emit(Instr::Jump(u32::MAX));
                let catch = self.code.len();
                self.patch(tp, catch);
                for st in handler { self.compile_stmt(st)?; }
                let end = self.code.len();
                self.patch(jend, end);
            }
            Stmt::LetDestructure(names, e) => {
                // let [a,b] = expr -> eval once into scratch, then index out
                let tmpn = format!("__d{}", self.next_slot);
                let sl = self.slot(&tmpn);
                self.compile_expr(e)?;
                self.emit(Instr::Store(sl));
                for (i, n) in names.iter().enumerate() {
                    self.emit(Instr::Load(sl));
                    let k = self.konst(Value::Int(i as i64));
                    self.emit(Instr::Const(k));
                    self.emit(Instr::IndexGet);
                    let ts = self.slot(n);
                    self.emit(Instr::Store(ts));
                }
            }
            Stmt::FieldAssign(obj, field, val) => {
                if let Expr::Var(name) = obj {
                    self.compile_expr(val)?;
                    let sl = self.slot(name);
                    let ni = self.name_idx(field);
                    self.emit(Instr::FieldSetVar(sl, ni));
                } else {
                    return Err("VM subset: field-assign only on variables".into());
                }
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
                if self.global_mode {
                    let i = self.name_idx(n);
                    self.emit(Instr::LoadG(i));
                } else {
                    match self.slots.get(n) {
                        Some(&sl) => self.emit(Instr::Load(sl)),
                        None => { let i = self.name_idx(n); self.emit(Instr::LoadG(i)) }
                    };
                }
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
                for a in args { self.compile_expr(a)?; }
                if let Some(v) = variant {
                    let ni = self.name_idx(name);
                    self.emit(Instr::CallId(ni, *v, args.len() as u32));
                } else if let Some(&sl) = self.slots.get(name) {
                    // name is a local var — may hold a Closure
                    self.emit(Instr::CallVar(sl, args.len() as u32));
                } else {
                    let ni = self.name_idx(name);
                    self.emit(Instr::Call(ni, args.len() as u32));
                }
            }
            Expr::MethodCall(obj, m, args) => {
                // `lib.f(...)`: receiver is a bare name that's an import
                // alias — mangle to `lib::f` and emit a plain Call.
                if let Expr::Var(n) = obj.as_ref() {
                    if !self.slots.contains_key(n) {
                        let mangled = format!("{}::{}", n, m);
                        if self.known_fns.contains(&mangled) {
                            for a in args { self.compile_expr(a)?; }
                            let ni = self.name_idx(&mangled);
                            self.emit(Instr::Call(ni, args.len() as u32));
                            return Ok(());
                        }
                    }
                }
                // hot mutating ops on a Var compile to dedicated instrs —
                // delegation costs an AST eval per call (list_ops was
                // 3.4x slower). xs.append(v) and d.insert(k, v) get
                // their own ops; everything else delegates.
                if let Expr::Var(n) = obj.as_ref() {
                    if let Some(&sl) = self.slots.get(n) {
                        if m == "append" && args.len() == 1 {
                            self.compile_expr(&args[0])?;
                            self.emit(Instr::AppendVar(sl));
                            return Ok(());
                        }
                        if m == "insert" && args.len() == 2 {
                            if let Expr::Str(key) = &args[0] {
                                self.compile_expr(&args[1])?;
                                let ki = self.name_idx(key);
                                self.emit(Instr::InsertVar(sl, ki));
                            } else {
                                self.compile_expr(&args[0])?;
                                self.compile_expr(&args[1])?;
                                self.emit(Instr::InsertDynVar(sl));
                            }
                            return Ok(());
                        }
                    }
                }
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
            Expr::Slice(o, lo, hi) => {
                self.compile_expr(o)?;
                match lo { Some(x) => self.compile_expr(x)?,
                           None => { let k = self.konst(Value::Null); self.emit(Instr::Const(k)); } }
                match hi { Some(x) => self.compile_expr(x)?,
                           None => { let k = self.konst(Value::Null); self.emit(Instr::Const(k)); } }
                self.emit(Instr::SliceGet);
            }
            Expr::FieldAccess(o, name) => {
                self.compile_expr(o)?;
                let ni = self.name_idx(name);
                self.emit(Instr::GetField(ni));
            }
            Expr::StructLit(name, fields) => {
                for (fname, fexpr) in fields {
                    let k = self.konst(Value::Str(fname.clone().into()));
                    self.emit(Instr::Const(k));
                    self.compile_expr(fexpr)?;
                }
                let ni = self.name_idx(name);
                self.emit(Instr::MakeStruct(ni, fields.len() as u32));
            }
            Expr::SafeGet(o, name, args) => {
                // obj?.field / obj?.m(args) — null propagates the Null
                self.compile_expr(o)?;
                let jn = self.emit(Instr::PeekJumpIfNull(u32::MAX));
                match args {
                    Some(a) => {
                        for x in a { self.compile_expr(x)?; }
                        let ni = self.name_idx(name);
                        self.emit(Instr::MethodCall(ni, a.len() as u32));
                    }
                    None => {
                        let ni = self.name_idx(name);
                        self.emit(Instr::GetField(ni));
                    }
                }
                let end = self.code.len();
                self.patch(jn, end);
            }
            Expr::ListComp(expr, var, iter, cond) => {
                // [expr for x in iter (if cond)] -> build via ListPush
                let resn = format!("__lc{}", self.next_slot);
                let res = self.slot(&resn);
                self.emit(Instr::MakeList(0));
                self.emit(Instr::Store(res));
                self.compile_expr(iter)?;
                self.emit(Instr::ForIter);
                let xs = self.slot(var);
                let start = self.code.len();
                let next = self.emit(Instr::ForNext(xs, u32::MAX));
                let mut skip = None;
                if let Some(c) = cond {
                    self.compile_expr(c)?;
                    skip = Some(self.emit(Instr::JumpIfFalse(u32::MAX)));
                }
                self.compile_expr(expr)?;
                self.emit(Instr::ListPush(res));
                if let Some(j) = skip {
                    let t = self.code.len();
                    self.patch(j, t);
                }
                self.emit(Instr::Jump(start as u32));
                let end = self.code.len();
                self.patch(next, end);
                self.emit(Instr::Load(res));
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
            jit_engine: None,
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
        // Import pass: compile module fns into self.fns (alias::name +
        // bare-name registration, matching the interpreter's v1).
        for s in stmts {
            if let Stmt::Import(path, alias) = s {
                self.import_module(path, alias.as_deref())?;
            }
        }
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
                        calls: std::cell::Cell::new(0),
                        jit_tried: std::cell::Cell::new(false),
                        jit: std::cell::RefCell::new(None),
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
                        calls: std::cell::Cell::new(0),
                        jit_tried: std::cell::Cell::new(false),
                        jit: std::cell::RefCell::new(None),
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

    /// Compile a module's functions into the VM — `import "x" as lib`
    /// registers `lib::name` (and the bare name so the module's internal
    /// calls resolve). Module top-level non-fn stmts are not executed in
    /// the VM subset.
    fn import_module(&mut self, path: &str, alias: Option<&str>)
        -> Result<(), String> {
        let src = crate::interpreter::resolve_import_source(path)?;
        let mut lx = crate::lexer::Lexer::new(&src);
        let toks = lx.tokenize()
            .map_err(|e| format!("import '{}' lex: {}", path, e))?;
        let mut p = crate::parser::Parser::new_with_lines(toks, lx.token_lines.clone());
        let stmts = p.parse_program()
            .map_err(|e| format!("import '{}' parse: {}", path, e))?;
        // nested imports first
        for st in &stmts {
            if let Stmt::Import(p2, a2) = st {
                self.import_module(p2, a2.as_deref())?;
            }
        }
        // module side-effect stmts: bare imports run them into globals
        if alias.is_none() {
            let top: Vec<Stmt> = stmts.iter()
                .filter(|st| !matches!(st, Stmt::FnDef(..)
                    | Stmt::Grow(..) | Stmt::Import(..)))
                .cloned().collect();
            if !top.is_empty() {
                // global mode: a compiler with no prebound slots —
                // let/assign emit StoreG so results land in globals
                let mut gc = Compiler {
                    code: Vec::new(), consts: Vec::new(),
                    names: Vec::new(), slot_names: Vec::new(),
                    known_fns: self.fns.keys().cloned().collect(),
                    global_mode: true,
                    loops: Vec::new(), slots: HashMap::new(),
                    next_slot: 0,
                };
                for st in &top { gc.compile_stmt(st)
                    .map_err(|e| format!("import '{}': {}", path, e))?; }
                gc.emit(Instr::Halt);
                let chunk = std::rc::Rc::new(Chunk {
                    code: gc.code, consts: gc.consts,
                    names: gc.names, slot_names: gc.slot_names,
                });
                let mut args = Vec::new();
                self.exec(chunk, &mut args)
                    .map_err(|e| format!("import '{}' exec: {}", path, e))?;
            }
        }
        for st in &stmts {
            if let Stmt::FnDef(name, params, body) = st {
                let pnames: Vec<String> =
                    params.iter().map(|p| p.name.clone()).collect();
                let code = self.compile_fn(&pnames, body)
                    .map_err(|e| format!("import '{}': {}: {}", path, name, e))?;
                let entry = CompiledFn {
                    id: 1, fitness: 1.0,
                    params: pnames, code: std::rc::Rc::new(code),
                    calls: std::cell::Cell::new(0),
                    jit_tried: std::cell::Cell::new(false),
                    jit: std::cell::RefCell::new(None),
                };
                if let Some(a) = alias {
                    self.fns.entry(format!("{}::{}", a, name))
                        .or_default().push(CompiledFn {
                            id: 1, fitness: 1.0,
                            params: entry.params.clone(),
                            code: entry.code.clone(),
                            calls: std::cell::Cell::new(0),
                            jit_tried: std::cell::Cell::new(false),
                            jit: std::cell::RefCell::new(None),
                        });
                    self.fns.entry(name.clone())
                        .or_insert_with(|| vec![CompiledFn {
                            id: 1, fitness: 1.0,
                            params: entry.params.clone(),
                            code: entry.code.clone(),
                            calls: std::cell::Cell::new(0),
                            jit_tried: std::cell::Cell::new(false),
                            jit: std::cell::RefCell::new(None),
                        }]);
                } else {
                    self.fns.entry(name.clone()).or_default().push(entry);
                }
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
            known_fns: self.fns.keys().cloned().collect(),
            global_mode: false,
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
            code, pc: 0, env: Vec::new(), base: 0,
            iters: Vec::new(), try_stack: Vec::new(),
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
            if let Err(e) = self.step(&mut stack, &mut frames, &mut code, instr) {
                // try/catch: unwind frames until one has a handler —
                // errors inside nested calls propagate to the caller's
                // try, matching the interpreter's exec_block semantics.
                loop {
                    let handled = {
                        let frame = frames.last_mut().unwrap();
                        if let Some((catch, errsl)) = frame.try_stack.pop() {
                            stack.truncate(frame.base);
                            if frame.env.len() <= errsl as usize {
                                frame.env.resize(errsl as usize + 1, None);
                            }
                            frame.env[errsl as usize] =
                                Some(Value::Str(e.clone().into()));
                            frame.pc = catch as usize;
                            true
                        } else { false }
                    };
                    if handled { break; }
                    if frames.len() == 1 { return Err(e); }
                    let f = frames.pop().unwrap();
                    stack.truncate(f.base);
                }
                code = frames.last().unwrap().code.clone();
                continue;
            }
            if frames.is_empty() { return Ok(()); }  // last frame returned
        }
    }

    /// One dispatch step — errors surface to exec's try/catch boundary.
    fn step(&mut self, stack: &mut Vec<Value>, frames: &mut Vec<Frame>,
            code: &mut Rc<Chunk>, instr: Instr) -> Result<(), String> {
        {
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
                Instr::Add => self.binop(stack, BinOp::Add)?,
                Instr::Sub => self.binop(stack, BinOp::Sub)?,
                Instr::Mul => self.binop(stack, BinOp::Mul)?,
                Instr::Div => self.binop(stack, BinOp::Div)?,
                Instr::Mod => self.binop(stack, BinOp::Mod)?,
                Instr::Eq => self.binop(stack, BinOp::Eq)?,
                Instr::Neq => self.binop(stack, BinOp::Neq)?,
                Instr::Lt => self.binop(stack, BinOp::Lt)?,
                Instr::Gt => self.binop(stack, BinOp::Gt)?,
                Instr::Le => self.binop(stack, BinOp::Le)?,
                Instr::Ge => self.binop(stack, BinOp::Ge)?,
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
                    // call_best dispatches on the VM's own table — the
                    // helper interpreter has no functions loaded.
                    if name == "call_best" {
                        let fname = match args.first() {
                            Some(Value::Str(s)) => (**s).to_string(),
                            _ => return Err("call_best(name, args...)".into()),
                        };
                        let variants = self.fns.get(&fname)
                            .ok_or_else(|| format!(
                                "call_best: no variants of '{}'", fname))?;
                        let chosen = variants.iter()
                            .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                                .unwrap_or(std::cmp::Ordering::Equal))
                            .unwrap();
                        let fargs: Vec<Option<Value>> =
                            args[1..].iter().cloned().map(Some).collect();
                        frames.push(Frame {
                            code: chosen.code.clone(),
                            pc: 0, env: fargs, base: stack.len(),
                            iters: Vec::new(), try_stack: Vec::new(),
                        });
                        *code = frames.last().unwrap().code.clone();
                        return Ok(());
                    }
                    // builtin first (same as tree-walk)
                    if let Some(v) = self.helper.try_builtin(&name, &args)? {
                        stack.push(v);
                        return Ok(());
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
                    // tier-2: after HOT_CALLS invocations on all-int
                    // inputs, JIT the bytecode to native and call it.
                    chosen.calls.set(chosen.calls.get() + 1);
                    if chosen.calls.get() >= 64 && !chosen.jit_tried.get() {
                        chosen.jit_tried.set(true);
                        if self.jit_engine.is_none() {
                            self.jit_engine = crate::jit::JitEngine::new();
                        }
                        if let Some(e) = self.jit_engine.as_mut() {
                            *chosen.jit.borrow_mut() =
                                e.compile_fn(&name, &self.fns);
                        }
                    }
                    if let Some(j) = chosen.jit.borrow().as_ref() {
                        if args.len() <= 4
                            && args.iter().all(|a|
                                matches!(a, Value::Int(_) | Value::Float(_))) {
                            let p = j.code_ptr;
                            if j.is_float {
                                let af: Vec<f64> = args.iter().map(|a| match a {
                                    Value::Int(n) => *n as f64,
                                    Value::Float(f) => *f,
                                    _ => 0.0,
                                }).collect();
                                let r = unsafe {
                                    match af.as_slice() {
                                        [] => std::mem::transmute::<_, crate::jit::NativeF0>(p)(),
                                        [a] => std::mem::transmute::<_, crate::jit::NativeF1>(p)(*a),
                                        [a,b] => std::mem::transmute::<_, crate::jit::NativeF2>(p)(*a,*b),
                                        [a,b,c] => std::mem::transmute::<_, crate::jit::NativeF3>(p)(*a,*b,*c),
                                        [a,b,c,d] => std::mem::transmute::<_, crate::jit::NativeF4>(p)(*a,*b,*c,*d),
                                        _ => -1.0,
                                    }
                                };
                                stack.push(Value::Float(r));
                            } else if args.iter()
                                .all(|a| matches!(a, Value::Int(_))) {
                                let av: Vec<i64> = args.iter().map(|a|
                                    if let Value::Int(n) = a { *n } else { 0 }
                                ).collect();
                                let r = unsafe {
                                    match av.as_slice() {
                                        [] => std::mem::transmute::<_, crate::jit::NativeFn0>(p)(),
                                        [a] => std::mem::transmute::<_, crate::jit::NativeFn1>(p)(*a),
                                        [a,b] => std::mem::transmute::<_, crate::jit::NativeFn2>(p)(*a,*b),
                                        [a,b,c] => std::mem::transmute::<_, crate::jit::NativeFn3>(p)(*a,*b,*c),
                                        [a,b,c,d] => std::mem::transmute::<_, crate::jit::NativeFn4>(p)(*a,*b,*c,*d),
                                        _ => -1,
                                    }
                                };
                                stack.push(Value::Int(r));
                            } else {
                                // float jit got non-numeric... falls through
                                // (args checked above so unreachable)
                                stack.push(Value::Null);
                            }
                            return Ok(());
                        }
                    }
                    let env: Vec<Option<Value>> =
                        args.into_iter().map(Some).collect();
                    frames.push(Frame {
                        code: chosen.code.clone(),   // Rc bump — no alloc
                        pc: 0, env, base: stack.len(), iters: Vec::new(),
                        try_stack: Vec::new(),
                    });
                    *code = frames.last().unwrap().code.clone();
                }
                Instr::CallId(ni, vid, argc) => {
                    let args: Vec<Value> =
                        stack.drain(stack.len() - argc as usize..).collect();
                    let name = code.names[ni as usize].clone();
                    let variants = self.fns.get(&name)
                        .ok_or_else(|| format!("Undefined function: {}", name))?;
                    let chosen = variants.iter().find(|v| v.id == vid)
                        .ok_or_else(|| format!("Variant {}#{} not found", name, vid))?;
                    if args.len() != chosen.params.len() {
                        return Err(format!("{}#{} expects {} args",
                            name, vid, chosen.params.len()));
                    }
                    let env: Vec<Option<Value>> =
                        args.into_iter().map(Some).collect();
                    frames.push(Frame {
                        code: chosen.code.clone(),
                        pc: 0, env, base: stack.len(), iters: Vec::new(),
                        try_stack: Vec::new(),
                    });
                    *code = frames.last().unwrap().code.clone();
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
                Instr::GetField(ni) => {
                    let obj = stack.pop().unwrap();
                    let name = &code.names[ni as usize];
                    let v = match &obj {
                        Value::Struct(_, f) => f.get(name).cloned()
                            .unwrap_or(Value::Null),
                        Value::Dict(d) => d.get(name).cloned()
                            .unwrap_or(Value::Null),
                        _ => return Err(format!(
                            "field access '{}' on {}", name, obj_tag(&obj))),
                    };
                    stack.push(v);
                }
                Instr::SliceGet => {
                    let hi = stack.pop().unwrap();
                    let lo = stack.pop().unwrap();
                    let obj = stack.pop().unwrap();
                    stack.push(slice_get(&obj, &lo, &hi)?);
                }
                Instr::ListPush(sl) => {
                    let v = stack.pop().unwrap();
                    let frame = frames.last_mut().unwrap();
                    match frame.env.get_mut(sl as usize)
                        .and_then(|o| o.as_mut()) {
                        Some(Value::List(l)) => l.push_back(v),
                        _ => return Err("list-push on non-list slot".into()),
                    }
                }
                Instr::FieldSetVar(sl, ni) => {
                    let v = stack.pop().unwrap();
                    let name = code.names[ni as usize].clone();
                    let frame = frames.last_mut().unwrap();
                    match frame.env.get_mut(sl as usize)
                        .and_then(|o| o.as_mut()) {
                        Some(Value::Struct(_, f)) => { f.insert(name, v); }
                        _ => return Err("field-assign on non-struct".into()),
                    }
                }
                Instr::MakeStruct(ni, n) => {
                    let mut fields = HashMap::new();
                    for _ in 0..n {
                        let v = stack.pop().unwrap();
                        let k = stack.pop().unwrap();
                        if let Value::Str(fname) = k {
                            fields.insert((*fname).to_string(), v);
                        }
                    }
                    let sname = code.names[ni as usize].clone();
                    stack.push(Value::Struct(sname, fields));
                }
                Instr::AppendVar(sl) => {
                    let v = stack.pop().unwrap();
                    let frame = frames.last_mut().unwrap();
                    match frame.env.get_mut(sl as usize)
                        .and_then(|o| o.as_mut()) {
                        Some(Value::List(l)) => l.push_back(v),
                        _ => return Err("append on non-list".into()),
                    }
                }
                Instr::InsertVar(sl, ki) => {
                    let v = stack.pop().unwrap();
                    let k = code.names[ki as usize].clone();
                    let frame = frames.last_mut().unwrap();
                    match frame.env.get_mut(sl as usize)
                        .and_then(|o| o.as_mut()) {
                        Some(Value::Dict(d)) => { d.insert(k, v); }
                        Some(Value::Set(st)) => { st.push(v); }
                        _ => return Err("insert on non-dict/set".into()),
                    }
                }
                Instr::InsertDynVar(sl) => {
                    let v = stack.pop().unwrap();
                    let k = stack.pop().unwrap();
                    let key = match &k {
                        Value::Str(s) => (**s).to_string(),
                        _ => return Err("dict key must be string".into()),
                    };
                    let frame = frames.last_mut().unwrap();
                    match frame.env.get_mut(sl as usize)
                        .and_then(|o| o.as_mut()) {
                        Some(Value::Dict(d)) => { d.insert(key, v); }
                        Some(Value::Set(st)) => { st.push(v); }
                        _ => return Err("insert on non-dict/set".into()),
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
                        // program end: export main-frame slots to globals
                        // by name so callers/tests can inspect results
                        for (i, val) in f.env.iter().enumerate() {
                            if let (Some(vv), Some(n)) =
                                (val, f.code.slot_names.get(i)) {
                                self.globals.insert(n.clone(), vv.clone());
                            }
                        }
                        return Ok(());
                    }
                    *code = frames.last().unwrap().code.clone();
                    stack.push(v);
                }
                Instr::Halt => {
                    if frames.len() == 1 {
                        let f = frames.pop().unwrap();
                        for (i, val) in f.env.iter().enumerate() {
                            if let (Some(vv), Some(n)) =
                                (val, f.code.slot_names.get(i)) {
                                self.globals.insert(n.clone(), vv.clone());
                            }
                        }
                        return Ok(());
                    }
                    let f = frames.pop().unwrap();
                    stack.truncate(f.base);
                    *code = frames.last().unwrap().code.clone();
                    stack.push(Value::Null);
                }
                Instr::TryPush(catch_pc, errsl) => {
                    frames.last_mut().unwrap()
                        .try_stack.push((catch_pc, errsl));
                }
                Instr::TryPop => {
                    frames.last_mut().unwrap().try_stack.pop();
                }
            }
        }
        Ok(())
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

fn slice_get(obj: &Value, lo: &Value, hi: &Value) -> Result<Value, String> {
    match obj {
        Value::List(l) => {
            let len = l.len() as i64;
            let a = match lo {
                Value::Null => 0,
                Value::Int(i) => if *i < 0 { len + i } else { *i },
                _ => return Err("slice bound must be int".into()),
            }.clamp(0, len);
            let b = match hi {
                Value::Null => len,
                Value::Int(i) => if *i < 0 { len + i } else { *i },
                _ => return Err("slice bound must be int".into()),
            }.clamp(0, len);
            Ok(Value::List(
                l.iter().skip(a as usize).take((b - a).max(0) as usize)
                    .cloned().collect()))
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as i64;
            let a = match lo {
                Value::Null => 0,
                Value::Int(i) => if *i < 0 { len + i } else { *i },
                _ => return Err("slice bound must be int".into()),
            }.clamp(0, len);
            let b = match hi {
                Value::Null => len,
                Value::Int(i) => if *i < 0 { len + i } else { *i },
                _ => return Err("slice bound must be int".into()),
            }.clamp(0, len);
            Ok(Value::Str(
                chars[a as usize..b.max(a) as usize]
                    .iter().collect::<String>().into()))
        }
        _ => Err("slice requires list or string".into()),
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
