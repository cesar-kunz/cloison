//! `SharedArrayBuffer` and `Atomics`.
//!
//! A shared buffer (class `SharedArrayBuffer`) is laid out as an
//! `ArrayBuffer` (see `typed`), but its bytes are in a [`Block`] outside
//! the heap, which agents (`agent`) share: each one's buffer on it holds
//! a reference, which the VM drops once the buffer is found dead (see
//! [`State`]). A growable one (flag `RESIZABLE`) has room for its maximum
//! from the start, so its bytes never move; the block holds its length,
//! which each agent's buffer caches (and brings its views up to date to,
//! as a resizable `ArrayBuffer`'s) when it reads it.
//!
//! `Atomics` reads and writes elements with the processor's atomic
//! operations, sequentially consistent. Waiters (`Atomics.wait`, and
//! `Atomics.waitAsync`'s) are in one list for the process, in the order
//! they came, under one lock: the spec's WaiterList critical sections. A
//! VM's async waiters' promises are settled by its event loop
//! ([`Vm::run_event_loop`]), between promise jobs ([`poll`]) and once
//! they have run out.

use std::alloc::Layout;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicI16, AtomicI32, AtomicI64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::builtins::{getter, is_class};
use crate::object::{Class, Object, TypedKind, as_object, object_value};
use crate::props::is_object;
use crate::typed::{
    MAX_BYTES, MAX_RESIZABLE, RESIZABLE, integer, is_resizable, kind_of, refresh_views, relative, species_constructor,
    ta_length, to_index,
};
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm};

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn str_value(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.string_constant(&u)
}

// ---- Blocks ----

/// A shared buffer's header, its bytes after it: the references to it
/// (the agents' buffers on it), its length, and its room (its maximum
/// length, a growable one's).
#[repr(C)]
pub(crate) struct Block {
    refs: AtomicUsize,
    len: AtomicUsize,
    room: usize,
    _pad: usize,
}

const HEADER: usize = size_of::<Block>();
const _: () = assert!(HEADER == 32);

fn layout(room: usize) -> Layout {
    Layout::from_size_align(HEADER + room.max(8).next_multiple_of(8), 16).expect("block layout")
}

/// A new block of `n` zero bytes, with room for `room` (`None` if there
/// is no memory for it), one reference to it.
fn new_block(n: usize, room: usize) -> Option<*mut Block> {
    unsafe {
        let p = std::alloc::alloc_zeroed(layout(room)).cast::<Block>();
        if p.is_null() {
            return None;
        }
        p.write(Block { refs: AtomicUsize::new(1), len: AtomicUsize::new(n), room, _pad: 0 });
        Some(p)
    }
}

fn bytes_of(b: *mut Block) -> *mut u8 {
    unsafe { b.cast::<u8>().add(HEADER) }
}

/// Shared buffer `o`'s block.
pub(crate) fn block_of(o: *mut Object) -> *mut Block {
    unsafe { (*o).elems.cast::<u8>().sub(HEADER).cast() }
}

/// A new reference to block `b` (an agent's buffer on it).
pub(crate) fn retain(b: *mut Block) {
    unsafe { (*b).refs.fetch_add(1, SeqCst) };
}

/// Drop a reference to block `b`: the last frees it.
pub(crate) fn release(b: *mut Block) {
    unsafe {
        if (*b).refs.fetch_sub(1, SeqCst) == 1 {
            std::alloc::dealloc(b.cast(), layout((*b).room));
        }
    }
}

/// A VM's part in the shared memory: `SharedArrayBuffer.prototype`, its
/// buffers' blocks (one reference each), a weak set of the buffers (which
/// tells the blocks still referred to: see [`prune`]), and its async
/// waiters, with their promises' resolve functions.
pub(crate) struct State {
    pub(crate) prototype: *mut Object,
    pub(crate) constructor: *mut Object,
    live: *mut Object,
    blocks: Vec<*mut Block>,
    prune_at: usize,
    /// The collections there had been at the last pruning.
    pruned: usize,
    /// The bytes of `blocks`, and how many make for a full collection
    /// (see [`new_shared`]).
    external: usize,
    external_at: usize,
    waits: Vec<(Arc<Waiter>, Value, Value)>,
    /// What wakes this VM's event loop: a notify of one of its async
    /// waiters (its `Waiter::wake`).
    signal: Arc<Condvar>,
}

impl Default for State {
    fn default() -> State {
        State {
            prototype: std::ptr::null_mut(),
            constructor: std::ptr::null_mut(),
            live: std::ptr::null_mut(),
            blocks: Vec::new(),
            prune_at: 64,
            pruned: 0,
            external: 0,
            external_at: EXTERNAL_MIN,
            waits: Vec::new(),
            signal: Arc::new(Condvar::new()),
        }
    }
}

impl State {
    pub(crate) fn roots(&self) -> Vec<Value> {
        let mut v: Vec<Value> =
            [self.prototype, self.constructor, self.live].into_iter().filter(|o| !o.is_null()).map(object_value).collect();
        for (_, p, resolve) in &self.waits {
            v.push(*p);
            v.push(*resolve);
        }
        v
    }

    /// Whether async waiters are pending.
    pub(crate) fn waiting(&self) -> bool {
        !self.waits.is_empty()
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let mut list = WAITERS.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|w| !self.waits.iter().any(|(x, _, _)| Arc::ptr_eq(w, x)));
        drop(list);
        for &b in &self.blocks {
            release(b);
        }
    }
}

/// The least block bytes a VM refers to that make for a full collection.
const EXTERNAL_MIN: usize = 64 << 20;

/// Release the blocks of the VM's buffers found dead (after a collection,
/// or once it has made twice as many as were alive last time). (Only a
/// full collection finds them dead: the weak set is old.)
fn prune(vm: &mut Vm) -> Result<()> {
    let count = vm.heap.gc.count;
    let s = &mut vm.shared;
    if (s.blocks.len() < s.prune_at && s.pruned == count) || s.live.is_null() {
        return Ok(());
    }
    s.pruned = count;
    let values = crate::collections::weak_set_values(s.live);
    // Each live buffer keeps one reference to its block (the VM holds one
    // per buffer: several on the same block, broadcast again, each hold
    // one), the dead ones' released.
    let mut live: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for &v in &values {
        *live.entry(block_of(as_object(v)) as usize).or_default() += 1;
    }
    let mut freed = 0;
    s.blocks.retain(|&b| {
        let keep = live.get_mut(&(b as usize)).is_some_and(|n| {
            let left = *n > 0;
            *n = n.saturating_sub(1);
            left
        });
        if !keep {
            freed += unsafe { (*b).room };
            release(b);
        }
        keep
    });
    s.external -= freed;
    s.external_at = s.external_at.min((2 * s.external).max(EXTERNAL_MIN));
    s.prune_at = 2 * s.blocks.len() + 64;
    // (A fresh set: a weak set keeps its dead entries' slots. The
    // buffers rooted meanwhile: the old set holds them weakly.)
    vm.handle_scope(|vm| {
        vm.handles.extend_from_slice(&values);
        let set = crate::collections::new_weak_set(vm)?;
        vm.shared.live = set;
        for v in values {
            crate::collections::weak_set_insert(vm, set, v)?;
        }
        Ok(())
    })
}

/// A shared buffer on block `b` (growable if `growable`), whose reference
/// to it the VM takes over.
pub(crate) fn new_shared(vm: &mut Vm, b: *mut Block, growable: bool) -> Result<Value> {
    // (Before `b` is among the blocks: no buffer on it is in the set yet.)
    prune(vm)?;
    let s = &mut vm.shared;
    s.blocks.push(b);
    // A full collection once the blocks have doubled since the last.
    s.external += unsafe { (*b).room };
    if s.external >= s.external_at {
        s.external_at = (2 * s.external).max(EXTERNAL_MIN);
        vm.heap.gc.external_full = true;
        vm.heap.requested.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if vm.shared.live.is_null() {
        vm.shared.live = crate::collections::new_weak_set(vm)?;
    }
    let proto = object_value(vm.shared.prototype);
    let o = vm.new_object_with(Class::SharedArrayBuffer, proto, 0);
    unsafe {
        (*o).elems = bytes_of(b).cast();
        (*o).elems_len = (*b).len.load(SeqCst) as u32;
        if growable {
            (*o).flags |= RESIZABLE;
            *Object::aux(o) = (*b).room as u32;
            let views = crate::collections::new_weak_set(vm)?;
            (*o).code = object_value(views).to_bits() as *const crate::vm::Code;
        }
    }
    let live = vm.shared.live;
    crate::collections::weak_set_insert(vm, live, object_value(o))?;
    Ok(object_value(o))
}

/// Shared buffer `o`'s length, as its block has it now (another agent
/// may have grown it): its views brought up to date to it.
pub(crate) fn sync_length(o: *mut Object) -> usize {
    let n = unsafe { (*block_of(o)).len.load(SeqCst) };
    if n != unsafe { (*o).elems_len } as usize {
        unsafe { (*o).elems_len = n as u32 };
        refresh_views(o);
    }
    n
}

pub(crate) fn is_shared(o: *mut Object) -> bool {
    unsafe { (*o).class == Class::SharedArrayBuffer }
}

// ---- SharedArrayBuffer ----

fn this_shared(this: Value, what: &str) -> Result<*mut Object> {
    if is_class(this, Class::SharedArrayBuffer) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error(format!("SharedArrayBuffer.prototype.{what} called on an incompatible receiver")))
    }
}

/// `new SharedArrayBuffer(length, {maxByteLength})`.
fn shared_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor SharedArrayBuffer requires 'new'"));
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
        return Err(Error::range_error("SharedArrayBuffer length exceeds its maxByteLength"));
    }
    let c = vm.shared.constructor;
    let proto = vm.take_new_target(c, object_value(vm.shared.prototype))?;
    let room = max.unwrap_or(n);
    if n > MAX_BYTES || room > if max.is_some() { MAX_RESIZABLE } else { MAX_BYTES } {
        return Err(Error::range_error("Array buffer allocation failed"));
    }
    let b = new_block(n, room).ok_or_else(|| Error::range_error("Array buffer allocation failed"))?;
    let o = new_shared(vm, b, max.is_some())?;
    vm.set_prototype_of(as_object(o), proto);
    Ok(o)
}

fn shared_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::number(sync_length(this_shared(this, "byteLength")?) as f64))
}

fn shared_growable(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(is_resizable(this_shared(this, "growable")?)))
}

fn shared_max_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_shared(this, "maxByteLength")?;
    let n = if is_resizable(o) { unsafe { (*block_of(o)).room } } else { sync_length(o) };
    Ok(Value::number(n as f64))
}

/// `buffer.grow(newLength)`: never shorter.
fn shared_grow(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_shared(this, "grow")?;
    if !is_resizable(o) {
        return Err(Error::type_error("SharedArrayBuffer.prototype.grow: the buffer is not growable"));
    }
    let n = to_index(vm, arg(a, 0))?;
    let b = block_of(o);
    let room = unsafe { (*b).room };
    if n > room {
        return Err(Error::range_error("SharedArrayBuffer.prototype.grow: beyond maxByteLength"));
    }
    let len = unsafe { &(*b).len };
    let mut current = len.load(SeqCst);
    loop {
        if n < current {
            return Err(Error::range_error("SharedArrayBuffer.prototype.grow: a buffer can't shrink"));
        }
        // (Its bytes past its length are zero: it never shrinks.)
        match len.compare_exchange(current, n, SeqCst, SeqCst) {
            Ok(_) => break,
            Err(now) => current = now,
        }
    }
    sync_length(o);
    Ok(Value::UNDEFINED)
}

/// `buffer.slice(begin, end)`: a copy of those bytes, in a shared buffer
/// of the species.
fn shared_slice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_shared(this, "slice")?;
    let len = sync_length(o);
    let first = relative(integer(vm, arg(a, 0))?, len);
    let end = arg(a, 1);
    let last = if end.is_undefined() { len } else { relative(integer(vm, end)?, len) };
    let n = last.saturating_sub(first);
    let default = object_value(vm.shared.constructor);
    let c = species_constructor(vm, this, default)?;
    let out = crate::intrinsics::construct(vm, c, &[Value::number(n as f64)])?;
    if !is_class(out, Class::SharedArrayBuffer) {
        return Err(Error::type_error("SharedArrayBuffer.prototype.slice: the species made no SharedArrayBuffer"));
    }
    let p = as_object(out);
    if p == o {
        return Err(Error::type_error("SharedArrayBuffer.prototype.slice: the species returned the same buffer"));
    }
    if sync_length(p) < n {
        return Err(Error::type_error("SharedArrayBuffer.prototype.slice: the new buffer is too small"));
    }
    unsafe { crate::racy::copy((*o).elems.cast::<u8>().add(first), (*p).elems.cast::<u8>(), n) };
    Ok(out)
}

// ---- Atomics ----

/// ValidateIntegerTypedArray and ValidateAtomicAccess: typed array `ta`
/// (of an integer kind; `waitable`: `Int32Array` or `BigInt64Array`) in
/// bounds (`write`: of a mutable buffer), and `index` converted, an
/// element's index in it.
fn access(vm: &mut Vm, ta: Value, index: Value, waitable: bool, write: bool) -> Result<(*mut Object, TypedKind, usize)> {
    if !is_class(ta, Class::TypedArray) {
        return Err(Error::type_error("Atomics: the argument is not a typed array"));
    }
    let o = as_object(ta);
    let Some(len) = ta_length(o) else {
        return Err(Error::type_error("Atomics: the typed array is detached or out of bounds"));
    };
    if write {
        crate::typed::writable_view(o)?;
    }
    let k = kind_of(o);
    let fits = if waitable {
        matches!(k, TypedKind::I32 | TypedKind::I64)
    } else {
        !matches!(k, TypedKind::U8Clamped | TypedKind::F16 | TypedKind::F32 | TypedKind::F64)
    };
    if !fits {
        return Err(Error::type_error(format!("Atomics: {} is not an integer typed array it works on", k.name())));
    }
    let i = to_index(vm, index)?;
    if i >= len {
        return Err(Error::range_error("Atomics: index out of range"));
    }
    Ok((o, k, i))
}

/// RevalidateAtomicAccess: the conversions may have detached or shrunk
/// the buffer.
fn revalidate(o: *mut Object, i: usize) -> Result<()> {
    match ta_length(o) {
        None => Err(Error::type_error("Atomics: the typed array is detached or out of bounds")),
        Some(n) if i >= n => Err(Error::range_error("Atomics: index out of range")),
        _ => Ok(()),
    }
}

/// `v` converted for an element of kind `k`: its bits (the low ones the
/// element's), and the value an operation returns of it (an integer, or
/// a BigInt).
fn operand(vm: &mut Vm, k: TypedKind, v: Value) -> Result<(u64, Value)> {
    if k.is_big() {
        let v = crate::typed::to_element(vm, k, v)?;
        return Ok((crate::bigint::to_bits(v), v));
    }
    let d = integer(vm, v)?;
    Ok((crate::interp::to_int32(d) as u32 as u64, Value::number(d)))
}

#[derive(Clone, Copy)]
enum Op {
    Load,
    Store,
    Add,
    Sub,
    And,
    Or,
    Xor,
    Exchange,
    /// Its expected value's bits.
    CompareExchange(u64),
}

/// Operation `op` with bits `v` on element `i` of typed array `o`: the
/// element's bits before.
fn apply(o: *mut Object, k: TypedKind, i: usize, op: Op, v: u64) -> u64 {
    macro_rules! with {
        ($a:ty, $t:ty) => {{
            let a = unsafe { <$a>::from_ptr((*o).elems.cast::<$t>().add(i)) };
            let v = v as $t;
            (match op {
                Op::Load => a.load(SeqCst),
                Op::Store => {
                    a.store(v, SeqCst);
                    v
                }
                Op::Add => a.fetch_add(v, SeqCst),
                Op::Sub => a.fetch_sub(v, SeqCst),
                Op::And => a.fetch_and(v, SeqCst),
                Op::Or => a.fetch_or(v, SeqCst),
                Op::Xor => a.fetch_xor(v, SeqCst),
                Op::Exchange => a.swap(v, SeqCst),
                Op::CompareExchange(e) => match a.compare_exchange(e as $t, v, SeqCst, SeqCst) {
                    Ok(x) | Err(x) => x,
                },
            }) as u64
        }};
    }
    match k.size() {
        1 => with!(AtomicI8, i8),
        2 => with!(AtomicI16, i16),
        4 => with!(AtomicI32, i32),
        _ => with!(AtomicI64, i64),
    }
}

/// Bits `bits` of an element of kind `k`, as a value.
fn element(k: TypedKind, bits: u64) -> Value {
    let bytes = bits.to_ne_bytes();
    let at = if cfg!(target_endian = "big") { 8 - k.size() } else { 0 };
    unsafe { k.load(bytes.as_ptr().add(at), 0) }
}

/// AtomicReadModifyWrite.
fn read_modify_write(vm: &mut Vm, a: &[Value], op: Op) -> Result<Value> {
    let (o, k, i) = access(vm, arg(a, 0), arg(a, 1), false, true)?;
    let (v, _) = operand(vm, k, arg(a, 2))?;
    revalidate(o, i)?;
    Ok(element(k, apply(o, k, i, op, v)))
}

fn atomics_add(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    read_modify_write(vm, a, Op::Add)
}

fn atomics_sub(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    read_modify_write(vm, a, Op::Sub)
}

fn atomics_and(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    read_modify_write(vm, a, Op::And)
}

fn atomics_or(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    read_modify_write(vm, a, Op::Or)
}

fn atomics_xor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    read_modify_write(vm, a, Op::Xor)
}

fn atomics_exchange(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    read_modify_write(vm, a, Op::Exchange)
}

fn atomics_compare_exchange(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (o, k, i) = access(vm, arg(a, 0), arg(a, 1), false, true)?;
    let (expected, _) = operand(vm, k, arg(a, 2))?;
    let (replacement, _) = operand(vm, k, arg(a, 3))?;
    revalidate(o, i)?;
    Ok(element(k, apply(o, k, i, Op::CompareExchange(expected), replacement)))
}

fn atomics_load(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (o, k, i) = access(vm, arg(a, 0), arg(a, 1), false, false)?;
    revalidate(o, i)?;
    Ok(element(k, apply(o, k, i, Op::Load, 0)))
}

/// `Atomics.store(ta, index, value)`: the value converted (not wrapped
/// to the element's type).
fn atomics_store(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (o, k, i) = access(vm, arg(a, 0), arg(a, 1), false, true)?;
    let (v, out) = operand(vm, k, arg(a, 2))?;
    revalidate(o, i)?;
    apply(o, k, i, Op::Store, v);
    Ok(out)
}

fn atomics_is_lock_free(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let n = integer(vm, arg(a, 0))?;
    Ok(Value::from_bool(matches!(n as i64, 1 | 2 | 4 | 8) && n.fract() == 0.0))
}

/// `Atomics.pause(n)`: a spin-wait hint (`n` an integral number, if
/// given).
fn atomics_pause(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let n = arg(a, 0);
    if !n.is_undefined() {
        if !n.is_number() {
            return Err(Error::type_error("Atomics.pause: the argument is not a number"));
        }
        let d = n.to_number();
        if !d.is_finite() || d.trunc() != d {
            return Err(Error::type_error("Atomics.pause: the argument is not an integral number"));
        }
    }
    std::hint::spin_loop();
    Ok(Value::UNDEFINED)
}

// ---- Waiting ----

/// A waiter, on element `index` (a byte index) of a block: notified or
/// not, and the condition variable to signal (a blocked agent's own, or
/// an async waiter's VM's `State::signal`).
pub(crate) struct Waiter {
    block: usize,
    index: usize,
    notified: AtomicBool,
    deadline: Option<Instant>,
    wake: Arc<Condvar>,
}

/// Every waiter in the process, in the order they came.
static WAITERS: Mutex<Vec<Arc<Waiter>>> = Mutex::new(Vec::new());

fn waiters() -> std::sync::MutexGuard<'static, Vec<Arc<Waiter>>> {
    WAITERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the agent may block (`Atomics.wait`): test262's
/// `[CanBlockIsTrue]` for the main one, as a shell has it.
pub(crate) fn can_block(vm: &Vm) -> bool {
    vm.can_block
}

/// DoWait: `Atomics.wait` (`!async_`) or `Atomics.waitAsync`.
fn do_wait(vm: &mut Vm, a: &[Value], async_: bool) -> Result<Value> {
    let (o, k, i) = access_shared(vm, arg(a, 0), arg(a, 1))?;
    let v = if k == TypedKind::I64 {
        let v = crate::typed::to_element(vm, k, arg(a, 2))?;
        crate::bigint::to_bits(v)
    } else {
        let d = crate::typed::to_element(vm, k, arg(a, 2))?.to_number();
        crate::interp::to_int32(d) as u32 as u64
    };
    let q = crate::typed::to_element(vm, TypedKind::F64, arg(a, 3))?.to_number();
    let t = if q.is_nan() { f64::INFINITY } else { q.max(0.0) };
    if !async_ && !can_block(vm) {
        return Err(Error::type_error("Atomics.wait: this agent can't block"));
    }
    let b = crate::typed::buffer_of(o);
    let block = block_of(b) as usize;
    let index = i * k.size() + crate::typed::offset_of(o);
    let deadline = (t.is_finite()).then(|| Instant::now() + Duration::from_secs_f64(t / 1000.0));
    let mut list = waiters();
    let w = apply(o, k, i, Op::Load, 0);
    let mask = if k.size() == 8 { u64::MAX } else { u32::MAX as u64 };
    if w & mask != v & mask {
        drop(list);
        let s = str_value(vm, "not-equal");
        return if async_ { wait_result(vm, false, s) } else { Ok(s) };
    }
    if async_ {
        if t == 0.0 {
            drop(list);
            let s = str_value(vm, "timed-out");
            return wait_result(vm, false, s);
        }
        let waiter = Arc::new(Waiter { block, index, notified: AtomicBool::new(false), deadline, wake: vm.shared.signal.clone() });
        list.push(waiter.clone());
        drop(list);
        let (p, resolve, _) = crate::promise::new_with_resolvers(vm);
        vm.shared.waits.push((waiter, p, resolve));
        return wait_result(vm, true, p);
    }
    let waiter = Arc::new(Waiter { block, index, notified: AtomicBool::new(false), deadline, wake: Arc::new(Condvar::new()) });
    list.push(waiter.clone());
    loop {
        if waiter.notified.load(SeqCst) {
            return Ok(str_value(vm, "ok"));
        }
        let now = Instant::now();
        match deadline {
            Some(d) if now >= d => {
                list.retain(|x| !Arc::ptr_eq(x, &waiter));
                drop(list);
                return Ok(str_value(vm, "timed-out"));
            }
            Some(d) => list = waiter.wake.wait_timeout(list, d - now).unwrap_or_else(|e| e.into_inner()).0,
            None => list = waiter.wake.wait(list).unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// ValidateIntegerTypedArray (waitable), of a shared buffer, and
/// ValidateAtomicAccess.
fn access_shared(vm: &mut Vm, ta: Value, index: Value) -> Result<(*mut Object, TypedKind, usize)> {
    if is_class(ta, Class::TypedArray) {
        let o = as_object(ta);
        if ta_length(o).is_some()
            && matches!(kind_of(o), TypedKind::I32 | TypedKind::I64)
            && !is_shared(crate::typed::buffer_of(o))
        {
            return Err(Error::type_error("Atomics.wait: the typed array's buffer is not shared"));
        }
    }
    access(vm, ta, index, true, false)
}

/// `{async, value}`: what `Atomics.waitAsync` returns.
fn wait_result(vm: &mut Vm, async_: bool, value: Value) -> Result<Value> {
    let op = object_value(vm.object_prototype);
    let r = vm.new_object(op);
    let a_async = vm.intern("async");
    let a_value = vm.intern("value");
    vm.add_property(r, a_async, Value::from_bool(async_));
    vm.add_property(r, a_value, value);
    Ok(object_value(r))
}

fn atomics_wait(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    do_wait(vm, a, false)
}

fn atomics_wait_async(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    do_wait(vm, a, true)
}

/// `Atomics.notify(ta, index, count)`: up to `count` (all) waiters on
/// the element woken, in the order they came; how many.
fn atomics_notify(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (o, k, i) = access(vm, arg(a, 0), arg(a, 1), true, false)?;
    let count = arg(a, 2);
    let c = if count.is_undefined() { f64::INFINITY } else { integer(vm, count)?.max(0.0) };
    let b = crate::typed::buffer_of(o);
    if !is_shared(b) {
        return Ok(Value::from_i32(0));
    }
    let block = block_of(b) as usize;
    let index = i * k.size() + crate::typed::offset_of(o);
    let mut list = waiters();
    let mut n = 0usize;
    list.retain(|w| {
        if (n as f64) < c && w.block == block && w.index == index {
            n += 1;
            w.notified.store(true, SeqCst);
            w.wake.notify_all();
            false
        } else {
            true
        }
    });
    Ok(Value::number(n as f64))
}

/// Settle the VM's async waiters notified or timed out: their promises
/// resolved (to `"ok"`, `"timed-out"`), which queues their reactions.
/// Whether any is still waiting.
pub(crate) fn poll(vm: &mut Vm) -> Result<bool> {
    let now = Instant::now();
    let mut done = Vec::new();
    {
        let mut list = waiters();
        vm.shared.waits.retain(|(w, p, resolve)| {
            let outcome = if w.notified.load(SeqCst) {
                "ok"
            } else if w.deadline.is_some_and(|d| now >= d) {
                list.retain(|x| !Arc::ptr_eq(x, w));
                "timed-out"
            } else {
                return true;
            };
            done.push((w.deadline, *p, *resolve, outcome));
            false
        });
    }
    // (Those timed out in the order of their deadlines.)
    done.sort_by_key(|&(d, _, _, outcome)| (outcome == "timed-out", d));
    vm.handle_scope(|vm| {
        for &(_, p, resolve, _) in &done {
            vm.handles.push(p);
            vm.handles.push(resolve);
        }
        for (_, _, resolve, outcome) in done {
            let s = str_value(vm, outcome);
            crate::interp::call_function(vm, resolve, Value::UNDEFINED, &[s])?;
        }
        Ok(())
    })?;
    Ok(vm.shared.waiting())
}

/// Block until one of the VM's async waiters is notified or times out.
pub(crate) fn idle(vm: &mut Vm) {
    let list = waiters();
    if vm.shared.waits.iter().any(|(w, _, _)| w.notified.load(SeqCst)) {
        return;
    }
    let first = vm.shared.waits.iter().filter_map(|(w, _, _)| w.deadline).min();
    let signal = vm.shared.signal.clone();
    match first {
        Some(d) => {
            let now = Instant::now();
            if d > now {
                drop(signal.wait_timeout(list, d - now));
            }
        }
        None => drop(signal.wait(list)),
    }
}

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let any = crate::jit::Ty::Any;
    let sp = vm.new_object(op);
    vm.shared.prototype = sp;
    let c = vm.define_native(g, "SharedArrayBuffer", shared_ctor, 1, any, true);
    vm.shared.constructor = c;
    crate::builtins::link_constructor(vm, c, sp);
    getter(vm, sp, "byteLength", shared_byte_length);
    getter(vm, sp, "growable", shared_growable);
    getter(vm, sp, "maxByteLength", shared_max_byte_length);
    vm.define_natives(sp, &[("grow", shared_grow as NativeFn, 1), ("slice", shared_slice, 2)]);

    let atomics = vm.new_object(op);
    let a_atomics = vm.intern("Atomics");
    vm.add_property_with(g, a_atomics, object_value(atomics), crate::object::DONT_ENUM);
    vm.define_natives(
        atomics,
        &[
            ("add", atomics_add as NativeFn, 3),
            ("and", atomics_and, 3),
            ("compareExchange", atomics_compare_exchange, 4),
            ("exchange", atomics_exchange, 3),
            ("isLockFree", atomics_is_lock_free, 1),
            ("load", atomics_load, 2),
            ("notify", atomics_notify, 3),
            ("or", atomics_or, 3),
            ("pause", atomics_pause, 0),
            ("store", atomics_store, 3),
            ("sub", atomics_sub, 3),
            ("wait", atomics_wait, 4),
            ("waitAsync", atomics_wait_async, 4),
            ("xor", atomics_xor, 3),
        ],
    );
    vm.shared_atomics = atomics;
}

/// `[@@toStringTag]`s (see `typed::install_symbols`).
pub(crate) fn install_symbols(vm: &mut Vm) {
    let sp = vm.shared.prototype;
    crate::symbol::to_string_tag(vm, sp, "SharedArrayBuffer");
    let atomics = vm.shared_atomics;
    crate::symbol::to_string_tag(vm, atomics, "Atomics");
}
