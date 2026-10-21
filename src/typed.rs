//! `ArrayBuffer` and the typed arrays (`%TypedArray%` and its kinds), as
//! the spec has them with resizable buffers (ES2024) and `transfer`.
//!
//! A buffer is an object of class `ArrayBuffer`: `elems` points to its
//! bytes, `elems_len` is their count (0 once detached, flag `DETACHED`).
//! A resizable one (flag [`RESIZABLE`]) has room for its maximum byte
//! length (`elems_cap`) from the start, so its bytes never move, and
//! `code` holds a hidden weak set of the views on it (typed arrays and
//! `DataView`s; `collections::new_weak_set`), which a resize brings up to
//! date.
//!
//! A typed array (class `TypedArray`) has `elems` pointing to its first
//! element in its buffer's bytes, `elems_len` its length (what compiled
//! code bounds element accesses by: 0 once out of bounds), `kind` its
//! element type, `code` its buffer, and `elems_cap` the length it was
//! made with, or [`TRACKING`] if it follows its resizable buffer's length.
//! A `DataView` is laid out the same way, in bytes.
//!
//! An immutable buffer (ES2026: `transferToImmutable`,
//! `sliceToImmutable`) is a fixed-length one whose `code` is itself; its
//! views have flag `IMMUTABLE_VIEW`, and nothing writes through them. From
//! the first one on, compiled code leaves typed arrays' elements to the
//! runtime, as it does once a buffer has been detached
//! (`Vm::views_detached`).

use crate::builtins::{getter, is_class, link_constructor};
use crate::interp::{call_function, to_boolean, to_number};
use crate::object::{
    Class, DETACHED, DONT_DELETE, DONT_ENUM, IMMUTABLE_VIEW, Object, READ_ONLY, TypedKind, as_object, object_value,
};
use crate::props::{is_callable, is_object};
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, string_units};

/// A buffer's flag: resizable (`DETACHED`'s neighbour).
pub(crate) const RESIZABLE: u8 = 128;
/// A view's `elems_cap`: it tracks its buffer's length.
pub(crate) const TRACKING: u32 = u32::MAX;
/// The most bytes a buffer may have (a resizable one at its maximum).
pub(crate) const MAX_BYTES: usize = 1 << 31;
pub(crate) const MAX_RESIZABLE: usize = 1 << 30;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn str_value(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

/// ToNumber, with what its `valueOf` threw.
fn number(vm: &mut Vm, v: Value) -> Result<f64> {
    let d = to_number(v);
    if vm.conv_failed() { Err(vm.take_conv_error()) } else { Ok(d) }
}

/// ToIntegerOrInfinity.
pub(crate) fn integer(vm: &mut Vm, v: Value) -> Result<f64> {
    if v.is_int32() {
        return Ok(v.as_i32_unchecked() as f64);
    }
    let d = number(vm, v)?;
    Ok(if d.is_nan() { 0.0 } else { d.trunc() + 0.0 })
}

/// ToIndex.
pub(crate) fn to_index(vm: &mut Vm, v: Value) -> Result<usize> {
    let d = if v.is_undefined() { 0.0 } else { integer(vm, v)? };
    if !(0.0..=9007199254740991.0).contains(&d) {
        return Err(Error::range_error(format!("Invalid index: {d}")));
    }
    Ok(d as usize)
}

/// A relative index (`begin` and such: from the end if negative) clamped
/// to `0..=len`.
pub(crate) fn relative(d: f64, len: usize) -> usize {
    if d < 0.0 { (len as f64 + d).max(0.0) as usize } else { d.min(len as f64) as usize }
}

/// SpeciesConstructor(o, default).
pub(crate) fn species_constructor(vm: &mut Vm, o: Value, default: Value) -> Result<Value> {
    let a_constructor = vm.a_constructor;
    let c = vm.get(o, a_constructor)?;
    if c.is_undefined() {
        return Ok(default);
    }
    if !is_object(c) {
        return Err(Error::type_error("object.constructor is not an object"));
    }
    let a_species = vm.a_sym_species;
    let s = vm.get(c, a_species)?;
    if s.is_undefined() || s.is_null() {
        return Ok(default);
    }
    if crate::intrinsics::is_constructor(s) {
        return Ok(s);
    }
    Err(Error::type_error("object.constructor[Symbol.species] is not a constructor"))
}

// ---- Buffers ----

pub(crate) fn is_detached(b: *mut Object) -> bool {
    unsafe { (*b).flags & DETACHED != 0 }
}

pub(crate) fn is_resizable(b: *mut Object) -> bool {
    unsafe { (*b).flags & RESIZABLE != 0 }
}

/// Whether buffer `b` is immutable.
pub(crate) fn is_immutable(b: *mut Object) -> bool {
    !is_resizable(b) && unsafe { (*b).code } as *mut Object == b
}

/// Whether view `o` (a typed array or a `DataView`) is of an immutable
/// buffer.
pub(crate) fn is_immutable_view(o: *mut Object) -> bool {
    unsafe { (*o).flags & IMMUTABLE_VIEW != 0 }
}

/// A TypeError unless view `o` is of a mutable buffer (ValidateTypedArray
/// for a write, before anything else is read).
pub(crate) fn writable_view(o: *mut Object) -> Result<()> {
    if is_immutable_view(o) {
        return Err(Error::type_error("Cannot write to an immutable ArrayBuffer"));
    }
    Ok(())
}

/// Make fixed-length buffer `b` immutable: from now on compiled code takes
/// typed arrays' elements through the runtime (see the module docs).
fn make_immutable(vm: &mut Vm, b: *mut Object) {
    unsafe { (*b).code = b.cast() };
    if !vm.views_detached {
        vm.views_detached = true;
        vm.bump_code_epoch();
    }
}

/// A view's buffer.
pub(crate) fn buffer_of(o: *mut Object) -> *mut Object {
    unsafe { (*o).code as *mut Object }
}

/// Whether view `o`'s bytes are a shared buffer's, which other agents may
/// access meanwhile (see `racy`).
pub(crate) fn shares(o: *mut Object) -> bool {
    crate::shared::is_shared(buffer_of(o))
}

/// A new buffer of `n` zero bytes (resizable up to `max` bytes, if given).
pub(crate) fn new_buffer_max(vm: &mut Vm, n: usize, max: Option<usize>) -> Result<*mut Object> {
    let room = max.unwrap_or(n);
    if n > MAX_BYTES || room > if max.is_some() { MAX_RESIZABLE } else { MAX_BYTES } {
        return Err(Error::range_error("Array buffer allocation failed"));
    }
    let abp = object_value(vm.array_buffer_prototype);
    let o = vm.new_object_with(Class::ArrayBuffer, abp, 0);
    unsafe {
        (*o).elems = vm.heap.alloc_bytes(8 * room.div_ceil(8).max(1)).cast();
        (*o).elems_len = n as u32;
        if let Some(max) = max {
            (*o).flags |= RESIZABLE;
            (*o).elems_cap = max as u32;
            let views = crate::collections::new_weak_set(vm)?;
            (*o).code = object_value(views).to_bits() as *const crate::vm::Code;
        }
    }
    Ok(o)
}

/// A `Uint8Array` of `bytes`, on an immutable buffer (a bytes module's
/// value).
pub(crate) fn immutable_bytes(vm: &mut Vm, bytes: &[u8]) -> Result<Value> {
    let b = new_buffer_max(vm, bytes.len(), None)?;
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*b).elems.cast::<u8>(), bytes.len()) };
    make_immutable(vm, b);
    new_view(vm, TypedKind::U8, b, 0, Some(bytes.len()))
}

/// A new fixed-length buffer of `n` zero bytes.
pub(crate) fn new_buffer(vm: &mut Vm, n: usize) -> *mut Object {
    new_buffer_max(vm, n, None).expect("buffer too large")
}

/// Record view `v` of resizable buffer `b` (see the module's docs).
fn register(vm: &mut Vm, b: *mut Object, v: Value) -> Result<()> {
    if is_resizable(b) {
        let views = as_object(Value::from_bits(unsafe { (*b).code } as u64));
        crate::collections::weak_set_insert(vm, views, v)?;
    }
    Ok(())
}

/// The views of resizable buffer `b` brought up to date (a resize, a
/// detach).
pub(crate) fn refresh_views(b: *mut Object) {
    if !is_resizable(b) {
        return;
    }
    let views = as_object(Value::from_bits(unsafe { (*b).code } as u64));
    for v in crate::collections::weak_set_values(views) {
        let v = as_object(v);
        unsafe {
            (*v).elems_len = if (*v).class == Class::TypedArray {
                ta_length(v).unwrap_or(0) as u32
            } else {
                view_byte_length(v).unwrap_or(0) as u32
            };
        }
    }
}

/// DetachArrayBuffer. A fixed-length buffer doesn't keep its views, whose
/// lengths then stay what they were: from the first such detach on, what
/// reads elements by a view's length checks its buffer too (the
/// interpreter's direct accesses), or reads the length from it (compiled
/// code, dropped now and compiled through the runtime: see
/// `Vm::views_detached`).
pub(crate) fn detach(vm: &mut Vm, b: *mut Object) -> Result<()> {
    if is_immutable(b) {
        return Err(Error::type_error("Cannot detach an immutable ArrayBuffer"));
    }
    unsafe {
        (*b).elems_len = 0;
        (*b).flags |= DETACHED;
    }
    if !is_resizable(b) && !vm.views_detached {
        vm.views_detached = true;
        vm.bump_code_epoch();
    }
    refresh_views(b);
    Ok(())
}

/// A view's byte offset in its buffer.
pub(crate) fn offset_of(o: *mut Object) -> usize {
    unsafe { (*o).elems.cast::<u8>().offset_from((*buffer_of(o)).elems.cast::<u8>()) as usize }
}

/// A typed array's length, `None` if out of bounds (or its buffer
/// detached): TypedArrayLength of IsTypedArrayOutOfBounds's record.
pub(crate) fn ta_length(o: *mut Object) -> Option<usize> {
    let b = buffer_of(o);
    if is_detached(b) {
        return None;
    }
    let (len, off, size) = (unsafe { (*b).elems_len } as usize, offset_of(o), kind_of(o).size());
    match unsafe { (*o).elems_cap } {
        TRACKING => (off <= len).then(|| (len - off) / size),
        n => (off + n as usize * size <= len).then_some(n as usize),
    }
}

/// A `DataView`'s byte length, `None` if out of bounds (or its buffer
/// detached).
pub(crate) fn view_byte_length(o: *mut Object) -> Option<usize> {
    let b = buffer_of(o);
    if is_detached(b) {
        return None;
    }
    let (len, off) = (unsafe { (*b).elems_len } as usize, offset_of(o));
    match unsafe { (*o).elems_cap } {
        TRACKING => (off <= len).then(|| len - off),
        n => (off + n as usize <= len).then_some(n as usize),
    }
}

/// A view (`class` `TypedArray` of kind `k`, or `DataView`) of `len`
/// elements of `b` from byte `offset` (`None`: tracking its length).
pub(crate) fn make_view(vm: &mut Vm, class: Class, proto: Value, k: TypedKind, b: *mut Object, offset: usize, len: Option<usize>) -> Result<Value> {
    if class == Class::TypedArray {
        note_proto(vm, k as u8, proto);
    }
    let o = vm.new_object_with(class, proto, 0);
    unsafe {
        // (A `DataView`'s `kind` is its inline capacity, as other objects'.)
        if class == Class::TypedArray {
            (*o).kind = k as u8;
        }
        (*o).elems = (*b).elems.cast::<u8>().add(offset).cast();
        (*o).code = b.cast();
        (*o).elems_cap = len.map_or(TRACKING, |n| n as u32);
        (*o).elems_len = if class == Class::TypedArray { ta_length(o) } else { view_byte_length(o) }.unwrap_or(0) as u32;
        if is_immutable(b) {
            (*o).flags |= IMMUTABLE_VIEW;
        }
    }
    register(vm, b, object_value(o))?;
    Ok(object_value(o))
}

/// A typed array of kind `k` gets prototype `proto`. Compiled code takes
/// a typed array's kind from its shape's prototype (`jit::typed_kind`),
/// so from the first that gets another kind's (`Reflect.construct`'s new
/// target, `Object.setPrototypeOf`) on it can't: compiled code is dropped
/// and checks the kind of each (`Vm::typed_protos_crossed`).
pub(crate) fn note_proto(vm: &mut Vm, k: u8, proto: Value) {
    if vm.typed_protos_crossed || !proto.is_cell() {
        return;
    }
    let p = as_object(proto);
    if vm.typed_prototypes.iter().enumerate().any(|(i, &q)| q == p && i != k as usize) {
        vm.typed_protos_crossed = true;
        vm.bump_code_epoch();
    }
}

fn new_view(vm: &mut Vm, k: TypedKind, b: *mut Object, offset: usize, len: Option<usize>) -> Result<Value> {
    let proto = object_value(vm.typed_prototypes[k as usize]);
    make_view(vm, Class::TypedArray, proto, k, b, offset, len)
}

fn this_buffer(this: Value, what: &str) -> Result<*mut Object> {
    if is_class(this, Class::ArrayBuffer) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error(format!("ArrayBuffer.prototype.{what} called on an incompatible receiver")))
    }
}

/// `new ArrayBuffer(length, {maxByteLength})`.
fn array_buffer_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor ArrayBuffer requires 'new'"));
    }
    let n = to_index(vm, arg(a, 0))?;
    let options = arg(a, 1);
    let mut max = None;
    if is_object(options) {
        let a_max = vm.intern("maxByteLength");
        let m = vm.get(options, a_max)?;
        if !m.is_undefined() {
            max = Some(to_index(vm, m)?);
        }
    }
    if max.is_some_and(|m| n > m) {
        return Err(Error::range_error("ArrayBuffer length exceeds its maxByteLength"));
    }
    let c = vm.typed_constructors[TypedKind::ALL.len()];
    let proto = vm.take_new_target(c, object_value(vm.array_buffer_prototype))?;
    let o = new_buffer_max(vm, n, max)?;
    vm.set_prototype_of(o, proto);
    Ok(object_value(o))
}

fn buffer_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_i32(unsafe { (*this_buffer(this, "byteLength")?).elems_len } as i32))
}

fn buffer_max_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let b = this_buffer(this, "maxByteLength")?;
    let n = unsafe {
        if is_detached(b) {
            0
        } else if is_resizable(b) {
            (*b).elems_cap
        } else {
            (*b).elems_len
        }
    };
    Ok(Value::number(n as f64))
}

fn buffer_resizable(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(is_resizable(this_buffer(this, "resizable")?)))
}

fn buffer_detached(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(is_detached(this_buffer(this, "detached")?)))
}

/// `buffer.resize(newLength)`: its bytes from there on zero.
fn buffer_resize(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let b = this_buffer(this, "resize")?;
    if !is_resizable(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.resize: the buffer is not resizable"));
    }
    let n = to_index(vm, arg(a, 0))?;
    if is_detached(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.resize: the buffer is detached"));
    }
    if n > unsafe { (*b).elems_cap } as usize {
        return Err(Error::range_error("ArrayBuffer.prototype.resize: beyond maxByteLength"));
    }
    unsafe {
        let old = (*b).elems_len as usize;
        if n > old {
            std::ptr::write_bytes((*b).elems.cast::<u8>().add(old), 0, n - old);
        }
        (*b).elems_len = n as u32;
    }
    refresh_views(b);
    Ok(Value::UNDEFINED)
}

/// `buffer.slice(begin, end)`: a copy of those bytes, in a buffer of the
/// species.
fn buffer_slice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let b = this_buffer(this, "slice")?;
    if is_detached(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.slice: the buffer is detached"));
    }
    let len = unsafe { (*b).elems_len } as usize;
    let first = relative(integer(vm, arg(a, 0))?, len);
    let end = arg(a, 1);
    let last = if end.is_undefined() { len } else { relative(integer(vm, end)?, len) };
    let n = last.saturating_sub(first);
    let default = object_value(vm.typed_constructors[TypedKind::ALL.len()]);
    let c = species_constructor(vm, this, default)?;
    let out = crate::intrinsics::construct(vm, c, &[Value::number(n as f64)])?;
    if !is_class(out, Class::ArrayBuffer) {
        return Err(Error::type_error("ArrayBuffer.prototype.slice: the species made no ArrayBuffer"));
    }
    let o = as_object(out);
    if is_detached(o) {
        return Err(Error::type_error("ArrayBuffer.prototype.slice: the new buffer is detached"));
    }
    if is_immutable(o) {
        return Err(Error::type_error("ArrayBuffer.prototype.slice: the new buffer is immutable"));
    }
    if o == b {
        return Err(Error::type_error("ArrayBuffer.prototype.slice: the species returned the same buffer"));
    }
    if (unsafe { (*o).elems_len } as usize) < n {
        return Err(Error::type_error("ArrayBuffer.prototype.slice: the new buffer is too small"));
    }
    if is_detached(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.slice: the buffer is detached"));
    }
    let current = unsafe { (*b).elems_len } as usize;
    if first < current {
        let count = n.min(current - first);
        unsafe { std::ptr::copy((*b).elems.cast::<u8>().add(first), (*o).elems.cast::<u8>(), count) };
    }
    Ok(out)
}

/// `transfer(newLength)` (`fixed`: `transferToFixedLength`): a new buffer
/// with the bytes (resizable if this one is, unless `fixed`), this one
/// detached.
fn transfer_with(vm: &mut Vm, this: Value, a: &[Value], fixed: bool) -> Result<Value> {
    let b = this_buffer(this, if fixed { "transferToFixedLength" } else { "transfer" })?;
    let n = match arg(a, 0) {
        v if v.is_undefined() => (unsafe { (*b).elems_len }) as usize,
        v => to_index(vm, v)?,
    };
    if is_detached(b) || is_immutable(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.transfer: the buffer is detached or immutable"));
    }
    let max = (!fixed && is_resizable(b)).then(|| unsafe { (*b).elems_cap } as usize);
    if max.is_some_and(|m| n > m) {
        return Err(Error::range_error("ArrayBuffer.prototype.transfer: beyond maxByteLength"));
    }
    let out = new_buffer_max(vm, n, max)?;
    let count = n.min(unsafe { (*b).elems_len } as usize);
    unsafe { std::ptr::copy_nonoverlapping((*b).elems.cast::<u8>(), (*out).elems.cast::<u8>(), count) };
    detach(vm, b)?;
    Ok(object_value(out))
}

/// `transferToImmutable(newLength)`: a new immutable buffer with the bytes
/// (zeros past them), this one detached.
fn buffer_transfer_immutable(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let b = this_buffer(this, "transferToImmutable")?;
    let n = match arg(a, 0) {
        v if v.is_undefined() => (unsafe { (*b).elems_len }) as usize,
        v => to_index(vm, v)?,
    };
    if is_detached(b) || is_immutable(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.transferToImmutable: the buffer is detached or immutable"));
    }
    let out = new_buffer_max(vm, n, None)?;
    let count = n.min(unsafe { (*b).elems_len } as usize);
    unsafe { std::ptr::copy_nonoverlapping((*b).elems.cast::<u8>(), (*out).elems.cast::<u8>(), count) };
    detach(vm, b)?;
    make_immutable(vm, out);
    Ok(object_value(out))
}

/// `sliceToImmutable(start, end)`: a new immutable buffer with those
/// bytes, the bounds from the length before their conversions (a
/// RangeError if it shrank below them since).
fn buffer_slice_immutable(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let b = this_buffer(this, "sliceToImmutable")?;
    if is_detached(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.sliceToImmutable: the buffer is detached"));
    }
    let len = unsafe { (*b).elems_len } as usize;
    let first = relative(integer(vm, arg(a, 0))?, len);
    let end = arg(a, 1);
    let last = if end.is_undefined() { len } else { relative(integer(vm, end)?, len) };
    let n = last.saturating_sub(first);
    if is_detached(b) {
        return Err(Error::type_error("ArrayBuffer.prototype.sliceToImmutable: the buffer is detached"));
    }
    if first + n > unsafe { (*b).elems_len } as usize {
        return Err(Error::range_error("ArrayBuffer.prototype.sliceToImmutable: the buffer shrank"));
    }
    let out = new_buffer_max(vm, n, None)?;
    unsafe { std::ptr::copy_nonoverlapping((*b).elems.cast::<u8>().add(first), (*out).elems.cast::<u8>(), n) };
    make_immutable(vm, out);
    Ok(object_value(out))
}

fn buffer_immutable(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(is_immutable(this_buffer(this, "immutable")?)))
}

fn buffer_transfer(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    transfer_with(vm, this, a, false)
}

fn buffer_transfer_fixed(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    transfer_with(vm, this, a, true)
}

/// `ArrayBuffer.isView(v)`: a typed array or a `DataView`.
fn buffer_is_view(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let v = arg(a, 0);
    Ok(Value::from_bool(is_class(v, Class::TypedArray) || is_class(v, Class::DataView)))
}

// ---- Typed arrays ----

pub(crate) fn kind_of(o: *mut Object) -> TypedKind {
    TypedKind::from_u8(unsafe { (*o).kind })
}

fn this_typed(this: Value) -> Result<*mut Object> {
    if is_class(this, Class::TypedArray) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error("this is not a typed array."))
    }
}

/// ValidateTypedArray: `v` a typed array in bounds, and its length.
fn validate(v: Value) -> Result<(*mut Object, usize)> {
    let o = this_typed(v)?;
    match ta_length(o) {
        Some(n) => Ok((o, n)),
        None => Err(Error::type_error("Cannot perform this operation on a detached or out of bounds typed array")),
    }
}

/// `v` converted for an element of kind `k`: ToBigInt for a BigInt kind
/// (a BigInt), else ToNumber (a number).
pub(crate) fn to_element(vm: &mut Vm, k: TypedKind, v: Value) -> Result<Value> {
    if k.is_big() {
        if crate::bigint::is_bigint(v) {
            return Ok(v);
        }
        let b = crate::bigint::to_bigint(vm, v)?;
        return Ok(crate::bigint::new_bigint(vm, b));
    }
    if v.is_number() {
        return Ok(v);
    }
    Ok(Value::number(number(vm, v)?))
}

/// `o[i]` (`! Get`): its element, `undefined` past its length.
fn get(o: *mut Object, i: usize) -> Value {
    match ta_length(o) {
        Some(n) if i < n => unsafe { kind_of(o).load((*o).elems.cast(), i) },
        _ => Value::UNDEFINED,
    }
}

/// Element `i` of `o` set to `v`, already converted, if within its length.
fn put(o: *mut Object, i: usize, v: Value) {
    if ta_length(o).is_some_and(|n| i < n) {
        unsafe { kind_of(o).store_value((*o).elems.cast(), i, v) };
    }
}

/// `o[i] = v` (TypedArraySetElement): `v` converted, then stored if `i`
/// is within its length.
fn set(vm: &mut Vm, o: *mut Object, i: usize, v: Value) -> Result<()> {
    let v = to_element(vm, kind_of(o), v)?;
    put(o, i, v);
    Ok(())
}

/// The intrinsic constructor of kind `k`.
fn constructor_of(vm: &mut Vm, k: TypedKind) -> Result<Value> {
    Ok(object_value(vm.typed_constructors[k as usize]))
}

/// TypedArrayCreateFromConstructor(c, args): a typed array in bounds
/// (with `n`, at least that long).
fn create_from(vm: &mut Vm, c: Value, args: &[Value], n: Option<usize>) -> Result<*mut Object> {
    let out = crate::intrinsics::construct(vm, c, args)?;
    let (o, len) = validate(out)?;
    // (With `n`, the caller writes into it.)
    if n.is_some() {
        writable_view(o)?;
    }
    if n.is_some_and(|n| len < n) {
        return Err(Error::type_error("The typed array constructor made too short a typed array"));
    }
    Ok(o)
}

/// TypedArraySpeciesCreate(exemplar, args).
fn species_create(vm: &mut Vm, exemplar: *mut Object, args: &[Value], n: Option<usize>) -> Result<*mut Object> {
    let k = kind_of(exemplar);
    let default = constructor_of(vm, k)?;
    let c = species_constructor(vm, object_value(exemplar), default)?;
    let o = create_from(vm, c, args, n)?;
    if kind_of(o).is_big() != k.is_big() {
        return Err(Error::type_error("The species typed array's content type differs"));
    }
    Ok(o)
}

/// TypedArrayCreateSameType(exemplar, n).
fn create_same(vm: &mut Vm, exemplar: *mut Object, n: usize) -> Result<*mut Object> {
    let c = constructor_of(vm, kind_of(exemplar))?;
    create_from(vm, c, &[Value::number(n as f64)], Some(n))
}

/// A new typed array of kind `k`, `n` zero elements.
pub(crate) fn allocate(vm: &mut Vm, k: TypedKind, n: usize) -> Result<Value> {
    if n.checked_mul(k.size()).is_none_or(|b| b > MAX_BYTES) {
        return Err(Error::range_error(format!("Invalid typed array length: {n}")));
    }
    let b = new_buffer(vm, n * k.size());
    new_view(vm, k, b, 0, Some(n))
}

/// `new X(length)`, `new X(typedArray)`, `new X(buffer, byteOffset,
/// length)`, or `new X(object)` (iterable, or array-like).
fn typed_ctor<const K: usize>(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let k = TypedKind::ALL[K];
    if !vm.is_construct() {
        return Err(Error::type_error(format!("Constructor {} requires 'new'", k.name())));
    }
    let first = arg(a, 0);
    if !is_object(first) {
        let n = to_index(vm, first)?;
        return allocate(vm, k, n);
    }
    let fo = as_object(first);
    match unsafe { (*fo).class } {
        Class::TypedArray => {
            let Some(n) = ta_length(fo) else {
                return Err(Error::type_error("Cannot construct from a detached or out of bounds typed array"));
            };
            let sk = kind_of(fo);
            if sk.is_big() != k.is_big() {
                return Err(Error::type_error(format!("Cannot mix BigInt and other types: {} from {}", k.name(), sk.name())));
            }
            let out = allocate(vm, k, n)?;
            let o = as_object(out);
            vm.handle_scope(|vm| {
                vm.handles.push(out);
                for i in 0..n {
                    let v = unsafe { sk.load((*fo).elems.cast(), i) };
                    unsafe { k.store_value((*o).elems.cast(), i, v) };
                }
            });
            Ok(out)
        }
        Class::ArrayBuffer | Class::SharedArrayBuffer => {
            let offset = to_index(vm, arg(a, 1))?;
            if offset % k.size() != 0 {
                return Err(Error::range_error(format!("start offset of {} should be a multiple of {}", k.name(), k.size())));
            }
            let length = arg(a, 2);
            let n = if length.is_undefined() { None } else { Some(to_index(vm, length)?) };
            on_buffer(vm, k, fo, offset, n)
        }
        _ => {
            let a_iter = vm.a_sym_iterator;
            let m = vm.get(first, a_iter)?;
            if !m.is_undefined() && !m.is_null() {
                if !is_callable(m) {
                    return Err(Error::type_error(format!("{} is not a function", crate::vm::display(m))));
                }
                return vm.handle_scope(|vm| {
                    let values = iterate_with(vm, first, m)?;
                    let out = allocate(vm, k, values.len())?;
                    vm.handles.push(out);
                    for (i, &v) in values.iter().enumerate() {
                        set(vm, as_object(out), i, v)?;
                    }
                    Ok(out)
                });
            }
            let n = vm.length_of(first)? as usize;
            let out = allocate(vm, k, n)?;
            vm.handle_scope(|vm| {
                vm.handles.push(out);
                for i in 0..n {
                    let v = vm.get_keyed(first, Value::number(i as f64))?;
                    set(vm, as_object(out), i, v)?;
                }
                Ok(out)
            })
        }
    }
}

/// A typed array of kind `k` on buffer `b` from byte `offset` (a multiple
/// of the element size), `n` elements (`None`: the rest, or tracking a
/// resizable buffer's length): InitializeTypedArrayFromArrayBuffer's
/// checks once its arguments are converted.
fn on_buffer(vm: &mut Vm, k: TypedKind, b: *mut Object, offset: usize, n: Option<usize>) -> Result<Value> {
    let size = k.size();
    if is_detached(b) {
        return Err(Error::type_error("Cannot construct a typed array on a detached ArrayBuffer"));
    }
    if crate::shared::is_shared(b) {
        crate::shared::sync_length(b);
    }
    let bytes = unsafe { (*b).elems_len } as usize;
    let len = match n {
        None if is_resizable(b) => {
            if offset > bytes {
                return Err(Error::range_error(format!("Start offset {offset} is outside the bounds of the buffer")));
            }
            None
        }
        None => {
            if bytes % size != 0 || offset > bytes {
                return Err(Error::range_error(format!("Invalid typed array length: byte length of {} should be a multiple of {size}", k.name())));
            }
            Some((bytes - offset) / size)
        }
        Some(n) => {
            if offset + n * size > bytes {
                return Err(Error::range_error(format!("Invalid typed array length: {n}")));
            }
            Some(n)
        }
    };
    new_view(vm, k, b, offset, len)
}

/// The values of iterating `v` with its `@@iterator` method `m` (rooted
/// in `Vm::handles`: the caller's in a handle scope).
fn iterate_with(vm: &mut Vm, v: Value, m: Value) -> Result<Vec<Value>> {
    let it = call_function(vm, m, v, &[])?;
    if !is_object(it) {
        return Err(Error::type_error("Result of the Symbol.iterator method is not an object"));
    }
    vm.handles.push(it);
    let a_next = vm.intern("next");
    let next = vm.get(it, a_next)?;
    vm.handles.push(next);
    let mut out = Vec::new();
    while let Some(x) = crate::symbol::step_with(vm, it, next)? {
        vm.handles.push(x);
        out.push(x);
    }
    Ok(out)
}

/// `%TypedArray%` itself: not to be called or constructed.
fn typed_array_abstract(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Err(Error::type_error("Abstract class TypedArray not directly constructable"))
}

/// `TA.from(source, mapFn, thisArg)`.
fn typed_from(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !crate::intrinsics::is_constructor(this) {
        return Err(Error::type_error("TypedArray.from: this is not a constructor"));
    }
    let (source, f, this_arg) = (arg(a, 0), arg(a, 1), arg(a, 2));
    if !f.is_undefined() && !is_callable(f) {
        return Err(Error::type_error("TypedArray.from: the mapping function is not callable"));
    }
    let a_iter = vm.a_sym_iterator;
    let m = vm.get(source, a_iter)?;
    vm.handle_scope(|vm| {
        let (values, src) = if !m.is_undefined() && !m.is_null() {
            if !is_callable(m) {
                return Err(Error::type_error(format!("{} is not a function", crate::vm::display(m))));
            }
            (Some(iterate_with(vm, source, m)?), Value::UNDEFINED)
        } else {
            (None, object_value(vm.to_object(source)?))
        };
        let n = match &values {
            Some(v) => v.len(),
            None => vm.length_of(src)? as usize,
        };
        let o = create_from(vm, this, &[Value::number(n as f64)], Some(n))?;
        vm.handles.push(object_value(o));
        for i in 0..n {
            let v = match &values {
                Some(v) => v[i],
                None => vm.get_keyed(src, Value::number(i as f64))?,
            };
            let v = if f.is_undefined() { v } else { call_function(vm, f, this_arg, &[v, Value::number(i as f64)])? };
            vm.set_keyed(object_value(o), Value::number(i as f64), v)?;
        }
        Ok(object_value(o))
    })
}

/// `TA.of(...items)`.
fn typed_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !crate::intrinsics::is_constructor(this) {
        return Err(Error::type_error("TypedArray.of: this is not a constructor"));
    }
    let o = create_from(vm, this, &[Value::number(a.len() as f64)], Some(a.len()))?;
    for (i, &v) in a.iter().enumerate() {
        vm.set_keyed(object_value(o), Value::number(i as f64), v)?;
    }
    Ok(object_value(o))
}

fn typed_buffer(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(object_value(buffer_of(this_typed(this)?)))
}

fn typed_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    Ok(Value::number((ta_length(o).unwrap_or(0) * kind_of(o).size()) as f64))
}

fn typed_byte_offset(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    Ok(Value::number(if ta_length(o).is_some() { offset_of(o) as f64 } else { 0.0 }))
}

fn typed_length_getter(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::number(ta_length(this_typed(this)?).unwrap_or(0) as f64))
}

/// `get %TypedArray%.prototype[Symbol.toStringTag]`: the kind's name.
pub(crate) fn typed_to_string_tag(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if !is_class(this, Class::TypedArray) {
        return Ok(Value::UNDEFINED);
    }
    Ok(str_value(vm, kind_of(as_object(this)).name()))
}

/// The callback argument, checked callable.
fn callback(a: &[Value], what: &str) -> Result<Value> {
    let f = arg(a, 0);
    if !is_callable(f) {
        return Err(Error::type_error(format!("{} is not a function (%TypedArray%.prototype.{what})", crate::vm::display(f))));
    }
    Ok(f)
}

fn typed_at(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let r = integer(vm, arg(a, 0))?;
    let k = if r >= 0.0 { r } else { len as f64 + r };
    if k < 0.0 || k >= len as f64 {
        return Ok(Value::UNDEFINED);
    }
    Ok(get(o, k as usize))
}

fn typed_copy_within(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    writable_view(o)?;
    let to = relative(integer(vm, arg(a, 0))?, len);
    let from = relative(integer(vm, arg(a, 1))?, len);
    let end = arg(a, 2);
    let last = if end.is_undefined() { len } else { relative(integer(vm, end)?, len) };
    let count = (last as isize - from as isize).min(len as isize - to as isize);
    if count > 0 {
        let Some(len) = ta_length(o) else {
            return Err(Error::type_error("Cannot perform copyWithin on a detached or out of bounds typed array"));
        };
        let size = kind_of(o).size();
        let limit = len * size;
        let (to, from) = (to * size, from * size);
        let n = (count as usize * size).min(limit.saturating_sub(from)).min(limit.saturating_sub(to));
        if n > 0 {
            unsafe {
                let base = (*o).elems.cast::<u8>();
                crate::racy::copy_if(shares(o), base.add(from), base.add(to), n);
            }
        }
    }
    Ok(this)
}

/// `every`'s, `some`'s and the `find`s' loop: `f(value, index, o)` for each
/// index from the start (`back`: the end), until `stop` of its result.
fn search(vm: &mut Vm, this: Value, a: &[Value], what: &str, back: bool, stop: bool) -> Result<Option<(usize, Value)>> {
    let (o, len) = validate(this)?;
    let f = callback(a, what)?;
    let this_arg = arg(a, 1);
    for j in 0..len {
        let i = if back { len - 1 - j } else { j };
        let v = get(o, i);
        let r = call_function(vm, f, this_arg, &[v, Value::number(i as f64), this])?;
        if to_boolean(r) == stop {
            return Ok(Some((i, v)));
        }
    }
    Ok(None)
}

fn typed_every(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(search(vm, this, a, "every", false, false)?.is_none()))
}

fn typed_some(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(search(vm, this, a, "some", false, true)?.is_some()))
}

fn typed_find(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(search(vm, this, a, "find", false, true)?.map_or(Value::UNDEFINED, |(_, v)| v))
}

fn typed_find_index(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(search(vm, this, a, "findIndex", false, true)?.map_or(-1.0, |(i, _)| i as f64)))
}

fn typed_find_last(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(search(vm, this, a, "findLast", true, true)?.map_or(Value::UNDEFINED, |(_, v)| v))
}

fn typed_find_last_index(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(search(vm, this, a, "findLastIndex", true, true)?.map_or(-1.0, |(i, _)| i as f64)))
}

fn typed_for_each(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let f = callback(a, "forEach")?;
    let this_arg = arg(a, 1);
    for i in 0..len {
        let v = get(o, i);
        call_function(vm, f, this_arg, &[v, Value::number(i as f64), this])?;
    }
    Ok(Value::UNDEFINED)
}

fn typed_fill(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    writable_view(o)?;
    let v = to_element(vm, kind_of(o), arg(a, 0))?;
    let s = relative(integer(vm, arg(a, 1))?, len);
    let end = arg(a, 2);
    let e = if end.is_undefined() { len } else { relative(integer(vm, end)?, len) };
    let Some(len) = ta_length(o) else {
        return Err(Error::type_error("Cannot perform fill on a detached or out of bounds typed array"));
    };
    for i in s..e.min(len) {
        put(o, i, v);
    }
    Ok(this)
}

fn typed_filter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let f = callback(a, "filter")?;
    let this_arg = arg(a, 1);
    vm.handle_scope(|vm| {
        let mut kept = Vec::new();
        for i in 0..len {
            let v = get(o, i);
            vm.handles.push(v);
            if to_boolean(call_function(vm, f, this_arg, &[v, Value::number(i as f64), this])?) {
                kept.push(v);
            }
        }
        let out = species_create(vm, o, &[Value::number(kept.len() as f64)], Some(kept.len()))?;
        for (i, &v) in kept.iter().enumerate() {
            set(vm, out, i, v)?;
        }
        Ok(object_value(out))
    })
}

/// `includes`' and `indexOf`'s start: `fromIndex` relative to `len`
/// (`None`: past the end).
fn from_index(vm: &mut Vm, v: Value, len: usize) -> Result<Option<usize>> {
    let n = integer(vm, v)?;
    if n == f64::INFINITY {
        return Ok(None);
    }
    let k = if n >= 0.0 { n } else { (len as f64 + n).max(0.0) };
    Ok((k < len as f64).then_some(k as usize))
}

fn typed_includes(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    if len == 0 {
        return Ok(Value::FALSE);
    }
    let Some(start) = from_index(vm, arg(a, 1), len)? else { return Ok(Value::FALSE) };
    let x = arg(a, 0);
    Ok(Value::from_bool((start..len).any(|i| crate::collections::same_value_zero(get(o, i), x))))
}

fn typed_index_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    if len == 0 {
        return Ok(Value::from_i32(-1));
    }
    let Some(start) = from_index(vm, arg(a, 1), len)? else { return Ok(Value::from_i32(-1)) };
    let x = arg(a, 0);
    let n = ta_length(o).unwrap_or(0).min(len);
    let found = (start..n).find(|&i| crate::interp::strict_equals(get(o, i), x));
    Ok(Value::number(found.map_or(-1.0, |i| i as f64)))
}

fn typed_last_index_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    if len == 0 {
        return Ok(Value::from_i32(-1));
    }
    let n = if a.len() > 1 { integer(vm, a[1])? } else { len as f64 - 1.0 };
    if n == f64::NEG_INFINITY {
        return Ok(Value::from_i32(-1));
    }
    let k = if n >= 0.0 { n.min(len as f64 - 1.0) } else { len as f64 + n };
    if k < 0.0 {
        return Ok(Value::from_i32(-1));
    }
    let x = arg(a, 0);
    let now = ta_length(o).unwrap_or(0);
    let found = (0..=k as usize).rev().find(|&i| i < now && crate::interp::strict_equals(get(o, i), x));
    Ok(Value::number(found.map_or(-1.0, |i| i as f64)))
}

fn typed_join(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let sep: Vec<u16> = match arg(a, 0) {
        s if s.is_undefined() => vec![b',' as u16],
        s => {
            let s = crate::intrinsics::to_string(vm, s)?;
            string_units(s).unwrap_or(&[]).to_vec()
        }
    };
    let mut out = Vec::new();
    for i in 0..len {
        if i > 0 {
            out.extend_from_slice(&sep);
        }
        let v = get(o, i);
        if !v.is_undefined() {
            out.extend(crate::vm::display(v).encode_utf16());
        }
    }
    Ok(vm.new_string(&out))
}

fn typed_map(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let f = callback(a, "map")?;
    let this_arg = arg(a, 1);
    let out = species_create(vm, o, &[Value::number(len as f64)], Some(len))?;
    vm.handle_scope(|vm| {
        vm.handles.push(object_value(out));
        for i in 0..len {
            let v = get(o, i);
            let r = call_function(vm, f, this_arg, &[v, Value::number(i as f64), this])?;
            vm.set_keyed(object_value(out), Value::number(i as f64), r)?;
        }
        Ok(object_value(out))
    })
}

/// `reduce` (`back`: `reduceRight`).
fn reduce(vm: &mut Vm, this: Value, a: &[Value], back: bool) -> Result<Value> {
    let (o, len) = validate(this)?;
    let f = callback(a, if back { "reduceRight" } else { "reduce" })?;
    let mut order: Box<dyn Iterator<Item = usize>> = if back { Box::new((0..len).rev()) } else { Box::new(0..len) };
    let mut acc = if a.len() >= 2 {
        a[1]
    } else {
        match order.next() {
            Some(i) => get(o, i),
            None => return Err(Error::type_error("Reduce of empty array with no initial value")),
        }
    };
    for i in order {
        let v = get(o, i);
        acc = call_function(vm, f, Value::UNDEFINED, &[acc, v, Value::number(i as f64), this])?;
    }
    Ok(acc)
}

fn typed_reduce(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    reduce(vm, this, a, false)
}

fn typed_reduce_right(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    reduce(vm, this, a, true)
}

fn typed_reverse(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    writable_view(o)?;
    for i in 0..len / 2 {
        let (x, y) = (get(o, i), get(o, len - 1 - i));
        put(o, i, y);
        put(o, len - 1 - i, x);
    }
    Ok(this)
}

/// `t.set(source, offset)`: `source`'s elements (a typed array's, or an
/// array-like's) stored from `offset` on.
fn typed_set(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    writable_view(o)?;
    let offset = integer(vm, arg(a, 1))?;
    if offset < 0.0 {
        return Err(Error::range_error("offset is out of bounds"));
    }
    let Some(len) = ta_length(o) else {
        return Err(Error::type_error("Cannot perform set on a detached or out of bounds typed array"));
    };
    let src = arg(a, 0);
    let k = kind_of(o);
    if is_class(src, Class::TypedArray) {
        let s = as_object(src);
        let Some(n) = ta_length(s) else {
            return Err(Error::type_error("Cannot set from a detached or out of bounds typed array"));
        };
        if offset == f64::INFINITY || n as f64 + offset > len as f64 {
            return Err(Error::range_error("offset is out of bounds"));
        }
        let sk = kind_of(s);
        if sk.is_big() != k.is_big() {
            return Err(Error::type_error("Cannot mix BigInt and other types"));
        }
        let offset = offset as usize;
        if sk == k {
            let (from, to) = unsafe { ((*s).elems.cast::<u8>(), (*o).elems.cast::<u8>().add(offset * k.size())) };
            unsafe { crate::racy::copy_if(shares(s) || shares(o), from, to, n * k.size()) };
            return Ok(Value::UNDEFINED);
        }
        // (Read them all first: the source may share the bytes.)
        return vm.handle_scope(|vm| {
            let values: Vec<Value> = (0..n).map(|i| unsafe { sk.load((*s).elems.cast(), i) }).collect();
            if sk.is_big() {
                vm.handles.extend_from_slice(&values);
            }
            for (i, v) in values.into_iter().enumerate() {
                unsafe { k.store_value((*o).elems.cast(), offset + i, v) };
            }
            Ok(Value::UNDEFINED)
        });
    }
    let src = object_value(vm.to_object(src)?);
    let n = vm.length_of(src)?;
    if offset == f64::INFINITY || n + offset > len as f64 {
        return Err(Error::range_error("offset is out of bounds"));
    }
    let offset = offset as usize;
    for i in 0..n as usize {
        let v = vm.get_keyed(src, Value::number(i as f64))?;
        set(vm, o, offset + i, v)?;
    }
    Ok(Value::UNDEFINED)
}

fn typed_slice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let s = relative(integer(vm, arg(a, 0))?, len);
    let end = arg(a, 1);
    let e = if end.is_undefined() { len } else { relative(integer(vm, end)?, len) };
    let count = e.saturating_sub(s);
    let out = species_create(vm, o, &[Value::number(count as f64)], Some(count))?;
    if count > 0 {
        let Some(len) = ta_length(o) else {
            return Err(Error::type_error("Cannot perform slice on a detached or out of bounds typed array"));
        };
        let e = e.min(len);
        let (k, ok) = (kind_of(o), kind_of(out));
        if k == ok {
            let size = k.size();
            let n = e.saturating_sub(s) * size;
            let room = ta_length(out).unwrap_or(0) * size;
            // (Byte by byte, first to last, as the spec has it: where `out`
            // views the same bytes, not as `memmove` would.)
            unsafe {
                let (from, to) = ((*o).elems.cast::<u8>().add(s * size), (*out).elems.cast::<u8>());
                for b in 0..n.min(room) {
                    crate::racy::store::<1>(to.add(b), crate::racy::load::<1>(from.add(b)));
                }
            }
        } else {
            vm.handle_scope(|vm| {
                vm.handles.push(object_value(out));
                for (n, i) in (s..e).enumerate() {
                    let v = get(o, i);
                    set(vm, out, n, v)?;
                }
                Ok::<(), Error>(())
            })?;
        }
    }
    Ok(object_value(out))
}

/// Sort `values`, numbers or BigInts, (stably) by `cmp` (a function, or
/// `undefined`: numeric order, -0 before +0, NaN last).
fn sort_numbers(vm: &mut Vm, values: &mut Vec<Value>, cmp: Value) -> Result<()> {
    let after = |vm: &mut Vm, x: Value, y: Value| -> Result<bool> {
        if cmp.is_undefined() {
            if crate::bigint::is_bigint(x) {
                let (p, q) = (crate::bigint::big_of(x), crate::bigint::big_of(y));
                return Ok(p.cmp(&q) == std::cmp::Ordering::Greater);
            }
            let (x, y) = (x.to_number(), y.to_number());
            return Ok(match (x.is_nan(), y.is_nan()) {
                (true, _) => !y.is_nan(),
                (false, true) => false,
                _ => x > y || (x == 0.0 && y == 0.0 && x.is_sign_positive() && y.is_sign_negative()),
            });
        }
        let r = call_function(vm, cmp, Value::UNDEFINED, &[x, y])?;
        Ok(number(vm, r)? > 0.0)
    };
    // A merge sort: a comparator that isn't consistent still ends.
    let mut width = 1;
    let n = values.len();
    let mut buf = values.clone();
    while width < n {
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut i, mut j, mut k) = (lo, mid, lo);
            while i < mid && j < hi {
                if after(vm, values[i], values[j])? {
                    buf[k] = values[j];
                    j += 1;
                } else {
                    buf[k] = values[i];
                    i += 1;
                }
                k += 1;
            }
            buf[k..k + mid - i].copy_from_slice(&values[i..mid]);
            k += mid - i;
            buf[k..k + hi - j].copy_from_slice(&values[j..hi]);
            lo = hi;
        }
        std::mem::swap(values, &mut buf);
        width *= 2;
    }
    Ok(())
}

fn comparator(a: &[Value]) -> Result<Value> {
    let cmp = arg(a, 0);
    if !cmp.is_undefined() && !is_callable(cmp) {
        return Err(Error::type_error("The comparison function must be either a function or undefined"));
    }
    Ok(cmp)
}

/// Typed array `o`'s first `len` elements (rooted in `Vm::handles`: the
/// caller's in a handle scope).
fn values(vm: &mut Vm, o: *mut Object, len: usize) -> Vec<Value> {
    let values: Vec<Value> = (0..len).map(|i| get(o, i)).collect();
    if kind_of(o).is_big() {
        vm.handles.extend_from_slice(&values);
    }
    values
}

fn typed_sort(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let cmp = comparator(a)?;
    let (o, len) = validate(this)?;
    writable_view(o)?;
    vm.handle_scope(|vm| {
        let mut values = values(vm, o, len);
        sort_numbers(vm, &mut values, cmp)?;
        for (i, e) in values.into_iter().enumerate() {
            put(o, i, e);
        }
        Ok(this)
    })
}

fn typed_to_sorted(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let cmp = comparator(a)?;
    let (o, len) = validate(this)?;
    let out = create_same(vm, o, len)?;
    vm.handle_scope(|vm| {
        vm.handles.push(object_value(out));
        let mut values = values(vm, o, len);
        sort_numbers(vm, &mut values, cmp)?;
        for (i, e) in values.into_iter().enumerate() {
            put(out, i, e);
        }
        Ok(object_value(out))
    })
}

fn typed_to_reversed(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let out = create_same(vm, o, len)?;
    for i in 0..len {
        put(out, i, get(o, len - 1 - i));
    }
    Ok(object_value(out))
}

fn typed_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let r = integer(vm, arg(a, 0))?;
    let i = if r >= 0.0 { r } else { len as f64 + r };
    let v = to_element(vm, kind_of(o), arg(a, 1))?;
    if !(i >= 0.0 && ta_length(o).is_some_and(|n| i < n as f64)) {
        return Err(Error::range_error("Invalid typed array index"));
    }
    vm.handle_scope(|vm| {
        vm.handles.push(v);
        let out = create_same(vm, o, len)?;
        for k in 0..len {
            put(out, k, if k == i as usize { v } else { get(o, k) });
        }
        Ok(object_value(out))
    })
}

fn typed_subarray(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let b = buffer_of(o);
    let len = ta_length(o).unwrap_or(0);
    let s = relative(integer(vm, arg(a, 0))?, len);
    let k = kind_of(o);
    let begin = offset_of(o) + s * k.size();
    let end = arg(a, 1);
    let n = if unsafe { (*o).elems_cap } == TRACKING && end.is_undefined() {
        None
    } else {
        let e = if end.is_undefined() { len } else { relative(integer(vm, end)?, len) };
        Some(e.saturating_sub(s))
    };
    // (The species the kind's own constructor: its view at once.)
    let default = constructor_of(vm, k)?;
    let c = species_constructor(vm, this, default)?;
    if c == default {
        return on_buffer(vm, k, b, begin, n);
    }
    let mut args = vec![object_value(b), Value::number(begin as f64)];
    args.extend(n.map(|n| Value::number(n as f64)));
    let out = create_from(vm, c, &args, None)?;
    if kind_of(out).is_big() != k.is_big() {
        return Err(Error::type_error("The species typed array's content type differs"));
    }
    Ok(object_value(out))
}

fn typed_to_locale_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (o, len) = validate(this)?;
    let mut out = Vec::new();
    let a_tls = vm.intern("toLocaleString");
    for i in 0..len {
        if i > 0 {
            out.push(b',' as u16);
        }
        let v = get(o, i);
        if v.is_undefined() || v.is_null() {
            continue;
        }
        let f = vm.get(v, a_tls)?;
        let s = call_function(vm, f, v, &[])?;
        let s = crate::intrinsics::to_string(vm, s)?;
        out.extend_from_slice(string_units(s).unwrap_or(&[]));
    }
    Ok(vm.new_string(&out))
}

/// `keys()`, `values()` and `entries()` (`K`: 0, 1, 2): an array iterator
/// over a typed array in bounds.
fn typed_iterator<const K: i32>(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    validate(this)?;
    crate::symbol::array_iterator(vm, this, K)
}

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let any = crate::jit::Ty::Any;
    let abp = vm.new_object(op);
    vm.array_buffer_prototype = abp;
    let ab = vm.define_native(g, "ArrayBuffer", array_buffer_ctor, 1, any, true);
    link_constructor(vm, ab, abp);
    getter(vm, abp, "byteLength", buffer_byte_length);
    getter(vm, abp, "maxByteLength", buffer_max_byte_length);
    getter(vm, abp, "resizable", buffer_resizable);
    getter(vm, abp, "detached", buffer_detached);
    getter(vm, abp, "immutable", buffer_immutable);
    vm.define_natives(
        abp,
        &[
            ("slice", buffer_slice as NativeFn, 2),
            ("resize", buffer_resize, 1),
            ("transfer", buffer_transfer, 0),
            ("transferToFixedLength", buffer_transfer_fixed, 0),
            ("transferToImmutable", buffer_transfer_immutable, 0),
            ("sliceToImmutable", buffer_slice_immutable, 2),
        ],
    );
    vm.define_native(ab, "isView", buffer_is_view, 1, any, false);

    // `%TypedArray%` (abstract) and its prototype, the concrete kinds'.
    let shared = vm.new_object(op);
    let ta = vm.native_function("TypedArray", typed_array_abstract, 0, any, true);
    link_constructor(vm, ta, shared);
    vm.define_natives(ta, &[("from", typed_from as NativeFn, 1), ("of", typed_of, 0)]);
    vm.define_natives(
        shared,
        &[
            ("at", typed_at as NativeFn, 1),
            ("copyWithin", typed_copy_within, 2),
            ("entries", typed_iterator::<2>, 0),
            ("every", typed_every, 1),
            ("fill", typed_fill, 1),
            ("filter", typed_filter, 1),
            ("find", typed_find, 1),
            ("findIndex", typed_find_index, 1),
            ("findLast", typed_find_last, 1),
            ("findLastIndex", typed_find_last_index, 1),
            ("forEach", typed_for_each, 1),
            ("includes", typed_includes, 1),
            ("indexOf", typed_index_of, 1),
            ("join", typed_join, 1),
            ("keys", typed_iterator::<0>, 0),
            ("lastIndexOf", typed_last_index_of, 1),
            ("map", typed_map, 1),
            ("reduce", typed_reduce, 1),
            ("reduceRight", typed_reduce_right, 1),
            ("reverse", typed_reverse, 0),
            ("set", typed_set, 1),
            ("slice", typed_slice, 2),
            ("some", typed_some, 1),
            ("sort", typed_sort, 1),
            ("subarray", typed_subarray, 2),
            ("toLocaleString", typed_to_locale_string, 0),
            ("toReversed", typed_to_reversed, 0),
            ("toSorted", typed_to_sorted, 1),
            ("with", typed_with, 2),
        ],
    );
    vm.define_native(shared, "values", typed_iterator::<1>, 0, any, false);
    // (`toString` is `Array.prototype.toString` itself.)
    let a_to_string = vm.intern("toString");
    let to_string = vm.get(object_value(vm.array_prototype), a_to_string).unwrap_or(Value::UNDEFINED);
    vm.add_property_with(shared, a_to_string, to_string, DONT_ENUM);
    getter(vm, shared, "buffer", typed_buffer);
    getter(vm, shared, "byteLength", typed_byte_length);
    getter(vm, shared, "byteOffset", typed_byte_offset);
    getter(vm, shared, "length", typed_length_getter);
    const CTORS: [NativeFn; 12] = [
        typed_ctor::<0>,
        typed_ctor::<1>,
        typed_ctor::<2>,
        typed_ctor::<3>,
        typed_ctor::<4>,
        typed_ctor::<5>,
        typed_ctor::<6>,
        typed_ctor::<7>,
        typed_ctor::<8>,
        typed_ctor::<9>,
        typed_ctor::<10>,
        typed_ctor::<11>,
    ];
    let a_bpe = vm.intern("BYTES_PER_ELEMENT");
    for (k, kind) in TypedKind::ALL.iter().enumerate() {
        let proto = vm.new_object(object_value(shared));
        let c = vm.define_native(g, kind.name(), CTORS[k], 3, any, true);
        link_constructor(vm, c, proto);
        vm.set_prototype_of(c, object_value(ta));
        let size = Value::from_i32(kind.size() as i32);
        vm.add_property_with(c, a_bpe, size, READ_ONLY | DONT_ENUM | DONT_DELETE);
        vm.add_property_with(proto, a_bpe, size, READ_ONLY | DONT_ENUM | DONT_DELETE);
        vm.typed_prototypes.push(proto);
        vm.typed_constructors.push(c);
    }
    vm.typed_constructors.push(ab);
    crate::shared::install(vm);
    crate::base64::install(vm);
}

/// What needs the well-known symbols (`symbol::install` calls it):
/// `%TypedArray%.prototype[@@iterator]` (its `values`) and
/// `[@@toStringTag]`, `ArrayBuffer.prototype[@@toStringTag]`.
pub(crate) fn install_symbols(vm: &mut Vm) {
    let shared = as_object(unsafe { (*(*vm.typed_prototypes[0]).shape).proto });
    let a_values = vm.intern("values");
    let values = vm.get(object_value(shared), a_values).unwrap_or(Value::UNDEFINED);
    let a_iter = vm.a_sym_iterator;
    vm.add_property_with(shared, a_iter, values, DONT_ENUM);
    let holder = vm.new_object(Value::NULL);
    let tag = vm.define_native(holder, "get [Symbol.toStringTag]", typed_to_string_tag, 0, crate::jit::Ty::Any, false);
    let a_tag = vm.a_sym_to_string_tag;
    vm.define_accessor(shared, a_tag, Some(object_value(tag)), None, DONT_ENUM);
    let abp = vm.array_buffer_prototype;
    crate::symbol::to_string_tag(vm, abp, "ArrayBuffer");
    crate::shared::install_symbols(vm);
}

/// Whether property name `s` is a canonical numeric string (but an array
/// index's, a `Key::Index`): a typed array's such properties are none,
/// never ordinary ones (`"-0"`, `"1.5"`, `"-1"`, `"NaN"`, …).
pub(crate) fn canonical_numeric(s: &str) -> bool {
    if s == "-0" {
        return true;
    }
    let Some(c) = s.bytes().next() else { return false };
    if !(c.is_ascii_digit() || c == b'-' || c == b'I' || c == b'N') {
        return false;
    }
    match s.parse::<f64>() {
        Ok(d) => crate::vm::number_to_string(d) == s,
        Err(_) => false,
    }
}
