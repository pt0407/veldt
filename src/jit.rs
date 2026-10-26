//! Cranelift tier-2 — hot CompiledFns compile to native code.
//! One shared JitEngine hosts a JITModule; every compiled fn gets a
//! declared FuncId upfront so calls (including recursion and
//! cross-function calls) resolve inside the module. The emitted code
//! targets the pure-int subset: params i64, int consts, slot vars,
//! +-*/ %, comparisons, jumps, calls, return. Anything outside falls
//! back to the bytecode VM.

use cranelift_codegen::ir::{types, AbiParam, Function, InstBuilder,
    Signature, UserFuncName, condcodes::IntCC};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_codegen::{settings, Context};
use cranelift_codegen::settings::Configurable;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_native::builder;

use crate::interpreter::Value;
use crate::vm::{Chunk, CompiledFn, Instr};
use std::collections::{HashMap, HashSet};

pub type NativeFn0 = unsafe extern "C" fn() -> i64;
pub type NativeFn1 = unsafe extern "C" fn(i64) -> i64;
pub type NativeFn2 = unsafe extern "C" fn(i64, i64) -> i64;
pub type NativeFn3 = unsafe extern "C" fn(i64, i64, i64) -> i64;
pub type NativeFn4 = unsafe extern "C" fn(i64, i64, i64, i64) -> i64;
pub type NativeF0 = unsafe extern "C" fn() -> f64;
pub type NativeF1 = unsafe extern "C" fn(f64) -> f64;
pub type NativeF2 = unsafe extern "C" fn(f64, f64) -> f64;
pub type NativeF3 = unsafe extern "C" fn(f64, f64, f64) -> f64;
pub type NativeF4 = unsafe extern "C" fn(f64, f64, f64, f64) -> f64;
pub type NativeF5 = unsafe extern "C" fn(f64, f64, f64, f64, f64) -> f64;
pub type NativeF6 = unsafe extern "C" fn(f64, f64, f64, f64, f64, f64) -> f64;
pub type NativeFn5 = unsafe extern "C" fn(i64, i64, i64, i64, i64) -> i64;
pub type NativeFn6 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64;

pub struct JitFn {
    pub code_ptr: *const u8,
    pub is_float: bool,   // signature is fn(f64...) -> f64
}

pub struct JitEngine {
    module: JITModule,
    fe_cfg: TargetFrontendConfig,
    ids: HashMap<String, FuncId>,
    ptrs: HashMap<String, (*const u8, bool)>,
    /// name#id -> bytecode pc that made the function unJITable
    pub fail_at: HashMap<String, usize>,
    compiling: HashSet<String>,
}

type Fns = HashMap<String, Vec<CompiledFn>>;

impl JitEngine {
    pub fn new() -> Option<JitEngine> {
        let mut flag_builder = settings::builder();
        flag_builder.set("opt_level", "speed").ok()?;
        let isa = builder().ok()?
            .finish(settings::Flags::new(flag_builder)).ok()?;
        let fe_cfg = isa.frontend_config();
        let module = JITModule::new(JITBuilder::with_isa(
            isa, cranelift_module::default_libcall_names()));
        Some(JitEngine {
            module, fe_cfg, ids: HashMap::new(), ptrs: HashMap::new(),
            compiling: HashSet::new(), fail_at: HashMap::new(),
        })
    }

    /// Declare (or fetch) the FuncId for `name` with `arity` i64 params.
    /// Declared before the body is built so recursive/forward calls
    /// resolve to a real reference.
    fn declare(&mut self, name: &str, arity: usize, float: bool)
        -> Option<FuncId> {
        if let Some(&f) = self.ids.get(name) { return Some(f); }
        let t = if float { types::F64 } else { types::I64 };
        let sig = Signature {
            params: vec![AbiParam::new(t); arity],
            returns: vec![AbiParam::new(t)],
            call_conv: CallConv::SystemV,
        };
        let f = self.module
            .declare_function(name, Linkage::Local, &sig).ok()?;
        self.ids.insert(name.to_string(), f);
        Some(f)
    }

    /// Compile `name` (fittest variant) and everything it calls into
    /// the engine. Keys ptrs by `name#id` — a newly-fittest variant
    /// after evolution compiles fresh instead of running stale code.
    pub fn compile_fn(&mut self, name: &str, fns: &Fns)
        -> Option<JitFn> {
        let f = fns.get(name)?;
        let chosen = f.iter()
            .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                .unwrap_or(std::cmp::Ordering::Equal))?;
        let key = format!("{}#{}", name, chosen.id);
        self.compile_variant(key, chosen, fns)
    }

    /// Compile a pinned `name#id` variant (explicit `name#N` calls).
    fn compile_pinned(&mut self, name: &str, id: u32, fns: &Fns)
        -> Option<JitFn> {
        let chosen = fns.get(name)?.iter().find(|v| v.id == id)?;
        self.compile_variant(format!("{}#{}", name, id), chosen, fns)
    }

    fn compile_variant(&mut self, key: String, chosen: &CompiledFn,
                       fns: &Fns) -> Option<JitFn> {
        if let Some(&(p, fl)) = self.ptrs.get(&key) {
            return Some(JitFn { code_ptr: p, is_float: fl });
        }
        if self.compiling.contains(&key) {
            return None;   // in-progress cycle on this variant
        }
        let arity = chosen.params.len();
        let fid = self.declare(&key, arity, false)?;
        self.compiling.insert(key.clone());
        let ok = self.define(fid, &key, &chosen.code, arity, fns, false);
        self.compiling.remove(&key);
        if ok {
            if let Err(e) = self.module.finalize_definitions() {
                eprintln!("jit: finalize {}: {:?}", key, e);
                return None;
            }
            let p = self.module.get_finalized_function(fid);
            self.ptrs.insert(key.clone(), (p, false));
            return Some(JitFn { code_ptr: p, is_float: false });
        }
        // int-subset failed — try the float signature
        let key = format!("{}$f", key);
        let fid = self.declare(&key, arity, true)?;
        self.compiling.insert(key.clone());
        let ok = self.define(fid, &key, &chosen.code, arity, fns, true);
        self.compiling.remove(&key);
        if !ok { return None; }
        if self.module.finalize_definitions().is_err() { return None; }
        let p = self.module.get_finalized_function(fid);
        self.ptrs.insert(key.clone(), (p, true));
        Some(JitFn { code_ptr: p, is_float: true })
    }

    /// Build the IR for one function body. Calls resolve eagerly —
    /// the callee is compiled first so its FuncId exists.
    fn define(&mut self, fid: FuncId, own_name: &str,
              chunk: &Chunk, arity: usize, fns: &Fns, float: bool)
        -> bool {
        // leaders: every jump/brif target + brif fallthrough
        let mut leaders: HashSet<usize> = HashSet::from([0]);
        for (i, instr) in chunk.code.iter().enumerate() {
            match *instr {
                Instr::Jump(t) | Instr::JumpIfFalse(t)
                | Instr::PeekJumpIfNull(t) => { leaders.insert(t as usize); }
                _ => {}
            }
            if matches!(instr, Instr::JumpIfFalse(_)) { leaders.insert(i + 1); }
        }
        // every called name must also be in the int subset — check arities
        for instr in &chunk.code {
            if let Instr::Call(ni, argc) = instr {
                let cn = &chunk.names[*ni as usize];
                // inline int builtins (abs/max/min) don't need fns —
                // they lower to native ops in the Call arm below
                let inlined = matches!(cn.as_str(),
                    "abs" | "min" | "max" | "clamp"
                    | "floor" | "ceil" | "round" | "sqrt");
                if cn != own_name && !inlined {
                    let vf = match fns.get(cn) {
                        Some(v) => v,
                        None => return false,   // builtin — not jittable
                    };
                    let cc = match vf.iter()
                        .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                            .unwrap_or(std::cmp::Ordering::Equal)) {
                        Some(c) => c,
                        None => return false,
                    };
                    if *argc as usize != cc.params.len() { return false; }
                }
            }
        }
        let t = if float { types::F64 } else { types::I64 };
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(
            UserFuncName::user(0, fid.as_u32()), self.fn_sig(arity, float));
        {
            let mut fb_ctx = FunctionBuilderContext::new();
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let nslots = chunk.slot_names.len().max(arity).max(1);
            let vars: Vec<Variable> = (0..nslots)
                .map(|_| b.declare_var(t)).collect();
            for i in 0..arity.min(nslots) {
                let v = b.block_params(entry)[i];
                b.def_var(vars[i], v);
            }
            let mut block_for: HashMap<usize, cranelift_codegen::ir::Block> =
                HashMap::new();
            // entry can't be a branch target — if pc 0 is a leader
            // (loop head), give it its own block and jump to it
            if leaders.contains(&0) && chunk.code.len() > 1 {
                let head = b.create_block();
                block_for.insert(0, head);
                b.ins().jump(head, &[]);
                // pc 0's code goes in head, not the now-filled entry
                b.switch_to_block(head);
            } else {
                block_for.insert(0, entry);
            }
            let mut get_block =
                |b: &mut FunctionBuilder,
                 fm: &mut HashMap<usize, cranelift_codegen::ir::Block>,
                 pc: usize| -> cranelift_codegen::ir::Block {
                    *fm.entry(pc).or_insert_with(|| b.create_block())
                };
            let mut stack: Vec<cranelift_codegen::ir::Value> = Vec::new();
            let mut pc = 0usize;
            let mut fail_pc = usize::MAX;
            let mut terminated = false;
            let mut ok = true;
            while pc < chunk.code.len() {
                if terminated || (pc > 0 && leaders.contains(&pc)) {
                    let bl = get_block(&mut b, &mut block_for, pc);
                    if !terminated { b.ins().jump(bl, &[]); }
                    b.switch_to_block(bl);
                    terminated = false;
                }
                match chunk.code[pc] {
                    Instr::Const(k) => match &chunk.consts[k as usize] {
                        Value::Int(n) => {
                            if float {
                                stack.push(b.ins().f64const(*n as f64));
                            } else {
                                stack.push(b.ins().iconst(types::I64, *n));
                            }
                        }
                        Value::Float(fv) if float => {
                            stack.push(b.ins().f64const(*fv));
                        }
                        _ => { ok = false; break; }
                    },
                    Instr::Load(sl) => {
                        stack.push(b.use_var(vars[sl as usize]));
                    }
                    Instr::Store(sl) => {
                        match stack.pop() {
                            Some(v) => b.def_var(vars[sl as usize], v),
                            None => { fail_pc = pc; ok = false; break; }
                        }
                    }
                    Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Mod => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop()) else {
                            fail_pc = pc; ok = false; break;
                        };
                        let v = match (chunk.code[pc], float) {
                            (Instr::Add, false) => b.ins().iadd(l, r),
                            (Instr::Sub, false) => b.ins().isub(l, r),
                            (Instr::Mul, false) => b.ins().imul(l, r),
                            (Instr::Div, false) => b.ins().sdiv(l, r),
                            (Instr::Mod, false) => b.ins().srem(l, r),
                            (Instr::Add, true) => b.ins().fadd(l, r),
                            (Instr::Sub, true) => b.ins().fsub(l, r),
                            (Instr::Mul, true) => b.ins().fmul(l, r),
                            (Instr::Div, true) => b.ins().fdiv(l, r),
                            // float mod: a - floor(a/b)*b — no fmod in
                            // cranelift; emit the expansion inline
                            (Instr::Mod, true) => {
                                let d = b.ins().fdiv(l, r);
                                let fl = b.ins().floor(d);
                                let m = b.ins().fmul(fl, r);
                                b.ins().fsub(l, m)
                            }
                            _ => { fail_pc = pc; ok = false; break; }
                        };
                        stack.push(v);
                    }
                    Instr::Eq | Instr::Neq | Instr::Lt | Instr::Gt | Instr::Le | Instr::Ge => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop()) else {
                            fail_pc = pc; ok = false; break;
                        };
                        if float {
                            use cranelift_codegen::ir::condcodes::FloatCC;
                            let cc = match chunk.code[pc] {
                                Instr::Eq => FloatCC::Equal,
                                Instr::Neq => FloatCC::NotEqual,
                                Instr::Lt => FloatCC::LessThan,
                                Instr::Gt => FloatCC::GreaterThan,
                                Instr::Le => FloatCC::LessThanOrEqual,
                                Instr::Ge => FloatCC::GreaterThanOrEqual,
                                _ => unreachable!(),
                            };
                            let c = b.ins().fcmp(cc, l, r);
                            let i = b.ins().uextend(types::I64, c);
                            stack.push(b.ins().fcvt_from_sint(types::F64, i));
                        } else {
                            let cc = match chunk.code[pc] {
                                Instr::Eq => IntCC::Equal,
                                Instr::Neq => IntCC::NotEqual,
                                Instr::Lt => IntCC::SignedLessThan,
                                Instr::Gt => IntCC::SignedGreaterThan,
                                Instr::Le => IntCC::SignedLessThanOrEqual,
                                Instr::Ge => IntCC::SignedGreaterThanOrEqual,
                                _ => unreachable!(),
                            };
                            let c = b.ins().icmp(cc, l, r);
                            stack.push(b.ins().uextend(types::I64, c));
                        }
                    }
                    Instr::Neg => {
                        let Some(v) = stack.pop() else { fail_pc = pc; ok = false; break; };
                        if float {
                            stack.push(b.ins().fneg(v));
                        } else {
                            let z = b.ins().iconst(types::I64, 0);
                            stack.push(b.ins().isub(z, v));
                        }
                    }
                    Instr::Call(ni, argc) => {
                        let cn = chunk.names[ni as usize].clone();
                        if float {
                            // float builtins — f64 single/dual arg ops
                            match (cn.as_str(), argc) {
                                ("floor", 1) | ("ceil", 1) | ("round", 1)
                                | ("sqrt", 1) | ("abs", 1) => {
                                    let Some(x) = stack.pop() else {
                                        fail_pc = pc; ok = false; break; };
                                    let v = match cn.as_str() {
                                        "floor" => b.ins().floor(x),
                                        "ceil" => b.ins().ceil(x),
                                        "round" => b.ins().nearest(x),
                                        "sqrt" => b.ins().sqrt(x),
                                        _ => b.ins().fabs(x),
                                    };
                                    stack.push(v); pc += 1; continue;
                                }
                                ("max", 2) | ("min", 2) => {
                                    let (Some(r), Some(l)) =
                                        (stack.pop(), stack.pop()) else {
                                            fail_pc = pc; ok = false; break; };
                                    let v = if cn == "max" {
                                        b.ins().fmax(l, r)
                                    } else {
                                        b.ins().fmin(l, r)
                                    };
                                    stack.push(v); pc += 1; continue;
                                }
                                _ => { fail_pc = pc; ok = false; break; }
                            }
                        }
                        // inline int builtins — abs/max/min don't need
                        // fn resolution, they lower to native ops
                        match (cn.as_str(), argc) {
                            ("abs", 1) => {
                                let Some(x) = stack.pop() else {
                                    fail_pc = pc; ok = false; break; };
                                let z = b.ins().iconst(types::I64, 0);
                                let nx = b.ins().isub(z, x);
                                let neg = b.ins().icmp(IntCC::SignedLessThan, x, z);
                                stack.push(b.ins().select(neg, nx, x));
                                pc += 1; continue;
                            }
                            ("clamp", 3) => {
                                // clamp(v, lo, hi) = v<lo?lo : v>hi?hi : v
                                let (Some(hi), Some(lo), Some(v)) =
                                    (stack.pop(), stack.pop(), stack.pop())
                                    else { fail_pc = pc; ok = false; break; };
                                let lt = b.ins().icmp(
                                    IntCC::SignedLessThan, v, lo);
                                let gt = b.ins().icmp(
                                    IntCC::SignedGreaterThan, v, hi);
                                let h = b.ins().select(gt, hi, v);
                                stack.push(b.ins().select(lt, lo, h));
                                pc += 1; continue;
                            }
                            ("max", 2) | ("min", 2) => {
                                let (Some(r), Some(l)) = (stack.pop(), stack.pop())
                                    else { fail_pc = pc; ok = false; break; };
                                // max/min = select(l>r / l<r, l, r)
                                let cc = if cn == "max" {
                                    IntCC::SignedGreaterThan
                                } else {
                                    IntCC::SignedLessThan
                                };
                                let cmp = b.ins().icmp(cc, l, r);
                                stack.push(b.ins().select(cmp, l, r));
                                pc += 1; continue;
                            }
                            _ => {}
                        }
                        // self-recursion: own_name is `name#id`
                        if cn != own_name.rsplit_once('#')
                            .map(|(n, _)| n).unwrap_or(own_name) {
                            match self.compile_fn(&cn, fns) {
                                Some(_) => {}
                                None => { fail_pc = pc; ok = false; break; }
                            }
                            // callee's key is cn#its-chosen-id
                            let ckey = fns.get(&cn).and_then(|vf|
                                vf.iter().max_by(|a, b|
                                    a.fitness.partial_cmp(&b.fitness)
                                        .unwrap_or(std::cmp::Ordering::Equal))
                                .map(|c| format!("{}#{}", cn, c.id)));
                            let Some(ckey) = ckey else { ok = false; break; };
                            let cfid = match self.ids.get(&ckey) {
                                Some(&f) => f,
                                None => { fail_pc = pc; ok = false; break; }
                            };
                            let fref = self.module.declare_func_in_func(
                                cfid, b.func);
                            let mut cargs = Vec::new();
                            for _ in 0..argc {
                                match stack.pop() {
                                    Some(v) => cargs.push(v),
                                    None => { fail_pc = pc; ok = false; break; }
                                }
                            }
                            if !ok { break; }
                            cargs.reverse();
                            let call = b.ins().call(fref, &cargs);
                            stack.push(b.inst_results(call)[0]);
                            pc += 1;
                            continue;
                        }
                        // self-recursion — own key
                        let cfid = match self.ids.get(own_name) {
                            Some(&f) => f,
                            None => { fail_pc = pc; ok = false; break; }
                        };
                        let fref = self.module.declare_func_in_func(
                            cfid, b.func);
                        let mut cargs = Vec::new();
                        for _ in 0..argc {
                            match stack.pop() {
                                Some(v) => cargs.push(v),
                                None => { fail_pc = pc; ok = false; break; }
                            }
                        }
                        if !ok { break; }
                        cargs.reverse();
                        let call = b.ins().call(fref, &cargs);
                        stack.push(b.inst_results(call)[0]);
                    }
                    Instr::Pop => { stack.pop(); }
                    Instr::Not => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        if float {
                            let z = b.ins().f64const(0.0);
                            let c = b.ins().fcmp(
                                cranelift_codegen::ir::condcodes
                                    ::FloatCC::Equal, v, z);
                            let i = b.ins().uextend(types::I64, c);
                            stack.push(
                                b.ins().fcvt_from_sint(types::F64, i));
                        } else {
                            let z = b.ins().iconst(types::I64, 0);
                            let c = b.ins().icmp(IntCC::Equal, v, z);
                            stack.push(b.ins().uextend(types::I64, c));
                        }
                    }
                    Instr::AndOp | Instr::OrOp => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                        let and = matches!(chunk.code[pc], Instr::AndOp);
                        if float {
                            let z = b.ins().f64const(0.0);
                            let cl = b.ins().fcmp(
                                cranelift_codegen::ir::condcodes
                                    ::FloatCC::NotEqual, l, z);
                            let cr = b.ins().fcmp(
                                cranelift_codegen::ir::condcodes
                                    ::FloatCC::NotEqual, r, z);
                            let c = if and { b.ins().band(cl, cr) }
                                    else { b.ins().bor(cl, cr) };
                            let i = b.ins().uextend(types::I64, c);
                            stack.push(
                                b.ins().fcvt_from_sint(types::F64, i));
                        } else {
                            let z = b.ins().iconst(types::I64, 0);
                            let cl = b.ins().icmp(IntCC::NotEqual, l, z);
                            let cr = b.ins().icmp(IntCC::NotEqual, r, z);
                            let c = if and { b.ins().band(cl, cr) }
                                    else { b.ins().bor(cl, cr) };
                            stack.push(b.ins().uextend(types::I64, c));
                        }
                    }
                    Instr::CallId(ni, vid, argc) => {
                        if float { fail_pc = pc; ok = false; break; }
                        let cn = chunk.names[ni as usize].clone();
                        let ckey = format!("{}#{}", cn, vid);
                        if self.compile_pinned(&cn, vid, fns).is_none() {
                            fail_pc = pc; ok = false; break;
                        }
                        let cfid = match self.ids.get(&ckey) {
                            Some(&f) => f,
                            None => { fail_pc = pc; ok = false; break; }
                        };
                        let fref = self.module.declare_func_in_func(
                            cfid, b.func);
                        let mut cargs = Vec::new();
                        for _ in 0..argc {
                            match stack.pop() {
                                Some(v) => cargs.push(v),
                                None => { fail_pc = pc; ok = false;
                                          break; }
                            }
                        }
                        if !ok { break; }
                        cargs.reverse();
                        let call = b.ins().call(fref, &cargs);
                        stack.push(b.inst_results(call)[0]);
                    }
                    Instr::Jump(t) => {
                        let bl = get_block(&mut b, &mut block_for, t as usize);
                        b.ins().jump(bl, &[]);
                        terminated = true;
                    }
                    Instr::JumpIfFalse(t) => {
                        let Some(c) = stack.pop() else { fail_pc = pc; ok = false; break; };
                        let is_false = if float {
                            let z = b.ins().f64const(0.0);
                            b.ins().fcmp(
                                cranelift_codegen::ir::condcodes::FloatCC::Equal,
                                c, z)
                        } else {
                            let z = b.ins().iconst(types::I64, 0);
                            b.ins().icmp(IntCC::Equal, c, z)
                        };
                        let tb = get_block(&mut b, &mut block_for, t as usize);
                        let fb = get_block(&mut b, &mut block_for, pc + 1);
                        b.ins().brif(is_false, tb, &[], fb, &[]);
                        terminated = true;
                    }
                    Instr::Return | Instr::Halt => {
                        let v = stack.pop().unwrap_or_else(|| {
                            if float { b.ins().f64const(0.0) }
                            else { b.ins().iconst(types::I64, 0) }
                        });
                        b.ins().return_(&[v]);
                        terminated = true;
                    }
                    _ => { fail_pc = pc; ok = false; break; }
                }
                pc += 1;
            }
            if !ok {
                let fp = if fail_pc != usize::MAX { fail_pc } else { pc };
                self.fail_at.insert(own_name.to_string(), fp);
                return false;
            }
            let blks: Vec<cranelift_codegen::ir::Block> =
                block_for.values().copied().collect();
            for bl in blks {
                if bl != entry && b.func.layout.block_insts(bl).count() == 0 {
                    if terminated { b.switch_to_block(bl); }
                    let z = b.ins().iconst(types::I64, 0);
                    b.ins().return_(&[z]);
                    terminated = true;
                }
            }
            b.seal_all_blocks();
            b.finalize(self.fe_cfg);
        }
        if let Err(e) = self.module.define_function(fid, &mut ctx) {
            eprintln!("jit: define {}: {:?}", own_name, e);
            return false;
        }
        true
    }

    fn fn_sig(&self, arity: usize, float: bool) -> Signature {
        let t = if float { types::F64 } else { types::I64 };
        Signature {
            params: vec![AbiParam::new(t); arity],
            returns: vec![AbiParam::new(t)],
            call_conv: CallConv::SystemV,
        }
    }
}
