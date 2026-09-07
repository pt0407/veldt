// Interpreter for the Veldt language — tree-walking

use crate::ast::*;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
    Struct(String, HashMap<String, Value>),
    Null,
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Null => write!(f, "null"),
            Value::List(items) => {
                let strs: Vec<String> = items.iter().map(|v| format!("{}", v)).collect();
                write!(f, "[{}]", strs.join(", "))
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
        }
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
                println!("{}", val);
                Ok(FlowControl::Normal)
            }
            Stmt::ExprStmt(expr) => {
                self.eval_expr(expr)?;
                Ok(FlowControl::Normal)
            }
            Stmt::Grow(_) => {
                // Grow statements are collected by the caller, not executed here
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
            Value::Str(s) => !s.is_empty(),
            Value::Null => false,
            Value::List(items) => !items.is_empty(),
            Value::Struct(_, fields) => !fields.is_empty(),
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Int(n) => Ok(Value::Int(*n)),
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
        }
    }

    fn eval_binop(&self, l: &Value, op: &BinOp, r: &Value) -> Result<Value, String> {
        match op {
            BinOp::Add => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{}{}", a, b))),
                _ => Err("Cannot add these types".into()),
            },
            BinOp::Sub => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                _ => Err("Cannot subtract non-integers".into()),
            },
            BinOp::Mul => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                _ => Err("Cannot multiply non-integers".into()),
            },
            BinOp::Div => match (l, r) {
                (Value::Int(_), Value::Int(0)) => Err("Division by zero".into()),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
                _ => Err("Cannot divide non-integers".into()),
            },
            BinOp::Mod => match (l, r) {
                (Value::Int(_), Value::Int(0)) => Err("Modulo by zero".into()),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                _ => Err("Cannot modulo non-integers".into()),
            },
            BinOp::Eq => Ok(Value::Bool(self.values_eq(l, r))),
            BinOp::Neq => Ok(Value::Bool(!self.values_eq(l, r))),
            BinOp::Lt => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                _ => Err("Cannot compare non-integers with <".into()),
            },
            BinOp::Gt => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                _ => Err("Cannot compare non-integers with >".into()),
            },
            BinOp::Le => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
                _ => Err("Cannot compare non-integers with <=".into()),
            },
            BinOp::Ge => match (l, r) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
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
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.values_eq(x, y))
            }
            (Value::Struct(n1, f1), Value::Struct(n2, f2)) => {
                n1 == n2 && f1 == f2
            }
            _ => false,
        }
    }

    fn call_function(&mut self, name: &str, variant: Option<u32>, args: Vec<Value>) -> Result<Value, String> {
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

    fn try_builtin(&self, name: &str, args: &[Value]) -> Result<Option<Value>, String> {
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
            _ => Ok(None),
        }
    }

    fn eval_method_call(&mut self, obj: Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match (&obj, method) {
            (Value::Str(s), "upper") => Ok(Value::Str(s.to_uppercase())),
            (Value::Str(s), "lower") => Ok(Value::Str(s.to_lowercase())),
            (Value::List(items), "push") => {
                let mut new_items = items.clone();
                if args.len() != 1 {
                    return Err("push() expects 1 argument".into());
                }
                new_items.push(args[0].clone());
                Ok(Value::List(new_items))
            }
            _ => Err(format!("No method '{}' on this value", method)),
        }
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
