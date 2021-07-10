use std::intrinsics::unlikely;
use std::mem::ManuallyDrop;

use crate::define_jsclass_with_symbol;
use crate::prelude::*;
use crate::vm::class::JsClass;
use crate::vm::object::TypedJsObject;
use crate::JsTryFrom;
pub struct JsWeakRef {
    value: WeakRef<JsObject>,
}

extern "C" fn fsz() -> usize {
    std::mem::size_of::<JsWeakRef>()
}

extern "C" fn ser(_: &JsObject, _: &mut SnapshotSerializer) {
    todo!()
}

extern "C" fn deser(_: &mut JsObject, _: &mut Deserializer, _: &mut Runtime) {
    todo!()
}
#[allow(improper_ctypes_definitions)]
extern "C" fn trace(tracer: &mut dyn Tracer, obj: &mut JsObject) {
    obj.data::<JsWeakRef>().value.trace(tracer);
}

impl JsWeakRef {
    define_jsclass_with_symbol!(
        JsObject,
        WeakRef,
        Object,
        None,
        Some(trace),
        Some(deser),
        Some(ser),
        Some(fsz)
    );
}

pub fn weak_ref_constructor(rt: &mut Runtime, args: &Arguments) -> Result<JsValue, JsValue> {
    let target = args.at(0);
    if unlikely(!target.is_jsobject()) {
        return Err(JsValue::new(
            rt.new_type_error("WeakRef: Target must be an object"),
        ));
    }
    let target = target.get_jsobject();
    let map = rt.global_data().weak_ref_structure.unwrap();
    let mut weak_ref = JsObject::new(rt, &map, JsWeakRef::get_class(), ObjectTag::Ordinary);
    *weak_ref.data::<JsWeakRef>() = ManuallyDrop::new(JsWeakRef {
        value: rt.gc.make_weak(target),
    });
    Ok(JsValue::new(weak_ref))
}

pub fn weak_ref_prototype_deref(rt: &mut Runtime, args: &Arguments) -> Result<JsValue, JsValue> {
    let weak_ref = TypedJsObject::<JsWeakRef>::try_from(rt, args.this)?;
    match weak_ref.value.upgrade() {
        Some(value) => Ok(JsValue::new(value)),
        None => Ok(JsValue::encode_undefined_value()),
    }
}

impl JsClass for JsWeakRef {
    fn class() -> &'static Class {
        Self::get_class()
    }
}