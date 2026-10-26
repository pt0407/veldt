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
    /// 0 = i64 sig, 1 = f64 sig, 2 = boxed-Value ABI
    /// (fn(*mut JitRt, *const Value, i64) -> *mut Value)
    pub mode: u8,
}

pub struct JitEngine {
    module: JITModule,
    fe_cfg: TargetFrontendConfig,
    ids: HashMap<String, FuncId>,
    ptrs: HashMap<String, (*const u8, u8)>,
    /// name#id -> bytecode pc that made the function unJITable
    pub fail_at: HashMap<String, usize>,
    compiling: HashSet<String>,
    rt: RtIds,
}

type Fns = HashMap<String, Vec<CompiledFn>>;

impl JitEngine {
    pub fn new() -> Option<JitEngine> {
        let mut flag_builder = settings::builder();
        flag_builder.set("opt_level", "speed").ok()?;
        let isa = builder().ok()?
            .finish(settings::Flags::new(flag_builder)).ok()?;
        let fe_cfg = isa.frontend_config();
        let mut jb = JITBuilder::with_isa(
            isa, cranelift_module::default_libcall_names());
        jb.symbol("veldt_rt_const", veldt_rt_const as *const u8);
        jb.symbol("veldt_rt_clone", veldt_rt_clone as *const u8);
        jb.symbol("veldt_rt_binop", veldt_rt_binop as *const u8);
        jb.symbol("veldt_rt_unop", veldt_rt_unop as *const u8);
        jb.symbol("veldt_rt_logop", veldt_rt_logop as *const u8);
        jb.symbol("veldt_rt_inop", veldt_rt_inop as *const u8);
        jb.symbol("veldt_rt_truthy", veldt_rt_truthy as *const u8);
        jb.symbol("veldt_rt_isnull", veldt_rt_isnull as *const u8);
        jb.symbol("veldt_rt_pusharg", veldt_rt_pusharg as *const u8);
        jb.symbol("veldt_rt_call", veldt_rt_call as *const u8);
        jb.symbol("veldt_rt_callid", veldt_rt_callid as *const u8);
        jb.symbol("veldt_rt_method", veldt_rt_method as *const u8);
        jb.symbol("veldt_rt_methodvar", veldt_rt_methodvar as *const u8);
        jb.symbol("veldt_rt_makelist", veldt_rt_makelist as *const u8);
        jb.symbol("veldt_rt_index", veldt_rt_index as *const u8);
        jb.symbol("veldt_rt_indexset", veldt_rt_indexset as *const u8);
        jb.symbol("veldt_rt_slice", veldt_rt_slice as *const u8);
        jb.symbol("veldt_rt_getfield", veldt_rt_getfield as *const u8);
        jb.symbol("veldt_rt_makestruct", veldt_rt_makestruct as *const u8);
        jb.symbol("veldt_rt_tostr", veldt_rt_tostr as *const u8);
        jb.symbol("veldt_rt_print", veldt_rt_print as *const u8);
        jb.symbol("veldt_rt_gget", veldt_rt_gget as *const u8);
        jb.symbol("veldt_rt_gset", veldt_rt_gset as *const u8);
        jb.symbol("veldt_rt_iternew", veldt_rt_iternew as *const u8);
        jb.symbol("veldt_rt_iternext", veldt_rt_iternext as *const u8);
        jb.symbol("veldt_rt_iterpop", veldt_rt_iterpop as *const u8);
        jb.symbol("veldt_rt_append", veldt_rt_append as *const u8);
        jb.symbol("veldt_rt_insert", veldt_rt_insert as *const u8);
        jb.symbol("veldt_rt_insertdyn", veldt_rt_insertdyn as *const u8);
        jb.symbol("veldt_rt_fieldset", veldt_rt_fieldset as *const u8);
        jb.symbol("veldt_rt_take_err", veldt_rt_take_err as *const u8);
        jb.symbol("veldt_rt_nullbox", veldt_rt_nullbox as *const u8);
        jb.symbol("veldt_rt_capture", veldt_rt_capture as *const u8);
        jb.symbol("veldt_rt_callvar", veldt_rt_callvar as *const u8);
        jb.symbol("veldt_rt_intbox", veldt_rt_intbox as *const u8);
        jb.symbol("veldt_rt_boolbox", veldt_rt_boolbox as *const u8);
        let mut module = JITModule::new(jb);
        let rt = RtIds::declare(&mut module);
        Some(JitEngine {
            module, fe_cfg, ids: HashMap::new(), ptrs: HashMap::new(),
            compiling: HashSet::new(), fail_at: HashMap::new(), rt,
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
    pub(crate) fn compile_pinned(&mut self, name: &str, id: u32, fns: &Fns)
        -> Option<JitFn> {
        let chosen = fns.get(name)?.iter().find(|v| v.id == id)?;
        self.compile_variant(format!("{}#{}", name, id), chosen, fns)
    }

    fn compile_variant(&mut self, key: String, chosen: &CompiledFn,
                       fns: &Fns) -> Option<JitFn> {
        if let Some(&(p, m)) = self.ptrs.get(&key) {
            return Some(JitFn { code_ptr: p, mode: m });
        }
        // a previous compile may have landed under a mode-suffixed
        // key — redefining the same FuncId is a cranelift error
        for suf in ["$f", "$v"] {
            if let Some(&(p, m)) = self.ptrs.get(&format!("{}{}", key, suf)) {
                return Some(JitFn { code_ptr: p, mode: m });
            }
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
            self.ptrs.insert(key.clone(), (p, 0));
            return Some(JitFn { code_ptr: p, mode: 0 });
        }
        // int-subset failed — try the float signature
        let base_key = key.clone();
        let key = format!("{}$f", key);
        let fid = self.declare(&key, arity, true)?;
        self.compiling.insert(key.clone());
        let ok = self.define(fid, &key, &chosen.code, arity, fns, true);
        self.compiling.remove(&key);
        if ok {
            if self.module.finalize_definitions().is_ok() {
                let p = self.module.get_finalized_function(fid);
                self.ptrs.insert(key.clone(), (p, 1));
                return Some(JitFn { code_ptr: p, mode: 1 });
            }
        }
        // scalar sigs failed — boxed-Value ABI: every operand is a
        // *mut Value and ops delegate to extern "C" helpers
        let vkey = format!("{}$v", base_key);
        let fid = self.declare_value(&vkey)?;
        self.compiling.insert(vkey.clone());
        let ok = self.define_value(fid, &vkey, &chosen.code, arity);
        self.compiling.remove(&vkey);
        if !ok { return None; }
        if self.module.finalize_definitions().is_err() { return None; }
        let p = self.module.get_finalized_function(fid);
        self.ptrs.insert(vkey.clone(), (p, 2));
        Some(JitFn { code_ptr: p, mode: 2 })
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
            macro_rules! catch_check_status {
                ($res:expr) => {{
                    if let Some(&(cpc, esl)) = try_ct.last() {
                        let r = $res;
                        let z = b.ins().iconst(types::I64, 0);
                        let isn = b.ins().icmp(IntCC::SignedLessThan, r, z);
                        let cb = *block_for.entry(cpc as usize)
                            .or_insert_with(|| b.create_block());
                        let eb = b.create_block();
                        let kb = b.create_block();
                        b.ins().brif(isn, eb, &[], kb, &[]);
                        b.switch_to_block(eb);
                        b.seal_block(eb);
                        let rtv3 = b.use_var(rt_v);
                        if let Some(m) = ecall!(rt.take_err, rtv3) {
                            b.def_var(vars[esl as usize], m);
                        }
                        b.ins().jump(cb, &[]);
                        b.switch_to_block(kb);
                        b.seal_block(kb);
                    }
                }};
            }
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
                            // only a callee compiled with THIS sig is
                            // callable — a stale ids entry from a failed
                            // attempt would be an unresolved import
                            let skey = if float {
                                format!("{}$f", ckey) } else { ckey };
                            let mode = if float { 1 } else { 0 };
                            let cfid = match self.ptrs.get(&skey)
                                .filter(|&&(_, m)| m == mode)
                                .and_then(|_| self.ids.get(&skey)) {
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
                        let cfid = match self.ptrs.get(&ckey)
                            .filter(|&&(_, m)| m == 0)
                            .and_then(|_| self.ids.get(&ckey)) {
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

    /// Value-mode signature: fn(*mut JitRt, *const Value, i64) -> *mut Value
    fn declare_value(&mut self, name: &str) -> Option<FuncId> {
        if let Some(&f) = self.ids.get(name) { return Some(f); }
        let sig = Signature {
            params: vec![AbiParam::new(types::I64); 3],
            returns: vec![AbiParam::new(types::I64)],
            call_conv: CallConv::SystemV,
        };
        let f = self.module
            .declare_function(name, Linkage::Local, &sig).ok()?;
        self.ids.insert(name.to_string(), f);
        Some(f)
    }

    /// Boxed-Value emission — every operand-stack slot is *mut Value.
    /// Covers nearly the whole instruction set; closures and
    /// try/catch still bail (they need interpreter context).
    fn define_value(&mut self, fid: FuncId, own_name: &str,
                    chunk: &Chunk, arity: usize) -> bool {
        let mut leaders: HashSet<usize> = HashSet::from([0]);
        for (i, instr) in chunk.code.iter().enumerate() {
            match *instr {
                Instr::Jump(t) | Instr::JumpIfFalse(t)
                | Instr::PeekJumpIfNull(t) | Instr::ForNext(_, t)
                | Instr::TryPush(t, _) => {
                    leaders.insert(t as usize);
                }
                _ => {}
            }
            if matches!(instr, Instr::JumpIfFalse(_)
                        | Instr::PeekJumpIfNull(_)
                        | Instr::ForNext(..)) {
                leaders.insert(i + 1);
            }
        }
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(
            UserFuncName::user(0, fid.as_u32()),
            Signature {
                params: vec![AbiParam::new(types::I64); 3],
                returns: vec![AbiParam::new(types::I64)],
                call_conv: CallConv::SystemV,
            });
        let mut fail_pc = usize::MAX;
        {
            let mut fb_ctx = FunctionBuilderContext::new();
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let params = b.func.dfg.block_params(entry).to_vec();
            let rt_v = b.declare_var(types::I64);
            b.def_var(rt_v, params[0]);
            let argp = params[1];
            // boxed slots own their Values — args arrive as a
            // borrowed array, box a clone per slot
            let nslots = chunk.slot_names.len().max(arity).max(1);
            let vars: Vec<Variable> = (0..nslots)
                .map(|_| b.declare_var(types::I64)).collect();
            for i in 0..arity.min(nslots) {
                // caller boxes args into the arena — slot owns its box
                let p = b.ins().load(types::I64,
                    cranelift_codegen::ir::MemFlagsData::trusted(),
                    argp, (i * 8) as i32);
                b.def_var(vars[i], p);
            }

            let mut block_for: HashMap<usize, cranelift_codegen::ir::Block> =
                HashMap::new();
            if leaders.contains(&0) && chunk.code.len() > 1 {
                let head = b.create_block();
                block_for.insert(0, head);
                b.ins().jump(head, &[]);
                b.switch_to_block(head);
            } else {
                block_for.insert(0, entry);
            }
            let mut stack: Vec<cranelift_codegen::ir::Value> = Vec::new();
            let mut iter_hs: Vec<cranelift_codegen::ir::Value> = Vec::new();
            // compile-time mirror of the try_stack — TryPush/TryPop
            // are lexically matched so no runtime stack is needed.
            let mut try_ct: Vec<(u32, u32)> = Vec::new();
            let mut pc = 0usize;
            let mut terminated = false;
            let mut ok = true;
            macro_rules! ecall {
                ($fid:expr, $($a:expr),*) => {{
                    let fr = self.module.declare_func_in_func($fid, b.func);
                    let c = b.ins().call(fr, &[$($a),*]);
                    b.inst_results(c).first().copied()
                }};
            }
            let rt = self.rt;
            for i in arity..nslots {
                // null-init so unset slots always hold a valid box —
                // CaptureEnv enumerates all slots unconditionally
                let rtv0 = b.use_var(rt_v);
                let fr = self.module
                    .declare_func_in_func(rt.nullbox, b.func);
                let c = b.ins().call(fr, &[rtv0]);
                if let Some(&n) = b.inst_results(c).first() {
                    b.def_var(vars[i], n);
                }
            }
            // after a fallible helper call inside try{}: on null, take
            // the pending error into the err slot and jump to catch
            macro_rules! catch_check {
                ($res:expr) => {{
                    if let Some(&(cpc, esl)) = try_ct.last() {
                        let r = $res;
                        let z = b.ins().iconst(types::I64, 0);
                        let isn = b.ins().icmp(IntCC::Equal, r, z);
                        let cb = *block_for.entry(cpc as usize)
                            .or_insert_with(|| b.create_block());
                        let eb = b.create_block();
                        let kb = b.create_block();
                        b.ins().brif(isn, eb, &[], kb, &[]);
                        b.switch_to_block(eb);
                        b.seal_block(eb);
                        let rtv3 = b.use_var(rt_v);
                        if let Some(m) = ecall!(rt.take_err, rtv3) {
                            b.def_var(vars[esl as usize], m);
                        }
                        b.ins().jump(cb, &[]);
                        b.switch_to_block(kb);
                        b.seal_block(kb);
                    }
                }};
            }
            macro_rules! catch_check_status {
                ($res:expr) => {{
                    if let Some(&(cpc, esl)) = try_ct.last() {
                        let r = $res;
                        let z = b.ins().iconst(types::I64, 0);
                        let isn = b.ins().icmp(IntCC::SignedLessThan, r, z);
                        let cb = *block_for.entry(cpc as usize)
                            .or_insert_with(|| b.create_block());
                        let eb = b.create_block();
                        let kb = b.create_block();
                        b.ins().brif(isn, eb, &[], kb, &[]);
                        b.switch_to_block(eb);
                        b.seal_block(eb);
                        let rtv3 = b.use_var(rt_v);
                        if let Some(m) = ecall!(rt.take_err, rtv3) {
                            b.def_var(vars[esl as usize], m);
                        }
                        b.ins().jump(cb, &[]);
                        b.switch_to_block(kb);
                        b.seal_block(kb);
                    }
                }};
            }
            while pc < chunk.code.len() {
                if terminated || (pc > 0 && leaders.contains(&pc)) {
                    let bl = *block_for.entry(pc)
                        .or_insert_with(|| b.create_block());
                    if !terminated { b.ins().jump(bl, &[]); }
                    b.switch_to_block(bl);
                    terminated = false;
                }
                let rtv = b.use_var(rt_v);
                match chunk.code[pc] {
                    Instr::Const(k) => {
                        let kk = b.ins().iconst(types::I64, k as i64);
                                                let v = ecall!(rt.const_, rtv, kk)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::Load(sl) => {
                        stack.push(b.use_var(vars[sl as usize]));
                    }
                    Instr::Store(sl) => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let slv = vars[sl as usize];
                        if let Some(c) = ecall!(rt.clone_, rtv, v) {
                            b.def_var(slv, c);
                        }
                    }
                    Instr::LoadG(ni) => {
                        let n = b.ins().iconst(types::I64, ni as i64);
                                                let v = ecall!(rt.gget, rtv, n)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::StoreG(ni) => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let n = b.ins().iconst(types::I64, ni as i64);
                        let st = ecall!(rt.gset, rtv, n, v)

                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));

                        catch_check_status!(st);
                    }
                    Instr::Pop => { stack.pop(); }
                    Instr::Add | Instr::Sub | Instr::Mul | Instr::Div
                    | Instr::Mod | Instr::Eq | Instr::Neq | Instr::Lt
                    | Instr::Gt | Instr::Le | Instr::Ge => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                        let op = match chunk.code[pc] {
                            Instr::Add => 0, Instr::Sub => 1,
                            Instr::Mul => 2, Instr::Div => 3,
                            Instr::Mod => 4, Instr::Eq => 5,
                            Instr::Neq => 6, Instr::Lt => 7,
                            Instr::Gt => 8, Instr::Le => 9, _ => 10,
                        };
                        let o = b.ins().iconst(types::I64, op);
                        // specialization: Int/Int ops inline — tag byte 0
                        // (Int) + i64 payload at offset 8 (#[repr(C,u8)]
                        // union). Skips the helper's match + dispatch.
                        let can_inline = matches!(chunk.code[pc],
                            Instr::Add|Instr::Sub|Instr::Mul|Instr::Div|
                            Instr::Mod|Instr::Eq|Instr::Neq|Instr::Lt|
                            Instr::Gt|Instr::Le|Instr::Ge);
                        let v = if can_inline {
                            let lt = b.ins().load(types::I8,
                                cranelift_codegen::ir::MemFlagsData::trusted(), l, 0);
                            let rtg = b.ins().load(types::I8,
                                cranelift_codegen::ir::MemFlagsData::trusted(), r, 0);
                            let z = b.ins().iconst(types::I8, 0);
                            let li = b.ins().icmp(IntCC::Equal, lt, z);
                            let ri = b.ins().icmp(IntCC::Equal, rtg, z);
                            let both = b.ins().band(li, ri);
                            let fb = b.create_block();
                            let hb = b.create_block();
                            let mb = b.create_block();
                            let arg = b.append_block_param(mb, types::I64);
                            b.ins().brif(both, fb, &[], hb, &[]);
                            // fb seals now; hb gets a SECOND predecessor
                            // from the Div/Mod zero-check — seal_all_blocks
                            // catches it at the end instead
                            b.seal_block(fb);
                            // fast: native op on the int payloads
                            b.switch_to_block(fb);
                            let a = b.ins().load(types::I64,
                                cranelift_codegen::ir::MemFlagsData::trusted(), l, 8);
                            let c = b.ins().load(types::I64,
                                cranelift_codegen::ir::MemFlagsData::trusted(), r, 8);
                            let is_cmp = matches!(chunk.code[pc],
                                Instr::Eq|Instr::Neq|Instr::Lt|Instr::Gt|
                                Instr::Le|Instr::Ge);
                            let fv = if is_cmp {
                                let cc = match chunk.code[pc] {
                                    Instr::Eq => IntCC::Equal,
                                    Instr::Neq => IntCC::NotEqual,
                                    Instr::Lt => IntCC::SignedLessThan,
                                    Instr::Gt => IntCC::SignedGreaterThan,
                                    Instr::Le => IntCC::SignedLessThanOrEqual,
                                    _ => IntCC::SignedGreaterThanOrEqual,
                                };
                                let c8 = b.ins().icmp(cc, a, c);
                                let c64 = b.ins().uextend(types::I64, c8);
                                ecall!(rt.boolbox, rtv, c64)
                                    .unwrap_or_else(|| b.ins().iconst(types::I64, 0))
                            } else if matches!(chunk.code[pc], Instr::Div|Instr::Mod) {
                                // sdiv/srem trap on i64::MIN/-1 and 0 —
                                // the helper wraps/errors correctly, so
                                // only inline when the divisor is safe.
                                // (z is fv's placeholder — emitted into fb
                                // BEFORE the brif, since fb fills after it)
                                let z = b.ins().iconst(types::I64, 0);
                                let nz = b.ins().icmp(IntCC::NotEqual, c, z);
                                let min = b.ins().iconst(types::I64, i64::MIN);
                                let neg1 = b.ins().iconst(types::I64, -1);
                                let a_min = b.ins().icmp(IntCC::Equal, a, min);
                                let c_neg1 = b.ins().icmp(IntCC::Equal, c, neg1);
                                let ovf = b.ins().band(a_min, c_neg1);
                                let ovf_not = b.ins().bnot(ovf);
                                let safe = b.ins().band(nz, ovf_not);
                                let db = b.create_block();
                                b.ins().brif(safe, db, &[], hb, &[]);
                                b.seal_block(db);
                                b.switch_to_block(db);
                                let res = match chunk.code[pc] {
                                    Instr::Div => b.ins().sdiv(a, c),
                                    _ => b.ins().srem(a, c),
                                };
                                let bv = ecall!(rt.intbox, rtv, res)
                                    .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                                b.ins().jump(mb, &[cranelift_codegen::ir::BlockArg::Value(bv)]);
                                z
                            } else {
                                let res = match chunk.code[pc] {
                                    Instr::Add => b.ins().iadd(a, c),
                                    Instr::Sub => b.ins().isub(a, c),
                                    _ => b.ins().imul(a, c),
                                };
                                ecall!(rt.intbox, rtv, res)
                                    .unwrap_or_else(|| b.ins().iconst(types::I64, 0))
                            };
                            if !matches!(chunk.code[pc], Instr::Div|Instr::Mod) {
                                b.ins().jump(mb, &[cranelift_codegen::ir::BlockArg::Value(fv)]);
                            }
                            // slow: helper covers every other type pair
                            b.switch_to_block(hb);
                            let hv = ecall!(rt.binop, rtv, o, l, r)
                                .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                            catch_check!(hv);
                            b.ins().jump(mb, &[cranelift_codegen::ir::BlockArg::Value(hv)]);
                            b.switch_to_block(mb);
                            b.seal_block(mb);
                            arg
                        } else {
                            let v = ecall!(rt.binop, rtv, o, l, r)
                                .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                            catch_check!(v);
                            v
                        };
                        stack.push(v);
                    }
                    Instr::Neg | Instr::Not => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let op = b.ins().iconst(types::I64,
                            matches!(chunk.code[pc], Instr::Not) as i64);
                        if let Some(r) = ecall!(rt.unop, rtv, op, v) {
                            stack.push(r);
                        }
                    }
                    Instr::AndOp | Instr::OrOp => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                        let a = b.ins().iconst(types::I64,
                            matches!(chunk.code[pc], Instr::AndOp) as i64);
                                                let v = ecall!(rt.logop, rtv, a, l, r)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::InOp => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                                                let v = ecall!(rt.inop, rtv, l, r)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::Jump(t) => {
                        let bl = *block_for.entry(t as usize)
                            .or_insert_with(|| b.create_block());
                        b.ins().jump(bl, &[]);
                        terminated = true;
                    }
                    Instr::JumpIfFalse(t) => {
                        let Some(c) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let tv = ecall!(rt.truthy, c)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        let z = b.ins().iconst(types::I64, 0);
                        let isf = b.ins().icmp(IntCC::Equal, tv, z);
                        let tb = *block_for.entry(t as usize)
                            .or_insert_with(|| b.create_block());
                        let fb = *block_for.entry(pc + 1)
                            .or_insert_with(|| b.create_block());
                        b.ins().brif(isf, tb, &[], fb, &[]);
                        terminated = true;
                    }
                    Instr::PeekJumpIfNull(t) => {
                        let Some(&v) = stack.last() else {
                            fail_pc = pc; ok = false; break; };
                        let n = ecall!(rt.isnull, v)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        let tb = *block_for.entry(t as usize)
                            .or_insert_with(|| b.create_block());
                        let fb = *block_for.entry(pc + 1)
                            .or_insert_with(|| b.create_block());
                        b.ins().brif(n, tb, &[], fb, &[]);
                        terminated = true;
                    }
                    Instr::Call(ni, argc) => {
                        for _ in 0..argc {
                            let Some(v) = stack.pop() else {
                                fail_pc = pc; ok = false; break; };
                            ecall!(rt.pusharg, rtv, v);
                        }
                        if !ok { break; }
                        let n = b.ins().iconst(types::I64, ni as i64);
                        let c = b.ins().iconst(types::I64, argc as i64);
                                                let v = ecall!(rt.call, rtv, n, c)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::CallId(ni, vid, argc) => {
                        for _ in 0..argc {
                            let Some(v) = stack.pop() else {
                                fail_pc = pc; ok = false; break; };
                            ecall!(rt.pusharg, rtv, v);
                        }
                        if !ok { break; }
                        let n = b.ins().iconst(types::I64, ni as i64);
                        let vv = b.ins().iconst(types::I64, vid as i64);
                        let c = b.ins().iconst(types::I64, argc as i64);
                                                let v = ecall!(rt.callid, rtv, n, vv, c)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::MethodCall(ni, argc) => {
                        for _ in 0..argc {
                            let Some(v) = stack.pop() else {
                                fail_pc = pc; ok = false; break; };
                            ecall!(rt.pusharg, rtv, v);
                        }
                        if !ok { break; }
                        let Some(recv) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let n = b.ins().iconst(types::I64, ni as i64);
                        let c = b.ins().iconst(types::I64, argc as i64);
                                                let v = ecall!(rt.method, rtv, recv, n, c)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::MethodCallVar(sl, ni, argc) => {
                        for _ in 0..argc {
                            let Some(v) = stack.pop() else {
                                fail_pc = pc; ok = false; break; };
                            ecall!(rt.pusharg, rtv, v);
                        }
                        if !ok { break; }
                        let _ = stack.pop();   // receiver slot id, not a value
                        let sp = b.use_var(vars[sl as usize]);
                        let n = b.ins().iconst(types::I64, ni as i64);
                        let c = b.ins().iconst(types::I64, argc as i64);
                                                let v = ecall!(rt.methodvar, rtv, sp, n, c)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::MakeList(n) => {
                        for _ in 0..n {
                            let Some(v) = stack.pop() else {
                                fail_pc = pc; ok = false; break; };
                            ecall!(rt.pusharg, rtv, v);
                        }
                        if !ok { break; }
                        let nn = b.ins().iconst(types::I64, n as i64);
                                                let v = ecall!(rt.makelist, rtv, nn)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::IndexGet => {
                        let (Some(i), Some(o)) = (stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                                                let v = ecall!(rt.index, rtv, o, i)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::IndexSetVar(sl) => {
                        let (Some(val), Some(idx)) =
                            (stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                        let sp = b.use_var(vars[sl as usize]);
                        let st = ecall!(rt.indexset, rtv, sp, idx, val)

                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));

                        catch_check_status!(st);
                    }
                    Instr::ToStr => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        if let Some(r) = ecall!(rt.tostr, rtv, v) {
                            stack.push(r);
                        }
                    }
                    Instr::ForIter => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let h = ecall!(rt.iternew, rtv, v)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, -1));
                        catch_check_status!(h);
                        iter_hs.push(h);
                    }
                    Instr::ForNext(sl, end) => {
                        let Some(&h) = iter_hs.last() else {
                            fail_pc = pc; ok = false; break; };
                        let p = ecall!(rt.iternext, rtv, h)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        let z = b.ins().iconst(types::I64, 0);
                        let isn = b.ins().icmp(IntCC::Equal, p, z);
                        let endb = *block_for.entry(end as usize)
                            .or_insert_with(|| b.create_block());
                        let epb = b.create_block();
                        let nb = b.create_block();
                        b.ins().brif(isn, epb, &[], nb, &[]);
                        b.switch_to_block(epb);
                        b.seal_block(epb);
                        let rtv2 = b.use_var(rt_v);
                        ecall!(rt.iterpop, rtv2, h);
                        iter_hs.pop();
                        b.ins().jump(endb, &[]);
                        b.switch_to_block(nb);
                        b.seal_block(nb);
                        b.def_var(vars[sl as usize], p);
                    }
                    Instr::SliceGet => {
                        let (Some(hi), Some(lo), Some(o)) =
                            (stack.pop(), stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                                                let v = ecall!(rt.slice, rtv, o, lo, hi)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::ListPush(sl) | Instr::AppendVar(sl) => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let sp = b.use_var(vars[sl as usize]);
                        let st = ecall!(rt.append, rtv, sp, v)

                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));

                        catch_check_status!(st);
                    }
                    Instr::InsertVar(sl, ki) => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let sp = b.use_var(vars[sl as usize]);
                        let kk = b.ins().iconst(types::I64, ki as i64);
                        let st = ecall!(rt.insert, rtv, sp, kk, v)

                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));

                        catch_check_status!(st);
                    }
                    Instr::InsertDynVar(sl) => {
                        let (Some(v), Some(k)) = (stack.pop(), stack.pop())
                            else { fail_pc = pc; ok = false; break; };
                        let sp = b.use_var(vars[sl as usize]);
                        let st = ecall!(rt.insertdyn, rtv, sp, k, v)

                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));

                        catch_check_status!(st);
                    }
                    Instr::FieldSetVar(sl, ni) => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let sp = b.use_var(vars[sl as usize]);
                        let n = b.ins().iconst(types::I64, ni as i64);
                        let st = ecall!(rt.fieldset, rtv, sp, n, v)

                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));

                        catch_check_status!(st);
                    }
                    Instr::GetField(ni) => {
                        let Some(o) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        let n = b.ins().iconst(types::I64, ni as i64);
                                                let v = ecall!(rt.getfield, rtv, o, n)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::MakeStruct(ni, n) => {
                        for _ in 0..(n * 2) {
                            let Some(v) = stack.pop() else {
                                fail_pc = pc; ok = false; break; };
                            ecall!(rt.pusharg, rtv, v);
                        }
                        if !ok { break; }
                        let nn = b.ins().iconst(types::I64, ni as i64);
                        let c = b.ins().iconst(types::I64, n as i64);
                                                let v = ecall!(rt.makestruct, rtv, nn, c)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::PrintOut => {
                        let Some(v) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        ecall!(rt.print, rtv, v);
                    }
                    Instr::CaptureEnv => {
                        let Some(proto) = stack.pop() else {
                            fail_pc = pc; ok = false; break; };
                        for i in 0..nslots {
                            let sv = b.use_var(vars[i]);
                            ecall!(rt.pusharg, rtv, sv);
                        }
                        let n = b.ins().iconst(types::I64, nslots as i64);
                        let v = ecall!(rt.capture, rtv, proto, n)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::CallVar(sl, argc) => {
                        for _ in 0..argc {
                            let Some(v) = stack.pop() else {
                                fail_pc = pc; ok = false; break; };
                            ecall!(rt.pusharg, rtv, v);
                        }
                        if !ok { break; }
                        let sp = b.use_var(vars[sl as usize]);
                        let c = b.ins().iconst(types::I64, argc as i64);
                        let v = ecall!(rt.callvar, rtv, sp, c)
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        catch_check!(v);
                        stack.push(v);
                    }
                    Instr::TryPush(cpc, esl) => {
                        try_ct.push((cpc, esl));
                    }
                    Instr::TryPop => { try_ct.pop(); }
                    Instr::Return | Instr::Halt => {
                        let v = stack.pop().unwrap_or_else(||
                            b.ins().iconst(types::I64, 0));
                        b.ins().return_(&[v]);
                        terminated = true;
                    }
                    _ => { fail_pc = pc; ok = false; break; }
                }
                pc += 1;
            }
            let _ = own_name;
            if !ok {
                self.fail_at.insert(own_name.to_string(),
                    if fail_pc != usize::MAX { fail_pc } else { pc });
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
        self.module.define_function(fid, &mut ctx).is_ok()
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

// =================== boxed-Value ABI (mode 2) ===================
// Scalar sigs cover pure int/float fns. Everything else compiles to
// fn(*mut JitRt, *const Value, i64 argc) -> *mut Value: each operand
// on the JIT stack is a boxed Value pointer and complex ops delegate
// to the extern helpers below, which reuse the interpreter's own
// semantics. JitRt is opaque to generated code — it only ever passes
// the pointer back into helpers. Arena: every box is tracked and
// freed by the caller after the call returns.

pub struct JitRt {
    pub vm: *mut crate::vm::Vm,
    pub consts: *const Value,
    pub names: *const String,
    pub slot_names: *const String,
    pub n_slot_names: usize,
    // shared across nested value-mode calls — the root caller owns
    // the backing storage so a whole call tree allocs into one arena
    pub arena: *mut Vec<*mut Value>,
    pub argbuf: *mut Vec<*mut Value>,
    pub iters: *mut Vec<(Vec<Value>, usize)>,
    pub err: *mut Option<String>,
}

unsafe fn mk(rt: *mut JitRt, v: Value) -> *mut Value {
    if rt.is_null() || (*(*rt).err).is_some() { return std::ptr::null_mut(); }
    let p = Box::into_raw(Box::new(v));
    (*(*rt).arena).push(p);
    p
}
unsafe fn rfail(rt: *mut JitRt, e: String) -> *mut Value {
    if !rt.is_null() && (*(*rt).err).is_none() { *(*rt).err = Some(e); }
    std::ptr::null_mut()
}
unsafe fn nok(rt: *mut JitRt, e: String) -> i64 {
    if !rt.is_null() && (*(*rt).err).is_none() { *(*rt).err = Some(e); }
    -1
}
unsafe fn nname(rt: *mut JitRt, ni: i64) -> String {
    (*(*rt).names.add(ni as usize)).clone()
}
unsafe fn take_args(rt: *mut JitRt, argc: i64) -> Vec<Value> {
    let n = argc as usize;
    let buf = &mut *(*rt).argbuf;
    let tail: Vec<Value> = buf.iter().rev().take(n)
        .filter(|p| !p.is_null()).map(|p| (**p).clone()).collect();
    let keep = buf.len().saturating_sub(n);
    buf.truncate(keep);
    tail.into_iter().rev().collect()
}

#[no_mangle]
pub unsafe extern "C" fn veldt_rt_const(rt: *mut JitRt, k: i64) -> *mut Value {
    let v = (*(*rt).consts.add(k as usize)).clone();
    mk(rt, v)
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_clone(rt: *mut JitRt, v: *mut Value) -> *mut Value {
    if v.is_null() { return std::ptr::null_mut(); }
    mk(rt, (*v).clone())
}
fn bop(op: i64) -> crate::ast::BinOp {
    use crate::ast::BinOp::*;
    match op {
        0 => Add, 1 => Sub, 2 => Mul, 3 => Div, 4 => Mod,
        5 => Eq, 6 => Neq, 7 => Lt, 8 => Gt, 9 => Le, _ => Ge,
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_binop(rt: *mut JitRt, op: i64,
    l: *mut Value, r: *mut Value) -> *mut Value {
    if l.is_null() || r.is_null() { return std::ptr::null_mut(); }
    match crate::vm::binop(&*l, &bop(op), &*r) {
        Ok(v) => mk(rt, v),
        Err(e) => rfail(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_unop(rt: *mut JitRt, op: i64,
    v: *mut Value) -> *mut Value {
    if v.is_null() { return std::ptr::null_mut(); }
    match &*v {
        _ if op == 1 => mk(rt, Value::Bool(!crate::vm::truthy(&*v))),
        Value::Int(n) => mk(rt, Value::Int(-n)),
        Value::Float(f) => mk(rt, Value::Float(-f)),
        _ => rfail(rt, "cannot negate".into()),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_logop(rt: *mut JitRt, and: i64,
    l: *mut Value, r: *mut Value) -> *mut Value {
    if l.is_null() || r.is_null() { return std::ptr::null_mut(); }
    let (tl, tr) = (crate::vm::truthy(&*l), crate::vm::truthy(&*r));
    mk(rt, Value::Bool(if and != 0 { tl && tr } else { tl || tr }))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_inop(rt: *mut JitRt,
    l: *mut Value, r: *mut Value) -> *mut Value {
    if l.is_null() || r.is_null() { return std::ptr::null_mut(); }
    let hit = match (&*l, &*r) {
        (v, Value::List(items)) =>
            items.iter().any(|i| crate::vm::values_eq(v, i)),
        (Value::Str(a), Value::Str(b)) => b.contains(&**a),
        (Value::Str(k), Value::Dict(d)) => d.contains_key(&**k),
        _ => return rfail(rt, "'in' requires list, dict, or string".into()),
    };
    mk(rt, Value::Bool(hit))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_truthy(v: *mut Value) -> i64 {
    if v.is_null() { return 0; }
    crate::vm::truthy(&*v) as i64
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_isnull(v: *mut Value) -> i64 {
    if v.is_null() { return 1; }
    matches!(&*v, Value::Null) as i64
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_pusharg(rt: *mut JitRt, v: *mut Value) -> i64 {
    (*(*rt).argbuf).push(v); 0
}
unsafe fn argbox_ptrs(rt: *mut JitRt, argc: i64) -> Vec<*mut Value> {
    let buf = &mut *(*rt).argbuf;
    let n = argc as usize;
    let tail: Vec<*mut Value> = buf.iter().rev().take(n).copied().collect();
    let keep = buf.len().saturating_sub(n);
    buf.truncate(keep);
    tail.into_iter().rev().collect()
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_call(rt: *mut JitRt, ni: i64,
    argc: i64) -> *mut Value {
    let name = nname(rt, ni);
    let ptrs = argbox_ptrs(rt, argc);
    // clone args into the shared arena — by-value semantics — then a
    // direct native call when the variant is already mode-2 compiled
    let argbox: Vec<*mut Value> = ptrs.iter().map(|p|
        if p.is_null() { std::ptr::null_mut() } else { mk(rt, (**p).clone()) }
    ).collect();
    if let Some(out) = (*(*rt).vm).jit_call_fast(&name, None, &argbox, rt) {
        return out;
    }
    let args: Vec<Value> = ptrs.iter()
        .filter(|p| !p.is_null()).map(|p| (**p).clone()).collect();
    match (*(*rt).vm).jit_call(&name, None, args) {
        Ok(v) => mk(rt, v),
        Err(e) => rfail(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_callid(rt: *mut JitRt, ni: i64, vid: i64,
    argc: i64) -> *mut Value {
    let name = nname(rt, ni);
    let ptrs = argbox_ptrs(rt, argc);
    let argbox: Vec<*mut Value> = ptrs.iter().map(|p|
        if p.is_null() { std::ptr::null_mut() } else { mk(rt, (**p).clone()) }
    ).collect();
    if let Some(out) =
        (*(*rt).vm).jit_call_fast(&name, Some(vid as u32), &argbox, rt) {
        return out;
    }
    let args: Vec<Value> = ptrs.iter()
        .filter(|p| !p.is_null()).map(|p| (**p).clone()).collect();
    match (*(*rt).vm).jit_call(&name, Some(vid as u32), args) {
        Ok(v) => mk(rt, v),
        Err(e) => rfail(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_method(rt: *mut JitRt, recv: *mut Value,
    ni: i64, argc: i64) -> *mut Value {
    if recv.is_null() { return std::ptr::null_mut(); }
    let m = nname(rt, ni);
    let args = take_args(rt, argc);
    match (*(*rt).vm).delegate_method((*recv).clone(), &m, args) {
        Ok(v) => mk(rt, v),
        Err(e) => rfail(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_methodvar(rt: *mut JitRt,
    sp: *mut Value, ni: i64, argc: i64) -> *mut Value {
    if sp.is_null() { return std::ptr::null_mut(); }
    let m = nname(rt, ni);
    let args = take_args(rt, argc);
    let vm = &mut *(*rt).vm;
    match vm.delegate_method((*sp).clone(), &m, args) {
        Ok(v) => {
            // mutating methods leave the new receiver in __vm_recv —
            // write it back into the slot's box
            let newv = vm.helper.vars.borrow()
                .get("__vm_recv").cloned();
            if let Some(nv) = newv {
                let old = std::ptr::replace(sp, nv);
                drop(old);
            }
            mk(rt, v)
        }
        Err(e) => rfail(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_makelist(rt: *mut JitRt, n: i64) -> *mut Value {
    let items = take_args(rt, n);
    mk(rt, Value::List(items.into()))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_index(rt: *mut JitRt, o: *mut Value,
    i: *mut Value) -> *mut Value {
    if o.is_null() || i.is_null() { return std::ptr::null_mut(); }
    match crate::vm::index_get(&*o, &*i) {
        Ok(v) => mk(rt, v),
        Err(e) => rfail(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_indexset(rt: *mut JitRt, sp: *mut Value,
    idx: *mut Value, val: *mut Value) -> i64 {
    if sp.is_null() || idx.is_null() || val.is_null() { return nok(rt, "jit set on err".into()); }
    match crate::vm::index_set(&*sp, &*idx, &*val) {
        Ok(nv) => { std::ptr::write(sp, nv); 0 }
        Err(e) => nok(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_slice(rt: *mut JitRt, o: *mut Value,
    lo: *mut Value, hi: *mut Value) -> *mut Value {
    if o.is_null() || lo.is_null() || hi.is_null() { return std::ptr::null_mut(); }
    match crate::vm::slice_get(&*o, &*lo, &*hi) {
        Ok(v) => mk(rt, v),
        Err(e) => rfail(rt, e),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_getfield(rt: *mut JitRt, o: *mut Value,
    ni: i64) -> *mut Value {
    if o.is_null() { return std::ptr::null_mut(); }
    let name = nname(rt, ni);
    let v = match &*o {
        Value::Struct(_, f) => f.get(&name).cloned().unwrap_or(Value::Null),
        Value::Dict(d) => d.get(&name).cloned().unwrap_or(Value::Null),
        _ => return rfail(rt, format!(
            "field access '{}' on {}", name, crate::vm::obj_tag(&*o))),
    };
    mk(rt, v)
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_makestruct(rt: *mut JitRt, ni: i64,
    n: i64) -> *mut Value {
    let mut fields = std::collections::HashMap::new();
    for _ in 0..n {
        let pair = take_args(rt, 2);
        if let [Value::Str(k), v] = pair.as_slice() {
            fields.insert((**k).to_string(), v.clone());
        }
    }
    mk(rt, Value::Struct(nname(rt, ni), fields))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_tostr(rt: *mut JitRt, v: *mut Value) -> *mut Value {
    if v.is_null() { return std::ptr::null_mut(); }
    mk(rt, Value::Str(format!("{}", &*v).into()))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_print(rt: *mut JitRt, v: *mut Value) -> i64 {
    if !v.is_null() { println!("{}", &*v); }
    0
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_gget(rt: *mut JitRt, ni: i64) -> *mut Value {
    let name = nname(rt, ni);
    let v = (*(*rt).vm).globals.get(&name).cloned().unwrap_or(Value::Null);
    mk(rt, v)
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_gset(rt: *mut JitRt, ni: i64,
    v: *mut Value) -> i64 {
    if v.is_null() { return nok(rt, "jit gset on err".into()); }
    (*(*rt).vm).globals.insert(nname(rt, ni), (*v).clone());
    0
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_iternew(rt: *mut JitRt, v: *mut Value) -> i64 {
    if v.is_null() { return nok(rt, "iter on err".into()); }
    let items: Vec<Value> = match &*v {
        Value::List(l) => l.iter().cloned().collect(),
        Value::Str(s) => s.chars()
            .map(|c| Value::Str(c.to_string().into())).collect(),
        Value::Dict(d) => d.keys()
            .map(|k| Value::Str(k.clone().into())).collect(),
        _ => return nok(rt, "for needs a list, string, or dict".into()),
    };
    let it = &mut *(*rt).iters;
    it.push((items, 0));
    (it.len() - 1) as i64
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_iternext(rt: *mut JitRt, h: i64) -> *mut Value {
    let r = &mut *rt;
    let it = &mut *r.iters;
    let (items, i) = &mut it[h as usize];
    if *i >= items.len() { return std::ptr::null_mut(); }
    let v = items[*i].clone(); *i += 1;
    mk(rt, v)
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_iterpop(rt: *mut JitRt, h: i64) -> i64 {
    let r = &mut *rt;
    let it = &mut *r.iters;
    if let Some(it) = it.get_mut(h as usize) {
        it.0.clear(); it.1 = 0;
    }
    0
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_append(rt: *mut JitRt, sp: *mut Value,
    v: *mut Value) -> i64 {
    if sp.is_null() || v.is_null() { return nok(rt, "append on err".into()); }
    match &mut *sp {
        Value::List(l) => { l.push_back((*v).clone()); 0 }
        _ => nok(rt, "append on non-list".into()),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_insert(rt: *mut JitRt, sp: *mut Value,
    ki: i64, v: *mut Value) -> i64 {
    if sp.is_null() || v.is_null() { return nok(rt, "insert on err".into()); }
    let k = nname(rt, ki);
    match &mut *sp {
        Value::Dict(d) => { d.insert(k, (*v).clone()); 0 }
        Value::Set(st) => { st.push((*v).clone()); 0 }
        _ => nok(rt, "insert on non-dict/set".into()),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_insertdyn(rt: *mut JitRt, sp: *mut Value,
    kp: *mut Value, v: *mut Value) -> i64 {
    if sp.is_null() || kp.is_null() || v.is_null() {
        return nok(rt, "insert on err".into()); }
    let key = match &*kp {
        Value::Str(s) => (**s).to_string(),
        _ => return nok(rt, "dict key must be string".into()),
    };
    match &mut *sp {
        Value::Dict(d) => { d.insert(key, (*v).clone()); 0 }
        Value::Set(st) => { st.push((*v).clone()); 0 }
        _ => nok(rt, "insert on non-dict/set".into()),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_capture(rt: *mut JitRt,
    proto: *mut Value, nslots: i64) -> *mut Value {
    if proto.is_null() { return std::ptr::null_mut(); }
    let Value::Closure(params, body, _) = &*proto else {
        return rfail(rt, "CaptureEnv on non-closure".into());
    };
    let (params, body) = (params.clone(), body.clone());
    // argbuf tail holds every slot's box — zip with slot_names
    let slots = take_args(rt, nslots);
    let mut env = std::collections::HashMap::new();
    for (i, v) in slots.iter().enumerate() {
        if i >= (*rt).n_slot_names { break; }
        let name = (*(*rt).slot_names.add(i)).clone();
        if !name.is_empty() { env.insert(name, v.clone()); }
    }
    mk(rt, Value::Closure(params, body,
        std::rc::Rc::new(std::cell::RefCell::new(env))))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_callvar(rt: *mut JitRt,
    sp: *mut Value, argc: i64) -> *mut Value {
    if sp.is_null() { return std::ptr::null_mut(); }
    let args = take_args(rt, argc);
    match &*sp {
        Value::Closure(..) => match
            (*(*rt).vm).helper.call_closure_pub(&*sp, args) {
            Ok(v) => mk(rt, v),
            Err(e) => rfail(rt, e),
        },
        _ => rfail(rt, "variable is not callable".into()),
    }
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_intbox(rt: *mut JitRt, i: i64) -> *mut Value {
    mk(rt, Value::Int(i))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_boolbox(rt: *mut JitRt, i: i64) -> *mut Value {
    mk(rt, Value::Bool(i != 0))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_nullbox(rt: *mut JitRt) -> *mut Value {
    mk(rt, Value::Null)
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_take_err(rt: *mut JitRt) -> *mut Value {
    let e = (*(*rt).err).take().unwrap_or_else(|| "error".into());
    mk(rt, Value::Str(e.into()))
}
#[no_mangle]
pub unsafe extern "C" fn veldt_rt_fieldset(rt: *mut JitRt, sp: *mut Value,
    ni: i64, v: *mut Value) -> i64 {
    if sp.is_null() || v.is_null() { return nok(rt, "fieldset on err".into()); }
    match &mut *sp {
        Value::Struct(_, f) => { f.insert(nname(rt, ni), (*v).clone()); 0 }
        _ => nok(rt, "field-assign on non-struct".into()),
    }
}

/// FuncIds for the value-ABI runtime helpers — declared once per
/// module as imports; generated code calls them via FuncRef.
#[derive(Clone, Copy)]
struct RtIds {
    const_: FuncId, clone_: FuncId, binop: FuncId, unop: FuncId,
    logop: FuncId, inop: FuncId, truthy: FuncId, isnull: FuncId,
    pusharg: FuncId, call: FuncId, callid: FuncId, method: FuncId,
    methodvar: FuncId,
    makelist: FuncId, index: FuncId, indexset: FuncId, slice: FuncId,
    getfield: FuncId, makestruct: FuncId, tostr: FuncId, print: FuncId,
    gget: FuncId, gset: FuncId, iternew: FuncId, iternext: FuncId,
    iterpop: FuncId, append: FuncId, insert: FuncId, insertdyn: FuncId,
    fieldset: FuncId, take_err: FuncId, nullbox: FuncId,
    capture: FuncId, callvar: FuncId,
    intbox: FuncId, boolbox: FuncId,
}

impl RtIds {
    fn declare(m: &mut JITModule) -> RtIds {
        // every helper is an i64-only signature: (argc params) -> i64
        // (0 return => void, encoded as params, false)
        let d = |m: &mut JITModule, name: &str, nargs: usize, ret: bool| {
            let sig = Signature {
                params: vec![AbiParam::new(types::I64); nargs],
                returns: if ret { vec![AbiParam::new(types::I64)] }
                         else { vec![] },
                call_conv: CallConv::SystemV,
            };
            m.declare_function(name, Linkage::Import, &sig).unwrap()
        };
        RtIds {
            const_: d(m, "veldt_rt_const", 2, true),
            clone_: d(m, "veldt_rt_clone", 2, true),
            binop: d(m, "veldt_rt_binop", 4, true),
            unop: d(m, "veldt_rt_unop", 3, true),
            logop: d(m, "veldt_rt_logop", 4, true),
            inop: d(m, "veldt_rt_inop", 3, true),
            truthy: d(m, "veldt_rt_truthy", 1, true),
            isnull: d(m, "veldt_rt_isnull", 1, true),
            pusharg: d(m, "veldt_rt_pusharg", 2, true),
            call: d(m, "veldt_rt_call", 3, true),
            callid: d(m, "veldt_rt_callid", 4, true),
            method: d(m, "veldt_rt_method", 4, true),
            methodvar: d(m, "veldt_rt_methodvar", 4, true),
            makelist: d(m, "veldt_rt_makelist", 2, true),
            index: d(m, "veldt_rt_index", 3, true),
            indexset: d(m, "veldt_rt_indexset", 4, true),
            slice: d(m, "veldt_rt_slice", 4, true),
            getfield: d(m, "veldt_rt_getfield", 3, true),
            makestruct: d(m, "veldt_rt_makestruct", 3, true),
            tostr: d(m, "veldt_rt_tostr", 2, true),
            print: d(m, "veldt_rt_print", 2, true),
            gget: d(m, "veldt_rt_gget", 2, true),
            gset: d(m, "veldt_rt_gset", 3, true),
            iternew: d(m, "veldt_rt_iternew", 2, true),
            iternext: d(m, "veldt_rt_iternext", 2, true),
            iterpop: d(m, "veldt_rt_iterpop", 2, true),
            append: d(m, "veldt_rt_append", 3, true),
            insert: d(m, "veldt_rt_insert", 4, true),
            insertdyn: d(m, "veldt_rt_insertdyn", 4, true),
            fieldset: d(m, "veldt_rt_fieldset", 4, true),
            take_err: d(m, "veldt_rt_take_err", 1, true),
            nullbox: d(m, "veldt_rt_nullbox", 1, true),
            capture: d(m, "veldt_rt_capture", 3, true),
            callvar: d(m, "veldt_rt_callvar", 3, true),
            intbox: d(m, "veldt_rt_intbox", 2, true),
            boolbox: d(m, "veldt_rt_boolbox", 2, true),
        }
    }
}
