//! SMT-backed equivalence checking for variant promotion.
//! Phase 12: bounded-verify (interp sweep) is honest but incomplete —
//! a variant that agrees on [-10..10] can still diverge at 1000000.
//! z3 translates each variant body to a formula over symbolic ints and
//! asks: does ANY input make candidate != fittest? Unsat => provably
//! equivalent; Sat => the model IS a real counterexample.
//!
//! Scope (v1): single-dispatch, int/bool, straight-line + if/call.
//! Loops, match, strings, floats, structured values -> bail to bounded.

use crate::ast::{BinOp, Expr, Stmt, UnaryOp};
use crate::veldt;
use z3::ast::{Bool, Dynamic, Int};
use z3::{Config, SatResult, Solver};
use std::collections::HashMap;

/// One function body -> z3 Dynamic, or None if it uses something we
/// don't encode. `fuel` bounds call-expansion depth.
fn body_to_smt(
    stmts: &[Stmt],
    env: &mut HashMap<String, Dynamic>,
    veldt: &veldt::Veldt,
    fuel: u32,
) -> Option<Dynamic> {
    let mut last = None;
    for s in stmts {
        match s {
            Stmt::Let(name, e) => {
                let v = expr_to_smt(e, env, veldt, fuel)?;
                env.insert(name.clone(), v);
            }
            Stmt::Return(Some(e)) => {
                return expr_to_smt(e, env, veldt, fuel);
            }
            Stmt::Return(None) => return None,
            Stmt::ExprStmt(e) => {
                last = expr_to_smt(e, env, veldt, fuel);
            }
            // if cond { A } else { B } — each arm's value is its last expr
            Stmt::If(cond, then_stmts, else_stmts) => {
                let c = expr_to_smt(cond, env, veldt, fuel)?;
                let cb = c.as_bool()?;
                let mut then_env = env.clone();
                let mut else_env = env.clone();
                let tv = body_to_smt(then_stmts, &mut then_env, veldt, fuel)?;
                let ev = match else_stmts {
                    Some(eb) => body_to_smt(eb, &mut else_env, veldt, fuel)?,
                    None => Int::from_i64(0).into(), // if w/o else => 0
                };
                let v = cb.ite(&tv, &ev);
                last = Some(v);
            }
            // loops/match/etc — bail to bounded
            _ => return None,
        }
    }
    last
}

fn expr_to_smt(
    e: &Expr,
    env: &mut HashMap<String, Dynamic>,
    veldt: &veldt::Veldt,
    fuel: u32,
) -> Option<Dynamic> {
    match e {
        Expr::Int(n) => Some(Int::from_i64(*n).into()),
        Expr::Bool(b) => Some(Bool::from_bool(*b).into()),
        Expr::Var(name) => env.get(name).cloned(),
        Expr::BinOp(l, op, r) => {
            let a = expr_to_smt(l, env, veldt, fuel)?;
            let b = expr_to_smt(r, env, veldt, fuel)?;
            use BinOp::*;
            match op {
                Add => Some((a.as_int()? + b.as_int()?).into()),
                Sub => Some((a.as_int()? - b.as_int()?).into()),
                Mul => Some((a.as_int()? * b.as_int()?).into()),
                Div => Some(a.as_int()?.div(&b.as_int()?).into()),
                Mod => Some(a.as_int()?.modulo(&b.as_int()?).into()),
                Lt => Some(a.as_int()?.lt(&b.as_int()?).into()),
                Le => Some(a.as_int()?.le(&b.as_int()?).into()),
                Gt => Some(a.as_int()?.gt(&b.as_int()?).into()),
                Ge => Some(a.as_int()?.ge(&b.as_int()?).into()),
                Eq => Some(a.as_int()?._eq(&b.as_int()?).into()),
                Neq => Some(a.as_int()?._eq(&b.as_int()?).not().into()),
                And => Some((a.as_bool()? & b.as_bool()?).into()),
                Or => Some((a.as_bool()? | b.as_bool()?).into()),
                _ => None, // In / NullCoalesce / etc — bail
            }
        }
        Expr::UnaryOp(op, e) => {
            let a = expr_to_smt(e, env, veldt, fuel)?;
            use UnaryOp::*;
            match op {
                Neg => Some(a.as_int()?.unary_minus().into()),
                Not => Some(a.as_bool()?.not().into()),
            }
        }
        // call to a non-recursive int fn — inline its body with args
        // substituted (fuel-bounded so recursion bottoms out to bail)
        Expr::Call(name, vid, args) => {
            if fuel == 0 { return None; }
            let entry = veldt.functions.get(name)?.iter()
                .find(|v| Some(v.id) == *vid)
                .or_else(|| veldt.get_fittest_variant(name))?;
            if entry.params.len() != args.len() { return None; }
            let mut callee_env = HashMap::new();
            for (i, p) in entry.params.iter().enumerate() {
                let a = expr_to_smt(&args[i], env, veldt, fuel - 1)?;
                callee_env.insert(p.name.clone(), a);
            }
            // recursion guard: don't inline a fn that calls `name`
            if calls_itself(&entry.body, name) { return None; }
            body_to_smt(&entry.body, &mut callee_env, veldt, fuel - 1)
        }
        _ => None, // strings, floats, lists, methods, lambdas, ?., match
    }
}

fn calls_itself(stmts: &[Stmt], name: &str) -> bool {
    fn expr_calls(e: &Expr, name: &str) -> bool {
        match e {
            Expr::Call(n, _, _) => n == name,
            Expr::BinOp(l, _, r) => expr_calls(l, name) || expr_calls(r, name),
            Expr::UnaryOp(_, e) => expr_calls(e, name),
            _ => false,
        }
    }
    stmts.iter().any(|s| match s {
        Stmt::ExprStmt(e) | Stmt::Let(_, e) | Stmt::Return(Some(e)) =>
            expr_calls(e, name),
        Stmt::If(c, t, e) => expr_calls(c, name)
            || calls_itself(t, name)
            || e.as_ref().map(|x| calls_itself(x, name)).unwrap_or(false),
        _ => false,
    })
}

/// Exact-verify `name`'s variants: for each non-fittest, ask z3 whether
/// any input makes it diverge. Returns (divergent_id, counterexample_args).
/// `Ok(None)` = all-equivalent (promotion candidate).
/// `Err` = a variant used something we don't encode — bail to bounded.
pub fn verify_exact(name: &str)
    -> Result<Option<(u32, Vec<String>)>, String> {
    let mut veldt = veldt::Veldt::new();
    veldt.load().map_err(|e| e.to_string())?;
    let variants = veldt.functions.get(name)
        .ok_or_else(|| format!("no family '{}'", name))?;
    if variants.len() < 2 { return Ok(None); }
    let fittest = veldt.get_fittest_variant(name)
        .ok_or_else(|| format!("no fittest for '{}'", name))?;
    let params = fittest.params.clone();
    if params.iter().any(|p| !matches!(p.typ, crate::ast::Type::Int)) {
        return Err("non-int params — bail to bounded".into());
    }
    let v_cloned = variants.clone();
    let f_body = fittest.body.clone();
    let f_id = fittest.id;

    let cfg = Config::new();
    z3::with_z3_config(&cfg, move || -> Result<Option<(u32, Vec<String>)>, String> {
        let solver = Solver::new();
        // symbolic param -> Int
        let syms: Vec<Int> = params.iter()
            .map(|p| Int::new_const(p.name.clone()))
            .collect();

        // fittest body -> SMT once
        let mut env: HashMap<String, Dynamic> = HashMap::new();
        for (i, p) in params.iter().enumerate() {
            env.insert(p.name.clone(), syms[i].clone().into());
        }
        let f_smt = body_to_smt(&f_body, &mut env, &veldt, 8)
            .ok_or_else(|| "fittest uses non-SMT ops".to_string())?;

        for v in &v_cloned {
            if v.id == f_id { continue; }
            let mut env2: HashMap<String, Dynamic> = HashMap::new();
            for (i, p) in params.iter().enumerate() {
                env2.insert(p.name.clone(), syms[i].clone().into());
            }
            let v_smt = body_to_smt(&v.body, &mut env2, &veldt, 8)
                .ok_or_else(|| format!("{}#{} uses non-SMT ops", name, v.id))?;
            // assert f(x) != v(x) — Sat means a real divergence
            let neq = f_smt._eq(&v_smt).not();
            solver.assert(&neq);
            match solver.check() {
                SatResult::Unsat => { /* equivalent */ }
                SatResult::Sat => {
                    let Some(model) = solver.get_model() else {
                        return Err("sat but no model".into());
                    };
                    let cex: Vec<String> = syms.iter()
                        .map(|s| model.eval(s, true)
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| "?".into()))
                        .collect();
                    return Ok(Some((v.id, cex)));
                }
                SatResult::Unknown => {
                    return Err(format!("{}#{} — z3 undecided", name, v.id));
                }
            }
        }
        Ok(None)
    })
}
