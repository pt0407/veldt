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

pub struct JitFn { pub code_ptr: *const u8 }

pub struct JitEngine {
    module: JITModule,
    fe_cfg: TargetFrontendConfig,
    ids: HashMap<String, FuncId>,
    pub ptrs: HashMap<String, *const u8>,
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
            compiling: HashSet::new(),
        })
    }

    /// Declare (or fetch) the FuncId for `name` with `arity` i64 params.
    /// Declared before the body is built so recursive/forward calls
    /// resolve to a real reference.
    fn declare(&mut self, name: &str, arity: usize) -> Option<FuncId> {
        if let Some(&f) = self.ids.get(name) { return Some(f); }
        let sig = Signature {
            params: vec![AbiParam::new(types::I64); arity],
            returns: vec![AbiParam::new(types::I64)],
            call_conv: CallConv::SystemV,
        };
        let f = self.module
            .declare_function(name, Linkage::Local, &sig).ok()?;
        self.ids.insert(name.to_string(), f);
        Some(f)
    }

    /// Compile `name` (fittest variant) and everything it calls into
    /// the engine. Returns the code pointer when the whole reachable
    /// set compiled — otherwise None (caller falls back to bytecode).
    pub fn compile_fn(&mut self, name: &str, fns: &Fns) -> Option<*const u8> {
        if let Some(&p) = self.ptrs.get(name) { return Some(p); }
        if self.compiling.contains(name) {
            // cycle — already declared; caller's call site works
            return self.ids.get(name).map(|_| usize::MAX as *const u8);
        }
        let f = fns.get(name)?;
        let chosen = f.iter()
            .max_by(|a, b| a.fitness.partial_cmp(&b.fitness)
                .unwrap_or(std::cmp::Ordering::Equal))?;
        let arity = chosen.params.len();
        let fid = self.declare(name, arity)?;
        self.compiling.insert(name.to_string());
        let ok = self.define(fid, name, &chosen.code, arity, fns);
        self.compiling.remove(name);
        if !ok { return None; }
        if self.module.finalize_definitions().is_err() { return None; }
        let p = self.module.get_finalized_function(fid);
        self.ptrs.insert(name.to_string(), p);
        Some(p)
    }

    /// Build the IR for one function body. Calls resolve eagerly —
    /// the callee is compiled first so its FuncId exists.
    fn define(&mut self, fid: FuncId, own_name: &str,
              chunk: &Chunk, arity: usize, fns: &Fns) -> bool {
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
                if cn != own_name {
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
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(
            UserFuncName::user(0, fid.as_u32()), self.fn_sig(arity));
        {
            let mut fb_ctx = FunctionBuilderContext::new();
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let nslots = chunk.slot_names.len().max(arity).max(1);
            let vars: Vec<Variable> = (0..nslots)
                .map(|_| b.declare_var(types::I64)).collect();
            for i in 0..arity.min(nslots) {
                let v = b.block_params(entry)[i];
                b.def_var(vars[i], v);
            }
            let mut block_for: HashMap<usize, cranelift_codegen::ir::Block> =
                HashMap::new();
            block_for.insert(0, entry);
            let mut get_block =
                |b: &mut FunctionBuilder,
                 fm: &mut HashMap<usize, cranelift_codegen::ir::Block>,
                 pc: usize| -> cranelift_codegen::ir::Block {
                    *fm.entry(pc).or_insert_with(|| b.create_block())
                };
            let mut stack: Vec<cranelift_codegen::ir::Value> = Vec::new();
            let mut pc = 0usize;
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
                        Value::Int(n) => { stack.push(b.ins().iconst(types::I64, *n)); }
                        _ => { ok = false; break; }
                    },
                    Instr::Load(sl) => {
                        stack.push(b.use_var(vars[sl as usize]));
                    }
                    Instr::Store(sl) => {
                        match stack.pop() {
                            Some(v) => b.def_var(vars[sl as usize], v),
                            None => { ok = false; break; }
                        }
                    }
                    Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Mod => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop()) else {
                            ok = false; break;
                        };
                        let v = match chunk.code[pc] {
                            Instr::Add => b.ins().iadd(l, r),
                            Instr::Sub => b.ins().isub(l, r),
                            Instr::Mul => b.ins().imul(l, r),
                            Instr::Div => b.ins().sdiv(l, r),
                            Instr::Mod => b.ins().srem(l, r),
                            _ => unreachable!(),
                        };
                        stack.push(v);
                    }
                    Instr::Eq | Instr::Neq | Instr::Lt | Instr::Gt | Instr::Le | Instr::Ge => {
                        let (Some(r), Some(l)) = (stack.pop(), stack.pop()) else {
                            ok = false; break;
                        };
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
                    Instr::Neg => {
                        let Some(v) = stack.pop() else { ok = false; break; };
                        let z = b.ins().iconst(types::I64, 0);
                        stack.push(b.ins().isub(z, v));
                    }
                    Instr::Call(ni, argc) => {
                        let cn = chunk.names[ni as usize].clone();
                        // compile the callee first so its FuncId exists
                        if cn != own_name {
                            match self.compile_fn(&cn, fns) {
                                Some(_) => {}
                                None => { ok = false; break; }
                            }
                        }
                        let cfid = match self.ids.get(&cn) {
                            Some(&f) => f,
                            None => { ok = false; break; }
                        };
                        let fref = self.module.declare_func_in_func(
                            cfid, b.func);
                        let mut cargs = Vec::new();
                        for _ in 0..argc {
                            match stack.pop() {
                                Some(v) => cargs.push(v),
                                None => { ok = false; break; }
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
                        let Some(c) = stack.pop() else { ok = false; break; };
                        let z = b.ins().iconst(types::I64, 0);
                        let is_false = b.ins().icmp(IntCC::Equal, c, z);
                        let tb = get_block(&mut b, &mut block_for, t as usize);
                        let fb = get_block(&mut b, &mut block_for, pc + 1);
                        b.ins().brif(is_false, tb, &[], fb, &[]);
                        terminated = true;
                    }
                    Instr::Return | Instr::Halt => {
                        let v = stack.pop()
                            .unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                        b.ins().return_(&[v]);
                        terminated = true;
                    }
                    _ => { ok = false; break; }
                }
                pc += 1;
            }
            if !ok { return false; }
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

    fn fn_sig(&self, arity: usize) -> Signature {
        Signature {
            params: vec![AbiParam::new(types::I64); arity],
            returns: vec![AbiParam::new(types::I64)],
            call_conv: CallConv::SystemV,
        }
    }
}
