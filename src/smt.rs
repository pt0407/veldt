//! SMT-backed equivalence checking for variant promotion.
//! Phase 12: bounded-verify (interp sweep) is honest but incomplete —
//! a variant that agrees on [-10..10] can still diverge at 1000000.
//! z3 translates each variant body to a formula over symbolic ints/reals/bools
//! and symbolic fixed-length lists, then asks: does ANY input make
//! candidate != fittest? Unsat => provably equivalent; Sat => the model IS a
//! real counterexample.
//!
//! Scope: single-dispatch, int/float/bool/list, straight-line + Let/Assign +
//! If (both early-return and stateful env merge) + Match + Try/Catch
//! (symbolic division-by-zero error guard) + For (over symbolic lists) +
//! bounded While unrolling (with exact loop-exit guards) + List methods
//! (len, is_empty, reverse, symbolic sorting-network sort) + math builtins
//! (abs, min, max, uninterpreted congruent sqrt).

use crate::ast::{BinOp, Expr, Stmt, Type, UnaryOp};
use crate::veldt;
use std::collections::HashMap;
use z3::ast::{Ast, Bool, Dynamic, Int, Real};
use z3::{Config, FuncDecl, Params, SatResult, Solver, Sort};

const UNROLL_BOUND: usize = 6;
const SYM_LIST_LEN: usize = 4;
const MAX_SYM_STEPS: u32 = 64;

#[derive(Clone)]
enum SymVal {
    Scalar(Dynamic),
    List(Vec<SymVal>),
}

impl SymVal {
    fn as_scalar(&self) -> Option<&Dynamic> {
        match self {
            SymVal::Scalar(d) => Some(d),
            _ => None,
        }
    }

    fn into_scalar(self) -> Option<Dynamic> {
        match self {
            SymVal::Scalar(d) => Some(d),
            _ => None,
        }
    }

    fn eq_val(&self, other: &SymVal) -> Option<Bool> {
        match (self, other) {
            (SymVal::Scalar(a), SymVal::Scalar(b)) => {
                if a.get_sort() != b.get_sort() {
                    return None;
                }
                Some(a.eq(b))
            }
            (SymVal::List(xs), SymVal::List(ys)) => {
                if xs.len() != ys.len() {
                    return Some(Bool::from_bool(false));
                }
                let mut acc = Bool::from_bool(true);
                for (x, y) in xs.iter().zip(ys.iter()) {
                    acc = acc & x.eq_val(y)?;
                }
                Some(acc)
            }
            _ => None,
        }
    }
}

fn merge_dynamic(cond: &Bool, tv: &Dynamic, fv: &Dynamic) -> Option<Dynamic> {
    if tv.get_sort() != fv.get_sort() {
        return None;
    }
    match cond.simplify().as_bool() {
        Some(true) => Some(tv.clone()),
        Some(false) => Some(fv.clone()),
        None => Some(cond.ite(tv, fv).simplify()),
    }
}

fn merge_symval(cond: &Bool, tv: &SymVal, fv: &SymVal) -> Option<SymVal> {
    match (tv, fv) {
        (SymVal::Scalar(a), SymVal::Scalar(b)) => Some(SymVal::Scalar(merge_dynamic(cond, a, b)?)),
        (SymVal::List(xs), SymVal::List(ys)) => {
            if xs.len() != ys.len() {
                return None;
            }
            let mut out = Vec::with_capacity(xs.len());
            for (x, y) in xs.iter().zip(ys.iter()) {
                out.push(merge_symval(cond, x, y)?);
            }
            Some(SymVal::List(out))
        }
        _ => None,
    }
}

#[derive(Clone)]
struct SymState {
    env: HashMap<String, SymVal>,
    /// `Some((cond, val))` when an early `return` may have occurred.
    ret: Option<(Bool, SymVal)>,
    /// Symbolic error condition raised inside a `try` block.
    err: Option<Bool>,
    in_try: u32,
    steps: u32,
    /// Expression value from a trailing `ExprStmt` / `If` / `Match`.
    last: Option<SymVal>,
    /// Path assumptions (e.g. loop exited within UNROLL_BOUND, divisor != 0).
    assumptions: Vec<Bool>,
}

impl SymState {
    fn new(env: HashMap<String, SymVal>) -> Self {
        Self {
            env,
            ret: None,
            err: None,
            in_try: 0,
            steps: 0,
            last: None,
            assumptions: Vec::new(),
        }
    }

    /// Guard that is true iff execution has NOT yet returned or errored.
    fn active(&self) -> Bool {
        let not_ret = match &self.ret {
            Some((rc, _)) => rc.not(),
            None => Bool::from_bool(true),
        };
        match &self.err {
            Some(ec) => not_ret & ec.not(),
            None => not_ret,
        }
    }

    fn record_div_zero(&mut self, is_zero: Bool) {
        let act = self.active();
        if self.in_try > 0 {
            let raised = act & &is_zero;
            self.err = Some(match self.err.take() {
                Some(prev) => prev | raised,
                None => raised,
            });
        } else {
            self.assumptions.push(act.implies(&is_zero.not()));
        }
    }

    fn final_value(self) -> Option<(SymVal, Vec<Bool>)> {
        let val = match (self.ret, self.last) {
            (Some((_, rv)), None) => Some(rv),
            (None, Some(lv)) => Some(lv),
            (Some((rc, rv)), Some(lv)) => merge_symval(&rc, &rv, &lv).or(Some(rv)),
            (None, None) => None,
        }?;
        Some((val, self.assumptions))
    }
}

fn merge_opt_bool(cond: &Bool, a: Option<Bool>, b: Option<Bool>) -> Option<Bool> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(cond & &x),
        (None, Some(y)) => Some(cond.not() & &y),
        (Some(x), Some(y)) => Some(cond.ite(&x, &y)),
    }
}

fn merge_states(cond: &Bool, mut t: SymState, mut f: SymState) -> Option<SymState> {
    match cond.simplify().as_bool() {
        Some(true) => return Some(t),
        Some(false) => return Some(f),
        None => {}
    }
    let mut env = f.env.clone();
    for (k, tv) in t.env.drain() {
        if let Some(fv) = env.get(&k) {
            let mv = merge_symval(cond, &tv, fv)?;
            env.insert(k, mv);
        } else {
            env.insert(k, tv);
        }
    }
    let ret = match (t.ret, f.ret) {
        (None, None) => None,
        (Some((rc1, rv1)), None) => Some((cond & &rc1, rv1)),
        (None, Some((rc2, rv2))) => Some((cond.not() & &rc2, rv2)),
        (Some((rc1, rv1)), Some((rc2, rv2))) => {
            let rc = cond.ite(&rc1, &rc2);
            let rv = merge_symval(cond, &rv1, &rv2)?;
            Some((rc, rv))
        }
    };
    let err = merge_opt_bool(cond, t.err, f.err);
    let last = match (t.last, f.last) {
        (Some(l1), Some(l2)) => merge_symval(cond, &l1, &l2),
        (Some(l1), None) => Some(l1),
        (None, Some(l2)) => Some(l2),
        (None, None) => None,
    };
    t.assumptions.append(&mut f.assumptions);
    Some(SymState {
        env,
        ret,
        err,
        in_try: f.in_try,
        steps: t.steps.max(f.steps),
        last,
        assumptions: t.assumptions,
    })
}

fn body_to_smt(
    stmts: &[Stmt],
    env: HashMap<String, SymVal>,
    veldt: &veldt::Veldt,
    fuel: u32,
) -> Option<(SymVal, Vec<Bool>)> {
    let mut state = SymState::new(env);
    exec_stmts(stmts, &mut state, veldt, fuel)?;
    state.final_value()
}

fn exec_stmts(
    stmts: &[Stmt],
    state: &mut SymState,
    veldt: &veldt::Veldt,
    fuel: u32,
) -> Option<()> {
    for s in stmts {
        state.steps += 1;
        if state.steps > MAX_SYM_STEPS {
            return None;
        }
        match s {
            Stmt::Let(name, e) | Stmt::Assign(name, e) => {
                let v = expr_to_smt(e, state, veldt, fuel)?;
                let act = state.active();
                let upd = if let Some(old) = state.env.get(name) {
                    merge_symval(&act, &v, old)?
                } else {
                    v
                };
                state.env.insert(name.clone(), upd);
            }
            Stmt::Return(Some(e)) => {
                let v = expr_to_smt(e, state, veldt, fuel)?;
                let act = state.active();
                state.ret = Some(match state.ret.take() {
                    None => (act, v),
                    Some((prev_c, prev_v)) => {
                        let mv = merge_symval(&prev_c, &prev_v, &v)?;
                        (prev_c | &act, mv)
                    }
                });
                return Some(());
            }
            Stmt::Return(None) => return None,
            Stmt::ExprStmt(e) => {
                state.last = Some(expr_to_smt(e, state, veldt, fuel)?);
            }
            Stmt::If(cond, then_stmts, else_stmts) => {
                let c = expr_to_smt(cond, state, veldt, fuel)?
                    .into_scalar()?
                    .as_bool()?;
                let mut then_st = state.clone();
                let mut else_st = state.clone();
                exec_stmts(then_stmts, &mut then_st, veldt, fuel)?;
                if let Some(eb) = else_stmts {
                    exec_stmts(eb, &mut else_st, veldt, fuel)?;
                }
                *state = merge_states(&c, then_st, else_st)?;
            }
            Stmt::Match(scrutinee, arms, else_arm) => {
                let sv = expr_to_smt(scrutinee, state, veldt, fuel)?;
                let mut cur = state.clone();
                if let Some(eb) = else_arm {
                    exec_stmts(eb, &mut cur, veldt, fuel)?;
                }
                for (pat, body) in arms.iter().rev() {
                    let mut arm_st = state.clone();
                    let pv = expr_to_smt(pat, &mut arm_st, veldt, fuel)?;
                    exec_stmts(body, &mut arm_st, veldt, fuel)?;
                    let c = sv.eq_val(&pv)?;
                    cur = merge_states(&c, arm_st, cur)?;
                }
                *state = cur;
            }
            Stmt::Try(try_stmts, _err_var, catch_stmts) => {
                let mut try_st = state.clone();
                let outer_err = try_st.err.take();
                try_st.in_try += 1;
                exec_stmts(try_stmts, &mut try_st, veldt, fuel)?;
                try_st.in_try -= 1;
                if let Some(err_c) = try_st.err.take() {
                    try_st.err = outer_err;
                    let mut catch_st = state.clone();
                    exec_stmts(catch_stmts, &mut catch_st, veldt, fuel)?;
                    *state = merge_states(&err_c, catch_st, try_st)?;
                } else {
                    try_st.err = outer_err;
                    *state = try_st;
                }
            }
            Stmt::For(var, iter_expr, body) => {
                let SymVal::List(items) = expr_to_smt(iter_expr, state, veldt, fuel)? else {
                    return None;
                };
                for item in items {
                    let go = state.active();
                    let mut body_st = state.clone();
                    body_st.env.insert(var.clone(), item);
                    exec_stmts(body, &mut body_st, veldt, fuel)?;
                    *state = merge_states(&go, body_st, state.clone())?;
                }
            }
            Stmt::While(cond, body) => {
                for _ in 0..UNROLL_BOUND {
                    let c = expr_to_smt(cond, state, veldt, fuel)?
                        .into_scalar()?
                        .as_bool()?;
                    let go = (state.active() & &c).simplify();
                    if go.as_bool() == Some(false) {
                        break;
                    }
                    let mut body_st = state.clone();
                    exec_stmts(body, &mut body_st, veldt, fuel)?;
                    *state = merge_states(&go, body_st, state.clone())?;
                }
                let c_after = expr_to_smt(cond, state, veldt, fuel)?
                    .into_scalar()?
                    .as_bool()?;
                let exited = (state.active() & &c_after).not();
                state.assumptions.push(exited);
            }
            _ => return None,
        }
    }
    Some(())
}

fn float_to_real(f: f64) -> Option<Real> {
    if !f.is_finite() {
        return None;
    }
    let scale = 1_000_000i64;
    let num = (f * (scale as f64)).round() as i64;
    Some(Real::from_rational(num, scale))
}

fn as_real_coerced(d: &Dynamic) -> Option<Real> {
    if let Some(r) = d.as_real() {
        Some(r)
    } else if let Some(i) = d.as_int() {
        Some(Real::from_int(&i))
    } else {
        None
    }
}

fn sym_sort(mut xs: Vec<SymVal>) -> Option<Vec<SymVal>> {
    let n = xs.len();
    for i in 0..n {
        for j in 0..(n.saturating_sub(1 + i)) {
            let a = xs[j].as_scalar()?.as_int()?;
            let b = xs[j + 1].as_scalar()?.as_int()?;
            let cond = a.le(&b);
            let lo: Dynamic = cond.ite(&a, &b).into();
            let hi: Dynamic = cond.ite(&b, &a).into();
            xs[j] = SymVal::Scalar(lo);
            xs[j + 1] = SymVal::Scalar(hi);
        }
    }
    Some(xs)
}

fn expr_to_smt(
    e: &Expr,
    state: &mut SymState,
    veldt: &veldt::Veldt,
    fuel: u32,
) -> Option<SymVal> {
    match e {
        Expr::Int(n) => Some(SymVal::Scalar(Int::from_i64(*n).into())),
        Expr::Float(f) => Some(SymVal::Scalar(float_to_real(*f)?.into())),
        Expr::Bool(b) => Some(SymVal::Scalar(Bool::from_bool(*b).into())),
        Expr::Str(v) => Some(SymVal::Scalar(vstr_const(&format!("lit_{}", v)))),
        Expr::Var(name) => state.env.get(name).cloned(),
        Expr::Interp(parts) => {
            // "a{e}b" folds into vstr_cat(vstr_cat(lit_a, vstr_of(e)), lit_b)
            let mut acc: Option<Dynamic> = None;
            let cat = |a: Dynamic, b: Dynamic| -> Dynamic {
                let srt = a.get_sort();
                FuncDecl::new("vstr_cat", &[&srt, &b.get_sort()], &srt)
                    .apply(&[&a, &b])
            };
            for p in parts {
                let d = match p {
                    Expr::Str(v) => vstr_const(&format!("lit_{}", v)),
                    other => {
                        let sv = expr_to_smt(other, state, veldt, fuel)?.into_scalar()?;
                        if sv.get_sort() == vstr_sort() {
                            sv
                        } else {
                            // non-str value -> vstr_of_<sort>: uninterpreted to-string
                            let srt = sv.get_sort();
                            FuncDecl::new("vstr_of", &[&srt], &vstr_sort()).apply(&[&sv])
                        }
                    }
                };
                acc = Some(match acc { None => d, Some(a) => cat(a, d) });
            }
            Some(SymVal::Scalar(acc.unwrap_or_else(|| vstr_const("lit_"))))
        }
        Expr::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(expr_to_smt(item, state, veldt, fuel)?);
            }
            Some(SymVal::List(out))
        }
        Expr::Index(obj, idx) => {
            let SymVal::List(items) = expr_to_smt(obj, state, veldt, fuel)? else {
                return None;
            };
            if items.is_empty() {
                return None;
            }
            let idx_i = expr_to_smt(idx, state, veldt, fuel)?
                .into_scalar()?
                .as_int()?
                .simplify();
            if let Some(k) = idx_i.as_i64() {
                if k >= 0 && (k as usize) < items.len() {
                    return Some(items[k as usize].clone());
                }
                return None;
            }
            let n = items.len() as i64;
            let in_bounds = idx_i.ge(Int::from_i64(0)) & idx_i.lt(Int::from_i64(n));
            state.assumptions.push(state.active().implies(&in_bounds));
            let mut cur = items.last()?.clone();
            for (i, item) in items.iter().enumerate().rev().skip(1) {
                let c = idx_i.eq(Int::from_i64(i as i64));
                cur = merge_symval(&c, item, &cur)?;
            }
            Some(cur)
        }
        Expr::MethodCall(obj, method, args) => {
            let recv = expr_to_smt(obj, state, veldt, fuel)?;
            match (recv, method.as_str(), args.len()) {
                (SymVal::List(items), "len", 0) => {
                    Some(SymVal::Scalar(Int::from_i64(items.len() as i64).into()))
                }
                (SymVal::List(items), "is_empty", 0) => {
                    Some(SymVal::Scalar(Bool::from_bool(items.is_empty()).into()))
                }
                (SymVal::List(items), "reverse", 0) => {
                    Some(SymVal::List(items.into_iter().rev().collect()))
                }
                (SymVal::List(items), "sort", 0) => Some(SymVal::List(sym_sort(items)?)),
                (SymVal::List(items), "sum", 0) => {
                    // exact: x0 + x1 + ... + xN
                    let mut acc: Option<Dynamic> = None;
                    for it in items {
                        let d = it.into_scalar()?;
                        acc = Some(match acc {
                            None => d,
                            Some(a) if a.as_int().is_some() && d.as_int().is_some() =>
                                (a.as_int()? + d.as_int()?).into(),
                            Some(a) => {
                                let ar = as_real_coerced(&a)?;
                                let br = as_real_coerced(&d)?;
                                (ar + br).into()
                            }
                        });
                    }
                    Some(SymVal::Scalar(acc?))
                }
                (SymVal::List(items), "contains", 1) => {
                    let needle = expr_to_smt(&args[0], state, veldt, fuel)?.into_scalar()?;
                    let mut acc = Bool::from_bool(false);
                    for it in items {
                        let d = it.into_scalar()?;
                        acc = acc | d.eq(&needle);
                    }
                    Some(SymVal::Scalar(acc.into()))
                }
                (SymVal::List(items), "first", 0) =>
                    items.first().cloned(),
                (SymVal::List(items), "last", 0) =>
                    items.last().cloned(),
                (SymVal::List(mut items), "push", 1) => {
                    items.push(expr_to_smt(&args[0], state, veldt, fuel)?);
                    Some(SymVal::List(items))
                }
                (SymVal::List(items), "index_of", 1) => {
                    // ite chain: (x0==v ? 0 : (x1==v ? 1 : ... : -1))
                    let needle = expr_to_smt(&args[0], state, veldt, fuel)?.into_scalar()?;
                    let mut acc: Dynamic = Int::from_i64(-1).into();
                    for (i, it) in items.iter().enumerate().rev() {
                        let d = it.clone().into_scalar()?;
                        acc = d.eq(&needle)
                            .ite(&Int::from_i64(i as i64), &acc.as_int()?)
                            .into();
                    }
                    Some(SymVal::Scalar(acc))
                }
                (SymVal::Scalar(sv), m, nargs) => {
                    // String methods as uninterpreted functions over VStr.
                    // (return sort, expected arg count)
                    let ret = match m {
                        "len" | "index_of" | "count" => Sort::int(),
                        "contains" | "starts_with" | "ends_with" | "is_empty" => Sort::bool(),
                        "upper" | "lower" | "to_upper" | "to_lower" | "trim"
                        | "ltrim" | "rtrim" | "replace" | "repeat" | "pad"
                        | "substr" => vstr_sort(),
                        _ => return None,
                    };
                    let expected = match m {
                        "replace" | "substr" => 2,
                        "index_of" | "count" | "contains" | "starts_with"
                        | "ends_with" | "repeat" => 1,
                        _ => 0,
                    };
                    if nargs != expected { return None; }
                    let mut sorts: Vec<Sort> = vec![sv.get_sort()];
                    let mut vals: Vec<Dynamic> = vec![sv];
                    for a in args {
                        let av = expr_to_smt(a, state, veldt, fuel)?.into_scalar()?;
                        sorts.push(av.get_sort());
                        vals.push(av);
                    }
                    let decl = FuncDecl::new(
                        format!("vstr_{}", m),
                        &sorts.iter().collect::<Vec<_>>(),
                        &ret,
                    );
                    Some(SymVal::Scalar(decl.apply(
                        &vals.iter().map(|v| v as &dyn z3::ast::Ast).collect::<Vec<_>>())))
                }
                _ => None,
            }
        }
        Expr::BinOp(l, op, r) => {
            let a_sv = expr_to_smt(l, state, veldt, fuel)?;
            let b_sv = expr_to_smt(r, state, veldt, fuel)?;
            let (a, b) = (a_sv.into_scalar()?, b_sv.into_scalar()?);
            use BinOp::*;
            if let (Some(ai), Some(bi)) = (a.as_int(), b.as_int()) {
                let d: Dynamic = match op {
                    Add => (ai + bi).into(),
                    Sub => (ai - bi).into(),
                    Mul => (ai * bi).into(),
                    Div => {
                        state.record_div_zero(bi.eq(Int::from_i64(0)));
                        ai.div(&bi).into()
                    }
                    Mod => {
                        state.record_div_zero(bi.eq(Int::from_i64(0)));
                        ai.modulo(&bi).into()
                    }
                    Lt => ai.lt(&bi).into(),
                    Le => ai.le(&bi).into(),
                    Gt => ai.gt(&bi).into(),
                    Ge => ai.ge(&bi).into(),
                    Eq => ai.eq(&bi).into(),
                    Neq => ai.eq(&bi).not().into(),
                    _ => return None,
                };
                return Some(SymVal::Scalar(d));
            }
            if a.as_real().is_some() || b.as_real().is_some() {
                let ar = as_real_coerced(&a)?;
                let br = as_real_coerced(&b)?;
                let d: Dynamic = match op {
                    Add => (ar + br).into(),
                    Sub => (ar - br).into(),
                    Mul => (ar * br).into(),
                    Div => {
                        state.record_div_zero(br.eq(Real::from_rational(0, 1)));
                        ar.div(&br).into()
                    }
                    Lt => ar.lt(&br).into(),
                    Le => ar.le(&br).into(),
                    Gt => ar.gt(&br).into(),
                    Ge => ar.ge(&br).into(),
                    Eq => ar.eq(&br).into(),
                    Neq => ar.eq(&br).not().into(),
                    _ => return None,
                };
                return Some(SymVal::Scalar(d));
            }
            let d: Dynamic = match op {
                Eq => a.eq(&b).into(),
                Neq => a.eq(&b).not().into(),
                And => (a.as_bool()? & b.as_bool()?).into(),
                Or => (a.as_bool()? | b.as_bool()?).into(),
                // string + string: uninterpreted concat (same-sort VStr pair)
                Add if a.get_sort() == b.get_sort()
                    && a.as_int().is_none() && a.as_real().is_none()
                    && a.as_bool().is_none() =>
                {
                    let srt = a.get_sort();
                    FuncDecl::new("vstr_cat", &[&srt, &srt], &srt)
                        .apply(&[&a, &b])
                }
                _ => return None,
            };
            Some(SymVal::Scalar(d))
        }
        Expr::UnaryOp(op, e) => {
            let a = expr_to_smt(e, state, veldt, fuel)?.into_scalar()?;
            use UnaryOp::*;
            let d: Dynamic = match op {
                Neg => {
                    if let Some(ai) = a.as_int() {
                        ai.unary_minus().into()
                    } else {
                        a.as_real()?.unary_minus().into()
                    }
                }
                Not => a.as_bool()?.not().into(),
            };
            Some(SymVal::Scalar(d))
        }
        Expr::Call(name, vid, args) => {
            // Builtin math functions
            if vid.is_none() && !veldt.functions.contains_key(name) {
                match (name.as_str(), args.len()) {
                    ("range", 1) => {
                        // concrete trip count only — symbolic lengths bail
                        let n = expr_to_smt(&args[0], state, veldt, fuel)?
                            .into_scalar()?
                            .as_int()?
                            .as_i64()?;
                        if !(0..=64).contains(&n) { return None; }
                        let items: Vec<SymVal> = (0..n)
                            .map(|i| SymVal::Scalar(Int::from_i64(i).into()))
                            .collect();
                        return Some(SymVal::List(items));
                    }
                    ("abs", 1) => {
                        let x = expr_to_smt(&args[0], state, veldt, fuel)?.into_scalar()?;
                        if let Some(xi) = x.as_int() {
                            let neg = xi.unary_minus();
                            return Some(SymVal::Scalar(xi.lt(Int::from_i64(0)).ite(&neg, &xi).into()));
                        }
                        if let Some(xr) = x.as_real() {
                            let neg = xr.unary_minus();
                            return Some(SymVal::Scalar(
                                xr.lt(Real::from_rational(0, 1)).ite(&neg, &xr).into(),
                            ));
                        }
                        return None;
                    }
                    ("min", 2) | ("max", 2) => {
                        let a = expr_to_smt(&args[0], state, veldt, fuel)?.into_scalar()?;
                        let b = expr_to_smt(&args[1], state, veldt, fuel)?.into_scalar()?;
                        let is_min = name == "min";
                        if let (Some(ai), Some(bi)) = (a.as_int(), b.as_int()) {
                            let c = if is_min { ai.le(&bi) } else { ai.ge(&bi) };
                            return Some(SymVal::Scalar(c.ite(&ai, &bi).into()));
                        }
                        let ar = as_real_coerced(&a)?;
                        let br = as_real_coerced(&b)?;
                        let c = if is_min { ar.le(&br) } else { ar.ge(&br) };
                        return Some(SymVal::Scalar(c.ite(&ar, &br).into()));
                    }
                    ("sqrt", 1) => {
                        let x = expr_to_smt(&args[0], state, veldt, fuel)?.into_scalar()?;
                        let xr = as_real_coerced(&x)?;
                        let r_sort = Sort::real();
                        let decl = FuncDecl::new("sqrt", &[&r_sort], &r_sort);
                        return Some(SymVal::Scalar(decl.apply(&[&xr])));
                    }
                    _ => return None,
                }
            }
            if fuel == 0 {
                return None;
            }
            let entry = veldt
                .functions
                .get(name)?
                .iter()
                .find(|v| Some(v.id) == *vid)
                .or_else(|| veldt.get_fittest_variant(name))?;
            if entry.params.len() != args.len() {
                return None;
            }
            let mut callee_env = HashMap::new();
            for (i, p) in entry.params.iter().enumerate() {
                let a = expr_to_smt(&args[i], state, veldt, fuel - 1)?;
                callee_env.insert(p.name.clone(), a);
            }
            if calls_itself(&entry.body, name) {
                return None;
            }
            let mut sub_st = SymState::new(callee_env);
            sub_st.steps = state.steps;
            exec_stmts(&entry.body, &mut sub_st, veldt, fuel - 1)?;
            state.steps = sub_st.steps;
            let (val, mut a_sub) = sub_st.final_value()?;
            state.assumptions.append(&mut a_sub);
            Some(val)
        }
        _ => None,
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
        Stmt::ExprStmt(e) | Stmt::Let(_, e) | Stmt::Assign(_, e) | Stmt::Return(Some(e)) => {
            expr_calls(e, name)
        }
        Stmt::If(c, t, e) => {
            expr_calls(c, name)
                || calls_itself(t, name)
                || e.as_ref().map(|x| calls_itself(x, name)).unwrap_or(false)
        }
        Stmt::While(c, b) | Stmt::For(_, c, b) => expr_calls(c, name) || calls_itself(b, name),
        _ => false,
    })
}

fn vstr_sort() -> Sort {
    Sort::uninterpreted(z3::Symbol::String("VStr".into()))
}

/// A symbolic string constant: an element of the uninterpreted VStr
/// sort. Every string builtin/method is an uninterpreted function
/// over it — congruence is enough to prove identical computations
/// equal and extract divergence witnesses.
fn vstr_const(name: &str) -> Dynamic {
    FuncDecl::new(name, &[], &vstr_sort()).apply(&[])
}

fn param_supported(t: &Type) -> bool {
    match t {
        Type::Int | Type::Float | Type::Bool | Type::Str => true,
        Type::List(inner) => matches!(**inner, Type::Int | Type::Float | Type::Bool),
        _ => false,
    }
}

fn make_sym_param(name: &str, t: &Type, flat_syms: &mut Vec<Dynamic>) -> SymVal {
    match t {
        Type::Int => {
            let d: Dynamic = Int::new_const(name).into();
            flat_syms.push(d.clone());
            SymVal::Scalar(d)
        }
        Type::Float => {
            let d: Dynamic = Real::new_const(name).into();
            flat_syms.push(d.clone());
            SymVal::Scalar(d)
        }
        Type::Bool => {
            let d: Dynamic = Bool::new_const(name).into();
            flat_syms.push(d.clone());
            SymVal::Scalar(d)
        }
        Type::Str => {
            let d = vstr_const(name);
            flat_syms.push(d.clone());
            SymVal::Scalar(d)
        }
        Type::List(inner) => {
            let mut items = Vec::with_capacity(SYM_LIST_LEN);
            for idx in 0..SYM_LIST_LEN {
                let elem_name = format!("{}[{}]", name, idx);
                items.push(make_sym_param(&elem_name, inner, flat_syms));
            }
            SymVal::List(items)
        }
        _ => unreachable!(),
    }
}

/// Exact-verify `name`'s variants: for each non-fittest, ask z3 whether
/// any input makes it diverge. Returns (divergent_id, counterexample_args).
/// `Ok(None)` = all-equivalent (promotion candidate).
/// `Err` = a variant used something we don't encode — bail to bounded.
pub fn verify_exact(name: &str) -> Result<Option<(u32, Vec<String>)>, String> {
    let mut veldt = veldt::Veldt::new();
    veldt.load().map_err(|e| e.to_string())?;
    verify_exact_on_veldt(&veldt, name)
}

/// Census-friendly version — the caller loads the garden once.
pub fn verify_exact_on_veldt(
    veldt: &veldt::Veldt,
    name: &str,
) -> Result<Option<(u32, Vec<String>)>, String> {
    let variants = veldt
        .functions
        .get(name)
        .ok_or_else(|| format!("no family '{}'", name))?;
    if variants.len() < 2 {
        return Ok(None);
    }
    let fittest = veldt
        .get_fittest_variant(name)
        .ok_or_else(|| format!("no fittest for '{}'", name))?;
    let params = fittest.params.clone();
    if params.iter().any(|p| !param_supported(&p.typ)) {
        return Err("unsupported param type — bail to bounded".into());
    }
    let v_cloned = variants.clone();
    let f_body = fittest.body.clone();
    let f_id = fittest.id;

    let mut cfg = Config::new();
    cfg.set_param_value("timeout", "250");
    z3::with_z3_config(&cfg, move || -> Result<Option<(u32, Vec<String>)>, String> {
        let mut flat_syms: Vec<Dynamic> = Vec::new();
        let mut base_env: HashMap<String, SymVal> = HashMap::new();
        for p in &params {
            let sv = make_sym_param(&p.name, &p.typ, &mut flat_syms);
            base_env.insert(p.name.clone(), sv);
        }

        let (f_smt, f_assumptions) = body_to_smt(&f_body, base_env.clone(), veldt, 8)
            .ok_or_else(|| "fittest uses non-SMT ops".to_string())?;

        for v in &v_cloned {
            if v.id == f_id || !matches!(v.health, veldt::Health::Healthy) {
                continue;
            }
            let (v_smt, v_assumptions) = body_to_smt(&v.body, base_env.clone(), veldt, 8)
                .ok_or_else(|| format!("{}#{} uses non-SMT ops", name, v.id))?;
            let eq = f_smt
                .eq_val(&v_smt)
                .ok_or_else(|| format!("{}#{} return sort mismatch", name, v.id))?;
            let solver = Solver::new();
            let mut sp = Params::new();
            sp.set_u32("timeout", 200);
            solver.set_params(&sp);
            for a in &f_assumptions {
                solver.assert(a);
            }
            for a in &v_assumptions {
                solver.assert(a);
            }
            solver.assert(&eq.not());
            match solver.check() {
                SatResult::Unsat => { /* equivalent */ }
                SatResult::Sat => {
                    let Some(model) = solver.get_model() else {
                        return Err("sat but no model".into());
                    };
                    let cex: Vec<String> = flat_syms
                        .iter()
                        .map(|s| {
                            model
                                .eval(s, true)
                                .map(|x| x.to_string())
                                .unwrap_or_else(|| "?".into())
                        })
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


/// Exact-verify a single target variant `vid` against the family's fittest.
/// `Ok(None)` = provably equivalent; `Ok(Some(cex))` = counterexample found;
/// `Err(why)` = uses ops outside the SMT fragment.
pub fn verify_variant_exact(
    veldt: &veldt::Veldt,
    name: &str,
    vid: u32,
) -> Result<Option<Vec<String>>, String> {
    let variants = veldt
        .functions
        .get(name)
        .ok_or_else(|| format!("no family '{}'", name))?;
    let target = variants
        .iter()
        .find(|v| v.id == vid)
        .ok_or_else(|| format!("no variant {}#{}", name, vid))?
        .clone();
    let fittest = veldt
        .get_fittest_variant(name)
        .ok_or_else(|| format!("no fittest for '{}'", name))?;
    if fittest.id == vid {
        return Ok(None);
    }
    let params = fittest.params.clone();
    if params.iter().any(|p| !param_supported(&p.typ)) {
        return Err("unsupported param type — bail to bounded".into());
    }
    let f_body = fittest.body.clone();

    let mut cfg = Config::new();
    cfg.set_param_value("timeout", "250");
    z3::with_z3_config(&cfg, move || -> Result<Option<Vec<String>>, String> {
        let mut flat_syms: Vec<Dynamic> = Vec::new();
        let mut base_env: HashMap<String, SymVal> = HashMap::new();
        for p in &params {
            let sv = make_sym_param(&p.name, &p.typ, &mut flat_syms);
            base_env.insert(p.name.clone(), sv);
        }
        let (f_smt, f_assumptions) = body_to_smt(&f_body, base_env.clone(), veldt, 8)
            .ok_or_else(|| "fittest uses non-SMT ops".to_string())?;
        let (v_smt, v_assumptions) = body_to_smt(&target.body, base_env, veldt, 8)
            .ok_or_else(|| format!("{}#{} uses non-SMT ops", name, vid))?;
        let eq = f_smt
            .eq_val(&v_smt)
            .ok_or_else(|| format!("{}#{} return sort mismatch", name, vid))?;
        let solver = Solver::new();
        let mut sp = Params::new();
        sp.set_u32("timeout", 200);
        solver.set_params(&sp);
        for a in &f_assumptions {
            solver.assert(a);
        }
        for a in &v_assumptions {
            solver.assert(a);
        }
        solver.assert(&eq.not());
        match solver.check() {
            SatResult::Unsat => Ok(None),
            SatResult::Sat => {
                let Some(model) = solver.get_model() else {
                    return Err("sat but no model".into());
                };
                let cex: Vec<String> = flat_syms
                    .iter()
                    .map(|s| {
                        model
                            .eval(s, true)
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| "?".into())
                    })
                    .collect();
                Ok(Some(cex))
            }
            SatResult::Unknown => Err(format!("{}#{} — z3 undecided", name, vid)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn plant(veldt: &mut veldt::Veldt, src: &str) {
        let tokens = Lexer::new(src).tokenize().unwrap();
        let stmts = Parser::new(tokens).parse_program().unwrap();
        for s in stmts {
            if let Stmt::Grow( crate::ast::GrowItem::Fn(name, params, body, tests)) = s {
                veldt.grow_function(&name, params, body, tests, src);
            }
        }
    }

    #[test]
    fn smt_proves_linear_scan_equals_sort_reverse() {
        let mut v = veldt::Veldt::new();
        plant(&mut v, "grow fn max_val(xs: list<int>) { let m = xs[0]
 let i = 1
 while i < xs.len() { if xs[i] > m { m = xs[i] }
 i = i + 1 }
 return m }");
        plant(&mut v, "grow fn max_val(xs: list<int>) { let s = xs.sort()
 let r = s.reverse()
 return r[0] }");
        let res = verify_exact_on_veldt(&v, "max_val").unwrap();
        assert!(res.is_none(), "expected O(n) scan and sort().reverse()[0] to be proven equivalent, got {:?}", res);
    }

    #[test]
    fn smt_finds_counterexample_on_divergent_variant() {
        let mut v = veldt::Veldt::new();
        plant(&mut v, "grow fn sq(x: int) { return x * x }");
        plant(&mut v, "grow fn sq(x: int) { return x * x + x }");
        let res = verify_exact_on_veldt(&v, "sq").unwrap();
        assert!(res.is_some(), "expected counterexample for x*x vs x*x+x");
    }

    #[test]
    fn smt_proves_try_catch_safe_divide() {
        let mut v = veldt::Veldt::new();
        plant(&mut v, "grow fn safe_div(a: int, b: int) { try { return a / b } catch e { return 0 } }");
        plant(&mut v, "grow fn safe_div(a: int, b: int) { if b == 0 { return 0 } else { return a / b } }");
        let res = verify_exact_on_veldt(&v, "safe_div").unwrap();
        assert!(res.is_none(), "expected try/catch and if b==0 guard to be proven equivalent, got {:?}", res);
    }

    #[test]
    fn smt_strings_uninterpreted_equivalence_and_divergence() {
        let mut v = veldt::Veldt::new();
        plant(&mut v, "grow fn bang(s: str) { return s + \"!\" }");
        plant(&mut v, "grow fn bang(s: str) { let e = \"!\" return s + e }");
        let res = verify_exact_on_veldt(&v, "bang").unwrap();
        assert!(res.is_none(),
            "expected identical string programs proven equivalent, got {:?}", res);
        // divergent: different literal suffix
        plant(&mut v, "grow fn bang(s: str) { return s + \"?\" }");
        let res = verify_exact_on_veldt(&v, "bang").unwrap();
        assert!(res.is_some(), "expected counterexample for different literal");
    }
}
