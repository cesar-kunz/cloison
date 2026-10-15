//! `Date`: a time value (milliseconds since the epoch, an `f64`) kept in
//! the object's `code` bits (see [`date_value`]). Installed by [`install`].

use crate::object::{Class, as_object, object_value};
use crate::value::Value;
use crate::vm::{Code, Error, NativeFn, Result, Vm};

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let dp = vm.date_prototype;
    let any = crate::jit::Ty::Any;
    let c = vm.define_native(g, "Date", date_constructor, 7, any, true);
    let (a_p, a_c) = (vm.a_prototype, vm.a_constructor);
    vm.add_property_with(c, a_p, object_value(dp), crate::object::READ_ONLY | crate::object::DONT_ENUM | crate::object::DONT_DELETE);
    vm.add_property_with(dp, a_c, object_value(c), crate::object::DONT_ENUM);
    vm.define_native(c, "now", date_now, 0, any, false);
    vm.define_native(dp, "getTime", date_get_time as NativeFn, 0, any, false);
    vm.define_native(dp, "valueOf", date_get_time, 0, any, false);
}

/// Milliseconds since the epoch, now.
pub(crate) fn now_ms() -> f64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    t.map_or(0.0, |d| d.as_millis() as f64)
}

/// `new Date()` (now) or `new Date(ms)`. (Called without `new`, it should
/// give a string; it gives the object.)
fn date_constructor(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value> {
    let t = match args.first() {
        Some(v) if v.is_number() => v.as_number().unwrap().trunc(),
        _ => now_ms(),
    };
    let dp = object_value(vm.date_prototype);
    let o = vm.new_object_with(Class::Date, dp, 0);
    unsafe { (*o).code = t.to_bits() as *const Code };
    Ok(object_value(o))
}

/// `Date.now()`.
fn date_now(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::number(now_ms()))
}

/// The time value of `v` if it is a `Date`.
pub(crate) fn date_value(v: Value) -> Option<f64> {
    let o = v.is_cell().then(|| as_object(v))?;
    unsafe { ((*o).class == Class::Date).then(|| f64::from_bits((*o).code as u64)) }
}

/// `Date.prototype.getTime()` and `valueOf()`.
fn date_get_time(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    match date_value(this) {
        Some(t) => Ok(Value::number(t)),
        None => Err(Error::type_error("this is not a Date object")),
    }
}

