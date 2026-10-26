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

pub type NativeFn = unsafe extern "C" fn(*const i64) -> i64;

pub struct JitFn {
    _module: JITModule,
    pub code_ptr: *const u8,
}

/// Try to JIT a chunk for the pure-int subset. None if any instr or
/// const falls outside it — the caller keeps the bytecode fallback.
pub fn try_compile(chunk: &Chunk, arity: usize) -> Option<JitFn> {
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
        params: vec![AbiParam::new(
            module.target_config().pointer_type())],
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

        // one cranelift Variable per slot
        let nslots = chunk.slot_names.len().max(arity).max(1);
        let vars: Vec<Variable> = (0..nslots)
            .map(|_| b.declare_var(types::I64)).collect();
        let argv = b.block_params(blocks[0])[0];
        for i in 0..arity.min(nslots) {
            let off = b.ins().iconst(types::I64, (i * 8) as i64);
            let addr = b.ins().iadd(argv, off);
            let v = b.ins().load(types::I64,
                cranelift_codegen::ir::MemFlagsData::trusted(), addr, 0);
            b.def_var(vars[i], v);
        }
        let _ = argv;

        // simulate the operand stack as cranelift SSA values (i64).
        // One cranelift block per bytecode index; switch_to_block is only
        // legal right after emitting a terminator, so track `terminated`.
        let mut stack: Vec<cranelift_codegen::ir::Value> = Vec::new();
        let mut pc = 0usize;
        let mut terminated = false;
        while pc < chunk.code.len() {
            if terminated {
                b.switch_to_block(blocks[pc]);
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
                Instr::Jump(t) => { b.ins().jump(blocks[t as usize], &[]); terminated = true; }
                Instr::JumpIfFalse(t) => {
                    let c = stack.pop()?;
                    let z = b.ins().iconst(types::I64, 0);
                    let is_false = b.ins().icmp(IntCC::Equal, c, z);
                    b.ins().brif(is_false, blocks[t as usize], &[],
                                 blocks[pc + 1], &[]);
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
    let code_ptr = module.get_finalized_function(fid);
    Some(JitFn { _module: module, code_ptr })
}
