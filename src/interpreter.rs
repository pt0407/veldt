// Interpreter for the Veldt language — tree-walking

use crate::ast::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
    Dict(HashMap<String, Value>),
    Struct(String, HashMap<String, Value>),
    Null,
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
        }
    }
}

pub struct FunctionDef {
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
}

pub struct Interpreter {
    pub vars: HashMap<String, Value>,
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
}

pub struct FunctionVariant {
    pub id: u32,
    pub def: FunctionDef,
}

pub enum FlowControl {
    Normal,
    Return(Option<Value>),
}

impl Interpreter {
    pub fn new() -> Self {
        Interpreter {
            vars: HashMap::new(),
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

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<FlowControl, String> {
        self.tick()?;
        match stmt {
            Stmt::Let(name, expr) => {
                let val = self.eval_expr(expr)?;
                self.vars.insert(name.clone(), val);
                Ok(FlowControl::Normal)
            }
            Stmt::Assign(name, expr) => {
                let val = self.eval_expr(expr)?;
                if self.vars.contains_key(name) {
                    self.vars.insert(name.clone(), val);
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
                            self.vars.insert(var_name.clone(), Value::Struct(name, fields));
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
                                    self.vars.insert(var_name.clone(), Value::List(items));
                                } else {
                                    return Err("Can only assign index on a variable".into());
                                }
                                Ok(FlowControl::Normal)
                            }
                            _ => Err("Index must be an integer".into()),
                        }
                    }
                    _ => Err("Cannot index-assign on non-list".into()),
                }
            }
            Stmt::FnDef(name, params, body) => {
                let id = self.functions.get(name)
                    .map(|v| v.len() as u32 + 1)
                    .unwrap_or(1);
                let variant = FunctionVariant {
                    id,
                    def: FunctionDef {
                        params: params.clone(),
                        body: body.clone(),
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
            Stmt::While(cond, body) => {
                loop {
                    let c = self.eval_expr(cond)?;
                    if !self.is_truthy(&c) { break; }
                    match self.exec_block(body)? {
                        FlowControl::Return(v) => return Ok(FlowControl::Return(v)),
                        FlowControl::Normal => {}
                    }
                }
                Ok(FlowControl::Normal)
            }
            Stmt::For(var, iter_expr, body) => {
                let iter_val = self.eval_expr(iter_expr)?;
                match iter_val {
                    Value::List(items) => {
                        for item in items {
                            self.vars.insert(var.clone(), item);
                            match self.exec_block(body)? {
                                FlowControl::Return(v) => return Ok(FlowControl::Return(v)),
                                FlowControl::Normal => {}
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
                        params: params.clone(),
                        body: body.clone(),
                    };
                    let variant = FunctionVariant { id: 1, def };
                    self.functions.entry(name.clone()).or_default().push(variant);
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

    fn is_truthy(&self, val: &Value) -> bool {
        match val {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Null => false,
            Value::List(items) => !items.is_empty(),
            Value::Dict(entries) => !entries.is_empty(),
            Value::Struct(_, fields) => !fields.is_empty(),
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(n) => Ok(Value::Float(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::List(items) => {
                let mut vals = Vec::new();
                for item in items {
                    vals.push(self.eval_expr(item)?);
                }
                Ok(Value::List(vals))
            }
            Expr::Var(name) => {
                self.vars.get(name)
                    .cloned()
                    .ok_or_else(|| format!("Undefined variable: {}", name))
            }
            Expr::BinOp(left, op, right) => {
                let l = self.eval_expr(left)?;
                let r = self.eval_expr(right)?;
                self.eval_binop(&l, op, &r)
            }
            Expr::UnaryOp(op, expr) => {
                let v = self.eval_expr(expr)?;
                match op {
                    UnaryOp::Neg => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        _ => Err("Cannot negate non-integer".into()),
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
                self.call_function(name, *variant, arg_vals)
            }
            Expr::MethodCall(obj, method, args) => {
                let obj_val = self.eval_expr(obj)?;
                let mut arg_vals = Vec::new();
                for a in args {
                    arg_vals.push(self.eval_expr(a)?);
                }
                self.eval_method_call(obj_val, method, arg_vals)
            }
            Expr::FieldAccess(obj, field) => {
                let obj_val = self.eval_expr(obj)?;
                match obj_val {
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
            Expr::Index(obj, index) => {
                let obj_val = self.eval_expr(obj)?;
                let idx_val = self.eval_expr(index)?;
                match (&obj_val, &idx_val) {
                    (Value::List(items), Value::Int(i)) => {
                        if *i < 0 || *i as usize >= items.len() {
                            Err(format!("Index {} out of bounds (len {})", i, items.len()))
                        } else {
                            Ok(items[*i as usize].clone())
                        }
                    }
                    (Value::Str(s), Value::Int(i)) => {
                        if *i < 0 || *i as usize >= s.len() {
                            Err(format!("Index {} out of bounds (len {})", i, s.len()))
                        } else {
                            Ok(Value::Str(s.chars().nth(*i as usize).unwrap().to_string()))
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
                (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b))),
                (Value::Str(a), Value::Int(b)) => Ok(Value::Str(format!("{}{}", a, b))),
                (Value::Str(a), Value::Float(b)) => {
                    let s = if b.fract() == 0.0 { format!("{:.1}", b) } else { format!("{}", b) };
                    Ok(Value::Str(format!("{}{}", a, s)))
                }
                (Value::Int(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b))),
                (Value::Float(a), Value::Str(b)) => {
                    let s = if a.fract() == 0.0 { format!("{:.1}", a) } else { format!("{}", a) };
                    Ok(Value::Str(format!("{}{}", s, b)))
                }
                (Value::Str(a), Value::Bool(b)) => Ok(Value::Str(format!("{}{}", a, b))),
                (Value::Bool(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b))),
                (Value::Str(a), Value::List(b)) => {
                    let strs: Vec<String> = b.iter().map(|v| format!("{}", v)).collect();
                    Ok(Value::Str(format!("{}[{}]", a, strs.join(", "))))
                }
                (Value::Str(a), Value::Dict(b)) => {
                    let strs: Vec<String> = b.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
                    Ok(Value::Str(format!("{}{{{}}}", a, strs.join(", "))))
                }
                (Value::Str(a), Value::Null) => Ok(Value::Str(format!("{}null", a))),
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
                    // dispatch to fittest — for now, just use the last one
                    // TODO: replace with fitness-based dispatch when veldt system is built
                    variants.last().unwrap()
                }
            };
            (chosen.id, chosen.def.params.clone(), chosen.def.body.clone())
        };

        // Log usage for veldt tracking
        self.usage_log.push((name.to_string(), chosen_id));

        // Create new scope
        let old_vars = self.vars.clone();
        self.vars.clear();

        // Bind params
        if args.len() != params.len() {
            self.vars = old_vars;
            return Err(format!("Function {} expects {} args but got {}", name, params.len(), args.len()));
        }
        for (param, val) in params.iter().zip(args.into_iter()) {
            self.vars.insert(param.name.clone(), val);
        }

        // Execute body
        let result = match self.exec_block(&body)? {
            FlowControl::Return(v) => v.unwrap_or(Value::Null),
            FlowControl::Normal => Value::Null,
        };

        // Restore scope
        self.vars = old_vars;
        Ok(result)
    }

    fn try_builtin(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
        match name {
            "range" => {
                if args.len() != 1 {
                    return Err("range() expects 1 argument".into());
                }
                match &args[0] {
                    Value::Int(n) => {
                        let items: Vec<Value> = (0..*n).map(Value::Int).collect();
                        Ok(Some(Value::List(items)))
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
                if args.len() != 2 { return Err("max() expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(a), Value::Int(b)) => Ok(Some(Value::Int(*a.max(b)))),
                    _ => Err("max() expects integers".into()),
                }
            }
            "min" => {
                if args.len() != 2 { return Err("min() expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(a), Value::Int(b)) => Ok(Some(Value::Int(*a.min(b)))),
                    _ => Err("min() expects integers".into()),
                }
            }
            "abs" => {
                if args.len() != 1 { return Err("abs() expects 1 argument".into()); }
                match &args[0] {
                    Value::Int(n) => Ok(Some(Value::Int(n.abs()))),
                    _ => Err("abs() expects an integer".into()),
                }
            }
            "str" => {
                if args.len() != 1 { return Err("str() expects 1 argument".into()); }
                Ok(Some(Value::Str(format!("{}", args[0]))))
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
            "read_file" => {
                if args.len() != 1 { return Err("read_file() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(path) => {
                        match std::fs::read_to_string(path) {
                            Ok(content) => Ok(Some(Value::Str(content))),
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
                        match std::fs::write(path, content) {
                            Ok(_) => Ok(Some(Value::Bool(true))),
                            Err(e) => Err(format!("Cannot write file '{}': {}", path, e)),
                        }
                    }
                    _ => Err("write_file() expects (path, content) strings".into()),
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
                    Ok(_) => Ok(Some(Value::Str(line.trim_end().to_string()))),
                    Err(e) => Err(format!("Input error: {}", e)),
                }
            }
            "dict" => Ok(Some(Value::Dict(HashMap::new()))),
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
            _ => Ok(None),
        }
    }

    fn eval_method_call(&mut self, obj: Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match (&obj, method) {
            // String methods
            (Value::Str(s), "upper") => Ok(Value::Str(s.to_uppercase())),
            (Value::Str(s), "lower") => Ok(Value::Str(s.to_lowercase())),
            (Value::Str(s), "trim") => Ok(Value::Str(s.trim().to_string())),
            (Value::Str(s), "split") => {
                if args.len() != 1 { return Err("split() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(sep) => {
                        let parts: Vec<Value> = s.split(sep).map(|p| Value::Str(p.to_string())).collect();
                        Ok(Value::List(parts))
                    }
                    _ => Err("split() expects a string separator".into()),
                }
            }
            (Value::Str(s), "contains") => {
                if args.len() != 1 { return Err("contains() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(sub) => Ok(Value::Bool(s.contains(sub))),
                    _ => Err("contains() expects a string".into()),
                }
            }
            (Value::Str(s), "replace") => {
                if args.len() != 2 { return Err("replace() expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::Str(old), Value::Str(new)) => Ok(Value::Str(s.replace(old, new))),
                    _ => Err("replace() expects two strings".into()),
                }
            }
            (Value::Str(s), "chars") => {
                let chars: Vec<Value> = s.chars().map(|c| Value::Str(c.to_string())).collect();
                Ok(Value::List(chars))
            }
            // List methods
            (Value::List(items), "push") => {
                let mut new_items = items.clone();
                if args.len() != 1 { return Err("push() expects 1 argument".into()); }
                new_items.push(args[0].clone());
                Ok(Value::List(new_items))
            }
            (Value::List(items), "len") => Ok(Value::Int(items.len() as i64)),
            (Value::List(items), "contains") => {
                if args.len() != 1 { return Err("contains() expects 1 argument".into()); }
                Ok(Value::Bool(items.contains(&args[0])))
            }
            (Value::List(items), "reverse") => {
                let mut new_items = items.clone();
                new_items.reverse();
                Ok(Value::List(new_items))
            }
            (Value::List(items), "join") => {
                if args.len() != 1 { return Err("join() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(sep) => {
                        let parts: Vec<String> = items.iter().map(|v| match v {
                            Value::Str(s) => s.clone(),
                            Value::Int(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            _ => format!("{:?}", v),
                        }).collect();
                        Ok(Value::Str(parts.join(sep)))
                    }
                    _ => Err("join() expects a string separator".into()),
                }
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
                sorted.sort_by(|a, b| match (a, b) {
                    (Value::Int(a), Value::Int(b)) => a.cmp(b),
                    (Value::Str(a), Value::Str(b)) => a.cmp(b),
                    _ => std::cmp::Ordering::Equal,
                });
                Ok(Value::List(sorted))
            }
            (Value::List(items), "slice") => {
                if args.len() != 2 { return Err("slice() expects 2 arguments".into()); }
                match (&args[0], &args[1]) {
                    (Value::Int(start), Value::Int(end)) => {
                        let s = (*start as usize).min(items.len());
                        let e = (*end as usize).min(items.len());
                        if s <= e {
                            Ok(Value::List(items[s..e].to_vec()))
                        } else {
                            Ok(Value::List(vec![]))
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
                        new_entries.insert(key.clone(), args[1].clone());
                        Ok(Value::Dict(new_entries))
                    }
                    _ => Err("put() expects a string key".into()),
                }
            }
            (Value::Dict(entries), "get") => {
                if args.len() != 1 { return Err("get() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(key) => {
                        Ok(entries.get(key).cloned().unwrap_or(Value::Null))
                    }
                    _ => Err("get() expects a string key".into()),
                }
            }
            (Value::Dict(entries), "has") => {
                if args.len() != 1 { return Err("has() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(key) => Ok(Value::Bool(entries.contains_key(key))),
                    _ => Err("has() expects a string key".into()),
                }
            }
            (Value::Dict(entries), "keys") => {
                let keys: Vec<Value> = entries.keys().map(|k| Value::Str(k.clone())).collect();
                Ok(Value::List(keys))
            }
            (Value::Dict(entries), "len") => Ok(Value::Int(entries.len() as i64)),
            (Value::Dict(entries), "remove") => {
                if args.len() != 1 { return Err("remove() expects 1 argument".into()); }
                match &args[0] {
                    Value::Str(key) => {
                        let mut new_entries = entries.clone();
                        new_entries.remove(key);
                        Ok(Value::Dict(new_entries))
                    }
                    _ => Err("remove() expects a string key".into()),
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
        assert_eq!(interp.vars.get("x"), Some(&Value::Int(7)));
    }

    #[test]
    fn test_function_call() {
        let interp = run_src("fn add(a: int, b: int) { return a + b }\nlet x = add(3, 4)");
        assert_eq!(interp.vars.get("x"), Some(&Value::Int(7)));
    }

    #[test]
    fn test_if_else() {
        let interp = run_src("let x = 5\nif x > 3 { let y = 10 } else { let y = 0 }");
        // y is scoped inside if block, so it won't be in vars
    }

    #[test]
    fn test_while_loop() {
        let interp = run_src("let i = 0\nwhile i < 5 { i = i + 1 }");
        assert_eq!(interp.vars.get("i"), Some(&Value::Int(5)));
    }

    #[test]
    fn test_for_loop() {
        let interp = run_src("let sum = 0\nfor i in range(5) { sum = sum + i }");
        assert_eq!(interp.vars.get("sum"), Some(&Value::Int(10)));
    }

    #[test]
    fn test_string_concat() {
        let interp = run_src("let x = \"hello\" + \" \" + \"world\"");
        assert_eq!(interp.vars.get("x"), Some(&Value::Str("hello world".into())));
    }

    #[test]
    fn test_struct() {
        let interp = run_src("struct Point { x: int, y: int }\nlet p = Point { x: 3, y: 4 }");
        match interp.vars.get("p") {
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
        assert_eq!(interp.vars.get("z"), Some(&Value::Int(3)));
    }

    #[test]
    fn test_method_call() {
        let interp = run_src("let s = \"hello\"\nlet u = s.upper()");
        assert_eq!(interp.vars.get("u"), Some(&Value::Str("HELLO".into())));
    }

    #[test]
    fn test_list_methods() {
        let interp = run_src("let l = [1, 2, 3]\nlet l2 = l.push(4)");
        match interp.vars.get("l2") {
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
        assert_eq!(interp.vars.get("x"), Some(&Value::Bool(false)));
        assert_eq!(interp.vars.get("y"), Some(&Value::Bool(true)));
        assert_eq!(interp.vars.get("z"), Some(&Value::Bool(true)));
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
