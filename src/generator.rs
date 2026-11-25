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

use crate::brand::{Args, Fresh, Scope};
use crate::interp::{native_stack_low, write_frame_header};
use crate::object::{Class, DONT_DELETE, DONT_ENUM, Obj, object_value};
use crate::raw::{Live, ObjectPtr};
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
/// `AG_YIELD` while a `return(v)` of a generator not started or done
/// awaits `v`.
const AWAITING_RETURN: Value = Value::from_i32(RETURN);
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
    if g.is_cell() && Obj::of(g).class() == Class::Generator { Some(Obj::of(g).code() as *mut Value) } else { None }
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
    unsafe { st.add(cap(st) - code.live().nregs as usize) }
}

/// The function of state `st`'s generator (its `FUNCTION` cell).
fn function_of(st: *mut Value) -> Obj {
    Obj::of(unsafe { *st.add(FUNCTION as usize) })
}

/// Whether state `st` is an async generator's (its function's kind).
fn is_async_generator(st: *mut Value) -> bool {
    unsafe { (*(*function_of(st).ptr()).code).kind == 6 }
}

/// Cell `k` of state `st` (undefined past its cells: its registers follow).
fn get(_: &mut Vm, st: *mut Value, k: i32) -> Value {
    let cells = cells_for(unsafe { (*(*function_of(st).ptr()).code).kind });
    if (k as usize) < cells { unsafe { *st.add(k as usize) } } else { Value::UNDEFINED }
}

fn set(_: &mut Vm, st: *mut Value, k: i32, v: Value) {
    unsafe {
        let at = st.add(k as usize);
        crate::heap::store(st.cast::<u8>().sub(8), at, v);
    }
}

/// Element `k` of an async generator's request queue (an array).
fn qget(vm: &mut Vm, q: Value, k: i32) -> Value {
    vm.get_keyed_b(q, Value::from_i32(k)).map(Fresh::take).unwrap_or(Value::UNDEFINED)
}

/// Drop the registers kept in state `st` (of `code`'s frame): done.
fn clear_regs(st: *mut Value, code: *const Code) {
    let r = regs_at(st, code);
    for i in 0..code.live().nregs as usize {
        unsafe { crate::layout::store_word(r.add(i), Value::UNDEFINED) };
    }
}

/// A new generator object for function `f`: its prototype `f.prototype`
/// (or `%GeneratorPrototype%`).
#[inline(always)]
pub(crate) fn create(vm: &mut Vm, f: Obj) -> Result<Fresh<'_>> {
    create_raw(vm, f).map(Fresh::new)
}

fn create_raw(vm: &mut Vm, f: Obj) -> Result<Value> {
    let a_prototype = vm.a_prototype;
    let p = vm.get_atom_b(object_value(f.ptr()), a_prototype)?;
    // (An async generator function's: kind 6, see `vm::Code::kind`.)
    let is_async = f.code().live().kind == 6;
    let root =
        if is_async { crate::symbol::Root::AsyncGeneratorPrototype } else { crate::symbol::Root::GeneratorPrototype };
    let proto =
        if crate::props::is_object(p.peek()) { p.peek() } else { object_value((vm.symbol_roots[root as usize]).ptr()) };
    let code = f.code();
    let n = cells_for(code.live().kind) + code.live().nregs as usize;
    // take: stored into the generator below; only allocation between.
    let queue = if is_async { vm.mu().new_array().v() } else { Value::UNDEFINED };
    // take: only calls that don't collect follow (allocation, stores, the VM's fields).
    let g = vm.mu().new_object_with(Class::Generator, proto, 0).obj();
    let st = vm.heap.alloc_values(n);
    let mut cells = vec![
        Value::from_i32(SUSPENDED_START),
        Value::UNDEFINED,
        Value::from_i32(0),
        object_value(f.ptr()),
        Value::from_i32(NEXT),
    ];
    if is_async {
        cells.extend([queue, Value::FALSE, Value::FALSE, Value::TRUE]);
    }
    unsafe {
        for (k, &v) in cells.iter().enumerate() {
            *st.add(k) = v;
        }
        crate::heap::store_young(g.ptr().cast(), &raw mut (*g.ptr()).code, st as *const Code);
    }
    Ok(object_value(g.ptr()))
}

/// Keep frame `regs` (of `code`, at offset `pc` to go on from) in
/// generator `g`, now in `state` (`yield`ed or not started).
///
/// # Safety
/// `regs` must be a frame of `code`.
pub(crate) unsafe fn suspend(vm: &mut Vm, g: Value, regs: *mut Value, code: *const Code, pc: usize, started: bool) {
    let Some(st) = state_of(g) else { return };
    let n = code.live().nregs as usize;
    let to = regs_at(st, code);
    for i in 0..n {
        unsafe {
            let v = *regs.add(i);
            crate::heap::store(st.cast::<u8>().sub(8), to.add(i), v);
        }
    }
    set(vm, st, PC, Value::from_i32(pc as i32));
    set(vm, st, STATE, Value::from_i32(if started { SUSPENDED_YIELD } else { SUSPENDED_START }));
}

/// Resume generator `g` (`mode` [`NEXT`]…, with `v`): the iterator
/// result.
#[inline(always)]
fn resume<'v>(vm: &'v mut Vm, g: Value, v: Value, mode: i32, what: &str) -> Result<Fresh<'v>> {
    let (out, done) = resume_done(vm, g, v, mode, what)?;
    crate::symbol::iter_result(vm, out, done)
}

/// [`resume`]'s value and whether the generator is done.
fn resume_done(vm: &mut Vm, g: Value, v: Value, mode: i32, what: &str) -> Result<(Value, bool)> {
    let Some(st) = state_of(g) else {
        return Err(Error::type_error(format!("{what} called on an incompatible receiver")));
    };
    let state = get(vm, st, STATE).as_i32_unchecked();
    if state == EXECUTING {
        return Err(Error::type_error("Generator is already running"));
    }
    if state == SUSPENDED_START && mode != NEXT {
        set(vm, st, STATE, Value::from_i32(COMPLETED));
        clear_regs(st, Obj::of(get(vm, st, FUNCTION)).code());
    }
    if get(vm, st, STATE).as_i32_unchecked() == COMPLETED {
        return match mode {
            THROW => Err(Error(String::new(), Some(v), 0)),
            RETURN => Ok((v, true)),
            _ => Ok((Value::UNDEFINED, true)),
        };
    }
    set(vm, st, STATE, Value::from_i32(EXECUTING));
    set(vm, st, MODE, Value::from_i32(mode));
    let f = get(vm, st, FUNCTION);
    let pc = get(vm, st, PC).as_i32_unchecked() as usize;
    // (`g` rooted, its state read after the run; `v` across the safepoint
    // before its frame holds it.)
    let r = {
        let held = Scope::new(vm);
        let _held = held.roots([g, v]);
        unsafe { run(vm, Obj::of(f), regs_at(st, Obj::of(f).code()), pc, v) }
    };
    match r {
        Ok(out) => {
            let yielded = get(vm, st, STATE).as_i32_unchecked() == SUSPENDED_YIELD;
            if !yielded {
                set(vm, st, STATE, Value::from_i32(COMPLETED));
                clear_regs(st, Obj::of(get(vm, st, FUNCTION)).code());
            }
            Ok((out, !yielded))
        }
        Err(e) => {
            set(vm, st, STATE, Value::from_i32(COMPLETED));
            clear_regs(st, Obj::of(get(vm, st, FUNCTION)).code());
            Err(e)
        }
    }
}

/// Run module generator `g` (see `module`) to its end.
pub(crate) fn run_module(vm: &mut Vm, g: Value) -> Result<()> {
    resume_done(vm, g, Value::UNDEFINED, NEXT, "a module").map(drop)
}

/// Run function `f`'s code from offset `pc` in a frame of registers
/// `saved` (a generator state's), `acc` `v`: what it returns or yields.
unsafe fn run(vm: &mut Vm, f: Obj, saved: *mut Value, pc: usize, v: Value) -> Result<Value> {
    unsafe {
        let top = vm.native_top;
        if top.is_null() {
            return Err(Error::unsupported("resuming a generator from here (compiled code)"));
        }
        vm.safepoint(top);
        let code = f.code();
        let new = top.add(crate::interp::HEADER);
        let n = code.live().nregs as usize;
        if new.add(n) > vm.stack_end || native_stack_low(vm) {
            return Err(Error::range_error("Maximum call stack size exceeded"));
        }
        write_frame_header(vm, new, code, std::ptr::null(), std::ptr::null_mut(), 0);
        crate::interp::set_frame_function(new, f.ptr());
        for i in 0..n {
            let r = *saved.add(i);
            *new.add(i) = if r == Value::EMPTY { Value::UNDEFINED } else { r };
        }
        let saved_top = vm.native_top;
        vm.native_top = std::ptr::null_mut();
        let start = crate::interp::Resume { regs: new, pc: code.live().bytecode.as_ptr().add(pc), acc: v };
        let out = crate::interp::run_in_realm(vm, code.live(), new, start);
        vm.native_top = saved_top;
        if out == Value::EMPTY {
            return Err(vm.error.take().unwrap_or_else(|| Error::unsupported("a generator's run")));
        }
        Ok(out)
    }
}

/// An async function's coroutine `g` started: its promise, the
/// coroutine run until it awaits or ends (see [`async_step`]).
#[inline(always)]
pub(crate) fn async_start(vm: &mut Vm, g: Value) -> Result<Fresh<'_>> {
    async_start_raw(vm, g).map(Fresh::new)
}

fn async_start_raw(vm: &mut Vm, g: Value) -> Result<Value> {
    // (No resolving functions: only its end settles it, `async_step`.)
    let p = crate::promise::new_promise(vm);
    let st = state_of(g).unwrap();
    set(vm, st, 5, p);
    {
        let held = Scope::new(vm);
        let _held = held.roots([p]);
        async_step(vm, g, NEXT, Value::UNDEFINED)
    }?;
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
    let (mut mode, mut v) = (mode, v);
    // (`g` rooted: a promise job's handler, only in Rust's copy.)
    let held = Scope::new(vm);
    let _rooted = held.roots([g]);
    loop {
        match resume_done(vm, g, v, mode, "an async function") {
            Ok((out, true)) => {
                let p = get(vm, st, 5);
                return crate::promise::resolve_own(vm, p, out);
            }
            Ok((awaited, false)) => match crate::promise::await_coroutine(vm, awaited, g) {
                Ok(()) => return Ok(()),
                // (PromiseResolve threw: the `await` throws it, now.)
                Err(e) => (mode, v) = (THROW, thrown(vm, e)),
            },
            Err(e) => {
                let reason = thrown(vm, e);
                let p = get(vm, st, 5);
                return crate::promise::reject_own(vm, p, reason);
            }
        }
    }
}

/// What error `e` throws, as a value.
fn thrown(vm: &mut Vm, e: Error) -> Value {
    match e.1 {
        Some(v) => v,
        None => crate::builtins::caught(vm, &e),
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
#[inline(always)]
fn async_gen_enqueue(vm: &mut Vm, g: Value, v: Value, mode: i32) -> Result<Fresh<'_>> {
    async_gen_enqueue_raw(vm, g, v, mode).map(Fresh::new)
}

fn async_gen_enqueue_raw(vm: &mut Vm, g: Value, v: Value, mode: i32) -> Result<Value> {
    let (p, resolve, reject) = crate::promise::new_with_resolvers(vm);
    let st = match state_of(g) {
        Some(st) if is_async_generator(st) => st,
        _ => {
            let e = crate::builtins::caught(vm, &Error::type_error("not an async generator"));
            vm.call_b(reject, Value::UNDEFINED, &[e])?.take();
            return Ok(p);
        }
    };
    let q = get(vm, st, AG_QUEUE);
    crate::vm::array_push(vm, q, &[Value::from_i32(mode), v, resolve, reject])?;
    if get(vm, st, AG_BUSY) != Value::TRUE {
        {
            let held = Scope::new(vm);
            let _held = held.roots([p]);
            async_gen_run(vm, g)
        }?;
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
    // (`g` rooted: a promise job's handler, only in Rust's copy.)
    {
        let held = Scope::new(vm);
        let _held = held.roots([g]);
        async_gen_step_rooted(vm, g, mode, v)
    }
}

/// [`async_gen_step`], `g` rooted.
fn async_gen_step_rooted(vm: &mut Vm, g: Value, mode: i32, v: Value) -> Result<()> {
    let st = state_of(g).unwrap();
    let state = get(vm, st, STATE).as_i32_unchecked();
    let settled = if get(vm, st, AG_YIELD) == AWAITING_RETURN {
        // A `return(v)`'s `v` settled.
        set(vm, st, AG_YIELD, Value::FALSE);
        if mode == THROW { Err(v) } else { Ok((v, true)) }
    } else if mode == RETURN && (state == SUSPENDED_START || state == COMPLETED) {
        // `return(v)` of a generator not started or done: `v` awaited
        // (AsyncGeneratorAwaitReturn), what it settles to the result.
        if state == SUSPENDED_START {
            set(vm, st, STATE, Value::from_i32(COMPLETED));
            clear_regs(st, Obj::of(get(vm, st, FUNCTION)).code());
        }
        set(vm, st, AG_YIELD, AWAITING_RETURN);
        match crate::promise::await_coroutine(vm, v, g) {
            Ok(()) => return Ok(()),
            Err(e) => {
                set(vm, st, AG_YIELD, Value::FALSE);
                Err(thrown(vm, e))
            }
        }
    } else {
        let (mut mode, mut v) = (mode, v);
        loop {
            match resume_done(vm, g, v, mode, "an async generator") {
                Ok((out, false)) if get(vm, st, AG_YIELD) == Value::TRUE => {
                    set(vm, st, AG_YIELD, Value::FALSE);
                    break Ok((out, false));
                }
                Ok((awaited, false)) => match crate::promise::await_coroutine(vm, awaited, g) {
                    Ok(()) => return Ok(()),
                    // (PromiseResolve threw: the `await` throws it, now.)
                    Err(e) => (mode, v) = (THROW, thrown(vm, e)),
                },
                Ok((out, true)) => break Ok((out, true)),
                Err(e) => break Err(thrown(vm, e)),
            }
        }
    };
    let q = get(vm, st, AG_QUEUE);
    let (resolve, reject) = (qget(vm, q, 2), qget(vm, q, 3));
    let len = vm.length_of(q)? as i32;
    let rest: Vec<Value> = (REQUEST as i32..len).map(|i| qget(vm, q, i)).collect();
    let sc_rest = Scope::new(vm);
    let rest = crate::array::new_array_of(vm, &rest)?.root(&sc_rest);
    set(vm, st, AG_QUEUE, rest.peek());
    match settled {
        Ok((v, done)) => {
            let sc_r = Scope::new(vm);
            let r = crate::symbol::iter_result(vm, v, done)?.root(&sc_r);
            vm.call_b(resolve, Value::UNDEFINED, &[r.peek()])?.take();
        }
        Err(reason) => {
            vm.call_b(reject, Value::UNDEFINED, &[reason])?.take();
        }
    }
    async_gen_run(vm, g)
}

fn async_generator_next(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    async_gen_enqueue(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), NEXT).map(Fresh::take)
}

fn async_generator_return(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    async_gen_enqueue(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), RETURN).map(Fresh::take)
}

fn async_generator_throw(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    async_gen_enqueue(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), THROW).map(Fresh::take)
}

fn return_this(_: &mut Vm, this: Value, _: Args<'_>) -> Result<Value> {
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

fn generator_next(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    resume(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), NEXT, "Generator.prototype.next").map(Fresh::take)
}

fn generator_return(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    resume(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), RETURN, "Generator.prototype.return")
        .map(Fresh::take)
}

fn generator_throw(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    resume(vm, this, a.first().copied().unwrap_or(Value::UNDEFINED), THROW, "Generator.prototype.throw")
        .map(Fresh::take)
}

/// `%GeneratorPrototype%`: next, return, throw; its iterator itself.
pub(crate) fn install(vm: &mut Vm) {
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    let sc_gp = Scope::new(vm);
    let gp = vm.mu().new_plain(object_value(ip.ptr())).root(&sc_gp);
    vm.symbol_roots.push(gp.obj());
    // `%AsyncIteratorPrototype%` (its async iterator itself) and
    // `%AsyncGeneratorPrototype%`.
    let op = object_value(vm.object_prototype);
    // take: only calls that don't collect follow (allocation, stores, the VM's fields).
    let aip = vm.mu().new_plain(op).obj();
    let a_async_iterator = vm.intern_name(&format!("{}Symbol.asyncIterator", crate::symbol::SYMBOL_MARK));
    let f = vm.native_function("[Symbol.asyncIterator]", return_this, 0, crate::jit::Ty::Any, false);
    vm.add_property_with(aip, a_async_iterator, object_value(f.ptr()), DONT_ENUM);
    let sc_agp = Scope::new(vm);
    let agp = vm.mu().new_plain(object_value(aip.ptr())).root(&sc_agp);
    vm.symbol_roots.push(agp.obj());
    vm.define_natives(
        agp.obj(),
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
    let _ = vm.define_own_property(agp.obj(), crate::vm::Key::Name(a), &d);
    vm.define_natives(
        gp.obj(),
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
    let _ = vm.define_own_property(gp.obj(), crate::vm::Key::Name(a), &d);

    // `%GeneratorFunction%` and `%AsyncFunction%`: constructors (not
    // globals) inheriting `Function`, their prototypes `Function.
    // prototype`'s, the functions' prototypes.
    let fp = object_value(vm.function_prototype);
    let function = {
        let a = vm.intern_name("Function");
        vm.get_atom_b(object_value(vm.global), a).map(Fresh::take).unwrap_or(Value::UNDEFINED)
    };
    let (a_p, a_c) = (vm.a_prototype, vm.a_constructor);
    for (name, ctor, instances) in [
        ("GeneratorFunction", crate::builtins::generator_function_ctor as crate::vm::NativeFn, gp.obj().ptr()),
        ("AsyncFunction", crate::builtins::async_function_ctor, std::ptr::null_mut()),
        ("AsyncGeneratorFunction", crate::builtins::async_generator_function_ctor, agp.obj().ptr()),
    ] {
        let generator = !instances.is_null();
        let sc_proto = Scope::new(vm);
        let proto = vm.mu().new_plain(fp).root(&sc_proto);
        vm.symbol_roots.push(proto.obj());
        let c = vm.native_function(name, ctor, 1, crate::jit::Ty::Any, true);
        vm.symbol_roots.push(c);
        vm.set_prototype_of(c, function);
        let d = crate::props::Desc::data(object_value(proto.obj().ptr()), false, false, false);
        let _ = vm.define_own_property(c, crate::vm::Key::Name(a_p), &d);
        let d = crate::props::Desc::data(object_value(c.ptr()), false, false, true);
        let _ = vm.define_own_property(proto.obj(), crate::vm::Key::Name(a_c), &d);
        if generator {
            let d = crate::props::Desc::data(object_value(instances), false, false, true);
            let _ = vm.define_own_property(proto.obj(), crate::vm::Key::Name(a_p), &d);
            let d = crate::props::Desc::data(object_value(proto.obj().ptr()), false, false, true);
            let _ = vm.define_own_property(unsafe { Obj::from_ptr(instances) }, crate::vm::Key::Name(a_c), &d);
        }
        let u: Vec<u16> = name.encode_utf16().collect();
        let s = vm.string_constant(&u);
        let d = crate::props::Desc::data(s, false, false, true);
        let _ = vm.define_own_property(proto.obj(), crate::vm::Key::Name(a), &d);
        // (The same capacity as `function_shape`'s: upvalues where
        // compiled code reads them.)
        let root = vm.root_shape(
            crate::object::Class::Function,
            object_value(proto.obj().ptr()),
            crate::vm::FUNCTION_CAPACITY,
        );
        if !generator {
            vm.async_function_shape = root;
        } else if instances == gp.obj().ptr() {
            vm.generator_function_shape = vm.transition_with(root, a_p, DONT_ENUM | DONT_DELETE);
        } else {
            vm.async_generator_function_shape = vm.transition_with(root, a_p, DONT_ENUM | DONT_DELETE);
        }
    }
}
