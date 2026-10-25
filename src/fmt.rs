// AST -> canonical Veldt source printer.
// Round-trips: parse(format(parse(src))) == parse(src) modulo formatting.

use crate::ast::*;

const INDENT: &str = "    ";

pub fn format_program(stmts: &[Stmt]) -> String {
    let mut out = String::new();
    for (i, s) in stmts.iter().enumerate() {
        format_stmt(s, 0, &mut out);
        if i + 1 < stmts.len() {
            out.push('\n');
        }
    }
    out
}

fn format_stmt(s: &Stmt, depth: usize, out: &mut String) {
    let pad = INDENT.repeat(depth);
    match s {
        Stmt::Let(name, e) => {
            out.push_str(&format!("{}let {} = {}\n", pad, name, fmt_expr(e)));
        }
        Stmt::LetDestructure(names, e) => {
            out.push_str(&format!("{}let [{}] = {}\n", pad, names.join(", "), fmt_expr(e)));
        }
        Stmt::Assign(name, e) => {
            out.push_str(&format!("{}{} = {}\n", pad, name, fmt_expr(e)));
        }
        Stmt::FieldAssign(obj, f, e) => {
            out.push_str(&format!("{}{}.{} = {}\n", pad, fmt_postfix(obj), f, fmt_expr(e)));
        }
        Stmt::IndexAssign(obj, i, e) => {
            out.push_str(&format!("{}{}[{}] = {}\n", pad, fmt_postfix(obj), fmt_expr(i), fmt_expr(e)));
        }
        Stmt::FnDef(name, params, body) => {
            out.push_str(&format!("{}fn {}({}) {{\n", pad, name, fmt_params(params)));
            format_body(body, depth + 1, out);
            out.push_str(&format!("{}}}\n", pad));
        }
        Stmt::StructDef(name, fields) => {
            let fs: Vec<String> = fields.iter()
                .map(|(n, t)| format!("{}: {}", n, fmt_type(t))).collect();
            out.push_str(&format!("{}struct {} {{ {} }}\n", pad, name, fs.join(", ")));
        }
        Stmt::If(cond, then, els) => {
            out.push_str(&format!("{}if {} {{\n", pad, fmt_expr(cond)));
            format_body(then, depth + 1, out);
            match els {
                Some(eb) => {
                    out.push_str(&format!("{}}} else {{\n", pad));
                    format_body(eb, depth + 1, out);
                    out.push_str(&format!("{}}}\n", pad));
                }
                None => out.push_str(&format!("{}}}\n", pad)),
            }
        }
        Stmt::While(cond, body) => {
            out.push_str(&format!("{}while {} {{\n", pad, fmt_expr(cond)));
            format_body(body, depth + 1, out);
            out.push_str(&format!("{}}}\n", pad));
        }
        Stmt::For(var, iter, body) => {
            out.push_str(&format!("{}for {} in {} {{\n", pad, var, fmt_expr(iter)));
            format_body(body, depth + 1, out);
            out.push_str(&format!("{}}}\n", pad));
        }
        Stmt::Return(e) => match e {
            Some(e) => out.push_str(&format!("{}return {}\n", pad, fmt_expr(e))),
            None => out.push_str(&format!("{}return\n", pad)),
        },
        Stmt::Print(e) => {
            out.push_str(&format!("{}print({})\n", pad, fmt_expr(e)));
        }
        Stmt::ExprStmt(e) => {
            out.push_str(&format!("{}{}\n", pad, fmt_expr(e)));
        }
        Stmt::Grow(item) => match item {
            GrowItem::Fn(name, params, body, tests) => {
                let test_str = fmt_tests(tests);
                if test_str.is_empty() {
                    out.push_str(&format!("{}grow fn {}({}) {{\n", pad, name, fmt_params(params)));
                } else {
                    out.push_str(&format!("{}grow fn {}({}) test {} {{\n",
                        pad, name, fmt_params(params), test_str));
                }
                format_body(body, depth + 1, out);
                out.push_str(&format!("{}}}\n", pad));
            }
            GrowItem::Let(name, e) => {
                out.push_str(&format!("{}grow let {} = {}\n", pad, name, fmt_expr(e)));
            }
            GrowItem::Struct(name, fields) => {
                let fs: Vec<String> = fields.iter()
                    .map(|(n, t)| format!("{}: {}", n, fmt_type(t))).collect();
                out.push_str(&format!("{}grow struct {} {{ {} }}\n", pad, name, fs.join(", ")));
            }
        },
        Stmt::Try(body, err_var, handler) => {
            out.push_str(&format!("{}try {{\n", pad));
            format_body(body, depth + 1, out);
            out.push_str(&format!("{}}} catch {} {{\n", pad, err_var));
            format_body(handler, depth + 1, out);
            out.push_str(&format!("{}}}\n", pad));
        }
        Stmt::Import(path) => {
            out.push_str(&format!("{}import \"{}\"\n", pad, path));
        }
        Stmt::Match(scrutinee, arms, else_arm) => {
            out.push_str(&format!("{}match {} {{\n", pad, fmt_expr(scrutinee)));
            for (pat, body) in arms {
                fmt_match_arm(&fmt_expr(pat), body, depth + 1, out);
            }
            if let Some(eb) = else_arm {
                fmt_match_arm("else", eb, depth + 1, out);
            }
            out.push_str(&format!("{}}}\n", pad));
        }
    }
}

fn fmt_match_arm(pat: &str, body: &[Stmt], depth: usize, out: &mut String) {
    let pad = INDENT.repeat(depth);
    if body.len() == 1 {
        let mut single = String::new();
        format_stmt(&body[0], 0, &mut single);
        out.push_str(&format!("{}{} => {}\n", pad, pat, single.trim_end()));
    } else {
        out.push_str(&format!("{}{} => {{\n", pad, pat));
        format_body(body, depth + 1, out);
        out.push_str(&format!("{}}}\n", pad));
    }
}

fn format_body(body: &[Stmt], depth: usize, out: &mut String) {
    for s in body {
        format_stmt(s, depth, out);
    }
}

fn fmt_params(params: &[Param]) -> String {
    params.iter().map(|p| match p.typ {
        Type::Any => p.name.clone(),
        _ => format!("{}: {}", p.name, fmt_type(&p.typ)),
    }).collect::<Vec<_>>().join(", ")
}

fn fmt_tests(tests: &[TestAssert]) -> String {
    tests.iter().map(|t| match &t.expected {
        Some(e) => format!("{} == {}", fmt_expr(&t.call), fmt_expr(e)),
        None => fmt_expr(&t.call),
    }).collect::<Vec<_>>().join(", ")
}

fn fmt_type(t: &Type) -> String {
    match t {
        Type::Int => "int".into(),
        Type::Float => "float".into(),
        Type::Str => "str".into(),
        Type::Bool => "bool".into(),
        Type::List(inner) => format!("list<{}>", fmt_type(inner)),
        Type::Fn => "fn".into(),
        Type::Struct(n) => n.clone(),
        Type::Any => "any".into(),
    }
}

// Binary operator precedence — parenthesize children when needed.
fn prec(op: &BinOp) -> u8 {
    match op {
        BinOp::NullCoal => 1,
        BinOp::Or => 2,
        BinOp::And => 3,
        BinOp::Eq | BinOp::Neq => 4,
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => 5,
        BinOp::Add | BinOp::Sub => 6,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 7,
    }
}

fn op_str(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*",
        BinOp::Div => "/", BinOp::Mod => "%",
        BinOp::Eq => "==", BinOp::Neq => "!=",
        BinOp::Lt => "<", BinOp::Gt => ">", BinOp::Le => "<=", BinOp::Ge => ">=",
        BinOp::And => "and", BinOp::Or => "or", BinOp::NullCoal => "??",
    }
}

// Object of a postfix access: parenthesize if it would parse differently.
fn fmt_postfix(e: &Expr) -> String {
    match e {
        Expr::BinOp(..) | Expr::UnaryOp(..) | Expr::Lambda(..) => format!("({})", fmt_expr(e)),
        _ => fmt_expr(e),
    }
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

pub fn fmt_expr(e: &Expr) -> String {
    match e {
        Expr::Int(n) => format!("{}", n),
        Expr::Float(f) => format!("{:?}", f),
        Expr::Str(s) => format!("\"{}\"", escape_str(s)),
        Expr::Bool(b) => format!("{}", b),
        Expr::Null => "null".into(),
        Expr::List(items) => format!("[{}]",
            items.iter().map(fmt_expr).collect::<Vec<_>>().join(", ")),
        Expr::Var(n) => n.clone(),
        Expr::Interp(parts) => {
            let mut s = String::from("\"");
            for p in parts {
                match p {
                    Expr::Str(lit) => s.push_str(&escape_str(lit)),
                    other => s.push_str(&format!("{{{}}}", fmt_expr(other))),
                }
            }
            s.push('"');
            s
        }
        Expr::BinOp(l, op, r) => {
            let p = prec(op);
            let ls = match l.as_ref() {
                Expr::BinOp(_, lop, _) if prec(lop) < p => format!("({})", fmt_expr(l)),
                _ => fmt_expr(l),
            };
            // right child at same precedence needs parens (non-commutative safety)
            let rs = match r.as_ref() {
                Expr::BinOp(_, rop, _) if prec(rop) <= p => format!("({})", fmt_expr(r)),
                _ => fmt_expr(r),
            };
            format!("{} {} {}", ls, op_str(op), rs)
        }
        Expr::UnaryOp(op, inner) => {
            let inner_s = match inner.as_ref() {
                Expr::BinOp(..) => format!("({})", fmt_expr(inner)),
                _ => fmt_expr(inner),
            };
            match op {
                UnaryOp::Neg => format!("-{}", inner_s),
                UnaryOp::Not => format!("not {}", inner_s),
            }
        }
        Expr::Call(name, variant, args) => {
            let v = variant.map(|n| format!("#{}", n)).unwrap_or_default();
            format!("{}{}({})", name, v,
                args.iter().map(fmt_expr).collect::<Vec<_>>().join(", "))
        }
        Expr::MethodCall(obj, m, args) => {
            format!("{}.{}({})", fmt_postfix(obj), m,
                args.iter().map(fmt_expr).collect::<Vec<_>>().join(", "))
        }
        Expr::FieldAccess(obj, f) => format!("{}.{}", fmt_postfix(obj), f),
        Expr::SafeGet(obj, name, args) => match args {
            Some(a) => format!("{}?.{}({})", fmt_postfix(obj), name,
                a.iter().map(fmt_expr).collect::<Vec<_>>().join(", ")),
            None => format!("{}?.{}", fmt_postfix(obj), name),
        },
        Expr::StructLit(name, fields) => {
            let fs: Vec<String> = fields.iter()
                .map(|(n, e)| format!("{}: {}", n, fmt_expr(e))).collect();
            format!("{} {{ {} }}", name, fs.join(", "))
        }
        Expr::Index(obj, i) => format!("{}[{}]", fmt_postfix(obj), fmt_expr(i)),
        Expr::Lambda(params, body) => {
            if body.len() == 1 {
                let mut single = String::new();
                format_stmt(&body[0], 0, &mut single);
                format!("fn({}) {{ {} }}", fmt_params(params), single.trim_end())
            } else {
                let mut inner = String::new();
                format_body(body, 1, &mut inner);
                format!("fn({}) {{\n{}}}", fmt_params(params), inner)
            }
        }
        Expr::ListComp(e, var, iter, cond) => match cond {
            Some(c) => format!("[{} for {} in {} if {}]", fmt_expr(e), var, fmt_expr(iter), fmt_expr(c)),
            None => format!("[{} for {} in {}]", fmt_expr(e), var, fmt_expr(iter)),
        },
    }
}
