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

use crate::interp::{drive, native_stack_low, write_frame_header};
use crate::object::{Class, Object, as_object, object_value};
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
    let proto = if crate::props::is_object(p) { p } else { object_value(vm.symbol_roots[crate::symbol::Root::GeneratorPrototype as usize]) };
    let st = crate::array::new_array_of(
        vm,
        &[Value::from_i32(SUSPENDED_START), Value::UNDEFINED, Value::from_i32(0), object_value(f), Value::from_i32(NEXT)],
    )?;
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
            RETURN => crate::symbol::iter_result(vm, v, true),
            _ => crate::symbol::iter_result(vm, Value::UNDEFINED, true),
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
            crate::symbol::iter_result(vm, out, !yielded)
        }
        Err(e) => {
            set(vm, st, STATE, Value::from_i32(COMPLETED));
            set(vm, st, REGS, Value::UNDEFINED);
            Err(e)
        }
    }
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
        let out = drive(vm, (*code).bytecode.as_ptr().add(pc), v, new);
        vm.native_top = saved_top;
        if out == Value::EMPTY {
            return Err(vm.error.take().unwrap_or_else(|| Error::unsupported("a generator's run")));
        }
        Ok(out)
    }
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
}

