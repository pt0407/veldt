// Interpreter for the Veldt language — tree-walking

use crate::ast::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::cell::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[repr(C, u8)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(Arc<str>), // interned — O(1) clone (Phase 3)
    Bool(bool),
    List(im::Vector<Value>), // persistent — O(1) clone (Phase 3)
    Dict(im::HashMap<String, Value>), // persistent — O(1) clone (Phase 6)
    Set(Vec<Value>),
    Struct(String, HashMap<String, Value>),
    Closure(Vec<Param>, Vec<Stmt>, #[serde(skip)] Rc<RefCell<HashMap<String, Value>>>),
    Null,
}

/// Read the char tile at (x, y) from a canvas's cells list.
fn r_tile(rows: &im::Vector<Value>, x: usize, y: usize) -> Option<Value> {
    rows.get(y).and_then(|r| if let Value::List(r) = r { r.get(x).cloned() } else { None })
}

/// Extract a numeric arg (Int or Float) as f64 — used by game-math builtins.
fn num_at(args: &[Value], i: usize) -> Result<f64, String> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(f)) => Ok(*f),
        _ => Err(format!("arg {} expects a number", i)),
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => {
                if n.fract() == 0.0 { write!(f, "{:.1}", n) } else { write!(f, "{}", n) }
            }
            Value::Str(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Null => write!(f, "null"),
            Value::Set(items) => {
                let strs: Vec<String> = items.iter().map(|v| format!("{}", v)).collect();
                write!(f, "set[{}]", strs.join(", "))
            }
            Value::List(items) => {
                let strs: Vec<String> = items.iter().map(|v| format!("{}", v)).collect();
                write!(f, "[{}]", strs.join(", "))
            }
            Value::Dict(entries) => {
                let strs: Vec<String> = entries.iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                write!(f, "{{{}}}", strs.join(", "))
            }
            Value::Struct(name, fields) => {
                let strs: Vec<String> = fields.iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                write!(f, "{} {{ {} }}", name, strs.join(", "))
            }
            Value::Closure(_, _, _) => write!(f, "<closure>"),
        }
    }
}

#[derive(Clone)]
pub struct FunctionDef {
    pub params: Rc<Vec<Param>>, // Rc — per-call clone was cloning every param
    pub body: Rc<Vec<Stmt>>, // Rc — per-call clone was cloning the whole AST
}

pub struct Interpreter {
    pub vars: Rc<RefCell<HashMap<String, Value>>>,
    pub functions: HashMap<String, Vec<FunctionVariant>>,
    pub structs: HashMap<String, Vec<(String, Type)>>,
    // track which veldt functions were called during this run
    pub usage_log: Vec<(String, u32)>, // (name, variant_id)
    // instruction counter — prevents infinite loops in mutants/trials
    pub step_count: u64,
    pub step_limit: u64,
    // print capture — when set, print goes here instead of stdout
    pub print_buffer: Option<String>,
    // RNG state for rand()
    rng_state: u64,
    // Current source line for error reporting
    pub current_line: usize,
    pub trace: bool,
    // SQLite connections (id -> connection)
    db_connections: HashMap<i64, rusqlite::Connection>,
    next_db_id: i64,
    // spawned threads: task id -> join handle returning Result<json, err>
    tasks: HashMap<i64, std::thread::JoinHandle<Result<String, String>>>,
    next_task_id: i64,
    // call traces: fn name -> (serialized args, serialized result)
    pub traces: HashMap<String, Vec<(Vec<String>, String)>>,
    pub trace_calls: bool,
    pub argv: Vec<String>,
    pub call_stack: Vec<(String, usize)>,
    pub importing: Vec<String>,
    // std:sandbox — capability set; denied builtins error before dispatch
    pub caps: Caps,
    // 2D game engine — when the program runs inside a game session
    // (/api/game/start), this is the shared input queue + frame buffer.
    // None = CLI mode — game_frame prints a JSON frame line instead.
    pub game_io: Option<std::sync::Arc<GameIo>>,
}

/// Shared state for a live game session: browser key events in,
/// latest scene JSON out, and a running flag the loop polls.
pub struct GameIo {
    /// keys the player currently holds (browser POSTs the held-set each frame)
    pub keys: std::sync::Mutex<Vec<String>>,
    /// latest emitted game_frame scene (serialized JSON) — /api/game/frame reads it
    pub frame: std::sync::Mutex<Option<String>>,
    /// pending sound events — sfx() pushes, game_frame drains into the scene
    pub sfx: std::sync::Mutex<Vec<String>>,
    /// session alive — game_running() polls this; /api/game/stop clears it
    pub running: std::sync::atomic::AtomicBool,
}

impl GameIo {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(GameIo {
            keys: std::sync::Mutex::new(Vec::new()),
            frame: std::sync::Mutex::new(None),
            sfx: std::sync::Mutex::new(Vec::new()),
            running: std::sync::atomic::AtomicBool::new(true),
        })
    }
}

#[derive(Clone)]
pub struct FunctionVariant {
    pub id: u32,
    pub def: FunctionDef,
    pub fitness: f64,
}

pub enum FlowControl {
    Normal,
    Return(Option<Value>),
}

/// std:sandbox — capability set for the interpreter. Untrusted code
/// (LLM mutants, immigrants, trials) runs with side-effects denied.
#[derive(Clone, Debug)]
pub struct Caps {
    pub fs_read: bool,
    pub fs_write: bool,
    pub net: bool,
    pub spawn: bool,
    pub env: bool,
    pub exit: bool,
}

impl Caps {
    pub fn allow_all() -> Self {
        Caps { fs_read: true, fs_write: true, net: true, spawn: true, env: true, exit: true }
    }
    /// Stage 0: everything off — pure compute only.
    pub fn sandboxed() -> Self {
        Caps { fs_read: false, fs_write: false, net: false, spawn: false, env: false, exit: false }
    }
    /// Is this builtin blocked under the current caps?
    pub fn denied(&self, name: &str) -> bool {
        match name {
            "read_file" | "read_dir" | "exists" | "ls" => !self.fs_read,
            "write_file" | "append_file" | "mkdir" | "rm" | "mv" | "cp" => !self.fs_write,
            "fetch" | "post" | "serve" | "import" => !self.net,
            "spawn" | "join" | "pmap" | "task" => !self.spawn,
            "env" | "args" | "read_stdin" | "input" | "readline" => !self.env,
            "exit" | "exec" | "system" | "shell" => !self.exit,
            _ => false,
        }
    }
}

impl Default for Caps {
    fn default() -> Self { Caps::allow_all() }
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            vars: Rc::new(RefCell::new(HashMap::new())),
            functions: HashMap::new(),
            structs: HashMap::new(),
            usage_log: Vec::new(),
            step_count: 0,
            step_limit: 100_000, // safety limit
            print_buffer: None,
            rng_state: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x1234567890ABCDEF),
            current_line: 0,
            trace: false,
            db_connections: HashMap::new(),
            next_db_id: 1,
            tasks: HashMap::new(),
            next_task_id: 0,
            traces: HashMap::new(),
            trace_calls: false,
            argv: Vec::new(),
            call_stack: Vec::new(),
            importing: Vec::new(),
            caps: Caps::allow_all(),
            game_io: None,
        }
    }

    pub fn with_step_limit(limit: u64) -> Self {
        let mut interp = Self::new();
        interp.step_limit = limit;
        interp
    }

    fn tick(&mut self) -> Result<(), String> {
        self.step_count += 1;
        if self.step_count > self.step_limit {
            return Err("Execution limit exceeded (possible infinite loop)".into());
        }
        Ok(())
    }

    pub fn run(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for stmt in stmts {
            match self.exec_stmt(stmt)? {
                FlowControl::Return(_) => break,
                FlowControl::Normal => {}
            }
        }
        Ok(())
    }

    pub fn run_with_lines(&mut self, stmts: &[Stmt], lines: &[usize]) -> Result<(), String> {
        for (i, stmt) in stmts.iter().enumerate() {
            if let Some(&line) = lines.get(i) {
                self.current_line = line;
            }
            match self.exec_stmt(stmt)? {
                FlowControl::Return(_) => break,
                FlowControl::Normal => {}
            }
        }
        Ok(())
    }

    pub fn exec_stmt_pub(&mut self, stmt: &Stmt) -> Result<FlowControl, String> {
        self.exec_stmt(stmt)
    }

    /// Line-emit honoring print_buffer — the VM's PrintOut routes here so
    /// /api/run (and any capture path) works under --vm too.
    pub fn emit_line(&mut self, line: &str) {
        if let Some(ref mut buf) = self.print_buffer {
            buf.push_str(line);
        } else {
            print!("{}", line);
        }
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<FlowControl, String> {
        self.tick()?;
        match stmt {
            Stmt::Let(name, expr) => {
                let val = self.eval_expr(expr)?;
                self.vars.borrow_mut().insert(name.clone(), val);
                Ok(FlowControl::Normal)
            }
            Stmt::Assign(name, expr) => {
                let val = self.eval_expr(expr)?;
                if self.vars.borrow().contains_key(name) {
                    self.vars.borrow_mut().insert(name.clone(), val);
                    Ok(FlowControl::Normal)
                } else {
                    Err(format!("Cannot assign to undefined variable: {}", name))
                }
            }
            Stmt::FieldAssign(obj_expr, field, val_expr) => {
                let obj_val = self.eval_expr(obj_expr)?;
                let new_val = self.eval_expr(val_expr)?;
                match obj_val {
                    Value::Struct(name, mut fields) => {
                        if !fields.contains_key(field.as_str()) {
                            return Err(format!("No field '{}' on struct {}", field, name));
                        }
                        fields.insert(field.clone(), new_val);
                        // Write back to the variable holding this struct
                        // This is tricky — we need to find where the struct lives
                        // For now, we only support field assignment on variables
                        if let Expr::Var(var_name) = obj_expr {
                            self.vars.borrow_mut().insert(var_name.clone(), Value::Struct(name, fields));
                        } else {
                            return Err("Can only assign fields on a variable".into());
                        }
                        Ok(FlowControl::Normal)
                    }
                    _ => Err("Cannot assign field on non-struct".into()),
                }
            }
            Stmt::IndexAssign(obj_expr, index_expr, val_expr) => {
                let obj_val = self.eval_expr(obj_expr)?;
                let index = self.eval_expr(index_expr)?;
                let new_val = self.eval_expr(val_expr)?;
                match obj_val {
                    Value::List(mut items) => {
                        match index {
                            Value::Int(i) => {
                                if i < 0 || i as usize >= items.len() {
                                    return Err(format!("Index {} out of bounds", i));
                                }
                                items[i as usize] = new_val;
                                if let Expr::Var(var_name) = obj_expr {
                                    self.vars.borrow_mut().insert(var_name.clone(), Value::List(items));
                                } else {
                                    return Err("Can only assign index on a variable".into());
                                }
                                Ok(FlowControl::Normal)
                            }
                            _ => Err("Index must be an integer".into()),
                        }
                    }
                    Value::Dict(mut map) => {
                        let key = match index {
                            Value::Str(k) => k.to_string(),
                            _ => return Err("Dict key must be a string".into()),
                        };
                        map.insert(key, new_val);
                        if let Expr::Var(var_name) = obj_expr {
                            self.vars.borrow_mut().insert(var_name.clone(), Value::Dict(map));
                        } else {
                            return Err("Can only assign index on a variable".into());
                        }
                        Ok(FlowControl::Normal)
                    }
                    _ => Err("Cannot index-assign on non-list/dict".into()),
                }
            }
            Stmt::FnDef(name, params, body) => {
                let id = self.functions.get(name)
                    .map(|v| v.len() as u32 + 1)
                    .unwrap_or(1);
                let variant = FunctionVariant {
                    id,
                    fitness: 1.0,
                    def: FunctionDef {
                        params: Rc::new(params.clone()),
                        body: Rc::new(body.clone()),
                    },
                };
                self.functions.entry(name.clone())
                    .or_insert_with(Vec::new)
                    .push(variant);
                Ok(FlowControl::Normal)
            }
            Stmt::StructDef(name, fields) => {
                self.structs.insert(name.clone(), fields.clone());
                Ok(FlowControl::Normal)
            }
            Stmt::If(cond, then_block, else_block) => {
                let c = self.eval_expr(cond)?;
                if self.is_truthy(&c) {
                    self.exec_block(then_block)
                } else if let Some(eb) = else_block {
                    self.exec_block(eb)
                } else {
                    Ok(FlowControl::Normal)
                }
            }
            Stmt::Break => Err("__veldt_break__".into()),
            Stmt::Continue => Err("__veldt_continue__".into()),
            Stmt::While(cond, body) => {
                loop {
                    let c = self.eval_expr(cond)?;
                    if !self.is_truthy(&c) { break; }
                    match self.exec_block(body) {
                        Err(e) if e == "__veldt_break__" => break,
                        Err(e) if e == "__veldt_continue__" => continue,
                        Err(e) => return Err(e),
                        Ok(FlowControl::Return(v)) => return Ok(FlowControl::Return(v)),
                        _ => {}
                    }
                }
                Ok(FlowControl::Normal)
            }
            Stmt::For(var, iter_expr, body) => {
                let iter_val = self.eval_expr(iter_expr)?;
                match iter_val {
                    Value::List(items) => {
                        for item in items {
                            self.vars.borrow_mut().insert(var.clone(), item);
                            match self.exec_block(body) {
                                Err(e) if e == "__veldt_break__" => break,
                                Err(e) if e == "__veldt_continue__" => continue,
                                Err(e) => return Err(e),
                                Ok(FlowControl::Return(v)) => return Ok(FlowControl::Return(v)),
                                _ => {}
                            }
                        }
                        Ok(FlowControl::Normal)
                    }
                    _ => Err("For loop expects a list to iterate".into()),
                }
            }
            Stmt::Return(expr) => {
                let val = match expr {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Null,
                };
                Ok(FlowControl::Return(Some(val)))
            }
            Stmt::Print(expr) => {
                let val = self.eval_expr(expr)?;
                let line = format!("{}\n", val);
                if let Some(ref mut buf) = self.print_buffer {
                    buf.push_str(&line);
                } else {
                    print!("{}", line);
                }
                Ok(FlowControl::Normal)
            }
            Stmt::ExprStmt(expr) => {
                self.eval_expr(expr)?;
                Ok(FlowControl::Normal)
            }
            Stmt::Grow(item) => {
                // Register grown functions in the interpreter immediately
                // so they can be called in the same run.
                // The caller also collects them for garden planting.
                if let GrowItem::Fn(name, params, body, _tests) = item {
                    let def = FunctionDef {
                        params: Rc::new(params.clone()),
                        body: Rc::new(body.clone()),
                    };
                    let variant = FunctionVariant { id: 1, fitness: 1.0, def };
                    self.functions.entry(name.clone()).or_default().push(variant);
                }
                Ok(FlowControl::Normal)
            }
            Stmt::Try(body, err_var, handler) => {
                match self.exec_block(body) {
                    Ok(flow) => Ok(flow),
                    Err(e) => {
                        self.vars.borrow_mut().insert(err_var.clone(), Value::Str(e.into()));
                        self.exec_block(handler)
                    }
                }
            }
            Stmt::Import(path, alias) => {
                let content = resolve_import_source(&path)?;
                if self.importing.iter().any(|p| p == path) {
                    return Err(format!("Circular import: {}", path));
                }
                let mut lexer = crate::lexer::Lexer::new(&content);
                let tokens = lexer.tokenize()?;
                let lines = lexer.token_lines.clone();
                let mut parser = crate::parser::Parser::new_with_lines(tokens, lines);
                let program = parser.parse_program()?;
                match alias {
                    None => {
                        self.importing.push(path.clone());
                        let r = self.exec_block(&program);
                        self.importing.pop();
                        r
                    }
                    Some(alias) => {
                        // Namespaced import: exec in a fresh interpreter,
                        // then register its defs under `alias::name`.
                        let mut sub = Interpreter::new();
                        sub.argv = self.argv.clone();
                        sub.importing = self.importing.clone();
                        sub.importing.push(path.clone());
                        sub.exec_block(&program)
                            .map_err(|e| format!("Import '{}' failed: {}", path, e))?;
                        for (name, defs) in sub.functions {
                            let mangled = format!("{}::{}", alias, name);
                            // v1: also expose bare name so module-internal
                            // calls resolve — collision = first importer wins
                            self.functions.entry(name).or_insert_with(|| defs.clone());
                            self.functions.insert(mangled, defs);
                        }
                        for (n, v) in sub.vars.borrow().iter() {
                            self.vars.borrow_mut()
                                .insert(format!("{}::{}", alias, n), v.clone());
                        }
                        let mut marker = HashMap::new();
                        marker.insert("__mod".to_string(), Value::Str(alias.clone().into()));
                        self.vars.borrow_mut().insert(alias.clone(), Value::Dict(marker.into_iter().collect()));
                        Ok(FlowControl::Normal)
                    }
                }
            }
            Stmt::LetDestructure(names, expr) => {
                let val = self.eval_expr(expr)?;
                match val {
                    Value::List(items) => {
                        for (i, n) in names.iter().enumerate() {
                            let v = items.get(i).cloned().unwrap_or(Value::Null);
                            self.vars.borrow_mut().insert(n.clone(), v);
                        }
                        Ok(FlowControl::Normal)
                    }
                    _ => Err("Cannot destructure non-list value".into()),
                }
            }
            Stmt::Match(scrutinee, arms, else_arm) => {
                let val = self.eval_expr(scrutinee)?;
                for (pat, body) in arms {
                    let pat_val = self.eval_expr(pat)?;
                    if self.values_eq(&val, &pat_val) {
                        return self.exec_block(body);
                    }
                }
                if let Some(body) = else_arm {
                    return self.exec_block(body);
                }
                Ok(FlowControl::Normal)
            }
        }
    }

    fn exec_block(&mut self, stmts: &[Stmt]) -> Result<FlowControl, String> {
        for stmt in stmts {
            match self.exec_stmt(stmt)? {
                FlowControl::Return(v) => return Ok(FlowControl::Return(v)),
                FlowControl::Normal => {}
            }
        }
        Ok(FlowControl::Normal)
    }

    pub(crate) fn is_truthy(&self, val: &Value) -> bool {
        match val {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Null => false,
            Value::List(items) => !items.is_empty(),
            Value::Dict(entries) => !entries.is_empty(),
            Value::Set(items) => !items.is_empty(),
            Value::Closure(_, _, _) => true,
            Value::Struct(_, fields) => !fields.is_empty(),
        }
    }

    /// Public wrapper so the VM can delegate method calls through the
    /// one canonical eval (keeps method/fast-path semantics shared).
    pub(crate) fn eval_expr_pub(&mut self, expr: &Expr) -> Result<Value, String> {
        self.eval_expr(expr)
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(n) => Ok(Value::Float(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone().into())),
            Expr::Interp(parts) => {
                let mut result = String::new();
                for part in parts {
                    let val = self.eval_expr(part)?;
                    match &val {
                        Value::Str(s) => result.push_str(s),
                        Value::Float(n) => {
                            if n.fract() == 0.0 { result.push_str(&format!("{:.1}", n)) }
                            else { result.push_str(&format!("{}", n)) }
                        }
                        _ => result.push_str(&format!("{}", val)),
                    }
                }
                Ok(Value::Str(result.into()))
            }
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::List(items) => {
                let mut vals = Vec::new();
                for item in items {
                    vals.push(self.eval_expr(item)?);
                }
                Ok(Value::List(vals.into_iter().collect()))
            }
            Expr::Var(name) => {
                self.vars.borrow().get(name).cloned()
                    .ok_or_else(|| format!("Undefined variable: {}", name))
            }
            Expr::BinOp(left, op, right) => {
                // Lazy: a ?? b — don't evaluate b if a is non-null
                if *op == BinOp::NullCoal {
                    let l = self.eval_expr(left)?;
                    return if matches!(l, Value::Null) { self.eval_expr(right) } else { Ok(l) };
                }
                let l = self.eval_expr(left)?;
                let r = self.eval_expr(right)?;
                let v = self.eval_binop(&l, op, &r)?;
                // Phase 3: alloc-weighed step accounting — big results cost
                // extra steps, making the step limit crude-but-fair about
                // memory churn, not just op count.
                match &v {
                    Value::Str(s) => self.step_count += (s.len() / 16) as u64,
                    Value::List(xs) => self.step_count += (xs.len() / 4) as u64,
                    Value::Dict(d) => self.step_count += d.len() as u64 / 4,
                    _ => {}
                }
                Ok(v)
            }
            Expr::SafeGet(obj, name, args) => {
                let o = self.eval_expr(obj)?;
                match o {
                    Value::Null => Ok(Value::Null),
                    _ => match args {
                        Some(a) => {
                            let mut av = Vec::new();
                            for e in a { av.push(self.eval_expr(e)?); }
                            self.eval_method_call(&o, name, av)
                        }
                        None => match o {
                            Value::Struct(_, fields) => Ok(fields.get(name).cloned().unwrap_or(Value::Null)),
                            Value::Dict(d) => Ok(d.get(name).cloned().unwrap_or(Value::Null)),
                            _ => Err(format!("Cannot access '{}' on this value", name)),
                        }
                    }
                }
            }
            Expr::UnaryOp(op, expr) => {
                let v = self.eval_expr(expr)?;
                match op {
                    UnaryOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => Err("Cannot negate non-number".into()),
                    },
                    UnaryOp::Not => match v {
                        Value::Bool(b) => Ok(Value::Bool(!b)),
                        _ => Err("Cannot 'not' non-boolean".into()),
                    },
                }
            }
            Expr::Call(name, variant, args) => {
                let mut arg_vals = Vec::new();
                for a in args {
                    arg_vals.push(self.eval_expr(a)?);
                }
                // First check if it's a variable holding a closure
                if variant.is_none() {
                    let closure_binding = self.vars.borrow().get(name).cloned();
                if let Some(Value::Closure(params, body, captured)) = closure_binding {
                        return self.call_closure(params.clone(), body.clone(), captured.clone(), arg_vals);
                    }
                }
                self.call_function(name, *variant, arg_vals)
            }
            Expr::MethodCall(obj, method, args) => {
                // Mutating ops on a variable receiver — O(1) in place, no
                // whole-container clone. xs.append(v) is the fast path vs
                // xs = xs.push(v).
                if let Expr::Var(name) = &**obj {
                    // Borrow-read fast paths: read via vars.borrow() instead
                    // of cloning the whole container for the call.
                    // On any miss we fall through to the normal eval below.
                    if let "get" | "has" = method.as_str() {
                        if let Some(key) = args.get(0)
                            .map(|e| self.eval_expr(e)).transpose()? {
                            let vars = self.vars.borrow();
                            let hit: Option<Result<Value, String>> = match vars.get(name) {
                                Some(Value::Dict(map)) => Some(match &key {
                                    Value::Str(k) => Ok(if method == "get" {
                                        map.get(&**k).cloned().unwrap_or(Value::Null)
                                    } else {
                                        Value::Bool(map.contains_key(&**k))
                                    }),
                                    _ => Err("dict key must be a string".into()),
                                }),
                                Some(Value::List(items)) => Some(match &key {
                                    Value::Int(i) => {
                                        let idx = if *i < 0 { items.len() as i64 + i } else { *i };
                                        Ok(if method == "get" {
                                            items.get(idx as usize).cloned().unwrap_or(Value::Null)
                                        } else {
                                            Value::Bool(idx >= 0 && (idx as usize) < items.len())
                                        })
                                    }
                                    _ => Err("list index must be an int".into()),
                                }),
                                Some(Value::Set(items)) => Some(Ok(
                                    Value::Bool(items.iter().any(|x| self.values_eq(x, &key))))),
                                _ => None,
                            };
                            if let Some(r) = hit { return r; }
                            drop(vars);
                        }
                    }
                    if let "len" | "keys" | "values" | "is_empty" = method.as_str() {
                        let vars = self.vars.borrow();
                        let hit: Option<Value> = match (vars.get(name), method.as_str()) {
                            (Some(Value::Dict(m)), "len") => Some(Value::Int(m.len() as i64)),
                            (Some(Value::List(l)), "len") => Some(Value::Int(l.len() as i64)),
                            (Some(Value::Set(l)), "len") => Some(Value::Int(l.len() as i64)),
                            (Some(Value::Str(s)), "len") => Some(Value::Int(s.chars().count() as i64)),
                            (Some(Value::Dict(m)), "keys") =>
                                Some(Value::List(m.keys().map(|k| Value::Str(k.clone().into())).collect())),
                            (Some(Value::Dict(m)), "values") =>
                                Some(Value::List(m.values().cloned().collect())),
                            (Some(Value::Dict(m)), "is_empty") => Some(Value::Bool(m.is_empty())),
                            (Some(Value::List(l)), "is_empty") => Some(Value::Bool(l.is_empty())),
                            _ => None,
                        };
                        if let Some(v) = hit { return Ok(v); }
                        drop(vars);
                    }
                    match method.as_str() {
                        "append" => {
                            let v = args.get(0)
                                .map(|e| self.eval_expr(e)).transpose()?
                                .ok_or("append() expects 1 argument")?;
                            let mut vars = self.vars.borrow_mut();
                            return match vars.get_mut(name) {
                                Some(Value::List(items)) => {
                                    items.push_back(v); Ok(Value::Null)
                                }
                                Some(_) => Err("append() needs a list variable".into()),
                                None => Err(format!("Undefined variable: {}", name)),
                            };
                        }
                        "insert" => {
                            if args.is_empty() { return Err("insert() needs args".into()); }
                            let first = self.eval_expr(&args[0])?;
                            let second = args.get(1)
                                .map(|e| self.eval_expr(e)).transpose()?;
                            let mut vars = self.vars.borrow_mut();
                            return match vars.get_mut(name) {
                                Some(Value::Dict(map)) => {
                                    match (first, second) {
                                        (Value::Str(k), Some(v)) => { map.insert(k.to_string(), v); Ok(Value::Null) }
                                        _ => Err("dict.insert(key, value) needs a string key + value".into()),
                                    }
                                }
                                Some(Value::Set(set)) => {
                                    if !set.iter().any(|x| self.values_eq(x, &first)) {
                                        set.push(first);
                                    }
                                    Ok(Value::Null)
                                }
                                Some(_) => Err("insert() needs a dict or set variable".into()),
                                None => Err(format!("Undefined variable: {}", name)),
                            };
                        }
                        "pop_in" => {
                            let mut vars = self.vars.borrow_mut();
                            return match vars.get_mut(name) {
                                Some(Value::List(items)) => {
                                    Ok(items.pop_back().unwrap_or(Value::Null))
                                }
                                Some(_) => Err("pop_in() needs a list variable".into()),
                                None => Err(format!("Undefined variable: {}", name)),
                            };
                        }
                        _ => {}
                    }
                }
                let obj_val = self.eval_expr(obj)?;
                let mut arg_vals = Vec::new();
                for a in args {
                    arg_vals.push(self.eval_expr(a)?);
                }
                self.eval_method_call(&obj_val, method, arg_vals)
            }
            Expr::FieldAccess(obj, field) => {
                let obj_val = self.eval_expr(obj)?;
                match obj_val {
                    // Module member: `lib.CONST` -> vars["lib::CONST"]
                    Value::Dict(ref d) if d.contains_key("__mod") => {
                        let m = match d.get("__mod") {
                            Some(Value::Str(s)) => s.clone(),
                            _ => unreachable!(),
                        };
                        self.vars.borrow()
                            .get(&format!("{}::{}", m, field))
                            .cloned()
                            .ok_or_else(|| format!("Module {} has no member '{}'", m, field))
                    }
                    Value::Struct(_, fields) => {
                        fields.get(field)
                            .cloned()
                            .ok_or_else(|| format!("No field '{}' on struct", field))
                    }
                    _ => Err(format!("Cannot access field '{}' on non-struct", field)),
                }
            }
            Expr::StructLit(name, fields) => {
                let mut field_map = HashMap::new();
                for (fname, fexpr) in fields {
                    let val = self.eval_expr(fexpr)?;
                    field_map.insert(fname.clone(), val);
                }
                Ok(Value::Struct(name.clone(), field_map))
            }
            Expr::Lambda(params, body) => {
                // Snapshot the env at creation — each closure owns its
                // own view so later calls to the SAME outer fn don't
                // rewrite earlier closures' captured params.
                Ok(Value::Closure(params.clone(), body.clone(),
                    Rc::new(RefCell::new(self.vars.borrow().clone()))))
            }
            Expr::ListComp(expr, var, iter, cond) => {
                let iter_val = self.eval_expr(iter)?;
                let items = match iter_val {
                    Value::List(items) => items,
                    Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string().into())).collect(),
                    _ => return Err("Comprehension requires a list or string to iterate".into()),
                };
                let mut result = Vec::new();
                let old_vars = self.vars.clone();
                for item in items {
                    self.vars.borrow_mut().insert(var.clone(), item);
                    let include = match cond {
                        Some(c) => {
                            let cv = self.eval_expr(c)?;
                            self.is_truthy(&cv)
                        }
                        None => true,
                    };
                    if include {
                        result.push(self.eval_expr(expr)?);
                    }
                }
                self.vars = old_vars;
                Ok(Value::List(result.into_iter().collect()))
            }
            Expr::Slice(obj, lo, hi) => {
                let obj_val = self.eval_expr(obj)?;
                let lo_v = match lo { Some(e) => Some(self.eval_expr(e)?), None => None };
                let hi_v = match hi { Some(e) => Some(self.eval_expr(e)?), None => None };
                // resolve an optional bound: negative counts back from len
                fn bound(v: &Option<Value>, len: i64, default: i64) -> Result<i64, String> {
                    match v {
                        None | Some(Value::Null) => Ok(default),
                        Some(Value::Int(i)) => Ok(if *i < 0 { len + i } else { *i }),
                        _ => Err("slice bounds must be integers".into()),
                    }
                }
                match &obj_val {
                    Value::List(items) => {
                        let n = items.len() as i64;
                        let a = bound(&lo_v, n, 0)?.clamp(0, n) as usize;
                        let b = bound(&hi_v, n, n)?.clamp(0, n) as usize;
                        if a > b { Ok(Value::List(im::vector![])) }
                        else { Ok(Value::List(items.iter().skip(a).take(b - a).cloned().collect())) }
                    }
                    Value::Str(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let n = chars.len() as i64;
                        let a = bound(&lo_v, n, 0)?.clamp(0, n) as usize;
                        let b = bound(&hi_v, n, n)?.clamp(0, n) as usize;
                        if a > b { Ok(Value::Str(String::new().into())) }
                        else { Ok(Value::Str(chars[a..b].iter().collect::<String>().into())) }
                    }
                    _ => Err("Cannot slice this value".into()),
                }
            }
            Expr::Index(obj, index) => {
                // Var fast path: xs[i] / d["k"] / s[i] read through a shared
                // borrow instead of cloning the whole container first.
                if let Expr::Var(name) = &**obj {
                    let idx_val = self.eval_expr(index)?;
                    let vars = self.vars.borrow();
                    let hit: Option<Result<Value, String>> = match (vars.get(name), &idx_val) {
                        (Some(Value::Dict(map)), Value::Str(k)) =>
                            Some(Ok(map.get(&**k).cloned().unwrap_or(Value::Null))),
                        (Some(Value::List(items)), Value::Int(i)) => {
                            let j = if *i < 0 { items.len() as i64 + i } else { *i };
                            if j < 0 || j as usize >= items.len() {
                                Some(Err(format!("Index {} out of bounds (len {})", i, items.len())))
                            } else {
                                Some(Ok(items[j as usize].clone()))
                            }
                        }
                        (Some(Value::Str(s)), Value::Int(i)) => {
                            let j = if *i < 0 { s.chars().count() as i64 + i } else { *i };
                            Some(s.chars().nth(j as usize)
                                .map(|c| Value::Str(c.to_string().into()))
                                .ok_or_else(|| format!("Index {} out of bounds", i)))
                        }
                        _ => None,
                    };
                    if let Some(r) = hit { return r; }
                    drop(vars);
                }
                let obj_val = self.eval_expr(obj)?;
                let idx_val = self.eval_expr(index)?;
                match (&obj_val, &idx_val) {
                    (Value::Dict(map), Value::Str(k)) =>
                        Ok(map.get(&**k).cloned().unwrap_or(Value::Null)),
                    (Value::List(items), Value::Int(i)) => {
                        let j = if *i < 0 { items.len() as i64 + i } else { *i };
                        if j < 0 || j as usize >= items.len() {
                            Err(format!("Index {} out of bounds (len {})", i, items.len()))
                        } else {
                            Ok(items[j as usize].clone())
                        }
                    }
                    (Value::Str(s), Value::Int(i)) => {
                        let chars: Vec<char> = s.chars().collect();
                        let j = if *i < 0 { chars.len() as i64 + i } else { *i };
                        if j < 0 || j as usize >= chars.len() {
                            Err(format!("Index {} out of bounds (len {})", i, chars.len()))
                        } else {
                            Ok(Value::Str(chars[j as usize].to_string().into()))
                        }
                    }
                    _ => Err("Cannot index this value".into()),
                }
            }
        }
    }

    fn eval_binop(&self, l: &Value, op: &BinOp, r: &Value) -> Result<Value, String> {
        match op {
            BinOp::Add => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
                (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b).into())),
                (Value::Str(a), Value::Int(b)) => Ok(Value::Str(format!("{}{}", a, b).into())),
                (Value::Str(a), Value::Float(b)) => {
                    let s = if b.fract() == 0.0 { format!("{:.1}", b) } else { format!("{}", b) };
                    Ok(Value::Str(format!("{}{}", a, s).into()))
                }
                (Value::Int(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b).into())),
                (Value::Float(a), Value::Str(b)) => {
                    let s = if a.fract() == 0.0 { format!("{:.1}", a) } else { format!("{}", a) };
                    Ok(Value::Str(format!("{}{}", s, b).into()))
                }
                (Value::Str(a), Value::Bool(b)) => Ok(Value::Str(format!("{}{}", a, b).into())),
                (Value::Bool(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b).into())),
                (Value::Str(a), Value::List(b)) => {
                    let strs: Vec<String> = b.iter().map(|v| format!("{}", v)).collect();
                    Ok(Value::Str(format!("{}[{}]", a, strs.join(", ")).into()))
                }
                (Value::Str(a), Value::Dict(b)) => {
                    let strs: Vec<String> = b.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
                    Ok(Value::Str(format!("{}{{{}}}", a, strs.join(", ")).into()))
                }
                (Value::Str(a), Value::Set(b)) => {
                    let strs: Vec<String> = b.iter().map(|v| format!("{}", v)).collect();
                    Ok(Value::Str(format!("{}set[{}]", a, strs.join(", ")).into()))
                }
                (Value::Str(a), Value::Null) => Ok(Value::Str(format!("{}null", a).into())),
                _ => Err("Cannot add these types".into()),
            },
            BinOp::Sub => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_sub(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
                _ => Err("Cannot subtract these types".into()),
            },
            BinOp::Mul => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
                _ => Err("Cannot multiply these types".into()),
            },
            BinOp::Div => match (l, r) {
                (Value::Int(_), Value::Int(0)) => Err("Division by zero".into()),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_div(*b))),
                (Value::Float(_), Value::Float(b)) if *b == 0.0 => Err("Division by zero".into()),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 / b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / *b as f64)),
                _ => Err("Cannot divide these types".into()),
            },
            BinOp::Mod => match (l, r) {
                (Value::Int(_), Value::Int(0)) => Err("Modulo by zero".into()),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_rem(*b))),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float((*a as f64) % b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % (*b as f64))),
                _ => Err("Cannot modulo these types".into()),
            },
            BinOp::In => match (l, r) {
                (v, Value::List(items)) => Ok(Value::Bool(
                    items.iter().any(|i| self.values_eq(&v, i)))),
                (Value::Str(a), Value::Str(b)) => Ok(Value::Bool(b.contains(&**a))),
                (Value::Str(k), Value::Dict(d)) => Ok(Value::Bool(d.contains_key(&**k))),
                _ => Err("right side of 'in' must be a list, dict, or string".into()),
            },
            BinOp::Eq => Ok(Value::Bool(self.values_eq(l, r))),
            BinOp::Neq => Ok(Value::Bool(!self.values_eq(l, r))),
            BinOp::Lt => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) < *b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a < (*b as f64))),
                _ => Err("Cannot compare these types with <".into()),
            },
            BinOp::Gt => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) > *b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a > (*b as f64))),
                _ => Err("Cannot compare these types with >".into()),
            },
            BinOp::Le => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) <= *b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a <= (*b as f64))),
                _ => Err("Cannot compare these types with <=".into()),
            },
            BinOp::Ge => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Bool((*a as f64) >= *b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(*a >= (*b as f64))),
                _ => Err("Cannot compare these types with >=".into()),
                _ => Err("Cannot compare non-integers with >=".into()),
            },
            BinOp::NullCoal => {
                // already handled lazily in eval_expr; fallback for direct calls
                if matches!(l, Value::Null) { Ok(r.clone()) } else { Ok(l.clone()) }
            }
            BinOp::And => match (l, r) {
                (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
                _ => Err("Cannot 'and' non-booleans".into()),
            },
            BinOp::Or => match (l, r) {
                (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
                _ => Err("Cannot 'or' non-booleans".into()),
            },
        }
    }

    fn cmp_values(&self, a: &Value, b: &Value) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
            (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
            (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => Ordering::Equal,
        }
    }

    /// 2D engine helpers — num coercion + Value→serde_json for game_frame
    fn num_f(v: &Value) -> Result<f64, String> {
        match v {
            Value::Int(i) => Ok(*i as f64),
            Value::Float(f) => Ok(*f),
            _ => Err("expected a number".into()),
        }
    }
    fn json_num(v: &Value) -> f64 {
        match v { Value::Int(i) => *i as f64, Value::Float(f) => *f, _ => 0.0 }
    }
    fn json_val(v: &Value) -> serde_json::Value {
        match v {
            Value::Int(i) => serde_json::json!(i),
            Value::Float(f) => serde_json::json!(f),
            Value::Str(s) => serde_json::json!(s),
            Value::Bool(b) => serde_json::json!(b),
            Value::Null => serde_json::Value::Null,
            Value::List(l) => serde_json::json!(
                l.iter().map(Self::json_val).collect::<Vec<_>>()),
            Value::Dict(d) => d.iter()
                .map(|(k, v)| (k.clone(), Self::json_val(v)))
                .collect::<serde_json::Map<_,_>>().into(),
            other => serde_json::json!(format!("{:?}", other)),
        }
    }
    fn entity_json(v: &Value) -> serde_json::Value {
        match v {
            Value::Dict(d) => serde_json::json!({
                "name": d.get("name").map(Self::json_val)
                    .unwrap_or(serde_json::Value::Null),
                "x": d.get("x").map(Self::json_num).unwrap_or(0.0),
                "y": d.get("y").map(Self::json_num).unwrap_or(0.0),
                "sprite": d.get("sprite").map(Self::json_val)
                    .unwrap_or(serde_json::Value::Null),
                "vx": d.get("vx").map(Self::json_num).unwrap_or(0.0),
                "vy": d.get("vy").map(Self::json_num).unwrap_or(0.0),
                "tag": d.get("tag").map(Self::json_val)
                    .unwrap_or(serde_json::Value::Null),
            }),
            other => serde_json::json!(format!("{:?}", other)),
        }
    }

    /// is the cell at (x,y) of a canvas/map dict solid? Solid = any
    /// non-floor char — walls/platforms/terrain; out-of-bounds is solid.
    fn cell_solid(d: &im::HashMap<String, Value>, x: i64, y: i64) -> bool {
        let Some(Value::List(rows)) = d.get("cells") else { return true };
        if y < 0 || y as usize >= rows.len() { return true }
        let Value::List(r) = &rows[y as usize] else { return true };
        if x < 0 || x as usize >= r.len() { return true }
        !matches!(&r[x as usize], Value::Str(s) if s.as_ref() == " " || s.as_ref() == ".")
    }

    fn values_eq(&self, l: &Value, r: &Value) -> bool {
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
            (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.values_eq(x, y))
            }
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a == b,
            (Value::Struct(n1, f1), Value::Struct(n2, f2)) => {
                n1 == n2 && f1 == f2
            }
            _ => false,
        }
    }

    pub fn call_function(&mut self, name: &str, variant: Option<u32>, args: Vec<Value>) -> Result<Value, String> {
        // Try builtin first (builtins don't have variants)
        if variant.is_none() {
            if let Some(result) = self.try_builtin(name, &args)? {
                return Ok(result);
            }
        }
        // Look up user function — clone the def to avoid borrow issues
        let (chosen_id, params, body) = {
            let variants = self.functions.get(name)
                .ok_or_else(|| format!("Undefined function: {}", name))?;
            let chosen = match variant {
                Some(id) => variants.iter().find(|v| v.id == id)
                    .ok_or_else(|| format!("Variant {}#{} not found", name, id))?,
                None => {
                    // dispatch to the fittest loaded variant
                    variants.iter()
                        .max_by(|a, b| a.fitness.partial_cmp(&b.fitness).unwrap_or(std::cmp::Ordering::Equal))
                        .unwrap()
                }
            };
            (chosen.id, chosen.def.params.clone(), chosen.def.body.clone())
        };

        // Log usage for veldt tracking
        self.usage_log.push((name.to_string(), chosen_id));

        // Create new scope — capacity hint skips the rehash allocs
        let old_vars = self.vars.clone();
        self.vars = Rc::new(RefCell::new(HashMap::with_capacity(params.len() + 4)));

        // Bind params
        if args.len() != params.len() {
            self.vars = old_vars;
            return Err(format!("Function {} expects {} args but got {}", name, params.len(), args.len()));
        }
        // Serialize args for tracing before they're consumed by binding
        let args_json: Vec<String> = if self.trace_calls {
            args.iter().map(|a| serde_json::to_string(a).unwrap_or_default()).collect()
        } else { Vec::new() };

        for (param, val) in params.iter().zip(args.into_iter()) {
            self.vars.borrow_mut().insert(param.name.clone(), val);
        }

        // Execute body — frame stays on the stack if the call fails,
        // so the error path can print a real call trace.
        self.call_stack.push((name.to_string(), self.current_line));
        let result = match self.exec_block(&body[..]) {
            Ok(FlowControl::Return(v)) => v.unwrap_or(Value::Null),
            Ok(FlowControl::Normal) => Value::Null,
            Err(e) => {
                self.vars = old_vars;
                return Err(e);
            }
        };
        self.call_stack.pop();

        // Restore scope
        self.vars = old_vars;

        // Record call as evidence (only when trace_calls is on — not during
        // trials/probes/spawned workers, so evidence stays user-driven)
        if self.trace_calls {
            let res_json = serde_json::to_string(&result).unwrap_or_default();
            let entry = self.traces.entry(name.to_string()).or_insert_with(Vec::new);
            entry.push((args_json, res_json));
            if entry.len() > 100 {
                let excess = entry.len() - 100;
                entry.drain(0..excess);
            }
        }

        Ok(result)
    }

    /// Minimal SHA-256 (no dependency) returning hex string.
    fn civil_from_secs(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
        // civil-from-days (Howard Hinnant) — (y, m, d, h, min, s) in UTC
        let days = secs.div_euclid(86400);
        let tod = secs.rem_euclid(86400);
        let (h, mi, sc) = (tod / 3600, (tod % 3600) / 60, tod % 60);
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097);
        let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365*yoe + yoe/4 - yoe/100);
        let mp = (5*doy + 2)/153;
        let d = doy - (153*mp + 2)/5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        (y, m, d, h, mi, sc)
    }

    fn sha256_hex(msg: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
            0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
            0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
            0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
            0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
            0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
            0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
            0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
        let mut h: [u32; 8] = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
                               0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
        let bitlen = (msg.len() as u64) * 8;
        let mut data = msg.to_vec();
        data.push(0x80);
        while data.len() % 64 != 56 { data.push(0); }
        data.extend_from_slice(&bitlen.to_be_bytes());
        for chunk in data.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3]]);
            }
            for i in 16..64 {
                let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
                let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
                w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g; g = f; f = e; e = d.wrapping_add(t1);
                d = c; c = b; b = a; a = t1.wrapping_add(t2);
            }
            h[0]=h[0].wrapping_add(a); h[1]=h[1].wrapping_add(b); h[2]=h[2].wrapping_add(c);
            h[3]=h[3].wrapping_add(d); h[4]=h[4].wrapping_add(e); h[5]=h[5].wrapping_add(f);
            h[6]=h[6].wrapping_add(g); h[7]=h[7].wrapping_add(hh);
        }
        h.iter().map(|x| format!("{:08x}", x)).collect()
    }

    /// Runs a garden function on a spawned thread with a fresh interpreter.
    /// Shared-nothing: args arrive as JSON strings, result leaves as JSON.
    fn thread_run(fname: &str, json_args: Vec<String>) -> Result<String, String> {
        let mut veldt = crate::veldt::Veldt::new();
        veldt.load().map_err(|e| format!("garden load failed: {}", e))?;
        let mut interp = Interpreter::new();
        veldt.load_into_interpreter(&mut interp);
        let args: Result<Vec<Value>, _> = json_args.iter()
            .map(|s| serde_json::from_str(s)).collect();
        let args = args.map_err(|e| format!("bad spawn args: {}", e))?;
        let result = interp.call_function(fname, None, args)?;
        serde_json::to_string(&result).map_err(|e| format!("result not serializable: {}", e))
    }

    fn call_closure(&mut self, params: Vec<Param>, body: Vec<Stmt>, captured: Rc<RefCell<HashMap<String, Value>>>, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != params.len() {
            return Err(format!("Closure expects {} args but got {}", params.len(), args.len()));
        }

        // Save current vars; the closure runs with its own snapshot
        // env + params — no sharing, no restore bookkeeping.
        let old_vars = self.vars.clone();
        self.vars = captured;
        for (param, val) in params.iter().zip(args.into_iter()) {
            self.vars.borrow_mut().insert(param.name.clone(), val);
        }

        // Execute body — frame stays on the stack if the call fails,
        // so the error path can print a real call trace.
        self.call_stack.push(("<closure>".to_string(), self.current_line));
        let result = match self.exec_block(&body) {
            Ok(FlowControl::Return(v)) => v.unwrap_or(Value::Null),
            Ok(FlowControl::Normal) => Value::Null,
            Err(e) => {
                self.vars = old_vars;
                return Err(e);
            }
        };
        self.call_stack.pop();
        self.vars = old_vars;
        Ok(result)
    }

    /// VM delegation: invoke a closure value through the reference
    /// interpreter so lambda semantics are shared, not duplicated.
    pub(crate) fn call_closure_pub(&mut self, closure: &Value, args: Vec<Value>)
        -> Result<Value, String> {
        self.call_closure_value(closure, args)
    }

    fn call_closure_value(&mut self, closure: &Value, args: Vec<Value>) -> Result<Value, String> {
        match closure {
            Value::Closure(params, body, captured) => {
                self.call_closure(params.clone(), body.clone(), captured.clone(), args)
            }
            _ => Err("Expected a closure".into()),
        }
    }

    fn serve_requests(&mut self, port: i64, params: Vec<Param>, body: Vec<Stmt>, captured: Rc<RefCell<HashMap<String, Value>>>) -> Result<(), String> {
        use std::net::TcpListener;
        use std::io::{BufRead, BufReader, Read, Write};

        let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
            .map_err(|e| format!("Cannot bind port {}: {}", port, e))?;

        println!("Veldt server listening on http://127.0.0.1:{}", port);

        for stream in listener.incoming() {
            let stream = stream.map_err(|e| format!("Accept error: {}", e))?;
            let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() { continue; }

            // Parse request line: METHOD PATH HTTP/1.1
            let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
            if parts.len() < 3 { continue; }
            let method = parts[0].to_string();
            let path = parts[1].to_string();

            // Read headers
            let mut headers_map = std::collections::HashMap::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() { break; }
                let line = line.trim_end().to_string();
                if line.is_empty() { break; }
                if let Some((k, v)) = line.split_once(": ") {
                    headers_map.insert(k.to_lowercase(), v.to_string());
                    if k.to_lowercase() == "content-length" {
                        content_length = v.parse().unwrap_or(0);
                    }
                }
            }

            // Read body
            let mut body_str = String::new();
            if content_length > 0 {
                let mut buf = vec![0u8; content_length];
                if reader.read_exact(&mut buf).is_ok() {
                    body_str = String::from_utf8_lossy(&buf).to_string();
                }
            }

            // Build request dict
            let mut req_map = std::collections::HashMap::new();
            req_map.insert("method".to_string(), Value::Str(method.into()));
            req_map.insert("path".to_string(), Value::Str(path.into()));
            req_map.insert("body".to_string(), Value::Str(body_str.into()));
            let mut headers_val = std::collections::HashMap::new();
            for (k, v) in headers_map {
                headers_val.insert(k, Value::Str(v.into()));
            }
            req_map.insert("headers".to_string(), Value::Dict(headers_val.into_iter().collect()));
            let req_val = Value::Dict(req_map.into_iter().collect());

            // Call Veldt handler
            let result = match self.call_closure(params.clone(), body.clone(), captured.clone(), vec![req_val]) {
                Ok(v) => v,
                Err(e) => {
                    let mut resp = std::collections::HashMap::new();
                    resp.insert("status".to_string(), Value::Int(500));
                    resp.insert("body".to_string(), Value::Str(format!("Handler error: {}", e).into()));
                    Value::Dict(resp.into_iter().collect())
                }
            };

            // Extract response
            let (status, resp_body) = match &result {
                Value::Dict(d) => {
                    let status = match d.get("status") { Some(Value::Int(n)) => *n, _ => 200 };
                    let body = match d.get("body") { Some(Value::Str(s)) => s.to_string(), Some(v) => format!("{}", v), None => "".into() };
                    (status, body)
                }
                Value::Str(s) => (200, s.to_string()),
                _ => (200, format!("{}", result)),
            };

            let response = format!(
                "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                status, resp_body.len(), resp_body
            );
            let mut stream = stream;
            if stream.write_all(response.as_bytes()).is_err() { continue; }
            if stream.flush().is_err() { continue; }
        }
        Ok(())
    }

    /// Evaluate a behavior-tree node: ["seq", ...] all must succeed,
    /// ["sel", ...] first success wins, ["inv", a] flips, ["act", fn]
    /// calls fn(ctx) → bool. Returns Ok(true) on success.
    fn bt_tick(&mut self, node: &im::Vector<Value>, ctx: &Value) -> Result<bool, String> {
        let Some(Value::Str(kind)) = node.get(0) else {
            return Err("bt node must start with a kind string".into());
        };
        match &**kind {
            "seq" => {
                for child in node.iter().skip(1) {
                    let Value::List(c) = child else { return Err("bt child must be a node".into()) };
                    if !self.bt_tick(c, ctx)? { return Ok(false); }
                }
                Ok(true)
            }
            "sel" => {
                for child in node.iter().skip(1) {
                    let Value::List(c) = child else { return Err("bt child must be a node".into()) };
                    if self.bt_tick(c, ctx)? { return Ok(true); }
                }
                Ok(false)
            }
            "inv" => {
                let Some(Value::List(c)) = node.get(1) else { return Err("inv expects one child".into()) };
                Ok(!self.bt_tick(c, ctx)?)
            }
            "act" => {
                let Some(f @ Value::Closure(_, _, _)) = node.get(1) else {
                    return Err("act expects a closure".into())
                };
                match self.call_closure_value(f, vec![ctx.clone()])? {
                    Value::Bool(b) => Ok(b),
                    _ => Err("act closure must return bool".into()),
                }
            }
            "cond" => {
                // ["cond", check_fn, then_node, else_node?]
                let Some(f @ Value::Closure(_, _, _)) = node.get(1) else {
                    return Err("cond expects a check closure".into())
                };
                let pass = match self.call_closure_value(f, vec![ctx.clone()])? {
                    Value::Bool(b) => b,
                    _ => return Err("cond check must return bool".into()),
                };
                if pass {
                    let Some(Value::List(c)) = node.get(2) else { return Err("cond expects a then-node".into()) };
                    self.bt_tick(c, ctx)
                } else if let Some(Value::List(c)) = node.get(3) {
                    self.bt_tick(c, ctx)
                } else {
                    Ok(false)
                }
            }
            "rep" => {
                // ["rep", n, child] — tick child n times, succeed on last
                let (Some(Value::Int(n)), Some(Value::List(c))) = (node.get(1), node.get(2))
                    else { return Err("rep expects n and a child".into()) };
                for _ in 0..*n {
                    if !self.bt_tick(c, ctx)? { return Ok(false); }
                }
                Ok(true)
            }
            "succeed" => Ok(true),
            "fail" => Ok(false),
            other => Err(format!("unknown bt node kind: {}", other)),
        }
    }

    pub(crate) fn try_builtin(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
        // std:sandbox gate — denied capabilities error before dispatch
        if self.caps.denied(name) {
            return Err(format!("sandbox: '{}' requires a denied capability", name));
        }
        // sandbox_run(name, args_list): eval a function with all caps off —
        // Veldt-side Stage-0 isolation (spawn-free, same interpreter).
        if name == "sandbox_run" {
            let fname = match args.first() {
                Some(Value::Str(s)) => s.to_string(),
                _ => return Err("sandbox_run() expects (name, args_list)".into()),
            };
            let fargs: Vec<Value> = match args.get(1) {
                Some(Value::List(l)) => l.iter().cloned().collect(),
                _ => Vec::new(),
            };
            let saved = std::mem::replace(&mut self.caps, Caps::sandboxed());
            let r = self.call_function(&fname, None, fargs);
            self.caps = saved;
            return r.map(Some);
        }
        // shadow_call(name, args) — Stage 1 trust ladder: every healthy
        // variant executes on the same input (side effects suppressed);
        // the champion's output is returned, divergent variants logged.
        if name == "shadow_call" {
            let fname = match args.first() {
                Some(Value::Str(s)) => s.to_string(),
                _ => return Err("shadow_call() expects (name, args_list)".into()),
            };
            let fargs: Vec<Value> = match args.get(1) {
                Some(Value::List(l)) => l.iter().cloned().collect(),
                _ => Vec::new(),
            };
            let vars: Vec<u32> = self.functions.get(&fname)
                .map(|vs| vs.iter().map(|v| v.id).collect())
                .unwrap_or_default();
            if vars.is_empty() {
                return Err(format!("shadow_call: no variants of '{}'", fname));
            }
            let champion = self.functions.get(&fname).unwrap().iter()
                .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                    .unwrap_or(std::cmp::Ordering::Equal))
                .map(|v| v.id).unwrap();
            let saved = std::mem::replace(&mut self.caps, Caps::sandboxed());
            let mut outs: Vec<(u32, Result<String, String>)> = Vec::new();
            for id in &vars {
                let r = self.call_function(&fname, Some(*id), fargs.clone())
                    .map(|v| serde_json::to_string(&v).unwrap_or_default());
                outs.push((*id, r));
            }
            self.caps = saved;
            let champ = outs.iter().find(|(id, _)| *id == champion).unwrap().1.clone();
            let champ_out = champ?;
            let div: Vec<String> = outs.iter()
                .filter(|(id, r)| *id != champion
                    && r.as_ref().map(|o| o != &champ_out).unwrap_or(true))
                .map(|(id, _)| format!("{}#{}", fname, id))
                .collect();
            if !div.is_empty() {
                eprintln!("[SHADOW] {} diverged from champion {}#{}",
                    div.join(", "), fname, champion);
            }
            let val: Value = serde_json::from_str(&champ_out).unwrap_or(Value::Null);
            return Ok(Some(val));
        }

        match name {
            "range" => {
                if args.len() != 1 {
                    return Err("range() expects 1 argument".into());
                }
                match &args[0] {
                    Value::Int(n) => {
                        let items: Vec<Value> = (0..*n).map(Value::Int).collect();
                        Ok(Some(Value::List(items.into_iter().collect())))
                    }
                    _ => Err("range() expects an integer".into()),
                }
            }
            "len" => {
                if args.len() != 1 {
                    return Err("len() expects 1 argument".into());
                }
                match &args[0] {
                    Value::Str(s) => Ok(Some(Value::Int(s.len() as i64))),
                    Value::List(items) => Ok(Some(Value::Int(items.len() as i64))),
                    _ => Err("len() expects a string or list".into()),
                }
            }
            "max" => {
                let nums = Self::flatten_args(args)?;
                let mut best: Option<f64> = None;
                let mut all_int = true;
                for v in &nums {
                    let f = self.to_f64(v)?;
                    if !matches!(v, Value::Int(_)) { all_int = false; }
                    best = Some(match best { Some(b) => b.max(f), None => f });
                }
                match best {
                    None => Err("max() of empty list".into()),
                    Some(f) if all_int => Ok(Some(Value::Int(f as i64))),
                    Some(f) => Ok(Some(Value::Float(f))),
                }
            }
            "min" => {
                let nums = Self::flatten_args(args)?;
                let mut best: Option<f64> = None;
                let mut all_int = true;
                for v in &nums {
                    let f = self.to_f64(v)?;
                    if !matches!(v, Value::Int(_)) { all_int = false; }
                    best = Some(match best { Some(b) => b.min(f), None => f });
                }
                match best {
                    None => Err("min() of empty list".into()),
                    Some(f) if all_int => Ok(Some(Value::Int(f as i64))),
                    Some(f) => Ok(Some(Value::Float(f))),
                }
            }
            "abs" => {
                if args.len() != 1 { return Err("abs() expects 1 argument".into()); }
                match &args[0] {
                    Value::Int(n) => Ok(Some(Value::Int(n.abs()))),
                    Value::Float(f) => Ok(Some(Value::Float(f.abs()))),
                    _ => Err("abs() expects a number".into()),
                }
            }
            "str" => {
                if args.len() != 1 { return Err("str() expects 1 argument".into()); }
                Ok(Some(Value::Str(format!("{}", args[0]).into())))
            }
            "int" => {
                if args.len() != 1 { return Err("int() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(s) => {
                        s.trim().parse::<i64>()
                            .map(|n| Some(Value::Int(n)))
                            .map_err(|_| format!("Cannot convert '{}' to int", s))
                    }
                    Value::Int(n) => Ok(Some(Value::Int(*n))),
                    Value::Bool(b) => Ok(Some(Value::Int(if *b { 1 } else { 0 }))),
                    _ => Err("int() expects a string or int".into()),
                }
            }
            "rand" => {
                if args.len() != 1 { return Err("rand() expects 1 argument".into()); }
                match &args[0] {
                    Value::Int(n) => {
                        if *n <= 0 {
                            Ok(Some(Value::Int(0)))
                        } else {
                            // xorshift64
                            let mut x = self.rng_state;
                            x ^= x << 13;
                            x ^= x >> 7;
                            x ^= x << 17;
                            self.rng_state = x;
                            Ok(Some(Value::Int((x % (*n as u64)) as i64)))
                        }
                    }
                    _ => Err("rand() expects an integer".into()),
                }
            }
            // === std:render / std:ui — ASCII canvas (game pillar) ===
            // A canvas is a dict {w, h, cells: [[str]]} — all ops return
            // a new dict (persistent, matches the immutable-value style).
            "canvas_new" => {
                let (w, h) = match (args.get(0), args.get(1)) {
                    (Some(Value::Int(w)), Some(Value::Int(h))) => (*w as usize, *h as usize),
                    _ => return Err("canvas_new(w, h) expects two ints".into()),
                };
                let row: im::Vector<Value> = (0..w).map(|_| Value::Str(" ".into())).collect();
                let cells: im::Vector<Value> = (0..h).map(|_| Value::List(row.clone())).collect();
                let mut d = im::HashMap::new();
                d.insert("w".to_string(), Value::Int(w as i64));
                d.insert("h".to_string(), Value::Int(h as i64));
                d.insert("cells".to_string(), Value::List(cells));
                Ok(Some(Value::Dict(d)))
            }
            "canvas_set" => {
                let (c, x, y, ch) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), Some(Value::Str(ch)))
                        => (d.clone(), *x as usize, *y as usize, ch.clone()),
                    _ => return Err("canvas_set(c, x, y, ch) expects dict, x, y, str".into()),
                };
                let Some(Value::List(rows)) = c.get("cells") else { return Err("canvas: no cells".into()) };
                if y >= rows.len() { return Ok(Some(Value::Dict(c))); }
                let mut rows2 = rows.clone();
                if let Value::List(r2) = &rows[y] {
                    let mut nr = r2.clone();
                    if x < nr.len() { nr[x] = Value::Str(ch); }
                    rows2[y] = Value::List(nr);
                }
                let mut d = c.clone();
                d.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d)))
            }
            "canvas_get" => {
                let (d, x, y) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)))
                        => (d, *x as usize, *y as usize),
                    _ => return Err("canvas_get(c, x, y) expects dict, x, y".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                if y >= rows.len() { return Ok(Some(Value::Null)); }
                let Value::List(r) = &rows[y] else { return Ok(Some(Value::Null)) };
                if x >= r.len() { return Ok(Some(Value::Null)); }
                Ok(Some(r[x].clone()))
            }
            "canvas_fill" => {
                let (c, x0, y0, w, h, ch) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x0)), Some(Value::Int(y0)),
                     Some(Value::Int(w)), Some(Value::Int(h)), Some(Value::Str(ch)))
                        => (d.clone(), *x0 as usize, *y0 as usize, *w as usize, *h as usize, ch.clone()),
                    _ => return Err("canvas_fill(c, x, y, w, h, ch) expects dict + 4 ints + str".into()),
                };
                let Some(Value::List(rows)) = c.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                for yy in y0..(y0 + h).min(rows2.len()) {
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for xx in x0..(x0 + w).min(nr.len()) { nr[xx] = Value::Str(ch.clone()); }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d = c.clone();
                d.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d)))
            }
            "canvas_line" => {
                let (c, x, y, x2, y2, ch) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(a)), Some(Value::Int(b)),
                     Some(Value::Int(c2)), Some(Value::Int(d2)), Some(Value::Str(ch)))
                        => (d.clone(), *a, *b, *c2, *d2, ch.clone()),
                    _ => return Err("canvas_line(c, x1, y1, x2, y2, ch) expects dict + 4 ints + str".into()),
                };
                let Some(Value::List(rows)) = c.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let (dx, dy) = (x2 - x, y2 - y);
                let steps = dx.abs().max(dy.abs()).max(1);
                for i in 0..=steps {
                    let px = x + dx * i / steps;
                    let py = y + dy * i / steps;
                    if py >= 0 && (py as usize) < rows2.len() {
                        if let Value::List(r) = &rows2[py as usize] {
                            if px >= 0 && (px as usize) < r.len() {
                                let mut nr = r.clone();
                                nr[px as usize] = Value::Str(ch.clone());
                                rows2[py as usize] = Value::List(nr);
                            }
                        }
                    }
                }
                let mut d = c.clone();
                d.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d)))
            }
            "canvas_render" => {
                let Some(Value::Dict(d)) = args.get(0) else { return Err("canvas_render(c) expects a canvas".into()) };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut out = String::new();
                for r in rows {
                    if let Value::List(row) = r {
                        for c in row {
                            if let Value::Str(s) = c { out.push_str(s.as_ref()); }
                        }
                        out.push('\n');
                    }
                }
                Ok(Some(Value::Str(out.into())))
            }
            "canvas_print" => {
                let Some(Value::Dict(d)) = args.get(0) else { return Err("canvas_print(c) expects a canvas".into()) };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                for r in rows {
                    if let Value::List(row) = r {
                        for c in row { if let Value::Str(s) = c { print!("{}", s); } }
                        println!();
                    }
                }
                Ok(Some(Value::Null))
            }
            "sprite" => {
                // sprite(c, x, y, lines) — draw a multi-line string sprite
                let (c, x0, y0, lines) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), Some(Value::Str(s)))
                        => (d.clone(), *x, *y, s.clone()),
                    _ => return Err("sprite(c, x, y, str) expects dict, x, y, str".into()),
                };
                let Some(Value::List(rows)) = c.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                for (dy, line) in lines.lines().enumerate() {
                    let yy = (y0 + dy as i64) as usize;
                    if yy >= rows2.len() { break; }
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for (dx, ch) in line.chars().enumerate() {
                            let xx = (x0 + dx as i64) as usize;
                            if xx < nr.len() { nr[xx] = Value::Str(ch.to_string().into()); }
                        }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d = c.clone();
                d.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d)))
            }
            "choice" => {
                let Value::List(items) = args.get(0).cloned().unwrap_or(Value::Null) else {
                    return Err("choice(list) expects a list".into())
                };
                if items.is_empty() { return Ok(Some(Value::Null)); }
                let mut x = self.rng_state;
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                self.rng_state = x;
                Ok(Some(items[(x % items.len() as u64) as usize].clone()))
            }
            "randf" => {
                let mut x = self.rng_state;
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                self.rng_state = x;
                Ok(Some(Value::Float((x % 1_000_000) as f64 / 1_000_000.0)))
            }
            "rand_range" => {
                let (lo, hi) = match (args.get(0), args.get(1)) {
                    (Some(Value::Int(lo)), Some(Value::Int(hi))) => (*lo, *hi),
                    _ => return Err("rand_range(lo, hi) expects two ints".into()),
                };
                if hi <= lo { return Ok(Some(Value::Int(lo))); }
                let mut x = self.rng_state;
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                self.rng_state = x;
                Ok(Some(Value::Int(lo + (x % ((hi - lo + 1) as u64)) as i64)))
            }
            "dice" => {
                let (n, sides) = match (args.get(0), args.get(1)) {
                    (Some(Value::Int(n)), Some(Value::Int(s))) => (*n as usize, *s as i64),
                    _ => return Err("dice(n, sides) expects two ints".into()),
                };
                let mut total = 0i64;
                for _ in 0..n {
                    let mut x = self.rng_state;
                    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                    self.rng_state = x;
                    total += 1 + (x % (sides.max(1) as u64)) as i64;
                }
                Ok(Some(Value::Int(total)))
            }
            "balance_compare" => {
                // balance_compare(f1, f2, trials) — A/B two strategies, f returns
                // a score; report which wins more often.
                let (f1, f2, trials) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(f1 @ Value::Closure(_, _, _)), Some(f2 @ Value::Closure(_, _, _)), Some(Value::Int(t)))
                        => (f1, f2, *t as usize),
                    _ => return Err("balance_compare(f1, f2, trials) expects two closures + int".into()),
                };
                let (mut a_wins, mut b_wins, mut draws) = (0usize, 0usize, 0usize);
                for _ in 0..trials {
                    let a = self.call_closure_value(f1, vec![])?;
                    let b = self.call_closure_value(f2, vec![])?;
                    let score_of = |v: &Value| match v {
                        Value::Int(i) => *i as f64,
                        Value::Float(f) => *f,
                        Value::Dict(d) => d.get("score").and_then(|v| match v {
                            Value::Int(i) => Some(*i as f64), Value::Float(f) => Some(*f), _ => None }).unwrap_or(0.0),
                        _ => 0.0,
                    };
                    let (sa, sb) = (score_of(&a), score_of(&b));
                    if sa > sb { a_wins += 1; } else if sb > sa { b_wins += 1; } else { draws += 1; }
                }
                let mut out = im::HashMap::new();
                out.insert("a_wins".to_string(), Value::Int(a_wins as i64));
                out.insert("b_wins".to_string(), Value::Int(b_wins as i64));
                out.insert("draws".to_string(), Value::Int(draws as i64));
                out.insert("winner".to_string(), Value::Str(if a_wins > b_wins { "a".into() } else if b_wins > a_wins { "b".into() } else { "draw".into() }));
                Ok(Some(Value::Dict(out)))
            }
            "lerp" => {
                let (a, b, t) = (num_at(args, 0)?, num_at(args, 1)?, num_at(args, 2)?);
                Ok(Some(Value::Float(a + (b - a) * t)))
            }
            "clamp" => {
                let (v, lo, hi) = (num_at(args, 0)?, num_at(args, 1)?, num_at(args, 2)?);
                Ok(Some(Value::Float(v.max(lo).min(hi))))
            }
            "dist" => {
                let (x1, y1, x2, y2) = (num_at(args, 0)?, num_at(args, 1)?, num_at(args, 2)?, num_at(args, 3)?);
                Ok(Some(Value::Float(((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt())))
            }
            "aabb" => {
                // aabb(ax,ay,aw,ah, bx,by,bw,bh) — rect overlap
                let (ax, ay, aw, ah, bx, by, bw, bh) = (
                    num_at(args, 0)?, num_at(args, 1)?, num_at(args, 2)?, num_at(args, 3)?,
                    num_at(args, 4)?, num_at(args, 5)?, num_at(args, 6)?, num_at(args, 7)?);
                Ok(Some(Value::Bool(ax < bx + bw && ax + aw > bx && ay < by + bh && ay + ah > by)))
            }
            "bfs" => {
                // bfs(c, sx, sy, tx, ty, walkable_ch) — shortest path on a
                // canvas, walking cells that contain walkable_ch. Returns
                // [[x,y],...] or null if unreachable.
                let (d, sx, sy, tx, ty, walk) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(sx)), Some(Value::Int(sy)),
                     Some(Value::Int(tx)), Some(Value::Int(ty)), Some(Value::Str(w)))
                        => (d, *sx as i64, *sy as i64, *tx as i64, *ty as i64, w.clone()),
                    _ => return Err("bfs(c, sx, sy, tx, ty, walkable_ch) expects dict + 4 ints + str".into()),
                };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w as i64) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h as i64) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let Some(Value::List(cells)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let cell_ok = |x: i64, y: i64| -> bool {
                    if x < 0 || y < 0 || x >= w || y >= h { return false; }
                    if let Value::List(r) = &cells[y as usize] {
                        if let Some(Value::Str(s)) = r.get(x as usize) {
                            return *s == walk;
                        }
                    }
                    false
                };
                // bfs
                let mut prev: std::collections::HashMap<(i64, i64), (i64, i64)> = std::collections::HashMap::new();
                let mut q: std::collections::VecDeque<(i64, i64)> = std::collections::VecDeque::new();
                let mut seen: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
                q.push_back((sx, sy));
                seen.insert((sx, sy));
                while let Some((cx, cy)) = q.pop_front() {
                    if cx == tx && cy == ty { break; }
                    for (dx, dy) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
                        let (nx, ny) = (cx + dx, cy + dy);
                        if !seen.contains(&(nx, ny)) && (cell_ok(nx, ny) || (nx == tx && ny == ty)) {
                            seen.insert((nx, ny));
                            prev.insert((nx, ny), (cx, cy));
                            q.push_back((nx, ny));
                        }
                    }
                }
                if !seen.contains(&(tx, ty)) { return Ok(Some(Value::Null)); }
                let mut path = Vec::new();
                let mut cur = (tx, ty);
                loop {
                    path.push(Value::List(im::vector![Value::Int(cur.0), Value::Int(cur.1)]));
                    if cur == (sx, sy) { break; }
                    cur = prev[&cur];
                }
                path.reverse();
                Ok(Some(Value::List(path.into_iter().collect())))
            }
            "los" => {
                // los(c, x1, y1, x2, y2, wall_ch) — Bresenham line of sight:
                // true if no cell containing wall_ch blocks the segment.
                let (d, x1, y1, x2, y2, wall) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(a)), Some(Value::Int(b)),
                     Some(Value::Int(c2)), Some(Value::Int(d2)), Some(Value::Str(w)))
                        => (d, *a, *b, *c2, *d2, w.clone()),
                    _ => return Err("los(c, x1, y1, x2, y2, wall_ch) expects dict + 4 ints + str".into()),
                };
                let Some(Value::List(cells)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w as i64) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h as i64) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let (mut x, mut y) = (x1, y1);
                let (dx, dy) = ((x2 - x1).abs(), (y2 - y1).abs());
                let (sx, sy) = (if x1 < x2 { 1 } else { -1 }, if y1 < y2 { 1 } else { -1 });
                let mut err = dx - dy;
                loop {
                    if x >= 0 && y >= 0 && x < w && y < h {
                        if let Value::List(r) = &cells[y as usize] {
                            if let Some(Value::Str(s)) = r.get(x as usize) {
                                if *s == wall && (x, y) != (x1, y1) && (x, y) != (x2, y2) {
                                    return Ok(Some(Value::Bool(false)));
                                }
                            }
                        }
                    }
                    if x == x2 && y == y2 { break; }
                    let e2 = 2 * err;
                    if e2 > -dy { err -= dy; x += sx; }
                    if e2 < dx { err += dx; y += sy; }
                }
                Ok(Some(Value::Bool(true)))
            }
            "astar" => {
                // astar(c, sx, sy, tx, ty, walkable_ch, diagonal?) — A* on a
                // canvas, Manhattan heuristic. Returns [[x,y],...] or null.
                let (d, sx, sy, tx, ty, walk, diag) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5), args.get(6)) {
                    (Some(Value::Dict(d)), Some(Value::Int(sx)), Some(Value::Int(sy)),
                     Some(Value::Int(tx)), Some(Value::Int(ty)), Some(Value::Str(w)), diag)
                        => (d, *sx, *sy, *tx, *ty, w.clone(), matches!(diag, Some(Value::Bool(true)))),
                    _ => return Err("astar(c, sx, sy, tx, ty, walkable_ch, diag?) expects dict + 4 ints + str + bool".into()),
                };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w as i64) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h as i64) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let Some(Value::List(cells)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let cell_ok = |x: i64, y: i64| -> bool {
                    if x < 0 || y < 0 || x >= w || y >= h { return false; }
                    if let Value::List(r) = &cells[y as usize] {
                        if let Some(Value::Str(s)) = r.get(x as usize) { return *s == walk; }
                    }
                    false
                };
                let dirs: &[(i64, i64)] = if diag {
                    &[(0,-1),(0,1),(-1,0),(1,0),(-1,-1),(1,-1),(-1,1),(1,1)]
                } else {
                    &[(0,-1),(0,1),(-1,0),(1,0)]
                };
                let mut open: std::collections::BinaryHeap<(std::cmp::Reverse<i64>, i64, i64)> = std::collections::BinaryHeap::new();
                let mut g: std::collections::HashMap<(i64, i64), i64> = std::collections::HashMap::new();
                let mut prev: std::collections::HashMap<(i64, i64), (i64, i64)> = std::collections::HashMap::new();
                let h_of = |x: i64, y: i64| (tx - x).abs() + (ty - y).abs();
                g.insert((sx, sy), 0);
                open.push((std::cmp::Reverse(h_of(sx, sy)), sx, sy));
                while let Some((_, cx, cy)) = open.pop() {
                    if cx == tx && cy == ty { break; }
                    let cg = g[&(cx, cy)];
                    for &(dx, dy) in dirs {
                        let (nx, ny) = (cx + dx, cy + dy);
                        let ok = cell_ok(nx, ny) || (nx == tx && ny == ty);
                        let ng = cg + if dx != 0 && dy != 0 { 14 } else { 10 };
                        if ok && g.get(&(nx, ny)).map(|&v| ng < v).unwrap_or(true) {
                            g.insert((nx, ny), ng);
                            prev.insert((nx, ny), (cx, cy));
                            open.push((std::cmp::Reverse(ng + h_of(nx, ny) * 10), nx, ny));
                        }
                    }
                }
                if !prev.contains_key(&(tx, ty)) && (sx, sy) != (tx, ty) {
                    return Ok(Some(Value::Null));
                }
                let mut path = Vec::new();
                let mut cur = (tx, ty);
                loop {
                    path.push(Value::List(im::vector![Value::Int(cur.0), Value::Int(cur.1)]));
                    if cur == (sx, sy) { break; }
                    cur = prev[&cur];
                }
                path.reverse();
                Ok(Some(Value::List(path.into_iter().collect())))
            }
            "flood_fill" => {
                // flood_fill(c, x, y, ch) — fill all connected cells that
                // share the source cell's char with ch.
                let (d, x0, y0, ch) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), Some(Value::Str(ch)))
                        => (d.clone(), *x as usize, *y as usize, ch.clone()),
                    _ => return Err("flood_fill(c, x, y, ch) expects dict + 2 ints + str".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let (w, h) = (rows.get(0).and_then(|r| if let Value::List(r) = r { Some(r.len()) } else { None }), rows.len());
                let Some(w) = w else { return Err("canvas: missing w".into()) };
                let Some(Value::Str(target)) = rows.get(y0)
                    .and_then(|r| if let Value::List(r) = r { r.get(x0) } else { None })
                    .cloned() else { return Ok(Some(Value::Dict(d))); };
                if target == ch { return Ok(Some(Value::Dict(d))); }
                let mut rows2 = rows.clone();
                let mut stack = vec![(x0, y0)];
                let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
                while let Some((x, y)) = stack.pop() {
                    if x >= w || y >= h || !seen.insert((x, y)) { continue; }
                    let is_target = rows2.get(y)
                        .and_then(|r| if let Value::List(r) = r { r.get(x) } else { None })
                        .map(|v| if let Value::Str(s) = v { *s == target } else { false })
                        .unwrap_or(false);
                    if !is_target { continue; }
                    if let Value::List(r) = &mut rows2[y] {
                        let mut nr = r.clone();
                        nr[x] = Value::Str(ch.clone());
                        *r = nr;
                    }
                    stack.extend([(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)]);
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "neighbors" => {
                // neighbors(c, x, y, diag?) — 4-dir (or 8-dir) in-bounds cells
                let (d, x0, y0, diag) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), dg)
                        => (d, *x, *y, matches!(dg, Some(Value::Bool(true)))),
                    _ => return Err("neighbors(c, x, y, diag?) expects dict + 2 ints + bool".into()),
                };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let dirs: &[(i64, i64)] = if diag {
                    &[(0,-1),(0,1),(-1,0),(1,0),(-1,-1),(1,-1),(-1,1),(1,1)]
                } else { &[(0,-1),(0,1),(-1,0),(1,0)] };
                let out: Vec<Value> = dirs.iter()
                    .map(|(dx, dy)| (x0 + dx, y0 + dy))
                    .filter(|(x, y)| *x >= 0 && *y >= 0 && *x < w && *y < h)
                    .map(|(x, y)| Value::List(im::vector![Value::Int(x), Value::Int(y)]))
                    .collect();
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            "noise2" => {
                // noise2(x, y) — deterministic value noise in [0,1]. Bilinear
                // interp over a hash-seeded lattice.
                let (x, y) = (num_at(args, 0)?, num_at(args, 1)?);
                let hash = |ix: i64, iy: i64| -> f64 {
                    let mut n = ix.wrapping_mul(374761393).wrapping_add(iy.wrapping_mul(668265263));
                    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
                    ((n ^ (n >> 16)) as u64 & 0xffffff) as f64 / 0xffffff as f64
                };
                let (ix, iy) = (x.floor() as i64, y.floor() as i64);
                let (fx, fy) = (x - x.floor(), y - y.floor());
                let s = |t: f64| t * t * (3.0 - 2.0 * t); // smoothstep
                let (a, b, c, d) = (hash(ix, iy), hash(ix + 1, iy), hash(ix, iy + 1), hash(ix + 1, iy + 1));
                let v = a + (b - a) * s(fx) + (c - a) * s(fy) + (a - b - c + d) * s(fx) * s(fy);
                Ok(Some(Value::Float(v)))
            }
            "ease_in" => { let t = num_at(args, 0)?; Ok(Some(Value::Float(t * t))) }
            "ease_out" => { let t = num_at(args, 0)?; Ok(Some(Value::Float(1.0 - (1.0 - t) * (1.0 - t)))) }
            "ease_in_out" => {
                let t = num_at(args, 0)?;
                Ok(Some(Value::Float(if t < 0.5 { 2.0 * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(2) / 2.0 })))
            }
            "bounce" => {
                // bounce(t) — quadratic bounce-out
                let t = num_at(args, 0)?.clamp(0.0, 1.0);
                let (n1, d1) = (7.5625f64, 2.75f64);
                let v = if t < 1.0 / d1 { n1 * t * t }
                    else if t < 2.0 / d1 { let t2 = t - 1.5 / d1; n1 * t2 * t2 + 0.75 }
                    else if t < 2.5 / d1 { let t2 = t - 2.25 / d1; n1 * t2 * t2 + 0.9375 }
                    else { let t2 = t - 2.625 / d1; n1 * t2 * t2 + 0.984375 };
                Ok(Some(Value::Float(v)))
            }
            // === std:ui — immediate-mode ASCII widgets on the canvas ===
            "ui_box" => {
                // ui_box(c, x, y, w, h, title?) — bordered panel
                let (d, x0, y0, w, h, title) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::Int(w)), Some(Value::Int(h)), t)
                        => (d.clone(), *x as usize, *y as usize, *w as usize, *h as usize,
                            if let Some(Value::Str(s)) = t { s.clone() } else { "".into() }),
                    _ => return Err("ui_box(c, x, y, w, h, title?) expects dict + 4 ints + str?".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let set = |rows2: &mut im::Vector<Value>, x: usize, y: usize, ch: &str| {
                    if y < rows2.len() {
                        if let Value::List(r) = &rows2[y] {
                            if x < r.len() {
                                let mut nr = r.clone();
                                nr[x] = Value::Str(ch.into());
                                rows2[y] = Value::List(nr);
                            }
                        }
                    }
                };
                for xx in x0..(x0 + w) {
                    set(&mut rows2, xx, y0, if xx == x0 { "+" } else if xx == x0 + w - 1 { "+" } else { "-" });
                    set(&mut rows2, xx, y0 + h - 1, if xx == x0 { "+" } else if xx == x0 + w - 1 { "+" } else { "-" });
                }
                for yy in y0..(y0 + h) {
                    set(&mut rows2, x0, yy, "|");
                    set(&mut rows2, x0 + w - 1, yy, "|");
                }
                for (i, ch) in title.chars().take(w.saturating_sub(4)).enumerate() {
                    set(&mut rows2, x0 + 2 + i, y0, &ch.to_string());
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "ui_label" => {
                // ui_label(c, x, y, text) — write text starting at (x, y)
                let (d, x0, y0, text) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), Some(Value::Str(t)))
                        => (d.clone(), *x as usize, *y as usize, t.clone()),
                    _ => return Err("ui_label(c, x, y, text) expects dict + 2 ints + str".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                for (dy, line) in text.lines().enumerate() {
                    let yy = y0 + dy;
                    if yy >= rows2.len() { break; }
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for (dx, ch) in line.chars().enumerate() {
                            let xx = x0 + dx;
                            if xx < nr.len() { nr[xx] = Value::Str(ch.to_string().into()); }
                        }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "ui_button" => {
                // ui_button(c, x, y, label, hot?) — bracketed button [ label ]
                let (d, x, y, label, hot) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), Some(Value::Str(l)), h)
                        => (d.clone(), *x as usize, *y as usize, l.clone(),
                            matches!(h, Some(Value::Bool(true)))),
                    _ => return Err("ui_button(c, x, y, label, hot?) expects dict + 2 ints + str + bool".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let rendered = if hot { format!(">{}<", label) } else { format!("[{}]", label) };
                if y < rows2.len() {
                    if let Value::List(r) = &rows2[y] {
                        let mut nr = r.clone();
                        for (i, ch) in rendered.chars().enumerate() {
                            if x + i < nr.len() { nr[x + i] = Value::Str(ch.to_string().into()); }
                        }
                        rows2[y] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "fov" => {
                // fov(c, x, y, radius, wall_ch) — list of [x,y] cells visible
                // from (x,y) within radius, walls blocking. Uses los() on each.
                let (d, x0, y0, rad, wall) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::Int(r)), Some(Value::Str(w)))
                        => (d, *x, *y, *r, w.clone()),
                    _ => return Err("fov(c, x, y, radius, wall_ch) expects dict + 3 ints + str".into()),
                };
                let Some(Value::List(cells)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let is_wall = |x: i64, y: i64| -> bool {
                    if x < 0 || y < 0 || x >= w || y >= h { return true; }
                    if let Value::List(r) = &cells[y as usize] {
                        if let Some(Value::Str(s)) = r.get(x as usize) { return *s == wall; }
                    }
                    false
                };
                let los_clear = |x1: i64, y1: i64, x2: i64, y2: i64| -> bool {
                    let (dx, dy) = ((x2 - x1).abs(), (y2 - y1).abs());
                    let (sx, sy) = (if x1 < x2 { 1 } else { -1 }, if y1 < y2 { 1 } else { -1 });
                    let (mut x, mut y, mut err) = (x1, y1, dx - dy);
                    loop {
                        if (x, y) != (x1, y1) && (x, y) != (x2, y2) && is_wall(x, y) { return false; }
                        if x == x2 && y == y2 { return true; }
                        let e2 = 2 * err;
                        if e2 > -dy { err -= dy; x += sx; }
                        if e2 < dx { err += dx; y += sy; }
                    }
                };
                let mut out = Vec::new();
                for y in (y0 - rad)..=(y0 + rad) {
                    for x in (x0 - rad)..=(x0 + rad) {
                        if x >= 0 && y >= 0 && x < w && y < h
                            && !is_wall(x, y)
                            && los_clear(x0, y0, x, y) {
                            out.push(Value::List(im::vector![Value::Int(x), Value::Int(y)]));
                        }
                    }
                }
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            "maze_gen" => {
                // maze_gen(w, h, wall_ch) — recursive-backtracker maze on a canvas.
                // Odd grid: wall on even coords, corridors on odd.
                let (w, h, wall) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Int(w)), Some(Value::Int(h)), Some(Value::Str(ch)))
                        => (*w as usize, *h as usize, ch.clone()),
                    _ => return Err("maze_gen(w, h, wall_ch) expects 2 ints + str".into()),
                };
                let mut grid: Vec<Vec<bool>> = vec![vec![true; w]; h]; // true = wall
                let carve = |x: usize, y: usize| x % 2 == 1 && y % 2 == 1 && x < w && y < h;
                let mut stack = vec![(1usize, 1usize)];
                let mut rng = self.rng_state;
                while let Some(&(cx, cy)) = stack.last() {
                    grid[cy][cx] = false;
                    let mut dirs = [(0i64,-2i64), (0,2), (-2,0), (2,0)];
                    // shuffle dirs
                    for i in (1..4).rev() {
                        rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
                        let j = (rng % (i + 1) as u64) as usize;
                        dirs.swap(i, j);
                    }
                    let mut moved = false;
                    for (dx, dy) in dirs {
                        let (nx, ny) = ((cx as i64 + dx) as usize, (cy as i64 + dy) as usize);
                        if carve(nx, ny) && grid[ny][nx] {
                            grid[(cy as i64 + dy / 2) as usize][(cx as i64 + dx / 2) as usize] = false;
                            grid[ny][nx] = false;
                            stack.push((nx, ny));
                            moved = true;
                            break;
                        }
                    }
                    if !moved { stack.pop(); }
                }
                self.rng_state = rng;
                let cells: im::Vector<Value> = grid.iter().map(|row|
                    Value::List(row.iter().map(|&wall_cell|
                        Value::Str(if wall_cell { wall.clone() } else { " ".into() })).collect())
                ).collect();
                let mut d = im::HashMap::new();
                d.insert("w".to_string(), Value::Int(w as i64));
                d.insert("h".to_string(), Value::Int(h as i64));
                d.insert("cells".to_string(), Value::List(cells));
                Ok(Some(Value::Dict(d)))
            }
            "rand_gauss" => {
                // rand_gauss(mean, stddev) — Box-Muller on two uniform draws
                let (mean, sd) = match (args.get(0), args.get(1)) {
                    (Some(Value::Int(m)), Some(Value::Int(s))) => (*m as f64, *s as f64),
                    (Some(Value::Float(m)), Some(Value::Float(s))) => (*m, *s),
                    (Some(Value::Int(m)), Some(Value::Float(s))) => (*m as f64, *s),
                    (Some(Value::Float(m)), Some(Value::Int(s))) => (*m, *s as f64),
                    _ => return Err("rand_gauss(mean, stddev) expects two numbers".into()),
                };
                let mut u = || {
                    let mut x = self.rng_state;
                    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                    self.rng_state = x;
                    (x % 1_000_000) as f64 / 1_000_000.0
                };
                let (u1, u2) = (u().max(1e-9), u());
                let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
                Ok(Some(Value::Float(mean + sd * z)))
            }
            "fsm_next" => {
                // fsm_next(transitions, state, event) — dict-of-dicts FSM:
                // {state: {event: next_state}}. Returns next state or null.
                let (d, state, event) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(Value::Str(s)), Some(Value::Str(e)))
                        => (d, s.clone(), e.clone()),
                    _ => return Err("fsm_next(transitions, state, event) expects dict + 2 strs".into()),
                };
                let Some(Value::Dict(row)) = d.get(state.as_ref() as &str) else {
                    return Ok(Some(Value::Null));
                };
                match row.get(event.as_ref() as &str) {
                    Some(Value::Str(next)) => Ok(Some(Value::Str(next.clone()))),
                    Some(v) => Ok(Some(v.clone())),
                    None => Ok(Some(Value::Null)),
                }
            }
            "weighted_choice" => {
                // weighted_choice([[item, weight], ...]) — weighted random pick
                let Value::List(items) = args.get(0).cloned().unwrap_or(Value::Null) else {
                    return Err("weighted_choice(list) expects [[item, w], ...]".into())
                };
                if items.is_empty() { return Ok(Some(Value::Null)); }
                let mut total = 0f64;
                let mut weights = Vec::with_capacity(items.len());
                for it in items.iter() {
                    let Value::List(pair) = it else { return Err("weighted_choice expects [item, w] pairs".into()) };
                    if pair.len() != 2 { return Err("weighted_choice expects [item, w] pairs".into()) }
                    let w = match &pair[1] { Value::Int(i) => *i as f64, Value::Float(f) => *f, _ => 0.0 };
                    weights.push(w);
                    total += w;
                }
                if total <= 0.0 { return Ok(Some(Value::Null)); }
                let mut x = self.rng_state;
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                self.rng_state = x;
                let mut roll = (x % 1_000_000) as f64 / 1_000_000.0 * total;
                for (i, it) in items.iter().enumerate() {
                    roll -= weights[i];
                    if roll <= 0.0 {
                        if let Value::List(pair) = it { return Ok(Some(pair[0].clone())); }
                    }
                }
                if let Value::List(pair) = items.last().unwrap() { return Ok(Some(pair[0].clone())); }
                Ok(Some(Value::Null))
            }
            "roll" => {
                // roll("2d6+3") — dice notation: NdS+M / NdS-M / NdS
                let Some(Value::Str(s)) = args.get(0) else { return Err("roll(str) expects dice notation".into()) };
                let s = s.trim();
                let Some(dpos) = s.find('d') else { return Err("roll expects NdS+M".into()) };
                let n: i64 = s[..dpos].parse::<i64>().map_err(|_| "roll: bad dice count".to_string())?;
                let rest = &s[dpos + 1..];
                let (sides, modifier): (i64, i64) = if let Some(p) = rest.find('+') {
                    (rest[..p].parse::<i64>().map_err(|_| "roll: bad sides".to_string())?,
                     rest[p + 1..].parse::<i64>().map_err(|_| "roll: bad modifier".to_string())?)
                } else if let Some(m) = rest.rfind('-') {
                    (rest[..m].parse::<i64>().map_err(|_| "roll: bad sides".to_string())?,
                     -rest[m + 1..].parse::<i64>().map_err(|_| "roll: bad modifier".to_string())?)
                } else {
                    (rest.parse::<i64>().map_err(|_| "roll: bad sides".to_string())?, 0i64)
                };
                let mut total = 0i64;
                for _ in 0..n {
                    let mut x = self.rng_state;
                    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                    self.rng_state = x;
                    total += 1 + (x % (sides.max(1) as u64)) as i64;
                }
                Ok(Some(Value::Int(total + modifier)))
            }
            "rand_shuffle" => {
                // rand_shuffle(list) — Fisher-Yates shuffle, returns new list
                let Value::List(items) = args.get(0).cloned().unwrap_or(Value::Null) else {
                    return Err("rand_shuffle(list) expects a list".into())
                };
                let mut out: Vec<Value> = items.iter().cloned().collect();
                for i in (1..out.len()).rev() {
                    let mut x = self.rng_state;
                    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                    self.rng_state = x;
                    let j = (x % (i + 1) as u64) as usize;
                    out.swap(i, j);
                }
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            "astar_cost" => {
                // astar_cost(c, sx, sy, tx, ty, cost_dict) — weighted pathfind:
                // cost_dict maps cell char → step cost (missing = unwalkable).
                // Diagonal = 1.4x straight cost. Returns [[x,y],...] or null.
                let (d, sx, sy, tx, ty, costs) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(sx)), Some(Value::Int(sy)),
                     Some(Value::Int(tx)), Some(Value::Int(ty)), Some(Value::Dict(c)))
                        => (d, *sx, *sy, *tx, *ty, c.clone()),
                    _ => return Err("astar_cost(c, sx, sy, tx, ty, cost_dict) expects 2 dicts + 4 ints".into()),
                };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let Some(Value::List(cells)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let step_cost = |x: i64, y: i64| -> Option<i64> {
                    if x < 0 || y < 0 || x >= w || y >= h { return None; }
                    if let Value::List(r) = &cells[y as usize] {
                        if let Some(Value::Str(s)) = r.get(x as usize) {
                            return costs.get(s.as_ref() as &str)
                                .and_then(|v| match v { Value::Int(i) => Some(*i), Value::Float(f) => Some(*f as i64), _ => None });
                        }
                    }
                    None
                };
                let mut open: std::collections::BinaryHeap<(std::cmp::Reverse<i64>, i64, i64)> = std::collections::BinaryHeap::new();
                let mut g: std::collections::HashMap<(i64, i64), i64> = std::collections::HashMap::new();
                let mut prev: std::collections::HashMap<(i64, i64), (i64, i64)> = std::collections::HashMap::new();
                let h_of = |x: i64, y: i64| ((tx - x).abs() + (ty - y).abs()) * 10;
                g.insert((sx, sy), 0);
                open.push((std::cmp::Reverse(h_of(sx, sy)), sx, sy));
                while let Some((_, cx, cy)) = open.pop() {
                    if cx == tx && cy == ty { break; }
                    let cg = g[&(cx, cy)];
                    for (dx, dy, diag) in [(0,-1,false),(0,1,false),(-1,0,false),(1,0,false),(-1,-1,true),(1,-1,true),(-1,1,true),(1,1,true)] {
                        let (nx, ny) = (cx + dx, cy + dy);
                        let Some(sc) = step_cost(nx, ny).or(if nx == tx && ny == ty { Some(1) } else { None }) else { continue };
                        let ng = cg + sc * if diag { 14 } else { 10 };
                        if g.get(&(nx, ny)).map(|&v| ng < v).unwrap_or(true) {
                            g.insert((nx, ny), ng);
                            prev.insert((nx, ny), (cx, cy));
                            open.push((std::cmp::Reverse(ng + h_of(nx, ny)), nx, ny));
                        }
                    }
                }
                if !prev.contains_key(&(tx, ty)) && (sx, sy) != (tx, ty) {
                    return Ok(Some(Value::Null));
                }
                let mut path = Vec::new();
                let mut cur = (tx, ty);
                loop {
                    path.push(Value::List(im::vector![Value::Int(cur.0), Value::Int(cur.1)]));
                    if cur == (sx, sy) { break; }
                    cur = prev[&cur];
                }
                path.reverse();
                Ok(Some(Value::List(path.into_iter().collect())))
            }
            "turn" => {
                // turn(units) — initiative order: sort by unit["speed"] desc.
                // Each unit is a dict with a "speed" field.
                let Value::List(units) = args.get(0).cloned().unwrap_or(Value::Null) else {
                    return Err("turn(units) expects a list of dicts with .speed".into())
                };
                let mut sorted: Vec<Value> = units.iter().cloned().collect();
                sorted.sort_by(|a, b| {
                    let sa = if let Value::Dict(d) = a {
                        d.get("speed").and_then(|v| match v { Value::Int(i) => Some(*i), Value::Float(f) => Some(*f as i64), _ => None }).unwrap_or(0)
                    } else { 0 };
                    let sb = if let Value::Dict(d) = b {
                        d.get("speed").and_then(|v| match v { Value::Int(i) => Some(*i), Value::Float(f) => Some(*f as i64), _ => None }).unwrap_or(0)
                    } else { 0 };
                    sb.cmp(&sa)
                });
                Ok(Some(Value::List(sorted.into_iter().collect())))
            }
            "smoothstep" => {
                let (e0, e1, x) = (num_at(args, 0)?, num_at(args, 1)?, num_at(args, 2)?);
                let t = ((x - e0) / (e1 - e0).max(1e-9)).clamp(0.0, 1.0);
                Ok(Some(Value::Float(t * t * (3.0 - 2.0 * t))))
            }
            "line_points" => {
                // line_points(x1, y1, x2, y2) — Bresenham cells on the segment
                let (x1, y1, x2, y2) = (num_at(args, 0)? as i64, num_at(args, 1)? as i64,
                                       num_at(args, 2)? as i64, num_at(args, 3)? as i64);
                let (dx, dy) = ((x2 - x1).abs(), (y2 - y1).abs());
                let (sx, sy) = (if x1 < x2 { 1 } else { -1 }, if y1 < y2 { 1 } else { -1 });
                let (mut x, mut y, mut err) = (x1, y1, dx - dy);
                let mut out = Vec::new();
                loop {
                    out.push(Value::List(im::vector![Value::Int(x), Value::Int(y)]));
                    if x == x2 && y == y2 { break; }
                    let e2 = 2 * err;
                    if e2 > -dy { err -= dy; x += sx; }
                    if e2 < dx { err += dx; y += sy; }
                }
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            "bezier" => {
                // bezier(t, p0x,p0y, p1x,p1y, p2x,p2y, p3x,p3y) — cubic bezier [x,y]
                let t = num_at(args, 0)?;
                let (p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y) = (
                    num_at(args, 1)?, num_at(args, 2)?, num_at(args, 3)?, num_at(args, 4)?,
                    num_at(args, 5)?, num_at(args, 6)?, num_at(args, 7)?, num_at(args, 8)?);
                let u = 1.0 - t;
                let x = u*u*u*p0x + 3.0*u*u*t*p1x + 3.0*u*t*t*p2x + t*t*t*p3x;
                let y = u*u*u*p0y + 3.0*u*u*t*p1y + 3.0*u*t*t*p2y + t*t*t*p3y;
                Ok(Some(Value::List(im::vector![Value::Float(x), Value::Float(y)])))
            }
            "point_in_poly" => {
                // point_in_poly(x, y, [[px,py],...]) — even-odd ray cast
                let (px, py) = (num_at(args, 0)?, num_at(args, 1)?);
                let Value::List(pts) = args.get(2).cloned().unwrap_or(Value::Null) else {
                    return Err("point_in_poly(x, y, [[px,py],...]) expects a point list".into())
                };
                let mut inside = false;
                let n = pts.len();
                let f_of = |v: &Value| match v { Value::Int(i) => *i as f64, Value::Float(f) => *f, _ => 0.0 };
                for i in 0..n {
                    let (Value::List(a), Value::List(b)) = (&pts[i], &pts[(i + 1) % n]) else { continue };
                    let (ax, ay) = (f_of(&a[0]), f_of(&a[1]));
                    let (bx, by) = (f_of(&b[0]), f_of(&b[1]));
                    if (ay > py) != (by > py)
                        && px < (bx - ax) * (py - ay) / (by - ay) + ax {
                        inside = !inside;
                    }
                }
                Ok(Some(Value::Bool(inside)))
            }
            "convex_hull" => {
                // convex_hull([[x,y],...]) — Andrew's monotone chain → hull
                let Value::List(pts) = args.get(0).cloned().unwrap_or(Value::Null) else {
                    return Err("convex_hull([[x,y],...]) expects a point list".into())
                };
                let f_of = |v: &Value| match v { Value::Int(i) => *i as f64, Value::Float(f) => *f, _ => 0.0 };
                let mut p: Vec<(f64, f64)> = pts.iter().filter_map(|v| {
                    if let Value::List(pair) = v {
                        if pair.len() == 2 { return Some((f_of(&pair[0]), f_of(&pair[1]))); }
                    }
                    None
                }).collect();
                p.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                p.dedup();
                if p.len() <= 1 {
                    return Ok(Some(Value::List(pts.iter().cloned().collect::<Vec<Value>>().into_iter().collect())));
                }
                let cross = |o: &(f64, f64), a: &(f64, f64), b: &(f64, f64)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
                let mut hull: Vec<(f64, f64)> = Vec::new();
                // lower hull: left→right
                for &pt in &p {
                    while hull.len() >= 2 && cross(&hull[hull.len() - 2], &hull[hull.len() - 1], &pt) <= 0.0 {
                        hull.pop();
                    }
                    hull.push(pt);
                }
                // upper hull: right→left, bounded by the lower hull's length
                let lower_len = hull.len();
                for &pt in p.iter().rev().skip(1) {
                    while hull.len() > lower_len && cross(&hull[hull.len() - 2], &hull[hull.len() - 1], &pt) <= 0.0 {
                        hull.pop();
                    }
                    hull.push(pt);
                }
                if !hull.is_empty() { hull.pop(); } // duplicated start
                Ok(Some(Value::List(hull.into_iter()
                    .map(|(x, y)| Value::List(im::vector![Value::Float(x), Value::Float(y)])).collect())))
            }
            "fbm" => {
                // fbm(x, y, octaves) — fractal Brownian motion: octaves of
                // noise2, halving amplitude, doubling frequency. In [-1,1]-ish.
                let (x, y, octaves) = (num_at(args, 0)?, num_at(args, 1)?,
                    args.get(2).and_then(|v| if let Value::Int(i) = v { Some(*i as usize) } else { None }).unwrap_or(4));
                let hash = |ix: i64, iy: i64| -> f64 {
                    let mut n = ix.wrapping_mul(374761393).wrapping_add(iy.wrapping_mul(668265263));
                    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
                    ((n ^ (n >> 16)) as u64 & 0xffffff) as f64 / 0xffffff as f64
                };
                let s = |t: f64| t * t * (3.0 - 2.0 * t);
                let noise = |x: f64, y: f64| -> f64 {
                    let (ix, iy) = (x.floor() as i64, y.floor() as i64);
                    let (fx, fy) = (x - x.floor(), y - y.floor());
                    let (a, b, c, d) = (hash(ix, iy), hash(ix + 1, iy), hash(ix, iy + 1), hash(ix + 1, iy + 1));
                    a + (b - a) * s(fx) + (c - a) * s(fy) + (a - b - c + d) * s(fx) * s(fy)
                };
                let (mut total, mut amp, mut freq, mut max) = (0f64, 1f64, 1f64, 0f64);
                for _ in 0..octaves {
                    total += noise(x * freq, y * freq) * amp;
                    max += amp;
                    amp *= 0.5;
                    freq *= 2.0;
                }
                Ok(Some(Value::Float(if max > 0.0 { total / max } else { 0.0 })))
            }
            "canvas_circle" => {
                // canvas_circle(c, cx, cy, r, ch) — midpoint circle
                let (d, cx, cy, r, ch) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4)) {
                    (Some(Value::Dict(d)), Some(Value::Int(cx)), Some(Value::Int(cy)),
                     Some(Value::Int(r)), Some(Value::Str(ch)))
                        => (d.clone(), *cx, *cy, *r, ch.clone()),
                    _ => return Err("canvas_circle(c, cx, cy, r, ch) expects dict + 3 ints + str".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let (w, h) = (rows.get(0).and_then(|r| if let Value::List(r) = r { Some(r.len()) } else { None }), rows.len());
                let Some(w) = w else { return Err("canvas: missing w".into()) };
                let (mut x, mut y, mut err) = (-r, 0i64, 2 - 2 * r);
                loop {
                    for (px, py) in [(cx + x, cy + y), (cx - y, cy - x), (cx - x, cy - y), (cx + y, cy + x),
                                     (cx + x, cy - y), (cx - y, cy + x), (cx - x, cy + y), (cx + y, cy - x)] {
                        if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                            if let Value::List(r) = &rows2[py as usize] {
                                let mut nr = r.clone();
                                nr[px as usize] = Value::Str(ch.clone());
                                rows2[py as usize] = Value::List(nr);
                            }
                        }
                    }
                    if x >= 0 { break; }
                    let r_err = err;
                    if r_err <= y { err += { y += 1; y * 2 + 1 }; }
                    if r_err > x || err > y { err += { x += 1; x * 2 + 1 }; }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "canvas_rect" => {
                // canvas_rect(c, x, y, w, h, ch, fill?) — rect outline or filled
                let (d, x0, y0, w, h, ch, fill) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5), args.get(6)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::Int(w)), Some(Value::Int(h)), Some(Value::Str(ch)), f)
                        => (d.clone(), *x as usize, *y as usize, *w as usize, *h as usize, ch.clone(),
                            matches!(f, Some(Value::Bool(true)))),
                    _ => return Err("canvas_rect(c, x, y, w, h, ch, fill?) expects dict + 4 ints + str + bool".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                for yy in y0..(y0 + h).min(rows2.len()) {
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for xx in x0..(x0 + w).min(nr.len()) {
                            let edge = xx == x0 || xx == x0 + w - 1 || yy == y0 || yy == y0 + h - 1;
                            if fill || edge { nr[xx] = Value::Str(ch.clone()); }
                        }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "ui_bar" => {
                // ui_bar(c, x, y, w, cur, max, fill_ch?) — [|||||   ] gauge
                let (d, x0, y0, w, cur, max, ch) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5), args.get(6)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::Int(w)), Some(Value::Int(cur)), Some(Value::Int(max)), c)
                        => (d.clone(), *x as usize, *y as usize, *w as usize, *cur, *max,
                            if let Some(Value::Str(s)) = c { s.clone() } else { "|".into() }),
                    _ => return Err("ui_bar(c, x, y, w, cur, max, ch?) expects dict + 4 ints + str".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let filled = if max > 0 { ((cur.max(0) as f64 / max.max(1) as f64) * w as f64) as usize } else { 0 };
                if y0 < rows2.len() {
                    if let Value::List(r) = &rows2[y0] {
                        let mut nr = r.clone();
                        for i in 0..w {
                            if x0 + i < nr.len() {
                                nr[x0 + i] = Value::Str(if i < filled { ch.clone() } else { " ".into() });
                            }
                        }
                        rows2[y0] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "game_new" => {
                // game_new(w, h) → a game-state dict: world dims + camera
                let (w, h) = match (args.get(0), args.get(1)) {
                    (Some(Value::Int(w)), Some(Value::Int(h))) => (*w as f64, *h as f64),
                    _ => return Err("game_new(w, h) expects two ints".into()),
                };
                let mut d = im::HashMap::new();
                d.insert("w".into(), Value::Float(w));
                d.insert("h".into(), Value::Float(h));
                d.insert("camera_x".into(), Value::Float(0.0));
                d.insert("camera_y".into(), Value::Float(0.0));
                d.insert("t".into(), Value::Int(0));
                Ok(Some(Value::Dict(d)))
            }
            "entity" => {
                // entity(name, x, y, sprite) → an entity dict {name,x,y,sprite,vx,vy,tag}
                let (name, x, y, sprite) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Str(n)), Some(x), Some(y), Some(Value::Str(s))) =>
                        (n.clone(), Self::num_f(x)?, Self::num_f(y)?, s.clone()),
                    _ => return Err("entity(name, x, y, sprite) expects str + 2 nums + str".into()),
                };
                let mut d = im::HashMap::new();
                d.insert("name".into(), Value::Str(name));
                d.insert("x".into(), Value::Float(x));
                d.insert("y".into(), Value::Float(y));
                d.insert("sprite".into(), Value::Str(sprite));
                d.insert("vx".into(), Value::Float(0.0));
                d.insert("vy".into(), Value::Float(0.0));
                d.insert("tag".into(), Value::Str("".into()));
                Ok(Some(Value::Dict(d)))
            }
            "ent_move" => {
                // ent_move(e, dx, dy) → entity moved by dx,dy
                let (d, dx, dy) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(x), Some(y)) =>
                        (d.clone(), Self::num_f(x)?, Self::num_f(y)?),
                    _ => return Err("ent_move(e, dx, dy) expects dict + 2 nums".into()),
                };
                let mut d2 = d.clone();
                let (nx, ny) = (Self::num_f(d.get("x").unwrap_or(&Value::Int(0)))? + dx,
                                Self::num_f(d.get("y").unwrap_or(&Value::Int(0)))? + dy);
                d2.insert("x".into(), Value::Float(nx));
                d2.insert("y".into(), Value::Float(ny));
                Ok(Some(Value::Dict(d2)))
            }
            "ent_vel" => {
                // ent_vel(e, vx, vy) → entity with velocity set
                let (d, vx, vy) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(x), Some(y)) =>
                        (d.clone(), Self::num_f(x)?, Self::num_f(y)?),
                    _ => return Err("ent_vel(e, vx, vy) expects dict + 2 nums".into()),
                };
                let mut d2 = d.clone();
                d2.insert("vx".into(), Value::Float(vx));
                d2.insert("vy".into(), Value::Float(vy));
                Ok(Some(Value::Dict(d2)))
            }
            "anim" => {
                // anim(e, frames) → entity with a sprite-frame list +
                // a cycling `frame` index — animated sprites on /play
                let (d, frames) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(d)), Some(Value::List(f))) => (d.clone(), f.clone()),
                    _ => return Err("anim(e, frames) expects dict + list".into()),
                };
                let mut d2 = d.clone();
                d2.insert("anim_frames".into(), Value::List(frames));
                d2.insert("frame".into(), Value::Int(0));
                Ok(Some(Value::Dict(d2)))
            }
            "anim_tick" => {
                // anim_tick(e) → advance the frame index (wrap around)
                let d = match args.get(0) {
                    Some(Value::Dict(d)) => d.clone(),
                    _ => return Err("anim_tick(e) expects an entity dict".into()),
                };
                let n = match d.get("anim_frames") {
                    Some(Value::List(f)) => f.len() as i64, _ => 0 };
                let mut d2 = d.clone();
                if n > 0 {
                    let cur = match d.get("frame") { Some(Value::Int(i)) => *i, _ => 0 };
                    d2.insert("frame".into(), Value::Int((cur + 1) % n));
                    if let Some(Value::List(f)) = d.get("anim_frames") {
                        if let Some(sp) = f.get((cur as usize + 1) % f.len()) {
                            d2.insert("sprite".into(), sp.clone());
                        }
                    }
                }
                Ok(Some(Value::Dict(d2)))
            }
            "collide" => {
                // collide(a, b) → AABB overlap — cells share the same ~1-tile space
                let (a, b) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(a)), Some(Value::Dict(b))) => (a, b),
                    _ => return Err("collide(a, b) expects two entity dicts".into()),
                };
                let ax = Self::num_f(a.get("x").unwrap_or(&Value::Int(0)))?;
                let ay = Self::num_f(a.get("y").unwrap_or(&Value::Int(0)))?;
                let bx = Self::num_f(b.get("x").unwrap_or(&Value::Int(0)))?;
                let by = Self::num_f(b.get("y").unwrap_or(&Value::Int(0)))?;
                let hit = (ax - bx).abs() < 1.0 && (ay - by).abs() < 1.0;
                Ok(Some(Value::Bool(hit)))
            }
            "tile_at" => {
                // tile_at(map, x, y) → the cell char at floor(x), floor(y)
                let (d, x, y) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(x), Some(y)) =>
                        (d, Self::num_f(x)? as i64, Self::num_f(y)? as i64),
                    _ => return Err("tile_at(map, x, y) expects dict + 2 nums".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Ok(Some(Value::Str(" ".into()))) };
                if y < 0 || y as usize >= rows.len() { return Ok(Some(Value::Str("#".into()))) }
                let Value::List(r) = &rows[y as usize] else { return Ok(Some(Value::Str("#".into()))) };
                if x < 0 || x as usize >= r.len() { return Ok(Some(Value::Str("#".into()))) }
                Ok(Some(r[x as usize].clone()))
            }
            "tile_solid" => {
                // tile_solid(map, x, y) → is the cell at floor(x),floor(y) solid?
                // Solid = not floor-space — walls, platforms, terrain
                let (d, x, y) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(x), Some(y)) =>
                        (d, Self::num_f(x)? as i64, Self::num_f(y)? as i64),
                    _ => return Err("tile_solid(map, x, y) expects dict + 2 nums".into()),
                };
                Ok(Some(Value::Bool(Self::cell_solid(d, x, y))))
            }
            "collide_tile" => {
                // collide_tile(e, map) → is the entity inside a solid cell?
                let (e, d) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(e)), Some(Value::Dict(d))) => (e, d),
                    _ => return Err("collide_tile(e, map) expects entity dict + map dict".into()),
                };
                let ex = Self::num_f(e.get("x").unwrap_or(&Value::Int(0)))? as i64;
                let ey = Self::num_f(e.get("y").unwrap_or(&Value::Int(0)))? as i64;
                Ok(Some(Value::Bool(Self::cell_solid(d, ex, ey))))
            }
            "on_ground" => {
                // on_ground(e, map) → the cell directly below the entity is solid
                let (e, d) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(e)), Some(Value::Dict(d))) => (e, d),
                    _ => return Err("on_ground(e, map) expects entity dict + map dict".into()),
                };
                let ex = Self::num_f(e.get("x").unwrap_or(&Value::Int(0)))? as i64;
                let ey = Self::num_f(e.get("y").unwrap_or(&Value::Int(0)))? as i64;
                Ok(Some(Value::Bool(Self::cell_solid(d, ex, ey + 1))))
            }
            "physics_tick" => {
                // physics_tick(e, map, grav) — apply gravity to e.vy, move e.y,
                // resolve tile collision (land = zero vy + clamp above the cell).
                // Returns the ticked entity dict.
                let (e, d, grav) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(e)), Some(Value::Dict(d)), Some(g)) =>
                        (e, d, Self::num_f(g)?),
                    _ => return Err("physics_tick(e, map, grav) expects dict + dict + num".into()),
                };
                let mut e2 = e.clone();
                let vy0 = Self::num_f(e.get("vy").unwrap_or(&Value::Int(0)))?;
                let ey0 = Self::num_f(e.get("y").unwrap_or(&Value::Int(0)))?;
                let ex = Self::num_f(e.get("x").unwrap_or(&Value::Int(0)))? as i64;
                // resting on the ground = cell below solid + not launching up —
                // don't accumulate gravity (kills the rest-bounce)
                let resting = vy0 <= 0.0
                    && Self::cell_solid(d, ex, ey0 as i64 + 1);
                let mut vy = if resting { 0.0 } else { vy0 + grav };
                let mut ey = ey0 + vy;
                if vy > 0.0 {
                    // falling — scan the traversed rows for the FIRST solid
                    // cell; land on the open row above it (no tunneling)
                    let mut r = ey0 as i64 + 1;
                    while r <= ey as i64 {
                        if Self::cell_solid(d, ex, r) {
                            vy = 0.0;
                            ey = (r - 1) as f64;
                            break;
                        }
                        r += 1;
                    }
                } else if vy < 0.0 {
                    // rising — scan up for the first solid cell; stop just
                    // below it
                    let mut r = ey0 as i64 - 1;
                    while r >= ey as i64 {
                        if Self::cell_solid(d, ex, r) {
                            vy = 0.0;
                            ey = (r + 1) as f64;
                            break;
                        }
                        r -= 1;
                    }
                }
                e2.insert("y".into(), Value::Float(ey));
                e2.insert("vy".into(), Value::Float(vy));
                Ok(Some(Value::Dict(e2)))
            }
            "scene_new" => {
                // scene_new(w, h) → a scene dict {w, h, entities: {}} —
                // managed entity container for a 2D game
                let (w, h) = match (args.get(0), args.get(1)) {
                    (Some(Value::Int(w)), Some(Value::Int(h))) => (*w as f64, *h as f64),
                    _ => return Err("scene_new(w, h) expects two ints".into()),
                };
                let mut d = im::HashMap::new();
                d.insert("w".into(), Value::Float(w));
                d.insert("h".into(), Value::Float(h));
                d.insert("camera_x".into(), Value::Float(0.0));
                d.insert("camera_y".into(), Value::Float(0.0));
                d.insert("t".into(), Value::Int(0));
                d.insert("entities".into(), Value::Dict(im::HashMap::new()));
                Ok(Some(Value::Dict(d)))
            }
            "scene_spawn" => {
                // scene_spawn(scene, e) → add entity e to the scene
                let (d, e) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(d)), Some(Value::Dict(e))) => (d.clone(), e.clone()),
                    _ => return Err("scene_spawn(scene, e) expects dict + entity dict".into()),
                };
                let name = match e.get("name") { Some(Value::Str(n)) => n.to_string(), _ => return Err("entity needs a name".into()) };
                let mut d2 = d.clone();
                let mut ents = match d.get("entities") {
                    Some(Value::Dict(m)) => m.clone(), _ => im::HashMap::new() };
                ents.insert(name.into(), Value::Dict(e));
                d2.insert("entities".into(), Value::Dict(ents));
                Ok(Some(Value::Dict(d2)))
            }
            "scene_despawn" => {
                // scene_despawn(scene, name) → remove the named entity
                let (d, name) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(d)), Some(Value::Str(n))) => (d.clone(), n.to_string()),
                    _ => return Err("scene_despawn(scene, name) expects dict + str".into()),
                };
                let mut d2 = d.clone();
                let mut ents = match d.get("entities") {
                    Some(Value::Dict(m)) => m.clone(), _ => im::HashMap::new() };
                ents.remove(&*name);
                d2.insert("entities".into(), Value::Dict(ents));
                Ok(Some(Value::Dict(d2)))
            }
            "scene_entities" => {
                // scene_entities(scene) → list of entity dicts (for game_frame)
                let d = match args.get(0) {
                    Some(Value::Dict(d)) => d.clone(),
                    _ => return Err("scene_entities(scene) expects a scene dict".into()),
                };
                let ents: im::Vector<Value> = match d.get("entities") {
                    Some(Value::Dict(m)) => m.values().cloned().collect(),
                    _ => im::Vector::new() };
                Ok(Some(Value::List(ents)))
            }
            "sfx" => {
                // sfx(name) → queue a sound event for this frame — the
                // browser plays it via WebAudio; CLI mode rings a bell
                let name = match args.get(0) {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => return Err("sfx(name) expects a string".into()),
                };
                match &self.game_io {
                    Some(io) => { io.sfx.lock().unwrap().push(name); }
                    None => {
                        if let Some(buf) = &mut self.print_buffer {
                            buf.push_str(&format!("[sfx:{}]", name));
                        }
                    }
                }
                Ok(Some(Value::Null))
            }
            "beep" => {
                // beep(freq, ms) → a terminal bell / tone for CLI games
                let (freq, ms) = match (args.get(0), args.get(1)) {
                    (Some(f), Some(m)) => (Self::num_f(f)?, Self::num_f(m)?),
                    _ => return Err("beep(freq, ms) expects two numbers".into()),
                };
                let _ = (freq, ms);
                if let Some(buf) = &mut self.print_buffer {
                    buf.push('\x07');
                } else { print!("\x07"); }
                Ok(Some(Value::Null))
            }
            "scene_find" => {
                // scene_find(scene, kind) → entities of that kind — the
                // sprite name is the type ("hunter", "coin"); also matches
                // an explicit tag field
                let (d, tag) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(d)), Some(Value::Str(t))) => (d.clone(), t.to_string()),
                    _ => return Err("scene_find(scene, kind) expects dict + str".into()),
                };
                let matches_kind = |ed: &im::HashMap<String, Value>| {
                    ["tag", "sprite"].iter().any(|f| matches!(
                        ed.get(*f), Some(Value::Str(ts)) if ts.as_ref() == tag))
                };
                let out: im::Vector<Value> = match d.get("entities") {
                    Some(Value::Dict(m)) => m.values().filter(|v| {
                        matches!(v, Value::Dict(ed) if matches_kind(ed))
                    }).cloned().collect(),
                    _ => im::Vector::new() };
                Ok(Some(Value::List(out)))
            }
            "game_over" => {
                // game_over(title, score) → a hud dict marking the game
                // ended — the browser renders a win/lose overlay
                let (title, score) = match (args.get(0), args.get(1)) {
                    (Some(Value::Str(t)), Some(s)) => (t.to_string(), Self::json_val(s)),
                    _ => return Err("game_over(title, score) expects str + any".into()),
                };
                let mut hud = im::HashMap::new();
                let mut ov = im::HashMap::new();
                ov.insert("title".into(), Value::Str(title.into()));
                ov.insert("score".into(), Value::Str(format!("{}", score).into()));
                hud.insert("overlay".into(), Value::Dict(ov));
                hud.insert("done".into(), Value::Bool(true));
                Ok(Some(Value::Dict(hud)))
            }
            "ui_menu" => {
                // ui_menu(title, items) → a hud dict carrying a menu the
                // browser renders; keys() up/down/enter select
                let (title, items) = match (args.get(0), args.get(1)) {
                    (Some(Value::Str(t)), Some(Value::List(items))) =>
                        (t.to_string(), items.clone()),
                    _ => return Err("ui_menu(title, items) expects str + list".into()),
                };
                let mut hud = im::HashMap::new();
                let mut m = im::HashMap::new();
                m.insert("title".into(), Value::Str(title.into()));
                m.insert("items".into(), Value::List(items));
                m.insert("sel".into(), Value::Int(0));
                hud.insert("menu".into(), Value::Dict(m));
                Ok(Some(Value::Dict(hud)))
            }
            "ui_prompt" => {
                // ui_prompt(text) → a hud dict carrying a centered prompt
                let text = match args.get(0) {
                    Some(Value::Str(t)) => t.to_string(),
                    _ => return Err("ui_prompt(text) expects a string".into()),
                };
                let mut hud = im::HashMap::new();
                hud.insert("prompt".into(), Value::Str(text.into()));
                Ok(Some(Value::Dict(hud)))
            }
            "keys" => {
                // keys() → list of held keys from the session input queue
                let pressed: Vec<String> = self.game_io.as_ref()
                    .map(|io| io.keys.lock().unwrap().clone())
                    .unwrap_or_default();
                Ok(Some(Value::List(pressed.into_iter()
                    .map(|s| Value::Str(s.into())).collect())))
            }
            "game_frame" => {
                // game_frame(state, entities, hud, map?) → emit the scene JSON to
                // the session frame buffer (or print it in CLI mode). The
                // optional map is a canvas dict — its cells become "map" rows
                // so the browser can paint terrain under the entities.
                let (state, entities, hud) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(s)), Some(Value::List(e)), Some(h)) =>
                        (s, e.clone(), h),
                    _ => return Err("game_frame(state, entities, hud, map?) expects dict + list + any".into()),
                };
                let map_rows: serde_json::Value = match args.get(3) {
                    Some(Value::Dict(md)) => match md.get("cells") {
                        Some(Value::List(rows)) => serde_json::json!(rows.iter().map(|r| {
                            if let Value::List(row) = r {
                                row.iter().map(|c| match c {
                                    Value::Str(s) => s.to_string(),
                                    _ => "?".into(),
                                }).collect::<String>()
                            } else { String::new() }
                        }).collect::<Vec<_>>()),
                        _ => serde_json::Value::Null,
                    },
                    _ => serde_json::Value::Null,
                };
                // drain any queued sfx into this frame — the browser plays them
                let sfx_events: Vec<String> = self.game_io.as_ref()
                    .map(|io| std::mem::take(&mut *io.sfx.lock().unwrap()))
                    .unwrap_or_default();
                let scene = serde_json::json!({
                    "w": state.get("w").map(Self::json_num).unwrap_or(0.0),
                    "h": state.get("h").map(Self::json_num).unwrap_or(0.0),
                    "camera_x": state.get("camera_x").map(Self::json_num).unwrap_or(0.0),
                    "camera_y": state.get("camera_y").map(Self::json_num).unwrap_or(0.0),
                    "entities": entities.iter().map(Self::entity_json).collect::<Vec<_>>(),
                    "map": map_rows,
                    "sfx": sfx_events,
                    "hud": match hud { Value::Dict(hd) => hd.iter()
                        .map(|(k,v)| (k.clone(), Self::json_val(v)))
                        .collect::<serde_json::Map<_,_>>().into(),
                        _ => serde_json::json!(hud) },
                });
                let line = scene.to_string();
                match &self.game_io {
                    Some(io) => {
                        *io.frame.lock().unwrap() = Some(line);
                        Ok(Some(Value::Null))
                    }
                    None => {
                        if let Some(buf) = &mut self.print_buffer {
                            buf.push_str(&line); buf.push('\n');
                        } else {
                            println!("{}", line);
                        }
                        Ok(Some(Value::Null))
                    }
                }
            }
            "game_running" => {
                // game_running() → bool — the session is alive
                let r = self.game_io.as_ref()
                    .map(|io| io.running.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(true);
                Ok(Some(Value::Bool(r)))
            }
            "camera_xy" => {
                // camera_xy(state, x, y) → state with the camera offset set
                let (d, x, y) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(x), Some(y)) =>
                        (d.clone(), Self::num_f(x)?, Self::num_f(y)?),
                    _ => return Err("camera_xy(state, x, y) expects dict + 2 nums".into()),
                };
                let mut d2 = d.clone();
                d2.insert("camera_x".into(), Value::Float(x));
                d2.insert("camera_y".into(), Value::Float(y));
                Ok(Some(Value::Dict(d2)))
            }
            "canvas_ellipse" => {
                // canvas_ellipse(c, cx, cy, rx, ry, ch) — parametric ellipse
                let (d, cx, cy, rx, ry, ch) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(cx)), Some(Value::Int(cy)),
                     Some(Value::Int(rx)), Some(Value::Int(ry)), Some(Value::Str(ch)))
                        => (d.clone(), *cx, *cy, *rx as f64, *ry as f64, ch.clone()),
                    _ => return Err("canvas_ellipse(c, cx, cy, rx, ry, ch) expects dict + 4 ints + str".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let (w, h) = (rows.get(0).and_then(|r| if let Value::List(r) = r { Some(r.len()) } else { None }), rows.len());
                let Some(w) = w else { return Err("canvas: missing w".into()) };
                for t in 0..360 {
                    let rad = (t as f64).to_radians();
                    let (px, py) = ((cx as f64 + rx * rad.cos()) as i64, (cy as f64 + ry * rad.sin()) as i64);
                    if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        if let Value::List(r) = &rows2[py as usize] {
                            let mut nr = r.clone();
                            nr[px as usize] = Value::Str(ch.clone());
                            rows2[py as usize] = Value::List(nr);
                        }
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "canvas_triangle" => {
                // canvas_triangle(c, x1,y1, x2,y2, x3,y3, ch) — three edges
                let mut d = match args.get(0) { Some(Value::Dict(d)) => d.clone(), _ => return Err("canvas_triangle expects a canvas".into()) };
                for &(a, b) in &[(1usize, 3usize), (3usize, 5usize), (5usize, 1usize)] {
                    let (x1, y1, x2, y2) = match (args.get(a), args.get(a + 1), args.get(b), args.get(b + 1)) {
                        (Some(Value::Int(a1)), Some(Value::Int(b1)), Some(Value::Int(c1)), Some(Value::Int(d1)))
                            => (*a1, *b1, *c1, *d1),
                        _ => return Err("canvas_triangle expects 6 int coords".into()),
                    };
                    let Some(Value::Str(ch)) = args.get(7) else { return Err("canvas_triangle expects a char".into()) };
                    // reuse the line logic inline
                    if let Some(Value::List(rows)) = d.get("cells") {
                        let mut rows2 = rows.clone();
                        let (dx, dy) = ((x2 - x1).abs(), (y2 - y1).abs());
                        let (sx, sy) = (if x1 < x2 { 1 } else { -1 }, if y1 < y2 { 1 } else { -1 });
                        let (mut x, mut y, mut err) = (x1, y1, dx - dy);
                        loop {
                            if x >= 0 && y >= 0 && (x as usize) < rows2.get(0).and_then(|r| if let Value::List(r) = r { Some(r.len()) } else { None }).unwrap_or(0) && (y as usize) < rows2.len() {
                                if let Value::List(r) = &rows2[y as usize] {
                                    let mut nr = r.clone();
                                    nr[x as usize] = Value::Str(ch.clone());
                                    rows2[y as usize] = Value::List(nr);
                                }
                            }
                            if x == x2 && y == y2 { break; }
                            let e2 = 2 * err;
                            if e2 > -dy { err -= dy; x += sx; }
                            if e2 < dx { err += dx; y += sy; }
                        }
                        d.insert("cells".to_string(), Value::List(rows2));
                    }
                }
                Ok(Some(Value::Dict(d)))
            }
            "rand_bool" => {
                let mut x = self.rng_state;
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                self.rng_state = x;
                Ok(Some(Value::Bool(x % 2 == 0)))
            }
            "poisson_disc" => {
                // poisson_disc(w, h, min_dist, seed?) — Bridson's algorithm:
                // even blue-noise point sampling for spawn placement.
                let (w, h, r) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Int(w)), Some(Value::Int(h)), Some(Value::Int(r)))
                        => (*w as usize, *h as usize, *r as f64),
                    _ => return Err("poisson_disc(w, h, r) expects 3 ints".into()),
                };
                let cell = r / std::f64::consts::SQRT_2;
                let (gw, gh) = ((w as f64 / cell).ceil() as usize + 1, (h as f64 / cell).ceil() as usize + 1);
                let mut grid: Vec<Vec<bool>> = vec![vec![false; gw]; gh];
                let mut points: Vec<(f64, f64)> = Vec::new();
                let mut active: Vec<(f64, f64)> = Vec::new();
                let mut rng = self.rng_state;
                let mut next = || {
                    rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
                    (rng % 1_000_000) as f64 / 1_000_000.0
                };
                let start = (next() * w as f64, next() * h as f64);
                points.push(start);
                active.push(start);
                grid[(start.1 / cell) as usize][(start.0 / cell) as usize] = true;
                while let Some(&p) = active.last() {
                    let mut found = false;
                    for _ in 0..30 {
                        let ang = next() * 2.0 * std::f64::consts::PI;
                        let rad = r * (1.0 + next());
                        let (nx, ny) = (p.0 + rad * ang.cos(), p.1 + rad * ang.sin());
                        if nx < 0.0 || ny < 0.0 || nx >= w as f64 || ny >= h as f64 { continue; }
                        let (gx, gy) = ((nx / cell) as usize, (ny / cell) as usize);
                        let mut ok = true;
                        for dy in -1..=1i64 {
                            for dx in -1..=1i64 {
                                let (cx, cy) = (gx as i64 + dx, gy as i64 + dy);
                                if cx >= 0 && cy >= 0 && (cx as usize) < gw && (cy as usize) < gh && grid[cy as usize][cx as usize] {
                                    // check actual distance to a nearby point — too coarse for the grid,
                                    // just check the cell is occupied
                                    ok = false;
                                }
                            }
                        }
                        if ok {
                            // verify no point is actually within r (grid cell check is approximate)
                            let mut too_close = false;
                            for &(px, py) in &points {
                                if (px - nx).powi(2) + (py - ny).powi(2) < r * r { too_close = true; break; }
                            }
                            if !too_close {
                                points.push((nx, ny));
                                active.push((nx, ny));
                                grid[gy][gx] = true;
                                found = true;
                                break;
                            }
                        }
                    }
                    if !found { active.pop(); }
                }
                self.rng_state = rng;
                Ok(Some(Value::List(points.into_iter()
                    .map(|(x, y)| Value::List(im::vector![Value::Int(x as i64), Value::Int(y as i64)])).collect())))
            }
            "ui_menu" => {
                // ui_menu(c, x, y, items, selected) — list with > on selected
                let (d, x0, y0, items, sel) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::List(items)), Some(Value::Int(s)))
                        => (d.clone(), *x as usize, *y as usize, items.clone(), *s as usize),
                    _ => return Err("ui_menu(c, x, y, items, sel) expects dict + 2 ints + list + int".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                for (i, it) in items.iter().enumerate() {
                    let yy = y0 + i;
                    if yy >= rows2.len() { break; }
                    let label = if let Value::Str(s) = it { s.clone() } else { continue };
                    let rendered = if i == sel { format!("> {}", label) } else { format!("  {}", label) };
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for (dx, ch) in rendered.chars().enumerate() {
                            if x0 + dx < nr.len() { nr[x0 + dx] = Value::Str(ch.to_string().into()); }
                        }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "ui_progress" => {
                // ui_progress(c, x, y, w, cur, max) — [====>   ] bar
                let (d, x0, y0, w, cur, max) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::Int(w)), Some(Value::Int(cur)), Some(Value::Int(max)))
                        => (d.clone(), *x as usize, *y as usize, *w as usize, *cur, *max),
                    _ => return Err("ui_progress(c, x, y, w, cur, max) expects dict + 4 ints".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let filled = if max > 0 { ((cur.max(0) as f64 / max.max(1) as f64) * w as f64) as usize } else { 0 };
                if y0 < rows2.len() {
                    if let Value::List(r) = &rows2[y0] {
                        let mut nr = r.clone();
                        for i in 0..w {
                            if x0 + i < nr.len() {
                                nr[x0 + i] = Value::Str(if i < filled {
                                    if i == filled - 1 { ">".into() } else { "=".into() }
                                } else { " ".into() });
                            }
                        }
                        rows2[y0] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "perlin" => {
                // perlin(x, y) — proper 2D Perlin gradient noise in [-1,1].
                // Gradient vectors on a hash-seeded lattice, quintic fade.
                let (x, y) = (num_at(args, 0)?, num_at(args, 1)?);
                let hash = |ix: i64, iy: i64| -> usize {
                    let mut n = ix.wrapping_mul(374761393).wrapping_add(iy.wrapping_mul(668265263));
                    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
                    ((n ^ (n >> 16)) as u64 & 0xff) as usize
                };
                let grad = |h: usize, dx: f64, dy: f64| -> f64 {
                    match h & 7 {
                        0 => dx + dy, 1 => -dx + dy, 2 => dx - dy, 3 => -dx - dy,
                        4 => dx, 5 => -dx, 6 => dy, _ => -dy,
                    }
                };
                let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
                let lerp = |a: f64, b: f64, t: f64| a + t * (b - a);
                let (ix, iy) = (x.floor() as i64, y.floor() as i64);
                let (fx, fy) = (x - x.floor(), y - y.floor());
                let (u, v) = (fade(fx), fade(fy));
                let n00 = grad(hash(ix, iy), fx, fy);
                let n10 = grad(hash(ix + 1, iy), fx - 1.0, fy);
                let n01 = grad(hash(ix, iy + 1), fx, fy - 1.0);
                let n11 = grad(hash(ix + 1, iy + 1), fx - 1.0, fy - 1.0);
                let v = lerp(lerp(n00, n10, u), lerp(n01, n11, u), v);
                Ok(Some(Value::Float(v)))
            }
            "ui_inventory" => {
                // ui_inventory(c, x, y, items, cols) — grid of item slots
                let (d, x0, y0, items, cols) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::List(items)), Some(Value::Int(c)))
                        => (d.clone(), *x as usize, *y as usize, items.clone(), *c as usize),
                    _ => return Err("ui_inventory(c, x, y, items, cols) expects dict + 2 ints + list + int".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                for (i, it) in items.iter().enumerate() {
                    let (dx, dy) = (i % cols.max(1), i / cols.max(1));
                    let (xx, yy) = (x0 + dx * 5, y0 + dy);
                    if yy >= rows2.len() { break; }
                    let label = if let Value::Str(s) = it { s.chars().take(3).collect::<String>() } else { continue };
                    let rendered = format!("[{}]", label);
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for (j, ch) in rendered.chars().enumerate() {
                            if xx + j < nr.len() { nr[xx + j] = Value::Str(ch.to_string().into()); }
                        }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "ui_log" => {
                // ui_log(c, x, y, w, h, messages) — scrolling text log, last
                // h lines visible, clipped to w chars
                let (d, x0, y0, w, h, msgs) = match (args.get(0), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)),
                     Some(Value::Int(w)), Some(Value::Int(h)), Some(Value::List(m)))
                        => (d.clone(), *x as usize, *y as usize, *w as usize, *h as usize, m.clone()),
                    _ => return Err("ui_log(c, x, y, w, h, messages) expects dict + 4 ints + list".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                let start = msgs.len().saturating_sub(h);
                for (i, m) in msgs.iter().skip(start).enumerate() {
                    let yy = y0 + i;
                    if yy >= rows2.len() || i >= h { break; }
                    let text = if let Value::Str(s) = m { s.chars().take(w).collect::<String>() } else { continue };
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for (dx, ch) in text.chars().enumerate() {
                            if x0 + dx < nr.len() { nr[x0 + dx] = Value::Str(ch.to_string().into()); }
                        }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "voronoi" => {
                // voronoi(c, [[x, y, ch], ...]) — nearest-site partition:
                // every cell gets the ch of its closest seed point.
                let (d, seeds) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(d)), Some(Value::List(s))) => (d.clone(), s.clone()),
                    _ => return Err("voronoi(c, [[x, y, ch], ...]) expects dict + seed list".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("canvas: no cells".into()) };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w as usize) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h as usize) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let seed_pts: Vec<(f64, f64, std::sync::Arc<str>)> = seeds.iter().filter_map(|s| {
                    if let Value::List(p) = s {
                        if p.len() == 3 {
                            if let (Value::Int(x), Value::Int(y), Value::Str(ch)) = (&p[0], &p[1], &p[2]) {
                                return Some((*x as f64, *y as f64, ch.clone()));
                            }
                        }
                    }
                    None
                }).collect();
                let mut rows2 = rows.clone();
                for yy in 0..h.min(rows2.len()) {
                    if let Value::List(r) = &rows2[yy] {
                        let mut nr = r.clone();
                        for xx in 0..w.min(nr.len()) {
                            let (px, py) = (xx as f64, yy as f64);
                            if let Some((_, _, ch)) = seed_pts.iter().min_by(|a, b| {
                                let da = (a.0 - px).powi(2) + (a.1 - py).powi(2);
                                let db = (b.0 - px).powi(2) + (b.1 - py).powi(2);
                                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                            }) {
                                nr[xx] = Value::Str(ch.clone());
                            }
                        }
                        rows2[yy] = Value::List(nr);
                    }
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "delaunay" => {
                // delaunay([[x, y], ...]) — Bowyer-Watson triangulation.
                // Returns [[p0, p1, p2], ...] triangles.
                let Value::List(pts) = args.get(0).cloned().unwrap_or(Value::Null) else {
                    return Err("delaunay([[x,y],...]) expects a point list".into())
                };
                let f_of = |v: &Value| match v { Value::Int(i) => *i as f64, Value::Float(f) => *f, _ => 0.0 };
                let mut points: Vec<(f64, f64)> = pts.iter().filter_map(|v| {
                    if let Value::List(p) = v {
                        if p.len() == 2 { return Some((f_of(&p[0]), f_of(&p[1]))); }
                    }
                    None
                }).collect();
                if points.len() < 3 { return Ok(Some(Value::List(im::vector![]))); }
                // super-triangle covering all points
                let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
                for &(x, y) in &points {
                    min_x = min_x.min(x); min_y = min_y.min(y);
                    max_x = max_x.max(x); max_y = max_y.max(y);
                }
                let span = (max_x - min_x).max(max_y - min_y).max(1.0);
                let (cx, cy) = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
                let super_tri = [
                    (cx - 20.0 * span, cy - span),
                    (cx, cy + 20.0 * span),
                    (cx + 20.0 * span, cy - span),
                ];
                points.extend_from_slice(&super_tri);
                let n = points.len();
                let mut tris: Vec<(usize, usize, usize)> = vec![(n - 3, n - 2, n - 1)];
                let circum = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| -> Option<(f64, f64, f64)> {
                    let d = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
                    if d.abs() < 1e-10 { return None; }
                    let (a2, b2, c2) = (a.0 * a.0 + a.1 * a.1, b.0 * b.0 + b.1 * b.1, c.0 * c.0 + c.1 * c.1);
                    let ux = (a2 * (b.1 - c.1) + b2 * (c.1 - a.1) + c2 * (a.1 - b.1)) / d;
                    let uy = (a2 * (c.0 - b.0) + b2 * (a.0 - c.0) + c2 * (b.0 - a.0)) / d;
                    let r2 = (ux - a.0).powi(2) + (uy - a.1).powi(2);
                    Some((ux, uy, r2))
                };
                for i in 0..(n - 3) {
                    let pt = points[i];
                    let bad: Vec<bool> = tris.iter().map(|&(a, b, c)| {
                        circum(points[a], points[b], points[c])
                            .map(|(ux, uy, r2)| (pt.0 - ux).powi(2) + (pt.1 - uy).powi(2) < r2)
                            .unwrap_or(false)
                    }).collect();
                    let mut edges: std::collections::HashMap<(usize, usize), usize> = std::collections::HashMap::new();
                    for (ti, &(a, b, c)) in tris.iter().enumerate() {
                        if !bad[ti] { continue; }
                        for &(e0, e1) in &[(a, b), (b, c), (c, a)] {
                            let key = if e0 < e1 { (e0, e1) } else { (e1, e0) };
                            *edges.entry(key).or_default() += 1;
                        }
                    }
                    let boundary: Vec<(usize, usize)> = edges.into_iter()
                        .filter(|(_, cnt)| *cnt == 1)
                        .map(|(e, _)| e).collect();
                    let mut ti = 0;
                    tris.retain(|_| { let keep = !bad[ti]; ti += 1; keep });
                    for (a, b) in boundary {
                        tris.push((a, b, i));
                    }
                }
                // remove triangles touching the super-triangle
                tris.retain(|&(a, b, c)| a < n - 3 && b < n - 3 && c < n - 3);
                let out: Vec<Value> = tris.into_iter().map(|(a, b, c)|
                    Value::List(im::vector![
                        Value::List(im::vector![Value::Float(points[a].0), Value::Float(points[a].1)]),
                        Value::List(im::vector![Value::Float(points[b].0), Value::Float(points[b].1)]),
                        Value::List(im::vector![Value::Float(points[c].0), Value::Float(points[c].1)]),
                    ])
                ).collect();
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            "wfc" => {
                // wfc(sample_canvas, w, h) — wave-function collapse lite:
                // learn 4-dir adjacency rules from a sample, generate a new
                // w×h canvas by lowest-entropy collapse.
                let (sample, w, h) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(Value::Int(w)), Some(Value::Int(h)))
                        => (d.clone(), *w as usize, *h as usize),
                    _ => return Err("wfc(sample, w, h) expects a canvas + 2 ints".into()),
                };
                let Some(Value::List(srows)) = sample.get("cells") else { return Err("wfc: sample needs cells".into()) };
                let (sw, sh) = (srows.get(0).and_then(|r| if let Value::List(r) = r { Some(r.len()) } else { None }).unwrap_or(0), srows.len());
                // learn adjacency: allowed[dir][tile] -> set of allowed neighbors
                let mut allowed: std::collections::HashMap<(usize, String), std::collections::HashSet<String>> =
                    std::collections::HashMap::new();
                for sy in 0..sh {
                    for sx in 0..sw {
                        if let (Value::List(r), Some(Value::Str(tile))) = (&srows[sy], r_tile(&srows, sx, sy)) {
                            let tile = tile.to_string();
                            for (dir, (dx, dy)) in [(0usize, (0i64, -1i64)), (1, (0, 1)), (2, (-1, 0)), (3, (1, 0))] {
                                let (nx, ny) = (sx as i64 + dx, sy as i64 + dy);
                                if nx < 0 || ny < 0 || nx >= sw as i64 || ny >= sh as i64 { continue; }
                                if let Some(Value::Str(n_tile)) = r_tile(&srows, nx as usize, ny as usize) {
                                    allowed.entry((dir, tile.clone()))
                                        .or_default()
                                        .insert(n_tile.to_string());
                                }
                            }
                        }
                    }
                }
                // wave[x][y] = set of possible tiles
                let all_tiles: Vec<String> = allowed.values().flatten().cloned()
                    .chain(allowed.keys().map(|(_, t)| t.clone()))
                    .collect::<std::collections::HashSet<_>>().into_iter().collect();
                if all_tiles.is_empty() { return Err("wfc: no tiles learned".into()) }
                let mut wave: Vec<Vec<std::collections::HashSet<String>>> =
                    vec![vec![all_tiles.iter().cloned().collect(); w]; h];
                let mut done: Vec<Vec<bool>> = vec![vec![false; w]; h];
                let mut rng = self.rng_state;
                let mut next = || {
                    rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
                    (rng % 1_000_000) as f64 / 1_000_000.0
                };
                for _ in 0..w * h {
                    // pick lowest-entropy un-done cell
                    let mut best: Option<(usize, usize, usize)> = None;
                    for y in 0..h { for x in 0..w {
                        if !done[y][x] && wave[y][x].len() > 0 {
                            let e = wave[y][x].len();
                            if best.map(|b| e < b.2).unwrap_or(true) { best = Some((x, y, e)); }
                        }
                    }}
                    let Some((cx, cy, _)) = best else { break };
                    // collapse to a random option
                    let opts: Vec<String> = wave[cy][cx].iter().cloned().collect();
                    let pick = opts[(next() * opts.len() as f64) as usize].clone();
                    wave[cy][cx] = [pick.clone()].into_iter().collect();
                    done[cy][cx] = true;
                    // propagate: neighbors must be in allowed[dir][pick]
                    for (dir, (dx, dy)) in [(0usize, (0i64, -1i64)), (1, (0, 1)), (2, (-1, 0)), (3, (1, 0))] {
                        let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 { continue; }
                        let (nx, ny) = (nx as usize, ny as usize);
                        if let Some(allowed_set) = allowed.get(&(dir, pick.clone())) {
                            wave[ny][nx].retain(|t| allowed_set.contains(t));
                        }
                    }
                }
                let cells: im::Vector<Value> = wave.iter().map(|row|
                    Value::List(row.iter().map(|s| {
                        let first = s.iter().next().cloned().unwrap_or_else(|| " ".to_string());
                        Value::Str(first.into())
                    }).collect())
                ).collect();
                self.rng_state = rng;
                let mut d = im::HashMap::new();
                d.insert("w".to_string(), Value::Int(w as i64));
                d.insert("h".to_string(), Value::Int(h as i64));
                d.insert("cells".to_string(), Value::List(cells));
                Ok(Some(Value::Dict(d)))
            }
            // === game state: Value-typed 2D grid ===
            "grid_new" => {
                let (w, h) = match (args.get(0), args.get(1)) {
                    (Some(Value::Int(w)), Some(Value::Int(h))) => (*w as usize, *h as usize),
                    _ => return Err("grid_new(w, h) expects 2 ints".into()),
                };
                let row: im::Vector<Value> = (0..w).map(|_| Value::Null).collect();
                let cells: im::Vector<Value> = (0..h).map(|_| Value::List(row.clone())).collect();
                let mut d = im::HashMap::new();
                d.insert("w".to_string(), Value::Int(w as i64));
                d.insert("h".to_string(), Value::Int(h as i64));
                d.insert("cells".to_string(), Value::List(cells));
                Ok(Some(Value::Dict(d)))
            }
            "grid_get" => {
                let (d, x, y) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)))
                        => (d, *x as usize, *y as usize),
                    _ => return Err("grid_get(g, x, y) expects dict + 2 ints".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("grid: no cells".into()) };
                match rows.get(y).and_then(|r| if let Value::List(r) = r { r.get(x) } else { None }) {
                    Some(v) => Ok(Some(v.clone())),
                    None => Ok(Some(Value::Null)),
                }
            }
            "grid_set" => {
                let (d, x, y, v) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), Some(v))
                        => (d.clone(), *x as usize, *y as usize, v.clone()),
                    _ => return Err("grid_set(g, x, y, v) expects dict + 2 ints + value".into()),
                };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("grid: no cells".into()) };
                if y >= rows.len() { return Ok(Some(Value::Dict(d))); }
                let mut rows2 = rows.clone();
                if let Value::List(r) = &rows[y] {
                    let mut nr = r.clone();
                    if x < nr.len() { nr[x] = v; }
                    rows2[y] = Value::List(nr);
                }
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            "grid_neighbors" => {
                // grid_neighbors(g, x, y, diag?) — in-bounds cells as [x, y, value]
                let (d, x0, y0, diag) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Int(x)), Some(Value::Int(y)), dg)
                        => (d, *x, *y, matches!(dg, Some(Value::Bool(true)))),
                    _ => return Err("grid_neighbors(g, x, y, diag?) expects dict + 2 ints + bool".into()),
                };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("grid: missing w/h".into()) };
                let Some(Value::List(rows)) = d.get("cells") else { return Err("grid: no cells".into()) };
                let dirs: &[(i64, i64)] = if diag {
                    &[(0,-1),(0,1),(-1,0),(1,0),(-1,-1),(1,-1),(-1,1),(1,1)]
                } else { &[(0,-1),(0,1),(-1,0),(1,0)] };
                let out: Vec<Value> = dirs.iter()
                    .map(|(dx, dy)| (x0 + dx, y0 + dy))
                    .filter(|(x, y)| *x >= 0 && *y >= 0 && *x < w && *y < h)
                    .map(|(x, y)| {
                        let v = rows.get(y as usize)
                            .and_then(|r| if let Value::List(r) = r { r.get(x as usize) } else { None })
                            .cloned().unwrap_or(Value::Null);
                        Value::List(im::vector![Value::Int(x), Value::Int(y), v])
                    })
                    .collect();
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            "rand_sign" => {
                let mut x = self.rng_state;
                x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                self.rng_state = x;
                Ok(Some(Value::Int(if x % 2 == 0 { 1 } else { -1 })))
            }
            "rand_permute" => {
                let Some(Value::Int(n)) = args.get(0) else { return Err("rand_permute(n) expects an int".into()) };
                let mut out: Vec<Value> = (0..*n).map(Value::Int).collect();
                for i in (1..out.len()).rev() {
                    let mut x = self.rng_state;
                    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                    self.rng_state = x;
                    let j = (x % (i + 1) as u64) as usize;
                    out.swap(i, j);
                }
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            // === std:fintech — limit-order book + matching engine ===
            "book_new" => {
                let mut d = im::HashMap::new();
                d.insert("bids".to_string(), Value::List(im::vector![]));
                d.insert("asks".to_string(), Value::List(im::vector![]));
                Ok(Some(Value::Dict(d)))
            }
            "book_add" => {
                // book_add(book, side, price, qty) — side "bid"|"ask"
                let (d, side, price, qty) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Str(s)), Some(Value::Int(p)), Some(Value::Int(q)))
                        => (d.clone(), s.clone(), *p, *q),
                    (Some(Value::Dict(d)), Some(Value::Str(s)), Some(Value::Float(p)), Some(Value::Float(q)))
                        => (d.clone(), s.clone(), *p as i64, *q as i64),
                    _ => return Err("book_add(book, side, price, qty) expects dict + str + 2 nums".into()),
                };
                let key = if *side == *"bid" { "bids" } else { "asks" };
                let mut d2 = d.clone();
                let mut orders: im::Vector<Value> = d.get(key)
                    .and_then(|v| if let Value::List(l) = v { Some(l.clone()) } else { None })
                    .unwrap_or_default();
                let mut o = im::HashMap::new();
                o.insert("price".to_string(), Value::Int(price));
                o.insert("qty".to_string(), Value::Int(qty));
                o.insert("side".to_string(), Value::Str(side));
                orders.push_back(Value::Dict(o));
                // keep bids sorted desc, asks sorted asc
                let mut v: Vec<Value> = orders.iter().cloned().collect();
                v.sort_by(|a, b| {
                    let pa = if let Value::Dict(d) = a { d.get("price").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0) } else { 0 };
                    let pb = if let Value::Dict(d) = b { d.get("price").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0) } else { 0 };
                    if key == "bids" { pb.cmp(&pa) } else { pa.cmp(&pb) }
                });
                d2.insert(key.to_string(), Value::List(v.into_iter().collect()));
                Ok(Some(Value::Dict(d2)))
            }
            "book_mid" => {
                let Some(Value::Dict(d)) = args.get(0) else { return Err("book_mid(book) expects a book".into()) };
                let best_bid = d.get("bids").and_then(|v| if let Value::List(l) = v { l.get(0) } else { None })
                    .and_then(|o| if let Value::Dict(d) = o { d.get("price") } else { None });
                let best_ask = d.get("asks").and_then(|v| if let Value::List(l) = v { l.get(0) } else { None })
                    .and_then(|o| if let Value::Dict(d) = o { d.get("price") } else { None });
                match (best_bid, best_ask) {
                    (Some(Value::Int(b)), Some(Value::Int(a))) => Ok(Some(Value::Float((*b as f64 + *a as f64) / 2.0))),
                    _ => Ok(Some(Value::Null)),
                }
            }
            "book_spread" => {
                let Some(Value::Dict(d)) = args.get(0) else { return Err("book_spread(book) expects a book".into()) };
                let best_bid = d.get("bids").and_then(|v| if let Value::List(l) = v { l.get(0) } else { None })
                    .and_then(|o| if let Value::Dict(d) = o { d.get("price") } else { None });
                let best_ask = d.get("asks").and_then(|v| if let Value::List(l) = v { l.get(0) } else { None })
                    .and_then(|o| if let Value::Dict(d) = o { d.get("price") } else { None });
                match (best_bid, best_ask) {
                    (Some(Value::Int(b)), Some(Value::Int(a))) => Ok(Some(Value::Int(a - b))),
                    _ => Ok(Some(Value::Null)),
                }
            }
            "match_order" => {
                // match_order(book, side, price, qty) — walk the book, fill at
                // the best prices. Returns {filled, avg_price, book}.
                let (d, side, price, qty) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Dict(d)), Some(Value::Str(s)), Some(Value::Int(p)), Some(Value::Int(q)))
                        => (d.clone(), s.clone(), *p, *q),
                    _ => return Err("match_order(book, side, price, qty) expects dict + str + 2 ints".into()),
                };
                let contra = if *side == *"bid" { "asks" } else { "bids" };
                let Some(Value::List(orders)) = d.get(contra) else { return Err("book: no contra side".into()) };
                let (mut remaining, mut filled, mut cost) = (qty, 0i64, 0i64);
                let mut new_orders = orders.clone();
                let mut i = 0;
                while remaining > 0 && i < new_orders.len() {
                    if let Value::Dict(o) = &new_orders[i] {
                        let (p, q) = (o.get("price").and_then(|v| if let Value::Int(v) = v { Some(*v) } else { None }).unwrap_or(0),
                                      o.get("qty").and_then(|v| if let Value::Int(v) = v { Some(*v) } else { None }).unwrap_or(0));
                        let fill = remaining.min(q);
                        if (side.as_ref() == "bid" && p <= price) || (side.as_ref() == "ask" && p >= price) {
                            filled += fill;
                            cost += fill * p;
                            remaining -= fill;
                            if fill == q {
                                new_orders.remove(i);
                            } else {
                                let mut no = o.clone();
                                no.insert("qty".to_string(), Value::Int(q - fill));
                                new_orders[i] = Value::Dict(no);
                                i += 1;
                            }
                        } else { break; } // price crossed — book sorted
                    } else { i += 1; }
                }
                let mut d2 = d.clone();
                d2.insert(contra.to_string(), Value::List(new_orders));
                let mut out = im::HashMap::new();
                out.insert("filled".to_string(), Value::Int(filled));
                out.insert("avg_price".to_string(), Value::Float(if filled > 0 { cost as f64 / filled as f64 } else { 0.0 }));
                out.insert("book".to_string(), Value::Dict(d2));
                Ok(Some(Value::Dict(out)))
            }
            // === std:robotics — sim-robot sensor/actuator ===
            "robot_new" => {
                // robot_new(x, y, dir, range?) — dir is "n"|"e"|"s"|"w"
                let (x, y, dir, range) = match (args.get(0), args.get(1), args.get(2), args.get(3)) {
                    (Some(Value::Int(x)), Some(Value::Int(y)), Some(Value::Str(d)), r)
                        => (*x, *y, d.clone(), if let Some(Value::Int(r)) = r { *r } else { 3 }),
                    _ => return Err("robot_new(x, y, dir, range?) expects 3 ints + str".into()),
                };
                let mut d = im::HashMap::new();
                d.insert("x".to_string(), Value::Int(x));
                d.insert("y".to_string(), Value::Int(y));
                d.insert("dir".to_string(), Value::Str(dir));
                d.insert("range".to_string(), Value::Int(range));
                Ok(Some(Value::Dict(d)))
            }
            "robot_sensor" => {
                // robot_sensor(robot, canvas, wall_ch) — scan ahead in the
                // facing dir up to range; returns {dist, hit}.
                let (r, c, wall) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(r)), Some(Value::Dict(c)), Some(Value::Str(w)))
                        => (r, c, w.clone()),
                    _ => return Err("robot_sensor(robot, canvas, wall_ch) expects 2 dicts + str".into()),
                };
                let (rx, ry) = (r.get("x").and_then(|v| if let Value::Int(x) = v { Some(*x) } else { None }),
                                r.get("y").and_then(|v| if let Value::Int(y) = v { Some(*y) } else { None }));
                let (Some(mut x), Some(mut y)) = (rx, ry) else { return Err("robot: missing x/y".into()) };
                let dir = r.get("dir").and_then(|v| if let Value::Str(d) = v { Some(d.clone()) } else { None }).unwrap_or("n".into());
                let range = r.get("range").and_then(|v| if let Value::Int(n) = v { Some(*n) } else { None }).unwrap_or(3);
                let (dx, dy) = match dir.as_ref() { "e" => (1,0), "s" => (0,1), "w" => (-1,0), _ => (0,-1) };
                let Some(Value::List(cells)) = c.get("cells") else { return Err("canvas: no cells".into()) };
                let (w, h) = (c.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w) } else { None }),
                              c.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let mut dist = 0i64;
                let mut hit = false;
                for _ in 0..range {
                    x += dx; y += dy;
                    if x < 0 || y < 0 || x >= w || y >= h { hit = true; break; }
                    if let Value::List(row) = &cells[y as usize] {
                        if let Some(Value::Str(s)) = row.get(x as usize) {
                            if *s == wall { hit = true; break; }
                        }
                    }
                    dist += 1;
                }
                let mut out = im::HashMap::new();
                out.insert("dist".to_string(), Value::Int(dist));
                out.insert("hit".to_string(), Value::Bool(hit));
                Ok(Some(Value::Dict(out)))
            }
            "robot_turn" => {
                // robot_turn(robot, "left"|"right") — rotate facing
                let (r, dir) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(r)), Some(Value::Str(d))) => (r.clone(), d.clone()),
                    _ => return Err("robot_turn(robot, dir) expects dict + str".into()),
                };
                let cur = r.get("dir").and_then(|v| if let Value::Str(d) = v { Some(d.clone()) } else { None }).unwrap_or("n".into());
                let order = ["n", "e", "s", "w"];
                let idx = order.iter().position(|&d| d == cur.as_ref()).unwrap_or(0);
                let next = if dir.as_ref() == "right" { order[(idx + 1) % 4] } else { order[(idx + 3) % 4] };
                let mut d2 = r.clone();
                d2.insert("dir".to_string(), Value::Str(next.into()));
                Ok(Some(Value::Dict(d2)))
            }
            "robot_move" => {
                // robot_move(robot, canvas, wall_ch) — step ahead if clear;
                // returns {robot, moved}.
                let (r, c, wall) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(r)), Some(Value::Dict(c)), Some(Value::Str(w)))
                        => (r.clone(), c, w.clone()),
                    _ => return Err("robot_move(robot, canvas, wall_ch) expects 2 dicts + str".into()),
                };
                let (rx, ry) = (r.get("x").and_then(|v| if let Value::Int(x) = v { Some(*x) } else { None }),
                                r.get("y").and_then(|v| if let Value::Int(y) = v { Some(*y) } else { None }));
                let (Some(x), Some(y)) = (rx, ry) else { return Err("robot: missing x/y".into()) };
                let dir = r.get("dir").and_then(|v| if let Value::Str(d) = v { Some(d.clone()) } else { None }).unwrap_or("n".into());
                let (dx, dy) = match dir.as_ref() { "e" => (1,0), "s" => (0,1), "w" => (-1,0), _ => (0,-1) };
                let (nx, ny) = (x + dx, y + dy);
                let Some(Value::List(cells)) = c.get("cells") else { return Err("canvas: no cells".into()) };
                let (w, h) = (c.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w) } else { None }),
                              c.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let mut moved = false;
                let mut d2 = r.clone();
                if nx >= 0 && ny >= 0 && nx < w && ny < h {
                    if let Value::List(row) = &cells[ny as usize] {
                        if let Some(Value::Str(s)) = row.get(nx as usize) {
                            if *s != wall {
                                d2.insert("x".to_string(), Value::Int(nx));
                                d2.insert("y".to_string(), Value::Int(ny));
                                moved = true;
                            }
                        }
                    }
                }
                let mut out = im::HashMap::new();
                out.insert("robot".to_string(), Value::Dict(d2));
                out.insert("moved".to_string(), Value::Bool(moved));
                Ok(Some(Value::Dict(out)))
            }
            "robot_draw" => {
                // robot_draw(robot, canvas) — stamp the robot's glyph on the canvas
                let (r, c) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(r)), Some(Value::Dict(c))) => (r, c.clone()),
                    _ => return Err("robot_draw(robot, canvas) expects 2 dicts".into()),
                };
                let (rx, ry) = (r.get("x").and_then(|v| if let Value::Int(x) = v { Some(*x) } else { None }),
                                r.get("y").and_then(|v| if let Value::Int(y) = v { Some(*y) } else { None }));
                let (Some(x), Some(y)) = (rx, ry) else { return Err("robot: missing x/y".into()) };
                let dir = r.get("dir").and_then(|v| if let Value::Str(d) = v { Some(d.clone()) } else { None }).unwrap_or("n".into());
                let glyph = match dir.as_ref() { "e" => ">", "s" => "v", "w" => "<", _ => "^" };
                let Some(Value::List(rows)) = c.get("cells") else { return Err("canvas: no cells".into()) };
                let mut rows2 = rows.clone();
                if (y as usize) < rows2.len() {
                    if let Value::List(row) = &rows2[y as usize] {
                        if (x as usize) < row.len() {
                            let mut nr = row.clone();
                            nr[x as usize] = Value::Str(glyph.into());
                            rows2[y as usize] = Value::List(nr);
                        }
                    }
                }
                let mut d2 = c.clone();
                d2.insert("cells".to_string(), Value::List(rows2));
                Ok(Some(Value::Dict(d2)))
            }
            // === std:microservice — service registry + health check ===
            "svc_register" => {
                // svc_register(registry, name, fn) — register a service impl
                let (reg, name, f) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(r)), Some(Value::Str(n)), Some(f @ Value::Closure(_, _, _)))
                        => (r.clone(), n.clone(), f.clone()),
                    _ => return Err("svc_register(reg, name, fn) expects dict + str + closure".into()),
                };
                let mut d = reg.clone();
                let mut meta = im::HashMap::new();
                meta.insert("impl".to_string(), f);
                meta.insert("calls".to_string(), Value::Int(0));
                meta.insert("fails".to_string(), Value::Int(0));
                d.insert(name.to_string(), Value::Dict(meta));
                Ok(Some(Value::Dict(d)))
            }
            "svc_call" => {
                // svc_call(registry, name, args) — dispatch to the impl, track
                // calls + failures. Returns {ok, result, calls, fails}.
                let (reg, name, call_args) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(r)), Some(Value::Str(n)), Some(Value::List(a)))
                        => (r, n.clone(), a.iter().cloned().collect::<Vec<Value>>()),
                    _ => return Err("svc_call(reg, name, args) expects dict + str + list".into()),
                };
                let Some(Value::Dict(meta)) = reg.get(name.as_ref()) else {
                    return Err(format!("service '{}' not registered", name));
                };
                let Some(f) = meta.get("impl") else { return Err("service: no impl".into()) };
                let calls = meta.get("calls").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                let fails = meta.get("fails").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                let mut out = im::HashMap::new();
                let mut reg2 = reg.clone();
                let mut meta2 = meta.clone();
                match self.call_closure_value(f, call_args) {
                    Ok(v) => {
                        out.insert("ok".to_string(), Value::Bool(true));
                        out.insert("result".to_string(), v);
                        meta2.insert("calls".to_string(), Value::Int(calls + 1));
                    }
                    Err(e) => {
                        out.insert("ok".to_string(), Value::Bool(false));
                        out.insert("error".to_string(), Value::Str(e.into()));
                        meta2.insert("calls".to_string(), Value::Int(calls + 1));
                        meta2.insert("fails".to_string(), Value::Int(fails + 1));
                    }
                }
                reg2.insert(name.to_string(), Value::Dict(meta2));
                out.insert("registry".to_string(), Value::Dict(reg2));
                Ok(Some(Value::Dict(out)))
            }
            "svc_health" => {
                // svc_health(registry) — per-service call/fail counts
                let Some(Value::Dict(reg)) = args.get(0) else { return Err("svc_health(reg) expects a registry".into()) };
                let mut out = im::HashMap::new();
                for (name, meta) in reg {
                    if let Value::Dict(m) = meta {
                        let calls = m.get("calls").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                        let fails = m.get("fails").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                        let mut h = im::HashMap::new();
                        h.insert("calls".to_string(), Value::Int(calls));
                        h.insert("fails".to_string(), Value::Int(fails));
                        h.insert("healthy".to_string(), Value::Bool(fails == 0 || calls > fails * 5));
                        out.insert(name.clone(), Value::Dict(h));
                    }
                }
                Ok(Some(Value::Dict(out)))
            }
            // === std:edge — edge-node cluster sim ===
            "edge_deploy" => {
                // edge_deploy(cluster, node, fn) — register an fn on an edge node
                let (c, node, f) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(c)), Some(Value::Str(n)), Some(f @ Value::Closure(_, _, _)))
                        => (c.clone(), n.clone(), f.clone()),
                    _ => return Err("edge_deploy(cluster, node, fn) expects dict + str + closure".into()),
                };
                let mut d = c.clone();
                let mut meta = im::HashMap::new();
                meta.insert("impl".to_string(), f);
                meta.insert("calls".to_string(), Value::Int(0));
                meta.insert("fails".to_string(), Value::Int(0));
                d.insert(node.to_string(), Value::Dict(meta));
                Ok(Some(Value::Dict(d)))
            }
            "edge_call" => {
                // edge_call(cluster, node, args) — run the node's impl, track
                // calls + fails. Returns {ok, result, cluster}.
                let (c, node, call_args) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Dict(c)), Some(Value::Str(n)), Some(Value::List(a)))
                        => (c, n.clone(), a.iter().cloned().collect::<Vec<Value>>()),
                    _ => return Err("edge_call(cluster, node, args) expects dict + str + list".into()),
                };
                let Some(Value::Dict(meta)) = c.get(node.as_ref()) else {
                    return Err(format!("edge node '{}' not deployed", node));
                };
                let Some(f) = meta.get("impl") else { return Err("edge: no impl".into()) };
                let calls = meta.get("calls").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                let fails = meta.get("fails").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                let mut out = im::HashMap::new();
                let mut c2 = c.clone();
                let mut meta2 = meta.clone();
                match self.call_closure_value(f, call_args) {
                    Ok(v) => {
                        out.insert("ok".to_string(), Value::Bool(true));
                        out.insert("result".to_string(), v);
                        meta2.insert("calls".to_string(), Value::Int(calls + 1));
                    }
                    Err(e) => {
                        out.insert("ok".to_string(), Value::Bool(false));
                        out.insert("error".to_string(), Value::Str(e.into()));
                        meta2.insert("calls".to_string(), Value::Int(calls + 1));
                        meta2.insert("fails".to_string(), Value::Int(fails + 1));
                    }
                }
                c2.insert(node.to_string(), Value::Dict(meta2));
                out.insert("cluster".to_string(), Value::Dict(c2));
                Ok(Some(Value::Dict(out)))
            }
            "edge_health" => {
                // edge_health(cluster) — per-node call/fail + healthy flag
                let Some(Value::Dict(c)) = args.get(0) else { return Err("edge_health(cluster) expects a cluster".into()) };
                let mut out = im::HashMap::new();
                for (node, meta) in c {
                    if let Value::Dict(m) = meta {
                        let calls = m.get("calls").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                        let fails = m.get("fails").and_then(|v| if let Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                        let mut h = im::HashMap::new();
                        h.insert("calls".to_string(), Value::Int(calls));
                        h.insert("fails".to_string(), Value::Int(fails));
                        h.insert("healthy".to_string(), Value::Bool(fails == 0 || calls > fails * 5));
                        out.insert(node.clone(), Value::Dict(h));
                    }
                }
                Ok(Some(Value::Dict(out)))
            }
            "edge_nodes" => {
                // edge_nodes(cluster) — list of deployed node names
                let Some(Value::Dict(c)) = args.get(0) else { return Err("edge_nodes(cluster) expects a cluster".into()) };
                let out: Vec<Value> = c.keys().map(|k| Value::Str(k.clone().into())).collect();
                Ok(Some(Value::List(out.into_iter().collect())))
            }
            "dist_stats" => {
                // dist_stats(fn, trials) — mean/stddev/min/max over n samples
                let (f, trials) = match (args.get(0), args.get(1)) {
                    (Some(f @ Value::Closure(_, _, _)), Some(Value::Int(t))) => (f, *t as usize),
                    _ => return Err("dist_stats(fn, trials) expects a closure + int".into()),
                };
                let mut vals: Vec<f64> = Vec::with_capacity(trials);
                for _ in 0..trials {
                    let v = self.call_closure_value(f, vec![])?;
                    let n = match v { Value::Int(i) => i as f64, Value::Float(f) => f, _ => return Err("fn must return a number".into()) };
                    vals.push(n);
                }
                let mean = vals.iter().sum::<f64>() / vals.len().max(1) as f64;
                let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len().max(1) as f64;
                let (mn, mx) = vals.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
                let mut out = im::HashMap::new();
                out.insert("mean".to_string(), Value::Float(mean));
                out.insert("stddev".to_string(), Value::Float(var.sqrt()));
                out.insert("min".to_string(), Value::Float(if vals.is_empty() { 0.0 } else { mn }));
                out.insert("max".to_string(), Value::Float(if vals.is_empty() { 0.0 } else { mx }));
                out.insert("n".to_string(), Value::Int(vals.len() as i64));
                Ok(Some(Value::Dict(out)))
            }
            "canvas_clear" => {
                let (d, ch) = match (args.get(0), args.get(1)) {
                    (Some(Value::Dict(d)), Some(Value::Str(ch))) => (d.clone(), ch.clone()),
                    (Some(Value::Dict(d)), None) => (d.clone(), " ".into()),
                    _ => return Err("canvas_clear(c, ch?) expects a canvas".into()),
                };
                let (w, h) = (d.get("w").and_then(|v| if let Value::Int(w) = v { Some(*w as usize) } else { None }),
                              d.get("h").and_then(|v| if let Value::Int(h) = v { Some(*h as usize) } else { None }));
                let (Some(w), Some(h)) = (w, h) else { return Err("canvas: missing w/h".into()) };
                let row: im::Vector<Value> = (0..w).map(|_| Value::Str(ch.clone())).collect();
                let cells: im::Vector<Value> = (0..h).map(|_| Value::List(row.clone())).collect();
                let mut d2 = d.clone();
                d2.insert("cells".to_string(), Value::List(cells));
                Ok(Some(Value::Dict(d2)))
            }
            // === std:balance — monte-carlo playtesting ===
            "balance_test" => {
                let (f, trials) = match (args.get(0), args.get(1)) {
                    (Some(f @ Value::Closure(_, _, _)), Some(Value::Int(t))) => (f, *t as usize),
                    _ => return Err("balance_test(fn, trials) expects a closure and int".into()),
                };
                let (mut wins, mut losses, mut draws, mut score_sum) = (0usize, 0usize, 0usize, 0f64);
                for _ in 0..trials {
                    let r = self.call_closure_value(f, vec![])?;
                    match r {
                        Value::Dict(d) => {
                            let win = d.get("win").map(|v| matches!(v, Value::Bool(true))).unwrap_or(false);
                            let draw = d.get("draw").map(|v| matches!(v, Value::Bool(true))).unwrap_or(false);
                            let score = d.get("score").and_then(|v| match v { Value::Int(i) => Some(*i as f64), Value::Float(f) => Some(*f), _ => None }).unwrap_or(0.0);
                            score_sum += score;
                            if draw { draws += 1; } else if win { wins += 1; } else { losses += 1; }
                        }
                        Value::Bool(true) => wins += 1,
                        Value::Bool(false) => losses += 1,
                        Value::Int(s) => { score_sum += s as f64; if s > 0 { wins += 1; } else { losses += 1; } }
                        _ => return Err("balance_test fn must return dict/bool/int".into()),
                    }
                }
                let mut out = im::HashMap::new();
                out.insert("trials".to_string(), Value::Int(trials as i64));
                out.insert("wins".to_string(), Value::Int(wins as i64));
                out.insert("losses".to_string(), Value::Int(losses as i64));
                out.insert("draws".to_string(), Value::Int(draws as i64));
                out.insert("win_rate".to_string(), Value::Float(if trials > 0 { wins as f64 / trials as f64 } else { 0.0 }));
                out.insert("avg_score".to_string(), Value::Float(if trials > 0 { score_sum / trials as f64 } else { 0.0 }));
                Ok(Some(Value::Dict(out)))
            }
            // === std:balance — headless game loop ===
            "game_loop" => {
                // game_loop(max_ticks, init_state, update_fn) — update_fn
                // (state, tick) → new state or null to stop. Returns final state.
                let (max_ticks, init, f) = match (args.get(0), args.get(1), args.get(2)) {
                    (Some(Value::Int(m)), Some(init), Some(f @ Value::Closure(_, _, _)))
                        => (*m as usize, init.clone(), f),
                    _ => return Err("game_loop(max_ticks, init, fn) expects int, state, closure".into()),
                };
                let mut state = init;
                for tick in 0..max_ticks {
                    let next = self.call_closure_value(f, vec![state.clone(), Value::Int(tick as i64)])?;
                    if matches!(next, Value::Null) { return Ok(Some(Value::Dict({
                        let mut d = im::HashMap::new();
                        d.insert("stopped".to_string(), Value::Bool(true));
                        d.insert("ticks".to_string(), Value::Int(tick as i64));
                        d.insert("state".to_string(), state);
                        d
                    }))); }
                    state = next;
                }
                let mut d = im::HashMap::new();
                d.insert("stopped".to_string(), Value::Bool(false));
                d.insert("ticks".to_string(), Value::Int(max_ticks as i64));
                d.insert("state".to_string(), state);
                Ok(Some(Value::Dict(d)))
            }
            // === std:behavior — behavior trees ===
            // tree: ["seq", a, b] | ["sel", a, b] | ["act", fn] | ["inv", a]
            "bt_tick" => {
                let (tree, ctx) = match (args.get(0), args.get(1)) {
                    (Some(Value::List(t)), Some(ctx)) => (t, ctx.clone()),
                    _ => return Err("bt_tick(tree, ctx) expects a tree and context".into()),
                };
                Ok(Some(Value::Bool(self.bt_tick(tree, &ctx)?)))
            }
            "read_file" => {
                if args.len() != 1 { return Err("read_file() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(path) => {
                        match std::fs::read_to_string(&**path) {
                            Ok(content) => Ok(Some(Value::Str(content.into()))),
                            Err(e) => Err(format!("Cannot read file '{}': {}", path, e)),
                        }
                    }
                    _ => Err("read_file() expects a string path".into()),
                }
            }
            "write_file" => {
                if args.len() != 2 { return Err("write_file() expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::Str(path), Value::Str(content)) => {
                        match std::fs::write(&**path, &**content) {
                            Ok(_) => Ok(Some(Value::Bool(true))),
                            Err(e) => Err(format!("Cannot write file '{}': {}", path, e)),
                        }
                    }
                    _ => Err("write_file() expects (path, content) strings".into()),
                }
            }
            "fetch" => {
                if args.len() < 1 { return Err("fetch() expects a URL string".into()); }
                match &args[0] {
                    Value::Str(url) => {
                        let client = reqwest::blocking::Client::builder()
                            .user_agent("Veldt/0.3")
                            .timeout(std::time::Duration::from_secs(30))
                            .build()
                            .map_err(|e| format!("HTTP client error: {}", e))?;
                        let mut req = client.get(&**url);
                        // Optional headers as dict: fetch(url, headers_dict)
                        if args.len() >= 2 {
                            if let Value::Dict(h) = &args[1] {
                                for (k, v) in h {
                                    if let Value::Str(sv) = v {
                                        req = req.header(k, &**sv);
                                    }
                                }
                            }
                        }
                        match req.send() {
                            Ok(resp) => {
                                let status = resp.status().as_u16() as i64;
                                let body = resp.text().unwrap_or_default();
                                let mut result = std::collections::HashMap::new();
                                result.insert("status".to_string(), Value::Int(status));
                                result.insert("body".to_string(), Value::Str(body.into()));
                                Ok(Some(Value::Dict(result.into_iter().collect())))
                            }
                            Err(e) => Err(format!("Fetch error: {}", e)),
                        }
                    }
                    _ => Err("fetch() expects a URL string".into()),
                }
            }
            "post" => {
                if args.len() < 2 { return Err("post() expects (url, body) strings".into()); }
                match (&args[0], &args[1]) {
                    (Value::Str(url), Value::Str(body_str)) => {
                        let client = reqwest::blocking::Client::builder()
                            .user_agent("Veldt/0.3")
                            .timeout(std::time::Duration::from_secs(30))
                            .build()
                            .map_err(|e| format!("HTTP client error: {}", e))?;
                        let mut req = client.post(&**url).header("Content-Type", "application/json").body(body_str.to_string());
                        if args.len() >= 3 {
                            if let Value::Dict(h) = &args[2] {
                                for (k, v) in h {
                                    if let Value::Str(sv) = v {
                                        req = req.header(k, &**sv);
                                    }
                                }
                            }
                        }
                        match req.send() {
                            Ok(resp) => {
                                let status = resp.status().as_u16() as i64;
                                let body = resp.text().unwrap_or_default();
                                let mut result = std::collections::HashMap::new();
                                result.insert("status".to_string(), Value::Int(status));
                                result.insert("body".to_string(), Value::Str(body.into()));
                                Ok(Some(Value::Dict(result.into_iter().collect())))
                            }
                            Err(e) => Err(format!("Post error: {}", e)),
                        }
                    }
                    _ => Err("post() expects (url, body) strings".into()),
                }
            }
            "sleep" => {
                if args.len() != 1 { return Err("sleep() expects 1 argument (ms)".into()); }
                match &args[0] {
                    Value::Int(ms) => {
                        std::thread::sleep(std::time::Duration::from_millis(*ms as u64));
                        Ok(Some(Value::Null))
                    }
                    _ => Err("sleep() expects an integer (milliseconds)".into()),
                }
            }
            "time_ms" => {
                let ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                Ok(Some(Value::Int(ms)))
            }
            "env" => {
                if args.len() != 1 { return Err("env() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(name) => {
                        match std::env::var(&**name) {
                            Ok(v) => Ok(Some(Value::Str(v.into()))),
                            Err(_) => Ok(Some(Value::Null)),
                        }
                    }
                    _ => Err("env() expects a string name".into()),
                }
            }
            "exec" => {
                if args.len() != 1 { return Err("exec() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(cmd) => {
                        let output = std::process::Command::new("sh")
                            .arg("-c")
                            .arg(&**cmd)
                            .output();
                        match output {
                            Ok(o) => {
                                let mut result = std::collections::HashMap::new();
                                result.insert("stdout".to_string(), Value::Str(String::from_utf8_lossy(&o.stdout).to_string().into()));
                                result.insert("stderr".to_string(), Value::Str(String::from_utf8_lossy(&o.stderr).to_string().into()));
                                result.insert("exit_code".to_string(), Value::Int(o.status.code().unwrap_or(-1) as i64));
                                Ok(Some(Value::Dict(result.into_iter().collect())))
                            }
                            Err(e) => Err(format!("exec() failed: {}", e)),
                        }
                    }
                    _ => Err("exec() expects a string command".into()),
                }
            }
            "set" => {
                let mut items: Vec<Value> = Vec::new();
                for arg in args {
                    if let Value::List(l) = arg {
                        for v in l {
                            if !items.iter().any(|x| self.values_eq(x, v)) { items.push(v.clone()); }
                        }
                    } else {
                        if !items.iter().any(|x| self.values_eq(x, arg)) { items.push(arg.clone()); }
                    }
                }
                Ok(Some(Value::Set(items)))
            }
            "spawn" => {
                // spawn("fn_name", [args]) -> task id
                // Runs a GARDEN function on an OS thread with its own
                // interpreter. Shared-nothing: args/results are marshalled
                // via JSON. Only functions persisted in the garden can spawn.
                let fname = match args.get(0) {
                    Some(Value::Str(s)) => s.clone(),
                    _ => return Err("spawn() expects a function name string".into()),
                };
                let fargs = match args.get(1) {
                    Some(Value::List(l)) => l.clone(),
                    Some(_) => return Err("spawn() args must be a list".into()),
                    None => im::Vector::new(),
                };
                let json_args: Result<Vec<String>, _> = fargs.iter()
                    .map(|a| serde_json::to_string(a)).collect();
                let json_args = json_args.map_err(|e| format!("spawn arg not serializable: {}", e))?;
                let handle = std::thread::spawn(move || Self::thread_run(&fname, json_args));
                self.next_task_id += 1;
                self.tasks.insert(self.next_task_id, handle);
                Ok(Some(Value::Int(self.next_task_id)))
            }
            "join" => {
                // join(task_id) -> result value; blocks until done
                let id = match args.get(0) {
                    Some(Value::Int(n)) => *n,
                    _ => return Err("join() expects a task id".into()),
                };
                match self.tasks.remove(&id) {
                    Some(h) => match h.join() {
                        Ok(Ok(json)) => Ok(Some(
                            serde_json::from_str(&json).unwrap_or(Value::Null))),
                        Ok(Err(e)) => Err(format!("spawned task failed: {}", e)),
                        Err(_) => Err("spawned task panicked".into()),
                    },
                    None => Err(format!("No task with id {}", id)),
                }
            }
            "pmap" => {
                // pmap("fn_name", list) -> list of results, one thread per item
                let fname = match args.get(0) {
                    Some(Value::Str(s)) => s.clone(),
                    _ => return Err("pmap() expects a function name string".into()),
                };
                let items = match args.get(1) {
                    Some(Value::List(l)) => l.clone(),
                    _ => return Err("pmap() expects (name, list)".into()),
                };
                let mut handles = Vec::new();
                for item in &items {
                    let ja = serde_json::to_string(item)
                        .map_err(|e| format!("pmap arg not serializable: {}", e))?;
                    let fname2 = fname.clone();
                    handles.push(std::thread::spawn(move || Self::thread_run(&fname2, vec![ja])));
                }
                let mut results = Vec::new();
                for h in handles {
                    match h.join() {
                        Ok(Ok(json)) => results.push(serde_json::from_str(&json).unwrap_or(Value::Null)),
                        Ok(Err(e)) => return Err(format!("pmap worker failed: {}", e)),
                        Err(_) => return Err("pmap worker panicked".into()),
                    }
                }
                Ok(Some(Value::List(results.into_iter().collect())))
            }
            "garden_reload" => {
                // garden_reload() — re-read the garden from disk and swap
                // in its current variant bodies + fitness, so a running
                // program adopts evolved code without restarting.
                // Garden-managed families are replaced wholesale (dead
                // variants disappear, new births appear); plain script
                // `fn`s are untouched.
                let mut v = crate::veldt::Veldt::new();
                v.load().map_err(|e| format!("garden_reload: {}", e))?;
                for name in v.functions.keys() {
                    self.functions.remove(name);
                }
                v.load_into_interpreter(self);
                return Ok(Some(Value::Null));
            }
            "call_best" => {
                // call_best("name", arg1, arg2, ...) — dispatch to the
                // currently-fittest variant of the named function
                if args.is_empty() { return Err("call_best() expects a function name".into()); }
                match &args[0] {
                    Value::Str(fname) => {
                        let fname = fname.clone();
                        let fargs = args[1..].to_vec();
                        let best_id = self.functions.get(&*fname)
                            .and_then(|vs| vs.iter()
                                .max_by(|a, b| a.fitness.partial_cmp(&b.fitness).unwrap_or(std::cmp::Ordering::Equal))
                                .map(|v| v.id))
                            .ok_or_else(|| format!("call_best: no variants of '{}'", fname))?;
                        return self.call_function(&fname, Some(best_id), fargs).map(Some);
                    }
                    _ => Err("call_best() expects a function name string".into()),
                }
            }
            "match_path" => {
                // match_path("/users/:id", "/users/42") -> {id: "42"} or null
                let (pat, path) = match (args.get(0), args.get(1)) {
                    (Some(Value::Str(p)), Some(Value::Str(q))) => (p.clone(), q.clone()),
                    _ => return Err("match_path(pattern, path) — two strings".into()),
                };
                let ps: Vec<&str> = pat.trim_matches('/').split('/').collect();
                let qs: Vec<&str> = path.trim_matches('/').split('/').collect();
                if ps.len() != qs.len() { return Ok(Some(Value::Null)); }
                let mut out = std::collections::HashMap::new();
                for (pseg, qseg) in ps.iter().zip(qs.iter()) {
                    if let Some(name) = pseg.strip_prefix(':') {
                        out.insert(name.to_string(), Value::Str(qseg.to_string().into()));
                    } else if pseg != qseg {
                        return Ok(Some(Value::Null));
                    }
                }
                Ok(Some(Value::Dict(out.into_iter().collect())))
            }
            "format" => {
                // format("%0 is %1", a, b) — %N indexed args, %% literal.
                // {} braces can't be used — veldt interpolates {expr} at lex time.
                let tpl = match args.get(0) {
                    Some(Value::Str(s)) => s.clone(),
                    _ => return Err("format(tpl, ...) — first arg must be a string".into()),
                };
                let mut out = String::new();
                let mut chars = tpl.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '%' {
                        if chars.peek() == Some(&'%') { chars.next(); out.push('%'); continue; }
                        let mut num = String::new();
                        while let Some(&d) = chars.peek() {
                            if d.is_ascii_digit() { num.push(d); chars.next(); } else { break; }
                        }
                        if num.is_empty() { out.push('%'); continue; }
                        let idx: usize = num.parse().unwrap_or(0);
                        out.push_str(&format!("{}", args.get(idx + 1).cloned().unwrap_or(Value::Null)));
                    } else { out.push(c); }
                }
                Ok(Some(Value::Str(out.into())))
            }
            "args" => {
                // args() -> script argv (veldt run prog.veldt -- a b c)
                Ok(Some(Value::List(self.argv.iter()
                    .map(|a| Value::Str(a.clone().into())).collect())))
            }
            "exit" => {
                // exit(code) — clean process exit via sentinel error
                let code = match args.get(0) {
                    Some(Value::Int(n)) => *n,
                    None => 0,
                    _ => return Err("exit() expects an int".into()),
                };
                Err(format!("__veldt_exit__:{}", code))
            }
            "eprint" => {
                // eprint(x) — print to stderr, returns null
                let v = args.get(0).cloned().unwrap_or(Value::Null);
                eprintln!("{}", v);
                Ok(Some(Value::Null))
            }
            "read_stdin" => {
                // read_stdin() -> read one line from stdin (no newline)
                use std::io::BufRead;
                let stdin = std::io::stdin();
                let mut line = String::new();
                match stdin.lock().read_line(&mut line) {
                    Ok(0) => Ok(Some(Value::Null)),       // EOF
                    Ok(_) => Ok(Some(Value::Str(line.trim_end_matches('\n').to_string().into()))),
                    Err(e) => Err(format!("stdin read failed: {}", e)),
                }
            }

            "exists" | "file_size" | "ls" | "mkdir" | "rm" | "cp" | "mv" | "cwd" | "home" => {
                let path = match args.get(0) {
                    Some(Value::Str(p)) => p.clone(),
                    None if name == "cwd" || name == "home" => String::new().into(),
                    _ => return Err(format!("{}() expects a path string", name)),
                };
                match name {
                    "home" => Ok(Some(Value::Str(
                        std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
                            .unwrap_or_else(|_| ".".into()).into()))),
                    "exists" => Ok(Some(Value::Bool(std::path::Path::new(&*path).exists()))),
                    "file_size" => Ok(Some(Value::Int(
                        std::fs::metadata(&*path).map(|m| m.len() as i64).unwrap_or(0)))),
                    "ls" => {
                        let dir = if path.is_empty() { "." } else { &path };
                        let mut names: Vec<Value> = Vec::new();
                        match std::fs::read_dir(dir) {
                            Ok(rd) => {
                                for e in rd.flatten() {
                                    names.push(Value::Str(e.file_name().to_string_lossy().to_string().into()));
                                }
                                Ok(Some(Value::List(names.into_iter().collect())))
                            }
                            Err(e) => Err(format!("ls failed: {}", e)),
                        }
                    }
                    "mkdir" => Ok(Some(Value::Bool(std::fs::create_dir_all(&*path).is_ok()))),
                    "rm" => {
                        let p = std::path::Path::new(&*path);
                        let r = if p.is_dir() { std::fs::remove_dir_all(p) }
                                else { std::fs::remove_file(p) };
                        Ok(Some(Value::Bool(r.is_ok())))
                    }
                    "cp" => {
                        let dst = match args.get(1) { Some(Value::Str(d)) => d.clone(), _ => return Err("cp(src, dst)".into()) };
                        Ok(Some(Value::Bool(std::fs::copy(&*path, &*dst).is_ok())))
                    }
                    "mv" => {
                        let dst = match args.get(1) { Some(Value::Str(d)) => d.clone(), _ => return Err("mv(src, dst)".into()) };
                        Ok(Some(Value::Bool(std::fs::rename(&*path, &*dst).is_ok())))
                    }
                    "cwd" => Ok(Some(Value::Str(
                        std::env::current_dir().map(|p| p.to_string_lossy().to_string().into())
                            .unwrap_or_default()))),
                    _ => unreachable!(),
                }
            }
            "read_lines" | "write_lines" => match name {
                "read_lines" => {
                    let path = match args.get(0) { Some(Value::Str(p)) => p.clone(), _ => return Err("read_lines(path)".into()) };
                    match std::fs::read_to_string(&*path) {
                        Ok(c) => Ok(Some(Value::List(c.lines().map(|l| Value::Str(l.into())).collect()))),
                        Err(e) => Err(format!("read_lines failed: {}", e)),
                    }
                }
                "write_lines" => {
                    let (path, items) = match (args.get(0), args.get(1)) {
                        (Some(Value::Str(p)), Some(Value::List(l))) => (p.clone(), l.clone()),
                        _ => return Err("write_lines(path, list)".into()),
                    };
                    let content = items.iter().map(|v| format!("{}", v)).collect::<Vec<_>>().join("\n");
                    Ok(Some(Value::Bool(std::fs::write(&*path, content + "\n").is_ok())))
                }
                _ => unreachable!(),
            }
            "b64_encode" | "b64_decode" | "url_encode" | "url_decode" => {
                let input = match args.get(0) {
                    Some(Value::Str(s)) => s.clone(),
                    _ => return Err(format!("{}(string)", name)),
                };
                const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                match name {
                    "b64_encode" => {
                        let b = input.as_bytes();
                        let mut out = String::new();
                        for chunk in b.chunks(3) {
                            let n = chunk.iter().enumerate()
                                .fold(0u32, |acc, (i, c)| acc | ((*c as u32) << (16 - i * 8)));
                            let cnt = chunk.len();
                            for i in 0..4 {
                                out.push(if i <= cnt { B64[((n >> (18 - i * 6)) & 63) as usize] as char } else { '=' });
                            }
                        }
                        Ok(Some(Value::Str(out.into())))
                    }
                    "b64_decode" => {
                        let mut table = [0u8; 256];
                        for (i, &c) in B64.iter().enumerate() { table[c as usize] = i as u8; }
                        let cleaned: Vec<u8> = input.bytes().filter(|&c| c != b'=').collect();
                        let mut out = Vec::new();
                        for chunk in cleaned.chunks(4) {
                            let n = chunk.iter().enumerate()
                                .fold(0u32, |acc, (i, c)| acc | ((table[*c as usize] as u32) << (18 - i * 6)));
                            for i in 0..3 {
                                if i < chunk.len().saturating_sub(1) || chunk.len() == 4 && i < 3 {
                                    let byte = ((n >> (16 - i * 8)) & 0xFF) as u8;
                                    if i < chunk.len() - 1 || chunk.len() == 4 { out.push(byte); }
                                }
                            }
                        }
                        Ok(Some(Value::Str(String::from_utf8_lossy(&out).to_string().into())))
                    }
                    "url_encode" => {
                        let mut out = String::new();
                        for b in input.bytes() {
                            match b {
                                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~'
                                    => out.push(b as char),
                                _ => out.push_str(&format!("%{:02X}", b)),
                            }
                        }
                        Ok(Some(Value::Str(out.into())))
                    }
                    "url_decode" => {
                        let bytes = input.as_bytes();
                        let mut out = Vec::new();
                        let mut i = 0;
                        while i < bytes.len() {
                            if bytes[i] == b'%' && i + 2 < bytes.len() {
                                let h = u8::from_str_radix(&input[i+1..i+3], 16).unwrap_or(0);
                                out.push(h); i += 3;
                            } else if bytes[i] == b'+' { out.push(b' '); i += 1; }
                            else { out.push(bytes[i]); i += 1; }
                        }
                        Ok(Some(Value::Str(String::from_utf8_lossy(&out).to_string().into())))
                    }
                    _ => unreachable!(),
                }
            }
            "sha256" => {
                let input = match args.get(0) { Some(Value::Str(s)) => s.clone(), _ => return Err("sha256(str)".into()) };
                Ok(Some(Value::Str(Self::sha256_hex(input.as_bytes()).into())))
            }
            "parse_date" => {
                // parse_date("YYYY-MM-DD[ HH:MM:SS]") -> epoch seconds (UTC)
                let s_input = match args.get(0) { Some(Value::Str(s)) => s.clone(), _ => return Err("parse_date(str)".into()) };
                let parts: Vec<&str> = s_input.split(|c| c == '-' || c == ' ' || c == ':').collect();
                if parts.len() < 3 { return Err("parse_date: expected YYYY-MM-DD".into()); }
                let num = |i: usize| parts.get(i).and_then(|p| p.parse::<i64>().ok()).unwrap_or(0);
                let (y, m, d) = (num(0), num(1), num(2));
                let (h, mi, sc) = (num(3), num(4), num(5));
                if y < 1970 || m < 1 || m > 12 || d < 1 || d > 31 {
                    return Err("parse_date: bad date".into());
                }
                // days-from-civil (Howard Hinnant)
                let y_adj = if m <= 2 { y - 1 } else { y };
                let era = if y_adj >= 0 { y_adj } else { y_adj - 399 }.div_euclid(400);
                let yoe = y_adj - era * 400;
                let mp = if m > 2 { m - 3 } else { m + 9 };
                let doy = (153 * mp + 2) / 5 + d - 1;
                let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
                let days = era * 146097 + doe - 719468;
                Ok(Some(Value::Int(days * 86400 + h * 3600 + mi * 60 + sc)))
            }
            "serve" => {
                if args.len() != 2 { return Err("serve() expects (port, handler_fn)".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(port), Value::Closure(params, body, captured)) => {
                        self.serve_requests(*port, params.clone(), body.clone(), captured.clone())
                            .map_err(|e| format!("Server error: {}", e))?;
                        Ok(Some(Value::Null))
                    }
                    _ => Err("serve() expects (port_int, closure)".into()),
                }
            }
            "re_match" | "re_find" | "re_replace" | "re_split" | "re_captures" => {
                if args.len() < 2 { return Err("regex builtin expects (string, pattern)".into()); }
                let (s, pat) = match (&args[0], &args[1]) {
                    (Value::Str(s), Value::Str(p)) => (s.clone(), p.clone()),
                    _ => return Err("regex expects (string, pattern)".into()),
                };
                let re = regex::Regex::new(&pat)
                    .map_err(|e| format!("Bad regex: {}", e))?;
                match name {
                    "re_match" => Ok(Some(Value::Bool(re.is_match(&s)))),
                    "re_find" => Ok(Some(match re.find(&s) {
                        Some(m) => Value::Str(m.as_str().to_string().into()),
                        None => Value::Null,
                    })),
                    "re_replace" => {
                        match args.get(2) {
                            Some(Value::Str(rep)) => Ok(Some(Value::Str(re.replace_all(&s, &**rep).to_string().into()))),
                            _ => Err("re_replace expects (string, pattern, replacement)".into()),
                        }
                    }
                    "re_split" => {
                        let parts: Vec<Value> = re.split(&s).map(|p| Value::Str(p.to_string().into())).collect();
                        Ok(Some(Value::List(parts.into_iter().collect())))
                    }
                    "re_captures" => {
                        // re_captures(s, pat) -> list of capture-group strings (or null)
                        Ok(Some(match re.captures(&s) {
                            Some(caps) => Value::List(
                                caps.iter()
                                    .map(|m| m.map(|g| Value::Str(g.as_str().to_string().into()))
                                              .unwrap_or(Value::Null))
                                    .collect()),
                            None => Value::Null,
                        }))
                    }
                    _ => unreachable!(),
                }
            }
            "date" | "date_fmt" => {
                // date() -> "YYYY-MM-DD HH:MM:SS" (now, UTC); date(fmt) formats now.
                // date_fmt(ts[, fmt]) formats an epoch-seconds timestamp.
                let (secs, fmt_arg) = if name == "date_fmt" {
                    let ts = match args.get(0) { Some(Value::Int(t)) => *t, _ => return Err("date_fmt(ts[, fmt])".into()) };
                    (ts, args.get(1))
                } else {
                    (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                     args.get(0))
                };
                let (y, m, d, h, mi, sc) = Self::civil_from_secs(secs);
                let base = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, mi, sc);
                let out = match fmt_arg {
                    Some(Value::Str(f)) => f
                        .replace("%Y", &format!("{:04}", y))
                        .replace("%m", &format!("{:02}", m))
                        .replace("%d", &format!("{:02}", d))
                        .replace("%H", &format!("{:02}", h))
                        .replace("%M", &format!("{:02}", mi))
                        .replace("%S", &format!("{:02}", sc)),
                    _ => base,
                };
                Ok(Some(Value::Str(out.into())))
            }
            "date_add" => {
                // date_add(ts, n[, unit]) -> ts shifted by n units
                //   unit: "days" (default) | "hours" | "mins" | "secs" | "weeks"
                let ts = match args.get(0) { Some(Value::Int(t)) => *t, _ => return Err("date_add(ts, n[, unit])".into()) };
                let n = match args.get(1) { Some(Value::Int(v)) => *v, _ => return Err("date_add(ts, n[, unit])".into()) };
                let mult = match args.get(2) {
                    Some(Value::Str(u)) => match &**u {
                        "secs" | "sec" | "s" => 1,
                        "mins" | "min" | "m" => 60,
                        "hours" | "hour" | "h" => 3600,
                        "days" | "day" | "d" => 86400,
                        "weeks" | "week" | "w" => 604800,
                        _ => return Err("date_add unit: secs|mins|hours|days|weeks".into()),
                    },
                    None => 86400,
                    _ => return Err("date_add(ts, n[, unit])".into()),
                };
                Ok(Some(Value::Int(ts + n * mult)))
            }
            "render" => {
                if args.len() != 2 { return Err("render() expects (template, data)".into()); }
                match (&args[0], &args[1]) {
                    (Value::Str(tpl), Value::Dict(data)) => {
                        let mut result = tpl.clone();
                        for (k, v) in data {
                            let key = format!("{{{}}}", k);
                            result = result.replace(&*key, &format!("{}", v)).into();
                        }
                        Ok(Some(Value::Str(result)))
                    }
                    _ => Err("render() expects (template_str, dict)".into()),
                }
            }
            "db_open" => {
                if args.len() != 1 { return Err("db_open() expects a path".into()); }
                match &args[0] {
                    Value::Str(path) => {
                        match rusqlite::Connection::open(&**path) {
                            Ok(conn) => {
                                // Store connection in a global — use a simple ID-based registry
                                let id = self.next_db_id;
                                self.db_connections.insert(id, conn);
                                self.next_db_id += 1;
                                Ok(Some(Value::Int(id)))
                            }
                            Err(e) => Err(format!("db_open failed: {}", e)),
                        }
                    }
                    _ => Err("db_open() expects a path string".into()),
                }
            }
            "db_exec" => {
                if args.len() != 2 { return Err("db_exec() expects (db_id, sql)".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(id), Value::Str(sql)) => {
                        match self.db_connections.get(id) {
                            Some(conn) => {
                                match conn.execute(sql, []) {
                                    Ok(n) => Ok(Some(Value::Int(n as i64))),
                                    Err(e) => Err(format!("db_exec failed: {}", e)),
                                }
                            }
                            None => Err(format!("Invalid db id: {}", id)),
                        }
                    }
                    _ => Err("db_exec() expects (db_id, sql_string)".into()),
                }
            }
            "db_query" => {
                if args.len() != 2 { return Err("db_query() expects (db_id, sql)".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(id), Value::Str(sql)) => {
                        match self.db_connections.get(id) {
                            Some(conn) => {
                                match conn.prepare(sql) {
                                    Ok(mut stmt) => {
                                        let cols = stmt.column_names();
                                        let col_names: Vec<String> = cols.iter().map(|c| c.to_string()).collect();
                                        match stmt.query([]) {
                                            Ok(mut rows) => {
                                                let mut results = Vec::new();
                                                while let Ok(Some(row)) = rows.next() {
                                                    let mut record = std::collections::HashMap::new();
                                                    for (i, col) in col_names.iter().enumerate() {
                                                        let val: rusqlite::Result<rusqlite::types::Value> = row.get(i);
                                                        let v = match val {
                                                            Ok(rusqlite::types::Value::Null) => Value::Null,
                                                            Ok(rusqlite::types::Value::Integer(n)) => Value::Int(n),
                                                            Ok(rusqlite::types::Value::Real(f)) => Value::Float(f),
                                                            Ok(rusqlite::types::Value::Text(s)) => Value::Str(s.into()),
                                                            Ok(rusqlite::types::Value::Blob(_)) => Value::Null,
                                                            _ => Value::Null,
                                                        };
                                                        record.insert(col.clone(), v);
                                                    }
                                                    results.push(Value::Dict(record.into_iter().collect()));
                                                }
                                                Ok(Some(Value::List(results.into_iter().collect())))
                                            }
                                            Err(e) => Err(format!("db_query failed: {}", e)),
                                        }
                                    }
                                    Err(e) => Err(format!("db_query prepare failed: {}", e)),
                                }
                            }
                            None => Err(format!("Invalid db id: {}", id)),
                        }
                    }
                    _ => Err("db_query() expects (db_id, sql_string)".into()),
                }
            }
            "db_close" => {
                if args.len() != 1 { return Err("db_close() expects a db_id".into()); }
                match &args[0] {
                    Value::Int(id) => {
                        self.db_connections.remove(id);
                        Ok(Some(Value::Bool(true)))
                    }
                    _ => Err("db_close() expects a db_id".into()),
                }
            }
            "input" => {
                if !args.is_empty() {
                    if let Value::Str(prompt) = &args[0] {
                        if let Some(ref buf) = self.print_buffer {
                            // IDE mode — can't read stdin, return empty
                            eprintln!("[input prompt: {}]", prompt);
                        } else {
                            print!("{}", prompt);
                            use std::io::Write;
                            std::io::stdout().flush().ok();
                        }
                    }
                }
                let mut line = String::new();
                if self.print_buffer.is_some() {
                    // IDE mode — no stdin available
                    return Ok(Some(Value::Str("".into())));
                }
                match std::io::stdin().read_line(&mut line) {
                    Ok(_) => Ok(Some(Value::Str(line.trim_end().to_string().into()))),
                    Err(e) => Err(format!("Input error: {}", e)),
                }
            }
            "dict" => Ok(Some(Value::Dict(im::HashMap::new()))),
            "float" => {
                if args.len() != 1 { return Err("float() expects 1 argument".into()); }
                match &args[0] {
                    Value::Int(n) => Ok(Some(Value::Float(*n as f64))),
                    Value::Float(n) => Ok(Some(Value::Float(*n))),
                    Value::Str(s) => {
                        s.trim().parse::<f64>()
                            .map(|n| Some(Value::Float(n)))
                            .map_err(|_| format!("Cannot convert '{}' to float", s))
                    }
                    _ => Err("float() expects a number or string".into()),
                }
            }
            // Math builtins
            "sqrt" => {
                if args.len() != 1 { return Err("sqrt() expects 1 argument".into()); }
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.sqrt())))
            }
            "sin" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.sin())))
            }
            "cos" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.cos())))
            }
            "tan" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.tan())))
            }
            "log" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.ln())))
            }
            "log10" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.log10())))
            }
            "exp" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.exp())))
            }
            "pow" => {
                if args.len() != 2 { return Err("pow() expects 2 arguments".into()); }
                let base = self.to_f64(&args[0])?;
                let exp = self.to_f64(&args[1])?;
                Ok(Some(Value::Float(base.powf(exp))))
            }
            "floor" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Int(n.floor() as i64)))
            }
            "ceil" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Int(n.ceil() as i64)))
            }
            "round" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Int(n.round() as i64)))
            }
            "fabs" => {
                let n = self.to_f64(&args[0])?;
                Ok(Some(Value::Float(n.abs())))
            }
            "pi" => Ok(Some(Value::Float(std::f64::consts::PI))),
            "e" => Ok(Some(Value::Float(std::f64::consts::E))),
            // JSON builtins
            "json_parse" => {
                if args.len() != 1 { return Err("json_parse() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(s) => {
                        let v: serde_json::Value = serde_json::from_str(s)
                            .map_err(|e| format!("JSON parse error: {}", e))?;
                        Ok(Some(Self::json_to_value(v)))
                    }
                    _ => Err("json_parse() expects a string".into()),
                }
            }
            "json_stringify" => {
                if args.len() != 1 { return Err("json_stringify() expects 1 argument".into()); }
                let json = Self::value_to_json(&args[0]);
                Ok(Some(Value::Str(serde_json::to_string_pretty(&json)
                    .map_err(|e| format!("JSON stringify error: {}", e))?.into())))
            }
            _ => Ok(None),
        }
    }

    fn to_f64(&self, v: &Value) -> Result<f64, String> {
        match v {
            Value::Int(n) => Ok(*n as f64),
            Value::Float(n) => Ok(*n),
            _ => Err(format!("Expected number, got {}", v)),
        }
    }

    fn flatten_args(args: &[Value]) -> Result<Vec<Value>, String> {
        if args.len() == 1 {
            if let Value::List(items) = &args[0] {
                return Ok(items.iter().cloned().collect());
            }
        }
        Ok(args.to_vec())
    }

    fn json_to_value(json: serde_json::Value) -> Value {
        match json {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    Value::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::String(s) => Value::Str(s.into()),
            serde_json::Value::Array(arr) => {
                Value::List(arr.into_iter().map(Self::json_to_value).collect())
            }
            serde_json::Value::Object(obj) => {
                let mut map = HashMap::new();
                for (k, v) in obj {
                    map.insert(k, Self::json_to_value(v));
                }
                Value::Dict(map.into_iter().collect())
            }
        }
    }

    fn value_to_json(v: &Value) -> serde_json::Value {
        match v {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Int(n) => serde_json::Value::Number((*n).into()),
            Value::Float(n) => {
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }
            Value::Str(s) => serde_json::Value::String(s.to_string()),
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(Self::value_to_json).collect())
            }
            Value::Dict(entries) => {
                let mut map = serde_json::Map::new();
                for (k, val) in entries {
                    map.insert(k.clone(), Self::value_to_json(val));
                }
                serde_json::Value::Object(map)
            }
            Value::Set(items) => {
                serde_json::Value::Array(items.iter().map(Self::value_to_json).collect())
            }
            Value::Struct(name, fields) => {
                let mut map = serde_json::Map::new();
                map.insert("__struct__".into(), serde_json::Value::String(name.clone()));
                for (k, val) in fields {
                    map.insert(k.clone(), Self::value_to_json(val));
                }
                serde_json::Value::Object(map)
            }
            Value::Closure(_, _, _) => serde_json::Value::String("<closure>".into()),
        }
    }

    fn eval_method_call(&mut self, obj: &Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        // Module dispatch: `lib.f()` — obj is the module marker dict
        if let Value::Dict(d) = obj {
            if let Some(Value::Str(m)) = d.get("__mod") {
                let fname = format!("{}::{}", m, method);
                if self.functions.contains_key(&fname) {
                    return self.call_function(&fname, None, args);
                }
            }
        }
        match (obj, method) {
            // String methods
            (Value::Str(s), "upper") => Ok(Value::Str(s.to_uppercase().into())),
            (Value::Str(s), "lower") => Ok(Value::Str(s.to_lowercase().into())),
            (Value::Str(s), "trim") => Ok(Value::Str(s.trim().to_string().into())),
            (Value::Str(s), "len") => Ok(Value::Int(s.chars().count() as i64)),
            (Value::Str(s), "split") => {
                if args.len() != 1 { return Err("split() expects 1 argument".into()); }
                self.step_count += (s.len() / 4) as u64;
                match &args[0] {
                    Value::Str(sep) => {
                        let parts: Vec<Value> = s.split(&**sep).map(|p| Value::Str(p.to_string().into())).collect();
                        Ok(Value::List(parts.into_iter().collect()))
                    }
                    _ => Err("split() expects a string separator".into()),
                }
            }
            (Value::Str(s), "contains") => {
                if args.len() != 1 { return Err("contains() expects 1 argument".into()); }
                self.step_count += (s.len() / 4) as u64;
                match &args[0] {
                    Value::Str(sub) => Ok(Value::Bool(s.contains(&**sub))),
                    _ => Err("contains() expects a string".into()),
                }
            }
            (Value::Str(s), "replace") => {
                if args.len() != 2 { return Err("replace() expects 2 arguments".into()); }
                self.step_count += (s.len() / 4) as u64;
                match (&args[0], &args[1]) {
                    (Value::Str(old), Value::Str(new)) => Ok(Value::Str(s.replace(&**old, &**new).into())),
                    _ => Err("replace() expects two strings".into()),
                }
            }
            (Value::Str(s), "chars") => {
                let chars: Vec<Value> = s.chars().map(|c| Value::Str(c.to_string().into())).collect();
                Ok(Value::List(chars.into_iter().collect()))
            }
            (Value::Str(s), "starts_with") => {
                if args.len() != 1 { return Err("starts_with() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(prefix) => Ok(Value::Bool(s.starts_with(&**prefix))),
                    _ => Err("starts_with() expects a string".into()),
                }
            }
            (Value::Str(s), "ends_with") => {
                if args.len() != 1 { return Err("ends_with() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(suffix) => Ok(Value::Bool(s.ends_with(&**suffix))),
                    _ => Err("ends_with() expects a string".into()),
                }
            }
            (Value::Str(s), "find") => {
                if args.len() != 1 { return Err("find() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(sub) => Ok(Value::Int(s.find(&**sub).map(|i| i as i64).unwrap_or(-1))),
                    _ => Err("find() expects a string".into()),
                }
            }
            (Value::Str(s), "substr") => {
                if args.len() != 2 { return Err("substr() expects 2 arguments (start, end)".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(start), Value::Int(end)) => {
                        let st = (*start).max(0) as usize;
                        let en = (*end).max(0) as usize;
                        let en = en.min(s.len());
                        if st >= en { return Ok(Value::Str("".into())); }
                        Ok(Value::Str(s[st..en].to_string().into()))
                    }
                    _ => Err("substr() expects two integers".into()),
                }
            }
            (Value::Str(s), "repeat") => {
                if args.len() != 1 { return Err("repeat() expects 1 argument".into()); }
                match &args[0] {
                    Value::Int(n) => Ok(Value::Str(s.repeat((*n).max(0) as usize).into())),
                    _ => Err("repeat() expects an integer".into()),
                }
            }
            (Value::Str(s), "index_of") => {
                if args.len() != 1 { return Err("index_of() expects 1 argument".into()); }
                self.step_count += (s.len() / 4) as u64;
                match &args[0] {
                    Value::Str(ch) => {
                        if let Some(c) = ch.chars().next() {
                            Ok(Value::Int(s.find(c).map(|i| i as i64).unwrap_or(-1)))
                        } else {
                            Ok(Value::Int(-1))
                        }
                    }
                    _ => Err("index_of() expects a string".into()),
                }
            }
            (Value::Str(s), "pad") => match args.get(0) {
                Some(Value::Int(n)) => {
                    let n = *n as usize;
                    let ch = match args.get(1) {
                        Some(Value::Str(c)) => c.chars().next().unwrap_or(' '),
                        _ => ' ',
                    };
                    Ok(Value::Str(if s.len() >= n { s.clone() }
                        else { format!("{}{}", s, ch.to_string().repeat(n - s.len())).into() }))
                }
                _ => Err("pad(len[, char]) expects int".into()),
            },
            (Value::Str(s), "split_at") => match args.get(0) {
                Some(Value::Int(i)) => {
                    let i = (*i).clamp(0, s.len() as i64) as usize;
                    Ok(Value::List(im::vector![
                        Value::Str(s[..i].to_string().into()), Value::Str(s[i..].to_string().into())]))
                }
                _ => Err("split_at(i) expects int".into()),
            },
            // === Phase 1 stdlib: lists ===
            (Value::List(items), "join") => {
                self.step_count += (items.len() as u64) * 2;
                let sep = match args.get(0) {
                    Some(Value::Str(s)) => s.clone(),
                    _ => ", ".into(),
                };
                Ok(Value::Str(items.iter().map(|v| format!("{}", v)).collect::<Vec<_>>().join(&sep).into()))
            }
            (Value::List(items), "contains") => {
                self.step_count += (items.len() as u64) * 2;
                match args.get(0) {
                Some(v) => Ok(Value::Bool(items.iter().any(|i| self.values_eq(i, v)))),
                None => Err("contains(x) needs an argument".into()),
                }
            },
            (Value::List(items), "index_of") => {
                self.step_count += (items.len() as u64) * 2;
                match args.get(0) {
                    Some(v) => Ok(Value::Int(
                        items.iter().position(|i| self.values_eq(i, v))
                            .map(|p| p as i64).unwrap_or(-1))),
                    None => Err("index_of(x) needs an argument".into()),
                }
            },
            (Value::List(items), "enumerate") => {
                self.step_count += (items.len() as u64) * 2;
                Ok(Value::List(
                items.iter().enumerate()
                    .map(|(i, v)| Value::List(im::vector![Value::Int(i as i64), v.clone()]))
                    .collect()))
            },
            (Value::List(items), "zip") => {
                self.step_count += (items.len() as u64) * 2;
                match args.get(0) {
                Some(Value::List(other)) => Ok(Value::List(
                    items.iter().zip(other.iter())
                        .map(|(a, b)| Value::List(im::vector![a.clone(), b.clone()]))
                        .collect())),
                _ => Err("zip(other) expects a list".into()),
                }
            },
            (Value::List(items), "flatten") => {
                self.step_count += (items.len() as u64) * 2;
                let mut out = Vec::new();
                for v in items {
                    match v {
                        Value::List(inner) => out.extend(inner.iter().cloned()),
                        _ => out.push(v.clone()),
                    }
                }
                Ok(Value::List(out.into_iter().collect()))
            }
            (Value::List(items), "take") => match args.get(0) {
                Some(Value::Int(n)) => {
                    self.step_count += ((*n).clamp(0, items.len() as i64) as u64) * 2;
                    Ok(Value::List(items.iter().take((*n).max(0) as usize).cloned().collect()))
                }
                _ => Err("take(n) expects int".into()),
            },
            (Value::List(items), "skip") => match args.get(0) {
                Some(Value::Int(n)) => {
                    self.step_count += (items.len() as u64).saturating_sub((*n).max(0) as u64) * 2;
                    Ok(Value::List(items.iter().skip((*n).max(0) as usize).cloned().collect()))
                }
                _ => Err("skip(n) expects int".into()),
            },
            (Value::List(items), "sort_by") => match args.get(0) {
                Some(f @ Value::Closure(_, _, _)) => {
                    let mut keyed: Vec<(Value, Value)> = Vec::new();
                    for v in items {
                        keyed.push((self.call_closure_value(f, vec![v.clone()])?, v.clone()));
                    }
                    keyed.sort_by(|a, b| self.cmp_values(&a.0, &b.0));
                    Ok(Value::List(keyed.into_iter().map(|(_, v)| v).collect()))
                }
                _ => Err("sort_by(fn) expects a closure".into()),
            },
            (Value::List(items), "group_by") => match args.get(0) {
                Some(f @ Value::Closure(_, _, _)) => {
                    let mut groups: HashMap<String, Value> = HashMap::new();
                    for v in items {
                        let k = format!("{}", self.call_closure_value(f, vec![v.clone()])?);
                        match groups.get_mut(&k) {
                            Some(Value::List(l)) => l.push_back(v.clone()),
                            _ => { groups.insert(k, Value::List(im::vector![v.clone()])); }
                        }
                    }
                    Ok(Value::Dict(groups.into_iter().collect()))
                }
                _ => Err("group_by(fn) expects a closure".into()),
            },
            (Value::List(items), m @ ("min_by" | "max_by")) => match args.get(0) {
                Some(f @ Value::Closure(_, _, _)) => {
                    if items.is_empty() { return Ok(Value::Null); }
                    let mut best = items[0].clone();
                    let mut best_key = self.call_closure_value(f, vec![best.clone()])?;
                    for v in items.iter().skip(1) {
                        let k = self.call_closure_value(f, vec![v.clone()])?;
                        let ord = self.cmp_values(&k, &best_key);
                        if (m == "min_by" && ord == std::cmp::Ordering::Less)
                            || (m == "max_by" && ord == std::cmp::Ordering::Greater) {
                            best = v.clone();
                            best_key = k;
                        }
                    }
                    Ok(best)
                }
                _ => Err("min_by/max_by(fn) expects a closure".into()),
            },
            // === additional list ops ===
            (Value::List(items), "sum") => {
                self.step_count += items.len() as u64;
                let mut total = 0i64;
                let mut ftotal = 0f64;
                let mut has_float = false;
                for v in items {
                    match v {
                        Value::Int(i) => { total += i; ftotal += *i as f64; }
                        Value::Float(f) => { has_float = true; ftotal += f; }
                        _ => return Err("sum() expects numeric items".into()),
                    }
                }
                Ok(if has_float { Value::Float(ftotal) } else { Value::Int(total) })
            }
            (Value::List(items), "product") => {
                self.step_count += items.len() as u64;
                let mut total = 1i64;
                let mut ftotal = 1f64;
                let mut has_float = false;
                for v in items {
                    match v {
                        Value::Int(i) => { total *= i; ftotal *= *i as f64; }
                        Value::Float(f) => { has_float = true; ftotal *= f; }
                        _ => return Err("product() expects numeric items".into()),
                    }
                }
                Ok(if has_float { Value::Float(ftotal) } else { Value::Int(total) })
            }
            (Value::List(items), "first") => Ok(items.head().cloned().unwrap_or(Value::Null)),
            (Value::List(items), "last") => Ok(items.last().cloned().unwrap_or(Value::Null)),
            (Value::List(items), "uniq") => {
                let mut seen = std::collections::HashSet::new();
                let out: Vec<Value> = items.iter()
                    .filter(|v| seen.insert(serde_json::to_string(v).unwrap_or_default()))
                    .cloned().collect();
                Ok(Value::List(out.into_iter().collect()))
            }
            (Value::List(items), m @ ("any" | "all" | "none")) => match args.get(0) {
                Some(f @ Value::Closure(_, _, _)) => {
                    let mut found_true = 0usize;
                    let mut found_false = 0usize;
                    for v in items {
                        match self.call_closure_value(f, vec![v.clone()])? {
                            Value::Bool(true) => found_true += 1,
                            Value::Bool(false) => found_false += 1,
                            _ => return Err(format!("{}(fn) must return bool", m)),
                        }
                    }
                    Ok(Value::Bool(match m {
                        "any" => found_true > 0,
                        "all" => found_false == 0,
                        _ => found_true == 0, // none
                    }))
                }
                _ => Err(format!("{}(fn) expects a closure", m)),
            },
            (Value::List(items), "partition") => match args.get(0) {
                Some(f @ Value::Closure(_, _, _)) => {
                    let mut t = Vec::new();
                    let mut fl = Vec::new();
                    for v in items {
                        match self.call_closure_value(f, vec![v.clone()])? {
                            Value::Bool(true) => t.push(v.clone()),
                            Value::Bool(false) => fl.push(v.clone()),
                            _ => return Err("partition(fn) must return bool".into()),
                        }
                    }
                    Ok(Value::List(im::vector![
                        Value::List(t.into_iter().collect()),
                        Value::List(fl.into_iter().collect()),
                    ]))
                }
                _ => Err("partition(fn) expects a closure".into()),
            },
            // === Phase 1 stdlib: dicts ===
            (Value::Dict(d), "values") =>
                Ok(Value::List(d.values().cloned().collect())),
            (Value::Dict(d), "items") =>
                Ok(Value::List(d.iter()
                    .map(|(k, v)| Value::List(im::vector![Value::Str(k.clone().into()), v.clone()]))
                    .collect())),
            (Value::Dict(d), "keys") =>
                Ok(Value::List(d.keys().map(|k| Value::Str(k.clone().into())).collect())),
            (Value::Dict(d), "merge") => match args.get(0) {
                Some(Value::Dict(other)) => {
                    let mut m = d.clone();
                    m.extend(other.iter().map(|(k, v)| (k.clone(), v.clone())));
                    Ok(Value::Dict(m))
                }
                _ => Err("merge(dict) expects a dict".into()),
            },
            (Value::Dict(d), "delete") => match args.get(0) {
                Some(Value::Str(k)) => {
                    let mut m = d.clone();
                    m.remove(&**k);
                    Ok(Value::Dict(m))
                }
                _ => Err("delete(key) expects a string".into()),
            },
            (Value::Dict(d), "get_or") => match (args.get(0), args.get(1)) {
                (Some(Value::Str(k)), Some(def)) =>
                    Ok(d.get(&**k).cloned().unwrap_or_else(|| def.clone())),
                _ => Err("get_or(key, default) — key must be a string".into()),
            },
            (Value::Str(s), "ltrim") => Ok(Value::Str(s.trim_start().to_string().into())),
            (Value::Str(s), "rtrim") => Ok(Value::Str(s.trim_end().to_string().into())),
            (Value::Str(s), "to_lower") => Ok(Value::Str(s.to_lowercase().into())),
            (Value::Str(s), "to_upper") => Ok(Value::Str(s.to_uppercase().into())),
            (Value::Str(s), "split_lines") => {
                let parts: Vec<Value> = s.lines().map(|l| Value::Str(l.to_string().into())).collect();
                Ok(Value::List(parts.into_iter().collect()))
            }
            (Value::Str(s), "count") => {
                if args.len() != 1 { return Err("count() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(sub) => Ok(Value::Int(s.matches(&**sub).count() as i64)),
                    _ => Err("count() expects a string".into()),
                }
            }
            (Value::Str(s), "is_empty") => Ok(Value::Bool(s.is_empty())),
            // List methods
            (Value::List(items), "push") => {
                let mut new_items = items.clone();
                if args.len() != 1 { return Err("push() expects 1 argument".into()); }
                new_items.push_back(args[0].clone());
                Ok(Value::List(new_items))
            }
            (Value::List(items), "len") => Ok(Value::Int(items.len() as i64)),
            (Value::List(items), "reverse") => {
                self.step_count += (items.len() as u64) * 4;
                let mut new_items = items.clone();
                new_items = new_items.iter().rev().cloned().collect();
                Ok(Value::List(new_items))
            }
            (Value::List(items), "get") => {
                if args.len() != 1 { return Err("get() expects 1 argument".into()); }
                match &args[0] {
                    Value::Int(i) => {
                        if *i < 0 || *i as usize >= items.len() {
                            Ok(Value::Null)
                        } else {
                            Ok(items[*i as usize].clone())
                        }
                    }
                    _ => Err("get() expects an integer index".into()),
                }
            }
            (Value::List(items), "sort") => {
                let mut sorted = items.clone();
                let cmp_steps = std::cell::Cell::new((items.len() as u64) * 4);
                sorted.sort_by(|a, b| {
                    cmp_steps.set(cmp_steps.get() + 6);
                    match (a, b) {
                        (Value::Int(a), Value::Int(b)) => a.cmp(b),
                        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
                        (Value::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
                        (Value::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)).unwrap_or(std::cmp::Ordering::Equal),
                        (Value::Str(a), Value::Str(b)) => a.cmp(b),
                        _ => std::cmp::Ordering::Equal,
                    }
                });
                self.step_count += cmp_steps.get();
                Ok(Value::List(sorted.into_iter().collect()))
            }
            (Value::List(items), "map") => {
                if args.len() != 1 { return Err("map() expects 1 closure argument".into()); }
                let closure = args[0].clone();
                let mut result = Vec::new();
                for item in items {
                    let r = self.call_closure_value(&closure, vec![item.clone()])?;
                    result.push(r);
                }
                Ok(Value::List(result.into_iter().collect()))
            }
            (Value::List(items), "filter") => {
                if args.len() != 1 { return Err("filter() expects 1 closure argument".into()); }
                let closure = args[0].clone();
                let mut result = Vec::new();
                for item in items.iter() {
                    let r = self.call_closure_value(&closure, vec![item.clone()])?;
                    if self.is_truthy(&r) {
                        result.push(item.clone());
                    }
                }
                Ok(Value::List(result.into_iter().collect()))
            }
            (Value::List(items), "reduce") => {
                if args.len() != 2 { return Err("reduce() expects (initial, closure) arguments".into()); }
                let mut acc = args[0].clone();
                let closure = args[1].clone();
                for item in items {
                    acc = self.call_closure_value(&closure, vec![acc, item.clone()])?;
                }
                Ok(acc)
            }
            (Value::List(items), "find") => {
                self.step_count += (items.len() as u64) * 2;
                if args.len() != 1 { return Err("find() expects 1 closure argument".into()); }
                let closure = args[0].clone();
                for item in items.iter() {
                    let r = self.call_closure_value(&closure, vec![item.clone()])?;
                    if self.is_truthy(&r) {
                        return Ok(item.clone());
                    }
                }
                Ok(Value::Null)
            }
            (Value::List(items), "slice") => {
                if args.len() != 2 { return Err("slice() expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(start), Value::Int(end)) => {
                        let s = (*start as usize).min(items.len());
                        let e = (*end as usize).min(items.len());
                        if s <= e {
                            Ok(Value::List(items.iter().skip(s).take(e - s).cloned().collect()))
                        } else {
                            Ok(Value::List(im::vector![]))
                        }
                    }
                    _ => Err("slice() expects two integers".into()),
                }
            }
            // Dict methods
            (Value::Dict(entries), "put") => {
                if args.len() != 2 { return Err("put() expects 2 arguments (key, value)".into()); }
                match &args[0] {
                    Value::Str(key) => {
                        let mut new_entries = entries.clone();
                        new_entries.insert(key.to_string(), args[1].clone());
                        Ok(Value::Dict(new_entries))
                    }
                    _ => Err("put() expects a string key".into()),
                }
            }
            (Value::Dict(entries), "get") => {
                if args.len() != 1 { return Err("get() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(key) => {
                        Ok(entries.get(&**key).cloned().unwrap_or(Value::Null))
                    }
                    _ => Err("get() expects a string key".into()),
                }
            }
            (Value::Dict(entries), "has") => {
                if args.len() != 1 { return Err("has() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(key) => Ok(Value::Bool(entries.contains_key(&**key))),
                    _ => Err("has() expects a string key".into()),
                }
            }
            (Value::Dict(entries), "len") => Ok(Value::Int(entries.len() as i64)),
            (Value::Dict(entries), "remove") => {
                if args.len() != 1 { return Err("remove() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(key) => {
                        let mut new_entries = entries.clone();
                        new_entries.remove(&**key);
                        Ok(Value::Dict(new_entries))
                    }
                    _ => Err("remove() expects a string key".into()),
                }
            }
            // Set methods
            (Value::Set(items), "add") => {
                if args.len() != 1 { return Err("add() expects 1 argument".into()); }
                let mut new_items = items.clone();
                if !new_items.iter().any(|x| self.values_eq(x, &args[0])) {
                    new_items.push(args[0].clone());
                }
                Ok(Value::Set(new_items))
            }
            (Value::Set(items), "remove") => {
                if args.len() != 1 { return Err("remove() expects 1 argument".into()); }
                let new_items: Vec<Value> = items.iter().filter(|x| !self.values_eq(x, &args[0])).cloned().collect();
                Ok(Value::Set(new_items))
            }
            (Value::Set(items), "has") => {
                if args.len() != 1 { return Err("has() expects 1 argument".into()); }
                Ok(Value::Bool(items.iter().any(|x| self.values_eq(x, &args[0]))))
            }
            (Value::Set(items), "to_list") => Ok(Value::List(items.clone().into_iter().collect())),
            (Value::Set(items), "len") => Ok(Value::Int(items.len() as i64)),
            (Value::Set(items), "union") => {
                if args.len() != 1 { return Err("union() expects 1 argument".into()); }
                match &args[0] {
                    Value::Set(other) => {
                        let mut new_items = items.clone();
                        for v in other {
                            if !new_items.iter().any(|x| self.values_eq(x, v)) {
                                new_items.push(v.clone());
                            }
                        }
                        Ok(Value::Set(new_items))
                    }
                    _ => Err("union() expects a set".into()),
                }
            }
            (Value::Set(items), "intersection") => {
                if args.len() != 1 { return Err("intersection() expects 1 argument".into()); }
                match &args[0] {
                    Value::Set(other) => {
                        let new_items: Vec<Value> = items.iter().filter(|x| other.iter().any(|y| self.values_eq(x, y))).cloned().collect();
                        Ok(Value::Set(new_items))
                    }
                    _ => Err("intersection() expects a set".into()),
                }
            }
            (Value::Set(items), "difference") => {
                if args.len() != 1 { return Err("difference() expects 1 argument".into()); }
                match &args[0] {
                    Value::Set(other) => {
                        let new_items: Vec<Value> = items.iter().filter(|x| !other.iter().any(|y| self.values_eq(x, y))).cloned().collect();
                        Ok(Value::Set(new_items))
                    }
                    _ => Err("difference() expects a set".into()),
                }
            }
            _ => Err(format!("No method '{}' on this value", method)),
        }
    }

    /// Evaluate an expression in the current scope (used by health checker for test assertions)
    pub fn eval_expr_in_scope(&mut self, expr: &Expr) -> Result<Value, String> {
        self.eval_expr(expr)
    }

    // Public method to collect grow statements from a program
    pub fn collect_grows(&self, stmts: &[Stmt]) -> Vec<GrowItem> {
        let mut grows = Vec::new();
        for stmt in stmts {
            if let Stmt::Grow(item) = stmt {
                grows.push(item.clone());
            }
        }
        grows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run_src(src: &str) -> Interpreter {
        let mut lex = Lexer::new(src);
        let tokens = lex.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse_program().unwrap();
        let mut interp = Interpreter::new();
        interp.run(&stmts).unwrap();
        interp
    }

    #[test]
    fn test_let_and_print() {
        // just test it doesn't crash
        run_src("let x = 5\nprint(x)");
    }

    #[test]
    fn test_arithmetic() {
        let interp = run_src("let x = 1 + 2 * 3");
        assert_eq!(interp.vars.borrow().get("x"), Some(&Value::Int(7)));
    }

    #[test]
    fn test_function_call() {
        let interp = run_src("fn add(a: int, b: int) { return a + b }\nlet x = add(3, 4)");
        assert_eq!(interp.vars.borrow().get("x"), Some(&Value::Int(7)));
    }

    #[test]
    fn test_game_engine_builtins() {
        // entities + movement + velocity
        let interp = run_src(r##"
            let e = entity("hero", 2, 7, "hero")
            e = ent_move(e, 3, -2)
            e = ent_vel(e, 1, -1)
            let px = e["x"]
            let py = e["y"]
            let pvy = e["vy"]
            let other = entity("npc", 5.4, 5.4, "hunter")
            let near = collide(e, other)
            let far = entity("npc", 30, 30, "hunter")
            let nohit = collide(e, far)
            let g = game_new(30, 15)
            let gw = g["w"]
            let run = game_running()
            let ks = keys()
            let kl = ks.len()
        "##);
        let v = interp.vars.borrow();
        assert_eq!(v.get("px"), Some(&Value::Float(5.0)));
        assert_eq!(v.get("py"), Some(&Value::Float(5.0)));
        assert_eq!(v.get("pvy"), Some(&Value::Float(-1.0)));
        assert_eq!(v.get("near"), Some(&Value::Bool(true)));
        assert_eq!(v.get("nohit"), Some(&Value::Bool(false)));
        assert_eq!(v.get("gw"), Some(&Value::Float(30.0)));
        assert_eq!(v.get("run"), Some(&Value::Bool(true)));
        assert_eq!(v.get("kl"), Some(&Value::Int(0)));
    }

    #[test]
    fn test_game_scene_and_tile_physics() {
        // scene management + tile collision + gravity landing
        let interp = run_src(r##"
            let s = scene_new(20, 10)
            s = scene_spawn(s, entity("a", 1, 1, "hero"))
            s = scene_spawn(s, entity("h1", 8, 4, "hunter"))
            s = scene_spawn(s, entity("h2", 12, 6, "hunter"))
            let n = scene_entities(s).len()
            let hs = scene_find(s, "hunter").len()
            s = scene_despawn(s, "h1")
            let n2 = scene_entities(s).len()
            let lvl = canvas_new(11, 8, " ")
            lvl = canvas_fill(lvl, 0, 7, 11, 1, "#")
            let p = entity("p", 5, 2, "hero")
            let grounded0 = on_ground(p, lvl)
            let solid_below = tile_solid(lvl, 5, 7)
            let p2 = physics_tick(ent_vel(p, 0, 0), lvl, 0.5)
            let vy1 = p2["vy"]
            let p3 = physics_tick(physics_tick(ent_vel(p, 0, 0), lvl, 0.5), lvl, 0.5)
            let y3 = p3["y"]
        "##);
        let v = interp.vars.borrow();
        assert_eq!(v.get("n"), Some(&Value::Int(3)));
        assert_eq!(v.get("hs"), Some(&Value::Int(2)));
        assert_eq!(v.get("n2"), Some(&Value::Int(2)));
        assert_eq!(v.get("grounded0"), Some(&Value::Bool(false)));
        assert_eq!(v.get("solid_below"), Some(&Value::Bool(true)));
        assert_eq!(v.get("vy1"), Some(&Value::Float(0.5)));
        // p3 falls: 2.5+0.5=3.0 (vy .5→1.0), lands on row-7 floor → y=6.0
        assert_eq!(v.get("y3"), Some(&Value::Float(3.5)));
    }

    #[test]
    fn test_if_else() {
        let interp = run_src("let x = 5\nif x > 3 { let y = 10 } else { let y = 0 }");
        // y is scoped inside if block, so it won't be in vars
    }

    #[test]
    fn test_while_loop() {
        let interp = run_src("let i = 0\nwhile i < 5 { i = i + 1 }");
        assert_eq!(interp.vars.borrow().get("i"), Some(&Value::Int(5)));
    }

    #[test]
    fn test_for_loop() {
        let interp = run_src("let sum = 0\nfor i in range(5) { sum = sum + i }");
        assert_eq!(interp.vars.borrow().get("sum"), Some(&Value::Int(10)));
    }

    #[test]
    fn test_string_concat() {
        let interp = run_src("let x = \"hello\" + \" \" + \"world\"");
        assert_eq!(interp.vars.borrow().get("x"), Some(&Value::Str("hello world".into())));
    }

    #[test]
    fn test_struct() {
        let interp = run_src("struct Point { x: int, y: int }\nlet p = Point { x: 3, y: 4 }");
        let _p_val = interp.vars.borrow().get("p").cloned();
        match _p_val {
            Some(Value::Struct(name, fields)) => {
                assert_eq!(name, "Point");
                assert_eq!(fields.get("x"), Some(&Value::Int(3)));
                assert_eq!(fields.get("y"), Some(&Value::Int(4)));
            }
            _ => panic!("Expected struct value"),
        }
    }

    #[test]
    fn test_field_access() {
        let interp = run_src("struct Point { x: int, y: int }\nlet p = Point { x: 3, y: 4 }\nlet z = p.x");
        assert_eq!(interp.vars.borrow().get("z"), Some(&Value::Int(3)));
    }

    #[test]
    fn test_method_call() {
        let interp = run_src("let s = \"hello\"\nlet u = s.upper()");
        assert_eq!(interp.vars.borrow().get("u"), Some(&Value::Str("HELLO".into())));
    }

    #[test]
    fn test_list_methods() {
        let interp = run_src("let l = [1, 2, 3]\nlet l2 = l.push(4)");
        let _l2_val = interp.vars.borrow().get("l2").cloned();
        match _l2_val {
            Some(Value::List(items)) => {
                assert_eq!(items.len(), 4);
                assert_eq!(items[3], Value::Int(4));
            }
            _ => panic!("Expected list"),
        }
    }

    #[test]
    fn test_boolean_ops() {
        let interp = run_src("let x = true and false\nlet y = true or false\nlet z = not x");
        assert_eq!(interp.vars.borrow().get("x"), Some(&Value::Bool(false)));
        assert_eq!(interp.vars.borrow().get("y"), Some(&Value::Bool(true)));
        assert_eq!(interp.vars.borrow().get("z"), Some(&Value::Bool(true)));
    }

    #[test]
    fn test_collect_grows() {
        let mut lex = Lexer::new("grow fn shout(name: str) { print(name) }");
        let tokens = lex.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse_program().unwrap();
        let interp = Interpreter::new();
        let grows = interp.collect_grows(&stmts);
        assert_eq!(grows.len(), 1);
    }

    #[test]
    fn test_function_variants() {
        let interp = run_src("fn sort(x: int) { return x }\nfn sort(x: int) { return x + 1 }");
        let variants = interp.functions.get("sort").unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].id, 1);
        assert_eq!(variants[1].id, 2);
    }
}

/// Shared module resolution — path as-given, then VELDTPATH dirs, then
/// the shipped stdlib. Used by both the interpreter and the VM so import
/// semantics stay in one place.
pub(crate) fn resolve_import_source(path: &str) -> Result<String, String> {
    let stdlib: &[(&str, &str)] = &[
        ("str", include_str!("../stdlib/str.veldt")),
        ("str.veldt", include_str!("../stdlib/str.veldt")),
        ("math", include_str!("../stdlib/math.veldt")),
        ("math.veldt", include_str!("../stdlib/math.veldt")),
    ];
    std::fs::read_to_string(path)
        .or_else(|_| {
            std::env::var("VELDTPATH").unwrap_or_default()
                .split(if cfg!(windows) { ';' } else { ':' })
                .filter(|d| !d.is_empty())
                .find_map(|d| std::fs::read_to_string(format!("{}/{}", d, path)).ok())
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "nf"))
        })
        .or_else(|_| stdlib.iter().find(|(n, _)| *n == path)
            .map(|(_, src)| src.to_string())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "nf")))
        .map_err(|e| format!("Cannot import '{}': {}", path, e))
}
