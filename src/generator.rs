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
const REGS: i32 = 1;
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

fn state_of(g: Value) -> Option<Value> {
    if g.is_cell() && unsafe { (*as_object(g)).class } == Class::Generator {
        Some(Value::from_bits(unsafe { (*as_object(g)).code } as u64))
    } else {
        None
    }
}

fn get(vm: &mut Vm, st: Value, k: i32) -> Value {
    vm.get_keyed(st, Value::from_i32(k)).unwrap_or(Value::UNDEFINED)
}

fn set(vm: &mut Vm, st: Value, k: i32, v: Value) {
    let _ = vm.set_keyed(st, Value::from_i32(k), v);
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
    let mut cells =
        vec![Value::from_i32(SUSPENDED_START), Value::UNDEFINED, Value::from_i32(0), object_value(f), Value::from_i32(NEXT)];
    if is_async {
        let queue = vm.new_array();
        cells.extend([queue, Value::FALSE, Value::FALSE, Value::TRUE]);
    }
    let st = crate::array::new_array_of(vm, &cells)?;
    let g = vm.new_object_with(Class::Generator, proto, 0);
    unsafe { (*g).code = st.to_bits() as *const Code };
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
    let values: Vec<Value> = (0..n).map(|i| unsafe { *regs.add(i) }).collect();
    let saved = crate::array::new_array_of(vm, &values).unwrap_or(Value::UNDEFINED);
    set(vm, st, REGS, saved);
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
        set(vm, st, REGS, Value::UNDEFINED);
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
    let saved = get(vm, st, REGS);
    let pc = get(vm, st, PC).as_i32_unchecked() as usize;
    let r = unsafe { run(vm, as_object(f), saved, pc, v) };
    match r {
        Ok(out) => {
            let yielded = get(vm, st, STATE).as_i32_unchecked() == SUSPENDED_YIELD;
            if !yielded {
                set(vm, st, STATE, Value::from_i32(COMPLETED));
                set(vm, st, REGS, Value::UNDEFINED);
            }
            Ok((out, !yielded))
        }
        Err(e) => {
            set(vm, st, STATE, Value::from_i32(COMPLETED));
            set(vm, st, REGS, Value::UNDEFINED);
            Err(e)
        }
    }
}

/// Run module generator `g` (see `module`) to its end.
pub(crate) fn run_module(vm: &mut Vm, g: Value) -> Result<()> {
    resume_raw(vm, g, Value::UNDEFINED, NEXT, "a module").map(drop)
}

/// Run function `f`'s code from offset `pc` in a frame of registers
/// `saved` (an array), `acc` `v`: what it returns or yields.
unsafe fn run(vm: &mut Vm, f: *mut Object, saved: Value, pc: usize, v: Value) -> Result<Value> {
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
        let so = as_object(saved);
        for i in 0..n {
            let r = if (i as u32) < (*so).elems_len { *(*so).elems.add(i) } else { Value::UNDEFINED };
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
    let (p, resolve, reject) = crate::promise::new_with_resolvers(vm);
    let st = state_of(g).unwrap();
    for (k, v) in [(5, p), (6, resolve), (7, reject)] {
        set(vm, st, k, v);
    }
    async_step(vm, g, NEXT, Value::UNDEFINED)?;
    Ok(p)
}

/// Run async coroutine `g` on (`mode`, `v`): its end resolves its
/// promise, its exception rejects it, an `await` waits for the promise
/// of the value awaited, whose reactions run this again.
pub(crate) fn async_step(vm: &mut Vm, g: Value, mode: i32, v: Value) -> Result<()> {
    let st = state_of(g).unwrap();
    if get(vm, st, AG_MARK) == Value::TRUE {
        return async_gen_step(vm, g, mode, v);
    }
    match resume_raw(vm, g, v, mode, "an async function") {
        Ok((out, true)) => {
            let resolve = get(vm, st, 6);
            crate::interp::call_function(vm, resolve, Value::UNDEFINED, &[out]).map(|_| ())
        }
        Ok((awaited, false)) => {
            let on_fulfilled = crate::promise::async_closure(vm, false, g);
            let on_rejected = crate::promise::async_closure(vm, true, g);
            crate::promise::await_then(vm, awaited, on_fulfilled, on_rejected)
        }
        Err(e) => {
            let reason = match e.1 {
                Some(v) => v,
                None => crate::builtins::caught(vm, &e),
            };
            let reject = get(vm, st, 7);
            crate::interp::call_function(vm, reject, Value::UNDEFINED, &[reason]).map(|_| ())
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
        Some(st) if get(vm, st, AG_MARK) == Value::TRUE => st,
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
    let mode = get(vm, q, 0).as_i32_unchecked();
    let v = get(vm, q, 1);
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
            let on_fulfilled = crate::promise::async_closure(vm, false, g);
            let on_rejected = crate::promise::async_closure(vm, true, g);
            return crate::promise::await_then(vm, awaited, on_fulfilled, on_rejected);
        }
        Ok((out, true)) => Ok((out, true)),
        Err(e) => Err(match e.1 {
            Some(v) => v,
            None => crate::builtins::caught(vm, &e),
        }),
    };
    let q = get(vm, st, AG_QUEUE);
    let (resolve, reject) = (get(vm, q, 2), get(vm, q, 3));
    let len = vm.length_of(q)? as i32;
    let rest: Vec<Value> = (REQUEST as i32..len).map(|i| get(vm, q, i)).collect();
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
        let root = vm.root_shape(crate::object::Class::Function, object_value(proto), 2);
        if !generator {
            vm.async_function_shape = root;
        } else if instances == gp {
            vm.generator_function_shape = vm.transition_with(root, a_p, DONT_ENUM | DONT_DELETE);
        } else {
            vm.async_generator_function_shape = vm.transition_with(root, a_p, DONT_ENUM | DONT_DELETE);
        }
    }
}

