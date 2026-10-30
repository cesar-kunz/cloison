//! Generators: a generator function's frame, suspended at each `yield`.
//!
//! Its code starts as any function's; once its parameters are set,
//! `InitialYield` makes the generator object, keeps the frame's registers
//! and where to go on in it, and returns the object. `next(v)` (and
//! `throw`, `return`) builds a frame of those registers again at the VM
//! stack's free top and runs it from there, `v` in `acc`; `Yield` keeps
//! them again and returns the value yielded. A generator keeps its
//! variables in its scope object (see `codegen::generate_from`), so no
//! closure points into a frame that moves.
//!
//! The object (class `Generator`) keeps a hidden array in `code`:
//! `[state, registers, pc, function, mode]`.

use crate::interp::{native_stack_low, write_frame_header};
use crate::object::{Class, DONT_DELETE, DONT_ENUM, Object, as_object, object_value};
use crate::value::Value;
use crate::vm::{Code, Error, Result, Vm};

const STATE: i32 = 0;
const PC: i32 = 2;
const FUNCTION: i32 = 3;
const MODE: i32 = 4;

const SUSPENDED_START: i32 = 0;
const SUSPENDED_YIELD: i32 = 1;
const EXECUTING: i32 = 2;
const COMPLETED: i32 = 3;

/// An async generator's cells past a generator's: its requests (`[mode,
/// value, resolve, reject]` each, oldest first), whether it last
/// suspended at a `yield` (else at an `await`), whether a request is
/// being run, and the mark of one (an async function's coroutine uses
/// cells 5 to 7 for its promise).
const AG_QUEUE: i32 = 5;
const AG_YIELD: i32 = 6;
const AG_BUSY: i32 = 7;
const AG_MARK: i32 = 8;
/// A request's width.
const REQUEST: usize = 4;

/// How a generator resumes: `next(v)`, `throw(e)`, `return(v)`.
pub(crate) const NEXT: i32 = 0;
pub(crate) const THROW: i32 = 1;
pub(crate) const RETURN: i32 = 2;

/// Generator `g`'s state: a buffer of values (`Heap::alloc_values`, its
/// `code`), its cells (`STATE`…, an async function's promise, an async
/// generator's `AG_…`) then its frame's registers while suspended (see
/// [`regs_at`]). (An array for the cells and another for the registers at
/// each suspension took a pending `async` call 4 cells, of 9.)
fn state_of(g: Value) -> Option<*mut Value> {
    if g.is_cell() && unsafe { (*as_object(g)).class } == Class::Generator {
        Some(unsafe { (*as_object(g)).code } as *mut Value)
    } else {
        None
    }
}

/// Cells of a generator's state before its registers: an async
/// generator's, else 6 (a generator's, and an async function's promise:
/// also an async module's, whose code's kind is a module's).
fn cells_for(kind: u8) -> usize {
    if kind == 6 { AG_MARK as usize + 1 } else { 6 }
}

/// The state's capacity.
fn cap(st: *mut Value) -> usize {
    (unsafe { *st.cast::<u64>().sub(1) } >> 32) as usize
}

/// Where the state's registers start: its cells past, for `code`'s.
fn regs_at(st: *mut Value, code: *const Code) -> *mut Value {
    unsafe { st.add(cap(st) - (*code).nregs as usize) }
}

/// The function of state `st`'s generator (its `FUNCTION` cell).
fn function_of(st: *mut Value) -> *mut Object {
    as_object(unsafe { *st.add(FUNCTION as usize) })
}

/// Whether state `st` is an async generator's (its function's kind).
fn is_async_generator(st: *mut Value) -> bool {
    unsafe { (*(*function_of(st)).code).kind == 6 }
}

/// Cell `k` of state `st` (undefined past its cells: its registers follow).
fn get(_: &mut Vm, st: *mut Value, k: i32) -> Value {
    let cells = cells_for(unsafe { (*(*function_of(st)).code).kind });
    if (k as usize) < cells { unsafe { *st.add(k as usize) } } else { Value::UNDEFINED }
}

fn set(_: &mut Vm, st: *mut Value, k: i32, v: Value) {
    unsafe {
        let at = st.add(k as usize);
        *at = v;
        crate::heap::barrier(st.cast::<u8>().sub(8), at, v);
    }
}

/// Element `k` of an async generator's request queue (an array).
fn qget(vm: &mut Vm, q: Value, k: i32) -> Value {
    vm.get_keyed(q, Value::from_i32(k)).unwrap_or(Value::UNDEFINED)
}

/// Drop the registers kept in state `st` (of `code`'s frame): done.
fn clear_regs(st: *mut Value, code: *const Code) {
    let r = regs_at(st, code);
    for i in 0..unsafe { (*code).nregs } as usize {
        unsafe { *r.add(i) = Value::UNDEFINED };
    }
}

/// A new generator object for function `f`: its prototype `f.prototype`
/// (or `%GeneratorPrototype%`).
pub(crate) fn create(vm: &mut Vm, f: *mut Object) -> Result<Value> {
    let a_prototype = vm.a_prototype;
    let p = vm.get(object_value(f), a_prototype)?;
    // (An async generator function's: kind 6, see `vm::Code::kind`.)
    let is_async = unsafe { (*(*f).code).kind } == 6;
    let root = if is_async { crate::symbol::Root::AsyncGeneratorPrototype } else { crate::symbol::Root::GeneratorPrototype };
    let proto = if crate::props::is_object(p) { p } else { object_value(vm.symbol_roots[root as usize]) };
    let code = unsafe { (*f).code };
    let n = cells_for(unsafe { (*code).kind }) + unsafe { (*code).nregs } as usize;
    let queue = if is_async { vm.new_array() } else { Value::UNDEFINED };
    let g = vm.new_object_with(Class::Generator, proto, 0);
    let st = vm.heap.alloc_values(n);
    let mut cells = vec![Value::from_i32(SUSPENDED_START), Value::UNDEFINED, Value::from_i32(0), object_value(f), Value::from_i32(NEXT)];
    if is_async {
        cells.extend([queue, Value::FALSE, Value::FALSE, Value::TRUE]);
    }
    unsafe {
        for (k, &v) in cells.iter().enumerate() {
            *st.add(k) = v;
        }
        (*g).code = st as *const Code;
        crate::heap::barrier_young(g.cast(), &raw const (*g).code);
    }
    Ok(object_value(g))
}

/// Keep frame `regs` (of `code`, at offset `pc` to go on from) in
/// generator `g`, now in `state` (`yield`ed or not started).
///
/// # Safety
/// `regs` must be a frame of `code`.
pub(crate) unsafe fn suspend(vm: &mut Vm, g: Value, regs: *mut Value, code: *const Code, pc: usize, started: bool) {
    let Some(st) = state_of(g) else { return };
    let n = unsafe { (*code).nregs } as usize;
    let to = regs_at(st, code);
    for i in 0..n {
        unsafe {
            let v = *regs.add(i);
            *to.add(i) = v;
            crate::heap::barrier(st.cast::<u8>().sub(8), to.add(i), v);
        }
    }
    set(vm, st, PC, Value::from_i32(pc as i32));
    set(vm, st, STATE, Value::from_i32(if started { SUSPENDED_YIELD } else { SUSPENDED_START }));
}

/// Resume generator `g` (`mode` [`NEXT`]…, with `v`): the iterator
/// result.
fn resume(vm: &mut Vm, g: Value, v: Value, mode: i32, what: &str) -> Result<Value> {
    let (out, done) = resume_raw(vm, g, v, mode, what)?;
    crate::symbol::iter_result(vm, out, done)
}

/// [`resume`]'s value and whether the generator is done.
fn resume_raw(vm: &mut Vm, g: Value, v: Value, mode: i32, what: &str) -> Result<(Value, bool)> {
    let Some(st) = state_of(g) else {
        return Err(Error::type_error(format!("{what} called on an incompatible receiver")));
    };
    let state = get(vm, st, STATE).as_i32_unchecked();
    if state == EXECUTING {
        return Err(Error::type_error("Generator is already running"));
    }
    if state == SUSPENDED_START && mode != NEXT {
        set(vm, st, STATE, Value::from_i32(COMPLETED));
        clear_regs(st, unsafe { (*as_object(get(vm, st, FUNCTION))).code });
    }
    if get(vm, st, STATE).as_i32_unchecked() == COMPLETED {
        return match mode {
            THROW => Err(Error(String::new(), Some(v))),
            RETURN => Ok((v, true)),
            _ => Ok((Value::UNDEFINED, true)),
        };
    }
    set(vm, st, STATE, Value::from_i32(EXECUTING));
    set(vm, st, MODE, Value::from_i32(mode));
    let f = get(vm, st, FUNCTION);
    let pc = get(vm, st, PC).as_i32_unchecked() as usize;
    let r = unsafe { run(vm, as_object(f), regs_at(st, (*as_object(f)).code), pc, v) };
    match r {
        Ok(out) => {
            let yielded = get(vm, st, STATE).as_i32_unchecked() == SUSPENDED_YIELD;
            if !yielded {
                set(vm, st, STATE, Value::from_i32(COMPLETED));
                clear_regs(st, unsafe { (*as_object(get(vm, st, FUNCTION))).code });
            }
            Ok((out, !yielded))
        }
        Err(e) => {
            set(vm, st, STATE, Value::from_i32(COMPLETED));
            clear_regs(st, unsafe { (*as_object(get(vm, st, FUNCTION))).code });
            Err(e)
        }
    }
}

/// Run module generator `g` (see `module`) to its end.
pub(crate) fn run_module(vm: &mut Vm, g: Value) -> Result<()> {
    resume_raw(vm, g, Value::UNDEFINED, NEXT, "a module").map(drop)
}

/// Run function `f`'s code from offset `pc` in a frame of registers
/// `saved` (a generator state's), `acc` `v`: what it returns or yields.
unsafe fn run(vm: &mut Vm, f: *mut Object, saved: *mut Value, pc: usize, v: Value) -> Result<Value> {
    unsafe {
        let top = vm.native_top;
        if top.is_null() {
            return Err(Error::unsupported("resuming a generator from here (compiled code)"));
        }
        vm.safepoint(top);
        let code = (*f).code;
        let new = top.add(crate::interp::HEADER);
        let n = (*code).nregs as usize;
        if new.add(n) > vm.stack_end || native_stack_low(vm) {
            return Err(Error::range_error("Maximum call stack size exceeded"));
        }
        write_frame_header(vm, new, code, std::ptr::null(), std::ptr::null_mut(), 0);
        crate::interp::set_frame_function(new, f);
        for i in 0..n {
            let r = *saved.add(i);
            *new.add(i) = if r == Value::EMPTY { Value::UNDEFINED } else { r };
        }
        let saved_top = vm.native_top;
        vm.native_top = std::ptr::null_mut();
        let start = crate::interp::Resume { regs: new, pc: (*code).bytecode.as_ptr().add(pc), acc: v };
        let out = crate::interp::run_in_realm(vm, &*code, new, start);
        vm.native_top = saved_top;
        if out == Value::EMPTY {
            return Err(vm.error.take().unwrap_or_else(|| Error::unsupported("a generator's run")));
        }
        Ok(out)
    }
}

/// An async function's coroutine `g` started: its promise, the
/// coroutine run until it awaits or ends (see [`async_step`]).
pub(crate) fn async_start(vm: &mut Vm, g: Value) -> Result<Value> {
    // (No resolving functions: only its end settles it, `async_step`.)
    let p = crate::promise::new_promise(vm);
    let st = state_of(g).unwrap();
    set(vm, st, 5, p);
    async_step(vm, g, NEXT, Value::UNDEFINED)?;
    Ok(p)
}

/// Run async coroutine `g` on (`mode`, `v`): its end resolves its
/// promise, its exception rejects it, an `await` waits for the promise
/// of the value awaited, whose reactions run this again.
pub(crate) fn async_step(vm: &mut Vm, g: Value, mode: i32, v: Value) -> Result<()> {
    let st = state_of(g).unwrap();
    if is_async_generator(st) {
        return async_gen_step(vm, g, mode, v);
    }
    match resume_raw(vm, g, v, mode, "an async function") {
        Ok((out, true)) => {
            let p = get(vm, st, 5);
            crate::promise::resolve_own(vm, p, out)
        }
        Ok((awaited, false)) => {
            crate::promise::await_coroutine(vm, awaited, g)
        }
        Err(e) => {
            let reason = match e.1 {
                Some(v) => v,
                None => crate::builtins::caught(vm, &e),
            };
            let p = get(vm, st, 5);
            crate::promise::reject_own(vm, p, reason)
        }
    }
}

/// The value of async generator `[g]`'s coming suspension is `yield`ed
/// (not awaited).
pub(crate) fn async_gen_yield(vm: &mut Vm, g: Value) {
    if let Some(st) = state_of(g) {
        set(vm, st, AG_YIELD, Value::TRUE);
    }
}

/// `next(v)`, `throw(v)` or `return(v)` (`mode`) of async generator `g`: a
/// promise for the iterator result, the request queued (and run unless
/// one is).
fn async_gen_enqueue(vm: &mut Vm, g: Value, v: Value, mode: i32) -> Result<Value> {
    let (p, resolve, reject) = crate::promise::new_with_resolvers(vm);
    let st = match state_of(g) {
        Some(st) if is_async_generator(st) => st,
        _ => {
            let e = crate::builtins::caught(vm, &Error::type_error("not an async generator"));
            crate::interp::call_function(vm, reject, Value::UNDEFINED, &[e])?;
            return Ok(p);
        }
    };
    let q = get(vm, st, AG_QUEUE);
    crate::vm::array_push(vm, q, &[Value::from_i32(mode), v, resolve, reject])?;
    if get(vm, st, AG_BUSY) != Value::TRUE {
        async_gen_run(vm, g)?;
    }
    Ok(p)
}

/// Run async generator `g`'s oldest request, if any.
fn async_gen_run(vm: &mut Vm, g: Value) -> Result<()> {
    let st = state_of(g).unwrap();
    let q = get(vm, st, AG_QUEUE);
    if vm.length_of(q)? == 0.0 {
        set(vm, st, AG_BUSY, Value::FALSE);
        return Ok(());
    }
    set(vm, st, AG_BUSY, Value::TRUE);
    let mode = qget(vm, q, 0).as_i32_unchecked();
    let v = qget(vm, q, 1);
    async_gen_step(vm, g, mode, v)
}

/// Resume async generator `g` (`mode`, `v`) until it awaits (the
/// reactions resuming it again: see [`async_step`]) or settles its oldest
/// request, by a `yield`, its end or its exception; the next request runs
/// then.
fn async_gen_step(vm: &mut Vm, g: Value, mode: i32, v: Value) -> Result<()> {
    let st = state_of(g).unwrap();
    let settled = match resume_raw(vm, g, v, mode, "an async generator") {
        Ok((out, false)) if get(vm, st, AG_YIELD) == Value::TRUE => {
            set(vm, st, AG_YIELD, Value::FALSE);
            Ok((out, false))
        }
        Ok((awaited, false)) => {
            return crate::promise::await_coroutine(vm, awaited, g);
        }
        Ok((out, true)) => Ok((out, true)),
        Err(e) => Err(match e.1 {
            Some(v) => v,
            None => crate::builtins::caught(vm, &e),
        }),
    };
    let q = get(vm, st, AG_QUEUE);
    let (resolve, reject) = (qget(vm, q, 2), qget(vm, q, 3));
    let len = vm.length_of(q)? as i32;
    let rest: Vec<Value> = (REQUEST as i32..len).map(|i| qget(vm, q, i)).collect();
    let rest = crate::array::new_array_of(vm, &rest)?;
    set(vm, st, AG_QUEUE, rest);
    match settled {
        Ok((v, done)) => {
            let r = crate::symbol::iter_result(vm, v, done)?;
            crate::interp::call_function(vm, resolve, Value::UNDEFINED, &[r])?;
        }
        Err(reason) => {
            crate::interp::call_function(vm, reject, Value::UNDEFINED, &[reason])?;
        }
    }
    async_gen_run(vm, g)
}

fn async_generator_next(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    async_gen_enqueue(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), NEXT)
}

fn async_generator_return(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    async_gen_enqueue(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), RETURN)
}

fn async_generator_throw(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    async_gen_enqueue(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), THROW)
}

fn return_this(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(this)
}

/// How the generator in `[g]` was resumed (the code after a `yield`
/// reads it: see `codegen::Gen::yield_acc`).
pub(crate) fn mode(vm: &mut Vm, g: Value) -> Value {
    match state_of(g) {
        Some(st) => get(vm, st, MODE),
        None => Value::from_i32(NEXT),
    }
}

fn generator_next(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    resume(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), NEXT, "Generator.prototype.next")
}

fn generator_return(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    resume(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), RETURN, "Generator.prototype.return")
}

fn generator_throw(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    resume(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), THROW, "Generator.prototype.throw")
}

/// `%GeneratorPrototype%`: next, return, throw; its iterator itself.
pub(crate) fn install(vm: &mut Vm) {
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    let gp = vm.new_object(object_value(ip));
    vm.symbol_roots.push(gp);
    // `%AsyncIteratorPrototype%` (its async iterator itself) and
    // `%AsyncGeneratorPrototype%`.
    let op = object_value(vm.object_prototype);
    let aip = vm.new_object(op);
    let a_async_iterator = vm.intern(&format!("{}Symbol.asyncIterator", crate::symbol::SYMBOL_MARK));
    let f = vm.native_function("[Symbol.asyncIterator]", return_this, 0, crate::jit::Ty::Any, false);
    vm.add_property_with(aip, a_async_iterator, object_value(f), DONT_ENUM);
    let agp = vm.new_object(object_value(aip));
    vm.symbol_roots.push(agp);
    vm.define_natives(
        agp,
        &[
            ("next", async_generator_next as crate::vm::NativeFn, 1),
            ("return", async_generator_return, 1),
            ("throw", async_generator_throw, 1),
        ],
    );
    let u: Vec<u16> = "AsyncGenerator".encode_utf16().collect();
    let s = vm.string_constant(&u);
    let a = vm.a_sym_to_string_tag;
    let d = crate::props::Desc::data(s, false, false, true);
    let _ = vm.define_own_property(agp, crate::vm::Key::Name(a), &d);
    vm.define_natives(
        gp,
        &[
            ("next", generator_next as crate::vm::NativeFn, 1),
            ("return", generator_return, 1),
            ("throw", generator_throw, 1),
        ],
    );
    let u: Vec<u16> = "Generator".encode_utf16().collect();
    let s = vm.string_constant(&u);
    let a = vm.a_sym_to_string_tag;
    let d = crate::props::Desc::data(s, false, false, true);
    let _ = vm.define_own_property(gp, crate::vm::Key::Name(a), &d);

    // `%GeneratorFunction%` and `%AsyncFunction%`: constructors (not
    // globals) inheriting `Function`, their prototypes `Function.
    // prototype`'s, the functions' prototypes.
    let fp = object_value(vm.function_prototype);
    let function = {
        let a = vm.intern("Function");
        vm.get(object_value(vm.global), a).unwrap_or(Value::UNDEFINED)
    };
    let (a_p, a_c) = (vm.a_prototype, vm.a_constructor);
    for (name, ctor, instances) in [
        ("GeneratorFunction", crate::builtins::generator_function_ctor as crate::vm::NativeFn, gp),
        ("AsyncFunction", crate::builtins::async_function_ctor, std::ptr::null_mut()),
        ("AsyncGeneratorFunction", crate::builtins::async_generator_function_ctor, agp),
    ] {
        let generator = !instances.is_null();
        let proto = vm.new_object(fp);
        vm.symbol_roots.push(proto);
        let c = vm.native_function(name, ctor, 1, crate::jit::Ty::Any, true);
        vm.symbol_roots.push(c);
        vm.set_prototype_of(c, function);
        let d = crate::props::Desc::data(object_value(proto), false, false, false);
        let _ = vm.define_own_property(c, crate::vm::Key::Name(a_p), &d);
        let d = crate::props::Desc::data(object_value(c), false, false, true);
        let _ = vm.define_own_property(proto, crate::vm::Key::Name(a_c), &d);
        if generator {
            let d = crate::props::Desc::data(object_value(instances), false, false, true);
            let _ = vm.define_own_property(proto, crate::vm::Key::Name(a_p), &d);
            let d = crate::props::Desc::data(object_value(proto), false, false, true);
            let _ = vm.define_own_property(instances, crate::vm::Key::Name(a_c), &d);
        }
        let u: Vec<u16> = name.encode_utf16().collect();
        let s = vm.string_constant(&u);
        let d = crate::props::Desc::data(s, false, false, true);
        let _ = vm.define_own_property(proto, crate::vm::Key::Name(a), &d);
        // (The same capacity as `function_shape`'s: upvalues where
        // compiled code reads them.)
        let root = vm.root_shape(crate::object::Class::Function, object_value(proto), crate::vm::FUNCTION_CAPACITY);
        if !generator {
            vm.async_function_shape = root;
        } else if instances == gp {
            vm.generator_function_shape = vm.transition_with(root, a_p, DONT_ENUM | DONT_DELETE);
        } else {
            vm.async_generator_function_shape = vm.transition_with(root, a_p, DONT_ENUM | DONT_DELETE);
        }
    }
}

