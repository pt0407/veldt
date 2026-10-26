//! Cranelift tier — JIT-compile hot CompiledFns to native code.
//! Targets the pure-int subset: params i64, body uses only
//! const-int/load/store/int-binops/cmp/jumps/return. Anything else
//! falls back to the bytecode VM. Signature: fn(i64...) -> i64.

use cranelift_codegen::ir::{types, AbiParam, BlockArg, Function, InstBuilder, Signature, UserFuncName, condcodes::IntCC};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::{settings, Context};
use cranelift_codegen::settings::Configurable;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use cranelift_native::builder;

use crate::interpreter::Value;
use crate::vm::{Chunk, Instr};
use std::collections::{HashMap, HashSet};

// args passed individually — the VM transmutes to the exact arity.
pub type NativeFn0 = unsafe extern "C" fn() -> i64;
pub type NativeFn1 = unsafe extern "C" fn(i64) -> i64;
pub type NativeFn2 = unsafe extern "C" fn(i64, i64) -> i64;
pub type NativeFn3 = unsafe extern "C" fn(i64, i64, i64) -> i64;
pub type NativeFn4 = unsafe extern "C" fn(i64, i64, i64, i64) -> i64;

pub struct JitFn {
    _module: JITModule,
    pub code_ptr: *const u8,
}

/// Try to JIT a chunk for the pure-int subset. None if any instr or
/// const falls outside it — the caller keeps the bytecode fallback.
pub fn try_compile(chunk: &Chunk, arity: usize, own_name: &str) -> Option<JitFn> {
    use cranelift_jit::JITBuilder;
    let mut flag_builder = settings::builder();
    flag_builder.set("opt_level", "speed").ok()?;
    let isa = match builder() {
        Ok(b) => match b.finish(settings::Flags::new(flag_builder)) {
            Ok(i) => i,
            Err(e) => { eprintln!("jit: isa finish: {:?}", e); return None; }
        },
        Err(e) => { eprintln!("jit: builder: {:?}", e); return None; }
    };
    let fe_cfg = isa.frontend_config();
    let mut module = JITModule::new(
        JITBuilder::with_isa(isa, cranelift_module::default_libcall_names()));
    let sig = Signature {
        params: vec![AbiParam::new(types::I64); arity],
        returns: vec![AbiParam::new(types::I64)],
        call_conv: CallConv::SystemV,
    };
    let fid = module
        .declare_function("veldt_jit", cranelift_module::Linkage::Local, &sig)
        .ok()?;
    let mut ctx = Context::new();
    ctx.func = Function::with_name_signature(UserFuncName::user(0, 0), sig);

    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let nblocks = chunk.code.len() + 1;
        let blocks: Vec<_> = (0..nblocks).map(|_| b.create_block()).collect();
        // entry block params: one i64* argv pointer
        b.append_block_params_for_function_params(blocks[0]);
        b.switch_to_block(blocks[0]);

        // one cranelift Variable per slot; params bind straight in
        let nslots = chunk.slot_names.len().max(arity).max(1);
        let vars: Vec<Variable> = (0..nslots)
            .map(|_| b.declare_var(types::I64)).collect();
        for i in 0..arity.min(nslots) {
            let v = b.block_params(blocks[0])[i];
            b.def_var(vars[i], v);
        }
        // self-call: declare our own FuncRef for recursion
        let self_ref = module.declare_func_in_func(fid, b.func);

        // Two passes: (1) collect leader pcs = every jump/brif target
        // plus the fallthrough after a brif — a block boundary must
        // exist there BEFORE emission, because back-edges reference
        // targets whose code position we haven't reached yet.
        let mut leaders: HashSet<usize> = HashSet::from([0]);
        for (i, instr) in chunk.code.iter().enumerate() {
            match *instr {
                Instr::Jump(t) | Instr::JumpIfFalse(t)
                | Instr::PeekJumpIfNull(t) => {
                    leaders.insert(t as usize);
                }
                _ => {}
            }
            if let Instr::JumpIfFalse(t) = instr {
                let _ = t;
                leaders.insert(i + 1);
            }
        }
        let mut block_for: HashMap<usize, cranelift_codegen::ir::Block> =
            HashMap::new();
        block_for.insert(0, blocks[0]);
        let mut get_block = |b: &mut FunctionBuilder,
                             fm: &mut HashMap<usize, cranelift_codegen::ir::Block>,
                             pc: usize|
            -> cranelift_codegen::ir::Block {
            *fm.entry(pc).or_insert_with(|| b.create_block())
        };

        let mut stack: Vec<cranelift_codegen::ir::Value> = Vec::new();
        let mut pc = 0usize;
        let mut terminated = false;
        while pc < chunk.code.len() {
            if terminated || (pc > 0 && leaders.contains(&pc)) {
                let bl = get_block(&mut b, &mut block_for, pc);
                if !terminated {
                    // fall-through into a leader: bridge with a jump so
                    // the predecessor block has a terminator
                    b.ins().jump(bl, &[]);
                }
                b.switch_to_block(bl);
                terminated = false;
            }
            match chunk.code[pc] {
                Instr::Const(k) => {
                    match &chunk.consts[k as usize] {
                        Value::Int(n) => { stack.push(b.ins().iconst(types::I64, *n)); }
                        _ => return None,
                    }
                }
                Instr::Load(sl) => {
                    stack.push(b.use_var(vars[sl as usize]));
                }
                Instr::Store(sl) => {
                    let v = stack.pop()?;
                    b.def_var(vars[sl as usize], v);
                }
                Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Mod => {
                    let r = stack.pop()?; let l = stack.pop()?;
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
                    let r = stack.pop()?; let l = stack.pop()?;
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
                    let v = b.ins().uextend(types::I64, c);
                    stack.push(v);
                }
                Instr::Neg => { let v = stack.pop()?; let z = b.ins().iconst(types::I64, 0); stack.push(b.ins().isub(z, v)); }
                Instr::Call(ni, argc) => {
                    if chunk.names[ni as usize] != own_name
                        || argc as usize != arity {
                        return None;
                    }
                    let mut cargs = Vec::new();
                    for _ in 0..argc { cargs.push(stack.pop()?); }
                    cargs.reverse();
                    let call = b.ins().call(self_ref, &cargs);
                    let res = b.inst_results(call)[0];
                    stack.push(res);
                }
                Instr::Jump(t) => {
                    let bl = get_block(&mut b, &mut block_for, t as usize);
                    b.ins().jump(bl, &[]);
                    terminated = true;
                }
                Instr::JumpIfFalse(t) => {
                    let c = stack.pop()?;
                    let z = b.ins().iconst(types::I64, 0);
                    let is_false = b.ins().icmp(IntCC::Equal, c, z);
                    let tb = get_block(&mut b, &mut block_for, t as usize);
                    let fb = get_block(&mut b, &mut block_for, pc + 1);
                    b.ins().brif(is_false, tb, &[], fb, &[]);
                    terminated = true;
                }
                Instr::Return | Instr::Halt => {
                    let v = stack.pop().unwrap_or_else(|| b.ins().iconst(types::I64, 0));
                    b.ins().return_(&[v]);
                    terminated = true;
                }
                _ => return None,   // outside the pure-int subset
            }
            pc += 1;
        }
        // any referenced-but-unfilled block still needs a terminator
        let blks: Vec<cranelift_codegen::ir::Block> =
            block_for.values().copied().collect();
        for bl in blks {
            if bl != blocks[0] && b.func.layout.block_insts(bl).count() == 0 {
                if terminated { b.switch_to_block(bl); }
                let z = b.ins().iconst(types::I64, 0);
                b.ins().return_(&[z]);
                terminated = true;
            }
        }
                b.seal_all_blocks();
        b.finalize(fe_cfg);
    }
    if let Err(e) = module.define_function(fid, &mut ctx) {
        eprintln!("jit: define_function: {:?}
IR:
{}", e, ctx.func.display());
        return None;
    }
    if let Err(e) = module.finalize_definitions() {
        eprintln!("jit: finalize: {:?}", e);
        return None;
    }
    if std::env::var("VELDT_JIT_DUMP").is_ok() {
        eprintln!("chunk.code for {}: {:?}", own_name, chunk.code);
        eprintln!("chunk.consts: {:?}", chunk.consts);
        eprintln!("jit IR for {}:
{}", own_name, ctx.func.display());
    }
    let code_ptr = module.get_finalized_function(fid);
    Some(JitFn { _module: module, code_ptr })
}
