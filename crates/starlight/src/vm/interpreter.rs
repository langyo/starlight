use self::{frame::CallFrame, stack::Stack};
use super::{
    arguments::*, array::*, code_block::CodeBlock, environment::*, error::JsTypeError, error::*,
    function::JsVMFunction, native_iterator::*, object::*, slot::*, string::JsString,
    symbol_table::*, value::*, Runtime,
};
use crate::root;
use crate::{
    bytecode::opcodes::Opcode,
    gc::{
        cell::{GcCell, GcPointer, Trace},
        snapshot::deserializer::Deserializable,
    },
};
use crate::{bytecode::*, gc::cell::Tracer};
use profile::{ArithProfile, ByValProfile};
use std::intrinsics::{likely, unlikely};
use wtf_rs::unwrap_unchecked;
pub mod frame;
pub mod stack;

impl Runtime {
    pub(crate) fn perform_vm_call(
        &mut self,
        func: &JsVMFunction,
        env: JsValue,
        args_: &Arguments,
        callee: JsValue,
    ) -> Result<JsValue, JsValue> {
        let stack = self.shadowstack();
        root!(scope = stack, unsafe {
            env.get_object().downcast::<Environment>().unwrap()
        });

        root!(
            nscope = stack,
            Environment::new(
                self,
                func.code.param_count
                    + func.code.var_count
                    + func.code.rest_at.map(|_| 1).unwrap_or(0)
                    + if func.code.use_arguments { 1 } else { 0 }
            )
        );
        nscope.parent = Some(*scope);
        let mut i = 0;
        for _ in 0..func.code.param_count {
            nscope.as_slice_mut()[i as usize].value = args_.at(i);
            i += 1;
        }

        if let Some(rest) = func.code.rest_at {
            let mut args_arr = JsArray::new(self, args_.size() as u32 - i as u32);
            let mut ai = 0;
            for ix in i..args_.size() {
                args_arr.put_indexed_slot(
                    self,
                    ai as _,
                    args_.at(ix as _),
                    &mut Slot::new(),
                    false,
                )?;
                ai += 1;
            }
            nscope.as_slice_mut()[rest as usize].value = JsValue::new(args_arr);
        }

        if func.code.use_arguments {
            let p = {
                let mut p = vec![];
                for i in 0..func.code.param_count {
                    p.push(Symbol::Index(i));
                }
                p
            };
            let mut args =
                JsArguments::new(self, nscope.clone(), &p, args_.size() as _, args_.values);

            for k in i..args_.size() {
                args.put(self, Symbol::Index(k as _), args_.at(k), false)?;
            }

            nscope.as_slice_mut()[func.code.args_at as usize].value = JsValue::new(args);
        }
        let _this = if func.code.strict && !args_.this.is_object() {
            JsValue::encode_undefined_value()
        } else {
            if args_.this.is_undefined() {
                JsValue::encode_object_value(self.global_object())
            } else {
                args_.this
            }
        };

        unsafe {
            eval_internal(
                self,
                func.code,
                &func.code.code[0] as *const u8 as *mut u8,
                _this,
                args_.ctor_call,
                *nscope,
                callee,
            )
        }
    }

    pub(crate) fn setup_for_vm_call(
        &mut self,
        func: &JsVMFunction,
        env: JsValue,
        args_: &Arguments,
    ) -> Result<(JsValue, GcPointer<Environment>), JsValue> {
        let stack = self.shadowstack();
        root!(scope = stack, unsafe {
            env.get_object().downcast::<Environment>().unwrap()
        });

        root!(
            nscope = stack,
            Environment::new(
                self,
                func.code.param_count
                    + func.code.var_count
                    + func.code.rest_at.map(|_| 1).unwrap_or(0)
                    + if func.code.use_arguments { 1 } else { 0 }
            )
        );
        nscope.parent = Some(*scope);
        let mut i = 0;
        for _ in 0..func.code.param_count {
            /*let _ = nscope
                .put(self, *p, args_.at(i), false)
                .unwrap_or_else(|_| unsafe { unreachable_unchecked() });
            */
            nscope.as_slice_mut()[i as usize].value = args_.at(i);
            i += 1;
        }

        if let Some(rest) = func.code.rest_at {
            let mut args_arr = JsArray::new(self, args_.size() as u32 - i as u32);
            let mut ai = 0;
            for ix in i..args_.size() {
                args_arr.put_indexed_slot(
                    self,
                    ai as _,
                    args_.at(ix as _),
                    &mut Slot::new(),
                    false,
                )?;
                ai += 1;
            }
            nscope.as_slice_mut()[rest as usize].value = JsValue::new(args_arr);
            //  nscope.put(self, rest, JsValue::encode_object_value(args_arr), false)?;
        }

        /*for j in 0..func.code.var_count {
            vscope.as_slice_mut()[j as usize + i as usize].0 = JsValue::encode_undefined_value();

            //   vscope.put(self, *val, JsValue::encode_undefined_value(), false)?;
        }*/
        if func.code.use_arguments {
            let p = {
                let mut p = vec![];
                for i in 0..func.code.param_count {
                    p.push(Symbol::Index(i));
                }
                p
            };
            let mut args =
                JsArguments::new(self, nscope.clone(), &p, args_.size() as _, args_.values);

            for k in i..args_.size() {
                args.put(self, Symbol::Index(k as _), args_.at(k), false)?;
            }

            nscope.as_slice_mut()[func.code.args_at as usize].value = JsValue::new(args);
        }
        let _this = if func.code.strict && !args_.this.is_object() {
            JsValue::encode_undefined_value()
        } else {
            if args_.this.is_undefined() {
                JsValue::encode_object_value(self.global_object())
            } else {
                args_.this
            }
        };

        Ok((_this, *nscope))
    }
}

#[inline(never)]
unsafe fn eval_internal(
    rt: &mut Runtime,
    code: GcPointer<CodeBlock>,
    ip: *mut u8,
    this: JsValue,
    ctor: bool,
    scope: GcPointer<Environment>,
    callee: JsValue,
) -> Result<JsValue, JsValue> {
    let frame = rt.stack.new_frame(0, callee, scope);
    if frame.is_none() {
        let msg = JsString::new(rt, "stack overflow");
        return Err(JsValue::encode_object_value(JsRangeError::new(
            rt, msg, None,
        )));
    }
    let mut frame = unwrap_unchecked(frame);
    (*frame).code_block = Some(code);
    (*frame).this = this;
    (*frame).env = scope;
    (*frame).ctor = ctor;
    (*frame).exit_on_return = true;
    (*frame).ip = ip;

    'interp: loop {
        let result = eval(rt, frame);
        match result {
            Ok(value) => return Ok(value),
            Err(e) => {
                rt.stacktrace = rt.stacktrace();
                loop {
                    if let Some((env, ip, sp)) = (*frame).try_stack.pop() {
                        (*frame).env = env.unwrap();
                        (*frame).ip = ip;
                        (*frame).sp = sp;
                        (*frame).push(e);
                        continue 'interp;
                    } else if !(*frame).exit_on_return {
                        frame = (*frame).prev;
                        rt.stack.pop_frame().unwrap();
                        continue;
                    }
                    break;
                }

                return Err(e);
            }
        }
    }
}

pub unsafe fn eval(rt: &mut Runtime, frame: *mut CallFrame) -> Result<JsValue, JsValue> {
    rt.heap().collect_if_necessary();
    let mut ip = (*frame).ip;

    let mut frame: &'static mut CallFrame = &mut *frame;
    let stack = &mut rt.stack as *mut Stack;
    let stack = &mut *stack;
    let gcstack = rt.shadowstack();
    loop {
        let opcode = ip.cast::<Opcode>().read_unaligned();
        ip = ip.add(1);
        #[cfg(feature = "perf")]
        {
            rt.perf.get_perf(opcode as u8);
        }
        /*println!(
            "exec block({:p}): {}: {:?}",
            unwrap_unchecked(frame.code_block),
            ip.sub(1).offset_from(&frame.code_block.unwrap().code[0]),
            opcode
        );*/
        stack.cursor = frame.sp;
        match opcode {
            Opcode::OP_GE0GL => {
                let index = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let env = frame.env;
                debug_assert!(
                    index < env.as_slice().len() as u32,
                    "invalid var index '{}' at pc: {}",
                    index,
                    ip as usize - &unwrap_unchecked(frame.code_block).code[0] as *const u8 as usize
                );

                frame.push(env.as_slice().get_unchecked(index as usize).value);
            }
            Opcode::OP_GE0SL => {
                let index = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let mut env = frame.env;
                debug_assert!(index < env.as_slice_mut().len() as u32);
                let val = frame.pop();
                if unlikely(!env.as_slice_mut()[index as usize].mutable) {
                    return Err(JsValue::new(
                        rt.new_type_error(format!("Cannot assign to immutable variable")),
                    ));
                }

                env.as_slice_mut().get_unchecked_mut(index as usize).value = val;
            }
            Opcode::OP_GET_LOCAL => {
                let index = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let env = frame.pop().get_object().downcast::<Environment>().unwrap();
                debug_assert!(
                    index < env.as_slice().len() as u32,
                    "invalid var index '{}' at pc: {}",
                    index,
                    ip as usize - &unwrap_unchecked(frame.code_block).code[0] as *const u8 as usize
                );

                frame.push(env.as_slice().get_unchecked(index as usize).value);
            }
            Opcode::OP_SET_LOCAL => {
                let index = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let mut env = frame.pop().get_object().downcast::<Environment>().unwrap();
                debug_assert!(index < env.as_slice_mut().len() as u32);
                let val = frame.pop();
                if unlikely(!env.as_slice_mut()[index as usize].mutable) {
                    return Err(JsValue::new(
                        rt.new_type_error(format!("Cannot assign to immutable variable")),
                    ));
                }

                env.as_slice_mut().get_unchecked_mut(index as usize).value = val;
            }
            Opcode::OP_GET_ENV => {
                let mut depth = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let mut env = frame.env;

                while depth != 0 {
                    env = unwrap_unchecked(env.parent);
                    depth -= 1;
                }

                frame.push(JsValue::new(env));
            }

            Opcode::OP_JMP => {
                rt.heap().collect_if_necessary();
                let offset = ip.cast::<i32>().read();
                ip = ip.add(4);
                ip = ip.offset(offset as isize);
            }
            Opcode::OP_JMP_IF_FALSE => {
                let offset = ip.cast::<i32>().read();
                ip = ip.add(4);
                let value = frame.pop();
                if !value.to_boolean() {
                    ip = ip.offset(offset as _);
                }
            }
            Opcode::OP_JMP_IF_TRUE => {
                let offset = ip.cast::<i32>().read();
                ip = ip.add(4);
                let value = frame.pop();
                if value.to_boolean() {
                    ip = ip.offset(offset as _);
                }
            }

            Opcode::OP_POP => {
                frame.pop();
            }
            Opcode::OP_PUSH_TRUE => {
                frame.push(JsValue::encode_bool_value(true));
            }
            Opcode::OP_PUSH_FALSE => {
                frame.push(JsValue::encode_bool_value(false));
            }
            Opcode::OP_PUSH_LITERAL => {
                let ix = ip.cast::<u32>().read();
                ip = ip.add(4);
                let constant = unwrap_unchecked(frame.code_block).literals[ix as usize];
                //assert!(constant.is_jsstring());
                frame.push(constant);
            }
            Opcode::OP_PUSH_THIS => {
                frame.push(frame.this);
            }
            Opcode::OP_PUSH_INT => {
                let int = ip.cast::<i32>().read();

                ip = ip.add(4);
                frame.push(JsValue::encode_int32(int));
            }
            Opcode::OP_PUSH_NAN => {
                frame.push(JsValue::encode_nan_value());
            }
            Opcode::OP_PUSH_NULL => {
                frame.push(JsValue::encode_null_value());
            }
            Opcode::OP_RET => {
                let mut value = if frame.sp <= frame.limit {
                    JsValue::encode_undefined_value()
                } else {
                    frame.pop()
                };

                if frame.ctor && !value.is_jsobject() {
                    value = frame.this;
                }
                let prev = rt.stack.pop_frame().unwrap();
                if prev.exit_on_return || prev.prev.is_null() {
                    return Ok(value);
                }
                frame = &mut *prev.prev;
                ip = frame.ip;

                frame.push(value);
            }
            Opcode::OP_ADD => {
                let profile = &mut *ip.cast::<ArithProfile>();
                ip = ip.add(4);

                let lhs = frame.pop();
                let rhs = frame.pop();
                profile.observe_lhs_and_rhs(lhs, rhs);
                if likely(lhs.is_int32() && rhs.is_int32()) {
                    if let Some(val) = lhs.get_int32().checked_add(rhs.get_int32()) {
                        frame.push(JsValue::encode_int32(val));
                        continue;
                    }
                    profile.set_observed_int32_overflow();
                }
                if likely(lhs.is_number() && rhs.is_number()) {
                    let result = JsValue::new(lhs.get_number() + rhs.get_number());

                    frame.push(result);
                    continue;
                }
                #[cold]
                unsafe fn add_slowpath(
                    rt: &mut Runtime,
                    frame: &mut CallFrame,
                    lhs: JsValue,
                    rhs: JsValue,
                ) -> Result<(), JsValue> {
                    let lhs = lhs.to_primitive(rt, JsHint::None)?;
                    let rhs = rhs.to_primitive(rt, JsHint::None)?;

                    if lhs.is_jsstring() || rhs.is_jsstring() {
                        #[inline(never)]
                        fn concat(
                            rt: &mut Runtime,
                            lhs: JsValue,
                            rhs: JsValue,
                        ) -> Result<JsValue, JsValue> {
                            let lhs = lhs.to_string(rt)?;
                            let rhs = rhs.to_string(rt)?;
                            let string = format!("{}{}", lhs, rhs);
                            Ok(JsValue::encode_object_value(JsString::new(rt, string)))
                        }

                        let result = concat(rt, lhs, rhs)?;
                        frame.push(result);
                    } else {
                        let lhs = lhs.to_number(rt)?;
                        let rhs = rhs.to_number(rt)?;
                        frame.push(JsValue::new(lhs + rhs));
                    }
                    Ok(())
                }
                add_slowpath(rt, frame, lhs, rhs)?;
            }
            Opcode::OP_SUB => {
                let profile = &mut *ip.cast::<ArithProfile>();

                ip = ip.offset(4);

                let lhs = frame.pop();
                let rhs = frame.pop();

                profile.observe_lhs_and_rhs(lhs, rhs);
                if likely(lhs.is_int32() && rhs.is_int32()) {
                    let result = lhs.get_int32().checked_sub(rhs.get_int32());
                    if likely(result.is_some()) {
                        frame.push(JsValue::encode_int32(result.unwrap()));
                        continue;
                    }
                    profile.set_observed_int32_overflow();
                }
                if likely(lhs.is_number() && rhs.is_number()) {
                    //profile.lhs_saw_number();
                    //profile.rhs_saw_number();
                    frame.push(JsValue::new(lhs.get_number() - rhs.get_number()));

                    continue;
                }
                // profile.observe_lhs_and_rhs(lhs, rhs);
                let lhs = lhs.to_number(rt)?;
                let rhs = rhs.to_number(rt)?;
                frame.push(JsValue::new(lhs - rhs));
            }
            Opcode::OP_DIV => {
                let profile = &mut *ip.cast::<ArithProfile>();
                ip = ip.add(4);

                let lhs = frame.pop();
                let rhs = frame.pop();
                profile.observe_lhs_and_rhs(lhs, rhs);
                if likely(lhs.is_number() && rhs.is_number()) {
                    //    profile.lhs_saw_number();
                    //    profile.rhs_saw_number();
                    frame.push(JsValue::new(lhs.get_number() / rhs.get_number()));
                    continue;
                }
                //profile.observe_lhs_and_rhs(lhs, rhs);
                let lhs = lhs.to_number(rt)?;
                let rhs = rhs.to_number(rt)?;
                frame.push(JsValue::new(lhs / rhs));
            }
            Opcode::OP_MUL => {
                let profile = &mut *ip.cast::<ArithProfile>();
                ip = ip.add(4);

                let lhs = frame.pop();
                let rhs = frame.pop();
                profile.observe_lhs_and_rhs(lhs, rhs);
                if likely(lhs.is_int32() && rhs.is_int32()) {
                    let result = lhs.get_int32().checked_mul(rhs.get_int32());
                    if likely(result.is_some()) {
                        frame.push(JsValue::encode_int32(result.unwrap()));
                        continue;
                    }
                    profile.set_observed_int32_overflow();
                }
                if likely(lhs.is_number() && rhs.is_number()) {
                    //  profile.lhs_saw_number();
                    //  profile.rhs_saw_number();

                    frame.push(JsValue::new(lhs.get_number() * rhs.get_number()));
                    continue;
                }
                //profile.observe_lhs_and_rhs(lhs, rhs);
                let lhs = lhs.to_number(rt)?;
                let rhs = rhs.to_number(rt)?;
                frame.push(JsValue::new(lhs * rhs));
            }
            Opcode::OP_REM => {
                let profile = &mut *ip.cast::<ArithProfile>();
                ip = ip.add(4);

                let lhs = frame.pop();
                let rhs = frame.pop();
                profile.observe_lhs_and_rhs(lhs, rhs);
                if likely(lhs.is_number() && rhs.is_number()) {
                    //  profile.lhs_saw_number();
                    //  profile.rhs_saw_number();
                    frame.push(JsValue::new(lhs.get_number() % rhs.get_number()));
                    continue;
                }
                // profile.observe_lhs_and_rhs(lhs, rhs);
                let lhs = lhs.to_number(rt)?;
                let rhs = rhs.to_number(rt)?;
                frame.push(JsValue::new(lhs % rhs));
            }
            Opcode::OP_SHL => {
                let lhs = frame.pop();
                let rhs = frame.pop();

                let left = lhs.to_int32(rt)?;
                let right = rhs.to_uint32(rt)?;
                frame.push(JsValue::new((left << (right & 0x1f)) as f64));
            }
            Opcode::OP_SHR => {
                let lhs = frame.pop();
                let rhs = frame.pop();

                let left = lhs.to_int32(rt)?;
                let right = rhs.to_uint32(rt)?;
                frame.push(JsValue::new((left >> (right & 0x1f)) as f64));
            }

            Opcode::OP_USHR => {
                let lhs = frame.pop();
                let rhs = frame.pop();

                let left = lhs.to_uint32(rt)?;
                let right = rhs.to_uint32(rt)?;
                frame.push(JsValue::new((left >> (right & 0x1f)) as f64));
            }
            Opcode::OP_LESS => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                if likely(lhs.is_int32() && rhs.is_int32()) {
                    frame.push(JsValue::new(lhs.get_int32() < rhs.get_int32()));
                    continue;
                }
                frame.push(JsValue::encode_bool_value(
                    lhs.compare(rhs, true, rt)? == CMP_TRUE,
                ));
            }
            Opcode::OP_LESSEQ => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                if likely(lhs.is_int32() && rhs.is_int32()) {
                    frame.push(JsValue::new(lhs.get_int32() <= rhs.get_int32()));
                    continue;
                }
                frame.push(JsValue::encode_bool_value(
                    rhs.compare(lhs, false, rt)? == CMP_FALSE,
                ));
            }

            Opcode::OP_GREATER => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                if likely(lhs.is_int32() && rhs.is_int32()) {
                    frame.push(JsValue::new(lhs.get_int32() > rhs.get_int32()));
                    continue;
                }
                frame.push(JsValue::encode_bool_value(
                    rhs.compare(lhs, false, rt)? == CMP_TRUE,
                ));
            }
            Opcode::OP_GREATEREQ => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                if likely(lhs.is_int32() && rhs.is_int32()) {
                    frame.push(JsValue::new(lhs.get_int32() >= rhs.get_int32()));
                    continue;
                }
                frame.push(JsValue::encode_bool_value(
                    lhs.compare(rhs, true, rt)? == CMP_FALSE,
                ));
            }
            Opcode::OP_GET_BY_ID | Opcode::OP_TRY_GET_BY_ID => {
                let name = ip.cast::<u32>().read_unaligned();
                let name = *unwrap_unchecked(frame.code_block)
                    .names
                    .get_unchecked(name as usize);
                ip = ip.add(4);
                let fdbk = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let object = frame.pop();
                if likely(object.is_jsobject()) {
                    root!(obj = gcstack, object.get_jsobject());

                    if let TypeFeedBack::PropertyCache { structure, offset } =
                        unwrap_unchecked(frame.code_block)
                            .feedback
                            .get_unchecked(fdbk as usize)
                    {
                        if let Some(structure) = structure.upgrade() {
                            if GcPointer::ptr_eq(&structure, &obj.structure()) {
                                frame.push(*obj.direct(*offset as _));

                                continue;
                            }
                        }
                    }

                    #[inline(never)]
                    #[cold]
                    unsafe fn slow_get_by_id(
                        rt: &mut Runtime,
                        frame: &mut CallFrame,
                        obj: &mut GcPointer<JsObject>,
                        name: Symbol,
                        fdbk: u32,
                        is_try: bool,
                    ) -> Result<(), JsValue> {
                        let mut slot = Slot::new();
                        let found = obj.get_property_slot(rt, name, &mut slot);
                        if slot.is_load_cacheable() {
                            *unwrap_unchecked(frame.code_block)
                                .feedback
                                .get_unchecked_mut(fdbk as usize) = TypeFeedBack::PropertyCache {
                                structure: rt.heap().make_weak(
                                    slot.base()
                                        .unwrap()
                                        .downcast_unchecked::<JsObject>()
                                        .structure(),
                                ),

                                offset: slot.offset(),
                            }
                        }
                        if found {
                            frame.push(slot.get(rt, JsValue::new(*obj))?);
                        } else {
                            if unlikely(is_try) {
                                let desc = rt.description(name);
                                return Err(JsValue::new(rt.new_reference_error(format!(
                                    "Property '{}' not found",
                                    desc
                                ))));
                            }
                            frame.push(JsValue::encode_undefined_value());
                        }
                        Ok(())
                    }
                    slow_get_by_id(
                        rt,
                        frame,
                        &mut obj,
                        name,
                        fdbk,
                        opcode == Opcode::OP_TRY_GET_BY_ID,
                    )?;
                    continue;
                }

                frame.push(get_by_id_slow(rt, name, object)?)
            }
            Opcode::OP_PUT_BY_ID => {
                let name = ip.cast::<u32>().read_unaligned();
                let name = *unwrap_unchecked(frame.code_block)
                    .names
                    .get_unchecked(name as usize);
                ip = ip.add(4);
                let fdbk = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);

                let object = frame.pop();
                let value = frame.pop();
                if likely(object.is_jsobject()) {
                    let mut obj = object.get_jsobject();
                    'exit: loop {
                        'slowpath: loop {
                            match unwrap_unchecked(frame.code_block).feedback[fdbk as usize] {
                                TypeFeedBack::PutByIdFeedBack {
                                    ref new_structure,
                                    ref old_structure,
                                    ref offset,
                                    ref structure_chain,
                                } => {
                                    if Some(obj.structure()) != *old_structure {
                                        break 'slowpath;
                                    }
                                    if let None = new_structure {
                                        *obj.direct_mut(*offset as usize) = value;
                                        break 'exit;
                                    }
                                    let vector = &structure_chain.unwrap().vector;
                                    let mut i = 0;

                                    let mut cur = old_structure.unwrap().prototype;
                                    while let Some(proto) = cur {
                                        let structure = proto.structure();
                                        if !GcPointer::ptr_eq(&structure, &vector[i]) {
                                            break 'slowpath;
                                        }
                                        i += 1;
                                        cur = structure.prototype;
                                    }

                                    *obj.direct_mut(*offset as usize) = value;
                                    break 'exit;
                                }
                                TypeFeedBack::None => {
                                    break 'slowpath;
                                }
                                _ => unreachable!(),
                            }
                        }

                        put_by_id_slow(rt, frame, &mut obj, name, value, fdbk)?;
                        break 'exit;
                    }
                    continue;
                }
            }

            Opcode::OP_CALL => {
                rt.heap().collect_if_necessary();
                let argc = ip.cast::<u32>().read();
                ip = ip.add(4);

                let args_start = frame.sp.sub(argc as _);
                frame.sp = args_start;
                let mut func = frame.pop();
                let mut this = frame.pop();
                let mut args = std::slice::from_raw_parts_mut(args_start, argc as _);
                if !func.is_callable() {
                    let msg = JsString::new(rt, "not a callable object");
                    return Err(JsValue::encode_object_value(JsTypeError::new(
                        rt, msg, None,
                    )));
                }
                root!(func_object = gcstack, func.get_jsobject());
                root!(funcc = gcstack, *&*func_object);
                let func = func_object.as_function_mut();
                root!(args_ = gcstack, Arguments::new(this, &mut args));

                frame.ip = ip;
                stack.cursor = frame.sp;

                if func.is_vm() {
                    let vm_fn = func.as_vm_mut();
                    let scope = JsValue::new(vm_fn.scope);
                    let (this, scope) = rt.setup_for_vm_call(vm_fn, scope, &args_)?;
                    let mut exit = false;
                    if false && opcode == Opcode::OP_TAILCALL {
                        exit = rt.stack.pop_frame().unwrap().exit_on_return;
                    }
                    let cframe = rt.stack.new_frame(0, JsValue::new(*funcc), scope);
                    if unlikely(cframe.is_none()) {
                        let msg = JsString::new(rt, "stack overflow");
                        return Err(JsValue::encode_object_value(JsRangeError::new(
                            rt, msg, None,
                        )));
                    }

                    //rt.stack.cursor = frame.sp;
                    let cframe = unwrap_unchecked(cframe);
                    (*cframe).code_block = Some(vm_fn.code);
                    (*cframe).this = this;

                    (*cframe).ctor = false;
                    (*cframe).exit_on_return = exit;
                    //(*cframe).limit = args_start;
                    (*cframe).sp = args_start.add(argc as _);
                    (*cframe).ip = &vm_fn.code.code[0] as *const u8 as *mut u8;
                    //frame.sp = args_start;

                    frame = &mut *cframe;

                    ip = (*cframe).ip;
                } else {
                    let result = func.call(rt, &mut args_, JsValue::new(*funcc))?;
                    // frame.sp = args_start;
                    frame.push(result);
                }
            }
            Opcode::OP_NEW => {
                rt.heap().collect_if_necessary();
                let argc = ip.cast::<u32>().read();
                ip = ip.add(4);

                let args_start = frame.sp.sub(argc as _);
                frame.sp = args_start;
                let mut func = frame.pop();
                let mut _this = frame.pop();
                let mut args = std::slice::from_raw_parts_mut(args_start, argc as _);

                if unlikely(!func.is_callable()) {
                    let msg = JsString::new(rt, "not a callable object");
                    return Err(JsValue::encode_object_value(JsTypeError::new(
                        rt, msg, None,
                    )));
                }

                root!(func_object = gcstack, func.get_jsobject());
                root!(funcc = gcstack, func.get_jsobject());
                let map = func_object.func_construct_map(rt)?;
                let func = func_object.as_function_mut();
                let object = JsObject::new(rt, &map, JsObject::get_class(), ObjectTag::Ordinary);
                root!(
                    args_ = gcstack,
                    Arguments::new(JsValue::new(object), &mut args)
                );

                args_.ctor_call = true;

                frame.ip = ip;

                frame.sp = args_start;
                if func.is_vm() {
                    let vm_fn = func.as_vm_mut();
                    let scope = JsValue::new(vm_fn.scope);
                    let (this, scope) = rt.setup_for_vm_call(vm_fn, scope, &args_)?;
                    let mut exit = false;
                    if false && opcode == Opcode::OP_TAILNEW {
                        exit = stack.pop_frame().unwrap().exit_on_return;
                    }
                    let cframe = rt.stack.new_frame(0, JsValue::new(*funcc), scope);
                    if unlikely(cframe.is_none()) {
                        let msg = JsString::new(rt, "stack overflow");
                        return Err(JsValue::encode_object_value(JsRangeError::new(
                            rt, msg, None,
                        )));
                    }

                    let cframe = unwrap_unchecked(cframe);
                    (*cframe).code_block = Some(vm_fn.code);
                    (*cframe).this = this;
                    //(*cframe).env = Some(scope);
                    (*cframe).ctor = true;
                    //(*cframe).limit = args_start;
                    (*cframe).sp = args_start.add(argc as _);
                    (*cframe).exit_on_return = exit;
                    (*cframe).ip = &vm_fn.code.code[0] as *const u8 as *mut u8;
                    frame = &mut *cframe;
                    ip = (*cframe).ip;
                } else {
                    let result = func.call(rt, &mut args_, JsValue::new(*funcc))?;

                    frame.push(result);
                }
                /*let result = func.construct(rt, &mut args_, Some(map))?;

                frame.push(result);*/
            }

            Opcode::OP_DUP => {
                let v1 = frame.pop();
                frame.push(v1);
                frame.push(v1);
            }
            Opcode::OP_SWAP => {
                let v1 = frame.pop();
                let v2 = frame.pop();
                frame.push(v1);
                frame.push(v2);
            }
            Opcode::OP_NEG => {
                let v1 = frame.pop();
                if v1.is_number() {
                    frame.push(JsValue::new(-v1.get_number()));
                } else {
                    let n = v1.to_number(rt)?;
                    frame.push(JsValue::new(-n));
                }
            }

            Opcode::OP_EQ => {
                let lhs = frame.pop();
                let rhs = frame.pop();

                frame.push(JsValue::encode_bool_value(lhs.abstract_equal(rhs, rt)?));
            }
            Opcode::OP_STRICTEQ => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                frame.push(JsValue::encode_bool_value(lhs.strict_equal(rhs)));
            }
            Opcode::OP_NEQ => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                frame.push(JsValue::encode_bool_value(!lhs.abstract_equal(rhs, rt)?));
            }
            Opcode::OP_NSTRICTEQ => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                frame.push(JsValue::encode_bool_value(!lhs.strict_equal(rhs)));
            }
            Opcode::OP_PUT_BY_VAL => {
                let profile = &mut *ip.cast::<ByValProfile>();
                ip = ip.add(4);
                let object = frame.pop();
                let key = frame.pop();
                profile.observe_key_and_object(key, object);
                let key = key.to_symbol(rt)?;
                let value = frame.pop();
                if likely(object.is_jsobject()) {
                    let mut obj = object.get_jsobject();
                    obj.put(rt, key, value, unwrap_unchecked(frame.code_block).strict)?;
                }
            }
            Opcode::OP_GET_BY_VAL => {
                let profile = &mut *ip.cast::<ByValProfile>();
                ip = ip.add(4);

                let object = frame.pop();
                let key = frame.pop();
                profile.observe_key_and_object(key, object);
                let key = key.to_symbol(rt)?;
                let mut slot = Slot::new();
                let value = object.get_slot(rt, key, &mut slot)?;

                frame.push(value);
            }
            Opcode::OP_INSTANCEOF => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                if unlikely(!rhs.is_jsobject()) {
                    let msg = JsString::new(rt, "'instanceof' requires object");
                    return Err(JsValue::encode_object_value(JsTypeError::new(
                        rt, msg, None,
                    )));
                }

                root!(robj = gcstack, rhs.get_jsobject());
                root!(robj2 = gcstack, *robj);
                if unlikely(!robj.is_callable()) {
                    let msg = JsString::new(rt, "'instanceof' requires constructor");
                    return Err(JsValue::encode_object_value(JsTypeError::new(
                        rt, msg, None,
                    )));
                }

                frame.push(JsValue::encode_bool_value(
                    robj.as_function().has_instance(&mut robj2, rt, lhs)?,
                ));
            }
            Opcode::OP_IN => {
                let lhs = frame.pop();
                let rhs = frame.pop();
                if unlikely(!rhs.is_jsobject()) {
                    let msg = JsString::new(rt, "'in' requires object");
                    return Err(JsValue::encode_object_value(JsTypeError::new(
                        rt, msg, None,
                    )));
                }
                let sym = lhs.to_symbol(rt)?;
                frame.push(JsValue::encode_bool_value(
                    rhs.get_jsobject().has_own_property(rt, sym),
                ));
            }
            Opcode::OP_FORIN_SETUP => {
                let offset = ip.cast::<i32>().read_unaligned();
                ip = ip.add(4);
                let enumerable = frame.pop();

                if enumerable.is_null() || enumerable.is_undefined() {
                    ip = ip.offset(offset as _);
                    continue;
                }

                let it = if enumerable.is_jsstring() {
                    NativeIterator::new(rt, enumerable.get_object())
                } else {
                    let obj = enumerable.to_object(rt)?;
                    NativeIterator::new(rt, obj.as_dyn())
                };
                frame.push(JsValue::new(it));
                assert!(ip.cast::<Opcode>().read_unaligned() == Opcode::OP_FORIN_ENUMERATE);
            }
            Opcode::OP_FORIN_ENUMERATE => {
                let offset = ip.cast::<i32>().read_unaligned();
                ip = ip.add(4);
                let mut it = frame
                    .pop()
                    .get_object()
                    .downcast_unchecked::<NativeIterator>();
                frame.push(JsValue::new(it));
                if let Some(sym) = it.next() {
                    let desc = rt.description(sym);
                    frame.push(JsValue::new(JsString::new(rt, desc)));
                } else {
                    ip = ip.offset(offset as _);
                }
            }
            Opcode::OP_FORIN_LEAVE => {
                frame.pop();
            }

            Opcode::OP_THROW => {
                let val = frame.pop();
                return Err(val);
            }

            Opcode::OP_GLOBALTHIS => {
                let global = rt.global_object();
                frame.push(JsValue::encode_object_value(global));
            }

            Opcode::OP_NEWOBJECT => {
                let obj = JsObject::new_empty(rt);
                frame.push(JsValue::encode_object_value(obj));
            }

            Opcode::OP_PUSH_CATCH => {
                let offset = ip.cast::<i32>().read();
                ip = ip.add(4);
                let env = frame.env;

                frame
                    .try_stack
                    .push((Some(env), ip.offset(offset as isize), frame.sp));
            }
            Opcode::OP_POP_CATCH => {
                frame.try_stack.pop();
            }

            Opcode::OP_LOGICAL_NOT => {
                let val = frame.pop();
                frame.push(JsValue::encode_bool_value(!val.to_boolean()));
            }
            Opcode::OP_NOT => {
                let v1 = frame.pop();
                if v1.is_number() {
                    let n = v1.get_number() as i32;
                    frame.push(JsValue::new((!n) as i32));
                } else {
                    let n = v1.to_number(rt)? as i32;
                    frame.push(JsValue::new((!n) as i32));
                }
            }
            Opcode::OP_POS => {
                let value = frame.pop();
                if value.is_number() {
                    frame.push(value);
                }
                let x = value.to_number(rt)?;
                frame.push(JsValue::new(x));
            }

            Opcode::OP_DECL_CONST => {
                let ix = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let mut env = frame.env;
                let val = frame.pop();
                env.as_slice_mut()[ix as usize] = Variable {
                    value: val,
                    mutable: false,
                };
            }
            Opcode::OP_DECL_LET => {
                let ix = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let mut env = frame.env;
                let val = frame.pop();
                env.as_slice_mut()[ix as usize] = Variable {
                    value: val,
                    mutable: true,
                };
            }

            Opcode::OP_DELETE_BY_ID => {
                let name = ip.cast::<u32>().read();
                ip = ip.add(4);
                let name = unwrap_unchecked(frame.code_block).names[name as usize];
                let object = frame.pop();
                object.check_object_coercible(rt)?;
                root!(object = gcstack, object.to_object(rt)?);
                frame.push(JsValue::new(object.delete(
                    rt,
                    name,
                    unwrap_unchecked(frame.code_block).strict,
                )?));
            }
            Opcode::OP_DELETE_BY_VAL => {
                let object = frame.pop();
                let name = frame.pop().to_symbol(rt)?;
                object.check_object_coercible(rt)?;
                root!(object = gcstack, object.to_object(rt)?);
                frame.push(JsValue::new(object.delete(
                    rt,
                    name,
                    unwrap_unchecked(frame.code_block).strict,
                )?));
            }
            Opcode::OP_GET_FUNCTION => {
                //vm.space().defer_gc();
                let ix = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let func = JsVMFunction::new(
                    rt,
                    unwrap_unchecked(frame.code_block).codes[ix as usize],
                    frame.env,
                );

                frame.push(JsValue::encode_object_value(func));
                // vm.space().undefer_gc();
            }

            Opcode::OP_PUSH_UNDEF => {
                frame.push(JsValue::encode_undefined_value());
            }
            Opcode::OP_NEWARRAY => {
                let count = ip.cast::<u32>().read_unaligned();

                ip = ip.add(4);
                root!(arr = gcstack, JsArray::new(rt, count));
                let mut index = 0;
                while index < count {
                    let value = frame.pop();
                    if unlikely(value.is_object() && value.get_object().is::<SpreadValue>()) {
                        root!(
                            spread = gcstack,
                            value.get_object().downcast_unchecked::<SpreadValue>()
                        );
                        for i in 0..spread.array.get(rt, "length".intern())?.get_number() as usize {
                            let real_arg = spread.array.get(rt, Symbol::Index(i as _))?;
                            arr.put(rt, Symbol::Index(index), real_arg, false)?;
                            index += 1;
                        }
                    } else {
                        arr.put(rt, Symbol::Index(index), value, false)?;
                        index += 1;
                    }
                }
                frame.push(JsValue::encode_object_value(*arr));
            }

            Opcode::OP_CALL_BUILTIN => {
                rt.heap().collect_if_necessary();
                let argc = ip.cast::<u32>().read();
                ip = ip.add(4);
                let builtin_id = ip.cast::<u32>().read();
                ip = ip.add(4);
                let effect = ip.cast::<u32>().read();
                ip = ip.add(4);
                super::builtins::BUILTINS[builtin_id as usize](
                    rt,
                    frame,
                    &mut ip,
                    argc,
                    effect as _,
                )?;
            }
            Opcode::OP_SPREAD => {
                /*
                    This opcode creates internal interpreter only value that is used to indicate that some argument is spread value
                    and if interpreter sees it then it tried to use `array` value from `SpreadValue`.
                    User code can't get access to this value, if it does this should be reported.

                */
                let value = frame.pop();
                let spread = SpreadValue::new(rt, value)?;
                frame.push(JsValue::encode_object_value(spread));
            }
            Opcode::OP_TYPEOF => {
                let val = frame.pop();
                let str = JsString::new(rt, val.type_of());
                frame.push(JsValue::new(str));
            }
            Opcode::OP_PUSH_ENV => {
                let count = ip.cast::<u32>().read_unaligned();
                ip = ip.add(4);
                let mut env = Environment::new(rt, count);
                env.parent = Some(frame.env);
                frame.env = env;
            }
            Opcode::OP_POP_ENV => {
                let env = frame.env;
                frame.env = env.parent.unwrap();
            }
            #[cfg(debug_assertions)]
            x => {
                panic!("NYI: {:?}", x);
            }
            #[cfg(not(debug_assertions))]
            _ => {
                std::hint::unreachable_unchecked();
            }
        }
    }
}

/// Type used internally in JIT/interpreter to represent spread result.
pub struct SpreadValue {
    pub(crate) array: GcPointer<JsObject>,
}

impl SpreadValue {
    pub fn new(rt: &mut Runtime, value: JsValue) -> Result<GcPointer<Self>, JsValue> {
        unsafe {
            if value.is_jsobject() {
                if value.get_object().downcast_unchecked::<JsObject>().tag() == ObjectTag::Array {
                    return Ok(rt.heap().allocate(Self {
                        array: value.get_object().downcast_unchecked(),
                    }));
                }
            }

            let msg = JsString::new(rt, "cannot create spread from non-array value");
            Err(JsValue::encode_object_value(JsTypeError::new(
                rt, msg, None,
            )))
        }
    }
}

impl GcCell for SpreadValue {
    fn deser_pair(&self) -> (usize, usize) {
        (Self::deserialize as _, Self::allocate as _)
    }
    vtable_impl!();
}
unsafe impl Trace for SpreadValue {
    fn trace(&mut self, visitor: &mut dyn Tracer) {
        self.array.trace(visitor);
    }
}

pub fn get_by_id_slow(rt: &mut Runtime, name: Symbol, val: JsValue) -> Result<JsValue, JsValue> {
    let mut slot = Slot::new();
    val.get_slot(rt, name, &mut slot)
}

unsafe fn put_by_id_slow(
    rt: &mut Runtime,
    frame: &mut CallFrame,
    obj: &mut GcPointer<JsObject>,
    name: Symbol,
    value: JsValue,
    fdbk: u32,
) -> Result<(), JsValue> {
    let mut slot = Slot::new();
    let old_structure = obj.structure();
    obj.put_slot(
        rt,
        name,
        value,
        &mut slot,
        unwrap_unchecked(frame.code_block).strict,
    )?;

    if slot.is_put_cacheable() && slot.base.is_some() {
        let mut base_cell = *obj;
        let mut new_structure = base_cell.structure();
        let mut m_old_structure = None;
        let mut m_offset = 0;
        let mut m_new_structure = None;
        let mut m_new_chain = None;

        if GcPointer::ptr_eq(&base_cell, &slot.base.unwrap()) {
            if slot.put_result_type() == PutResultType::New {
                return Ok(());
                // TODO
                if !new_structure.is_unique()
                    && new_structure
                        .previous
                        .map(|x| new_structure.storage_capacity() == x.storage_capacity())
                        .unwrap_or(false)
                {
                    assert!(GcPointer::ptr_eq(
                        &new_structure.previous.unwrap(),
                        &old_structure
                    ));
                    if new_structure
                        .previous
                        .map(|x| GcPointer::ptr_eq(&old_structure, &x))
                        .unwrap_or(false)
                    {
                        let (result, saw_poly_proto) =
                            crate::vm::operations::normalize_prototype_chain(rt, &base_cell);

                        if result != usize::MAX && !saw_poly_proto {
                            m_old_structure = Some(old_structure);
                            m_offset = slot.offset();
                            m_new_structure = Some(new_structure);
                            m_new_chain = Some(new_structure.prototype_chain(rt, base_cell));
                        }
                    }
                }
            } else {
                m_old_structure = Some(new_structure);
                m_offset = slot.offset();
            }

            unwrap_unchecked(frame.code_block).feedback[fdbk as usize] =
                TypeFeedBack::PutByIdFeedBack {
                    new_structure: m_new_structure,
                    old_structure: m_old_structure,
                    offset: m_offset,
                    structure_chain: m_new_chain,
                };
            assert!(!matches!(
                unwrap_unchecked(frame.code_block).feedback[fdbk as usize],
                TypeFeedBack::None
            ));
        }
    }

    Ok(())
}
