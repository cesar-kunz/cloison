//! Realms: each its own global object and intrinsics, in the one heap.
//!
//! The current realm's intrinsics are the `Vm`'s own fields (the global
//! object, the builtins' prototypes, the shapes made from them, …), which
//! the runtime and the interpreter read as they always do. Every other
//! realm's are kept in a [`Realm`], and a call of a function made in
//! another realm (its `Code::realm`) swaps them in for the call and back
//! after: so what a function makes, throws or looks up is its own
//! realm's.
//!
//! Until a second realm is made (`$262.createRealm`, a `ShadowRealm`),
//! there is nothing to check: then every call of a bytecode function goes
//! through `interp::enter_compiled` (all codes' `calls_left` kept at 0),
//! which compares realms, and the JIT is off.

use crate::object::{Atom, Class, FieldType, Object, Shape, as_object, object_value};
use crate::value::Value;
use crate::vm::{Code, Error, Result, Vm};

/// The realm of code that runs in its caller's (a proxy's trampoline).
pub(crate) const NONE: u32 = u32::MAX;

/// A realm field: what it roots, and how it pairs with another realm's
/// (for [`counterpart`]).
trait Field {
    fn trace(&self, _: &mut Vec<Value>) {}
    fn pairs(&self, _: &Self, _: &mut dyn FnMut(Value, Value)) {}
    /// The shapes it holds, which a full marking keeps (see `Shape::mark`).
    fn shapes(&self, _: &mut Vec<*const Shape>) {}
}

impl Field for *mut Object {
    fn trace(&self, out: &mut Vec<Value>) {
        if !self.is_null() {
            out.push(object_value(*self));
        }
    }
    fn pairs(&self, other: &Self, f: &mut dyn FnMut(Value, Value)) {
        if !self.is_null() && !other.is_null() {
            f(object_value(*self), object_value(*other));
        }
    }
}

impl Field for Value {
    fn trace(&self, out: &mut Vec<Value>) {
        if self.is_cell() {
            out.push(*self);
        }
    }
    fn pairs(&self, other: &Self, f: &mut dyn FnMut(Value, Value)) {
        if self.is_cell() && other.is_cell() {
            f(*self, *other);
        }
    }
}

impl<T: Field> Field for Vec<T> {
    fn shapes(&self, out: &mut Vec<*const Shape>) {
        self.iter().for_each(|x| x.shapes(out));
    }
    fn trace(&self, out: &mut Vec<Value>) {
        self.iter().for_each(|x| x.trace(out));
    }
    fn pairs(&self, other: &Self, f: &mut dyn FnMut(Value, Value)) {
        self.iter().zip(other).for_each(|(a, b)| a.pairs(b, f));
    }
}

impl<T: Field, const N: usize> Field for [T; N] {
    fn shapes(&self, out: &mut Vec<*const Shape>) {
        self.iter().for_each(|x| x.shapes(out));
    }
    fn trace(&self, out: &mut Vec<Value>) {
        self.iter().for_each(|x| x.trace(out));
    }
    fn pairs(&self, other: &Self, f: &mut dyn FnMut(Value, Value)) {
        self.iter().zip(other).for_each(|(a, b)| a.pairs(b, f));
    }
}

impl Field for (Value, crate::regexp::Captures) {
    fn trace(&self, out: &mut Vec<Value>) {
        self.0.trace(out);
    }
}

type MatchShape = (FieldType, *const Shape, *const Shape, *const Shape, *const Shape);
impl Field for *const Shape {
    fn shapes(&self, out: &mut Vec<*const Shape>) {
        if !self.is_null() {
            out.push(*self);
        }
    }
}
impl Field for *const Code {}
impl Field for i32 {}
impl Field for bool {}
impl Field for (*const Shape, *const Shape) {
    fn shapes(&self, out: &mut Vec<*const Shape>) {
        self.0.shapes(out);
        self.1.shapes(out);
    }
}
impl Field for MatchShape {
    fn shapes(&self, out: &mut Vec<*const Shape>) {
        [self.1, self.2, self.3, self.4].shapes(out);
    }
}
impl Field for crate::jit::HashSet<Atom> {}

macro_rules! realm_fields {
    ($($name:ident: $t:ty = $blank:expr => $($path:ident).+;)*) => {
        /// A realm's intrinsics, while another realm is current (see the
        /// module docs).
        pub(crate) struct Realm {
            $($name: $t,)*
        }

        impl Realm {
            fn blank() -> Realm {
                Realm { $($name: $blank,)* }
            }

            /// The cells it holds, for the collector.
            pub(crate) fn trace(&self, out: &mut Vec<Value>) {
                $(Field::trace(&self.$name, out);)*
            }

            /// The shapes it holds.
            pub(crate) fn shapes(&self, out: &mut Vec<*const Shape>) {
                $(Field::shapes(&self.$name, out);)*
            }

            /// Each of its intrinsics with `other`'s same one.
            fn pairs(&self, other: &Realm, f: &mut dyn FnMut(Value, Value)) {
                $(Field::pairs(&self.$name, &other.$name, f);)*
            }
        }

        /// The shapes the current realm's intrinsics hold.
        pub(crate) fn current_shapes(vm: &Vm, out: &mut Vec<*const Shape>) {
            $(Field::shapes(&vm.$($path).+, out);)*
        }

        /// Exchange the VM's current intrinsics with `r`'s.
        fn swap(vm: &mut Vm, r: &mut Realm) {
            $(std::mem::swap(&mut vm.$($path).+, &mut r.$name);)*
        }
    };
}

const NULL: *mut Object = std::ptr::null_mut();
const NO_SHAPE: *const Shape = std::ptr::null();
const NO_CODE: *const Code = std::ptr::null();

realm_fields! {
    global: *mut Object = NULL => global;
    object_prototype: *mut Object = NULL => object_prototype;
    function_prototype: *mut Object = NULL => function_prototype;
    array_prototype: *mut Object = NULL => array_prototype;
    string_prototype: *mut Object = NULL => string_prototype;
    number_prototype: *mut Object = NULL => number_prototype;
    boolean_prototype: *mut Object = NULL => boolean_prototype;
    bigint_prototype: *mut Object = NULL => bigint_prototype;
    date_prototype: *mut Object = NULL => date_prototype;
    regexp_prototype: *mut Object = NULL => regexp_prototype;
    array_buffer_prototype: *mut Object = NULL => array_buffer_prototype;
    array_fn: *mut Object = NULL => array_fn;
    regexp_constructor: *mut Object = NULL => regexp_constructor;
    eval_function: *mut Object = NULL => eval_function;
    thrower: *mut Object = NULL => thrower;
    has_instance_fn: *mut Object = NULL => has_instance_fn;
    shared_atomics: *mut Object = NULL => shared_atomics;
    function_call_fn: *mut Object = NULL => function_call_fn;
    array_push_fn: *mut Object = NULL => array_push_fn;
    array_pop_fn: *mut Object = NULL => array_pop_fn;
    char_code_at_fn: *mut Object = NULL => char_code_at_fn;
    from_char_code_fn: *mut Object = NULL => from_char_code_fn;
    math_sqrt_fn: *mut Object = NULL => math_sqrt_fn;
    math_pow_fn: *mut Object = NULL => math_pow_fn;
    math_max_fn: *mut Object = NULL => math_max_fn;
    math_min_fn: *mut Object = NULL => math_min_fn;
    shared_prototype: *mut Object = NULL => shared.prototype;
    shared_constructor: *mut Object = NULL => shared.constructor;
    error_prototypes: Vec<*mut Object> = Vec::new() => error_prototypes;
    typed_prototypes: Vec<*mut Object> = Vec::new() => typed_prototypes;
    typed_constructors: Vec<*mut Object> = Vec::new() => typed_constructors;
    collection_prototypes: Vec<*mut Object> = Vec::new() => collection_prototypes;
    promise_roots: Vec<*mut Object> = Vec::new() => promise_roots;
    iterator_roots: Vec<*mut Object> = Vec::new() => iterator_roots;
    symbol_roots: Vec<*mut Object> = Vec::new() => symbol_roots;
    shadow_roots: Vec<*mut Object> = Vec::new() => shadow_roots;
    temporal_protos: Vec<*mut Object> = Vec::new() => temporal.protos;
    temporal_ctors: Vec<*mut Object> = Vec::new() => temporal.ctors;
    intl_roots: Vec<*mut Object> = Vec::new() => intl_roots;
    regexp_fns: [*mut Object; 6] = [NULL; 6] => regexp_fns;
    dispose_roots: Vec<Value> = Vec::new() => dispose_roots;
    abstract_source: Vec<Value> = Vec::new() => modules.abstract_source;
    bind_helper: Value = Value::UNDEFINED => bind_helper;
    from_async_helper: Value = Value::UNDEFINED => from_async_helper;
    legacy_match: (Value, crate::regexp::Captures) = (Value::EMPTY, Vec::new()) => legacy_match;
    legacy_input: Value = Value::EMPTY => legacy_input;
    legacy_valid: bool = true => legacy_valid;
    function_shape: *const Shape = NO_SHAPE => function_shape;
    method_shape: *const Shape = NO_SHAPE => method_shape;
    generator_function_shape: *const Shape = NO_SHAPE => generator_function_shape;
    async_function_shape: *const Shape = NO_SHAPE => async_function_shape;
    async_generator_function_shape: *const Shape = NO_SHAPE => async_generator_function_shape;
    array_shape: *const Shape = NO_SHAPE => array_shape;
    proto_object_shape: *const Shape = NO_SHAPE => proto_object_shape;
    arguments_shape: *const Shape = NO_SHAPE => arguments_shape;
    regexp_proto_shape: *const Shape = NO_SHAPE => regexp_proto_shape;
    regexp_slots: [i32; 6] = [0; 6] => regexp_slots;
    regexp_shapes: (*const Shape, *const Shape) = (NO_SHAPE, NO_SHAPE) => regexp_shapes;
    match_shapes: Vec<MatchShape> = Vec::new() => match_shapes;
    function_call: *const Code = NO_CODE => function_call;
    function_apply: *const Code = NO_CODE => function_apply;
    declared: crate::jit::HashSet<Atom> = Default::default() => declared;
    lexical_declared: crate::jit::HashSet<Atom> = Default::default() => lexical_declared;
    script_declared: crate::jit::HashSet<Atom> = Default::default() => script_declared;
}

/// Make the current realm `r` (unless it is, or `r` is [`NONE`]): the
/// realm that was.
pub(crate) fn enter(vm: &mut Vm, r: u32) -> u32 {
    let old = vm.realm;
    if r != old && r != NONE {
        unsafe {
            let from: *mut Realm = &mut *vm.realms[old as usize];
            swap(vm, &mut *from);
            let to: *mut Realm = &mut *vm.realms[r as usize];
            swap(vm, &mut *to);
        }
        vm.realm = r;
    }
    old
}

/// `f` run in realm `r`: what it throws made an object of `r`'s (an error
/// of the runtime's own is a message until caught, which would make it
/// one of the catching realm's).
pub(crate) fn run_in<T>(vm: &mut Vm, r: u32, f: impl FnOnce(&mut Vm) -> Result<T>) -> Result<T> {
    if r == vm.realm || r == NONE {
        return f(vm);
    }
    let old = enter(vm, r);
    let out = f(vm).map_err(|e| own_error(vm, e));
    enter(vm, old);
    out
}

/// Error `e` with its value made in the current realm.
pub(crate) fn own_error(vm: &mut Vm, e: Error) -> Error {
    if e.1.is_some() || e.0.starts_with("unsupported") || is_callers(&e) {
        return e;
    }
    let v = crate::builtins::caught(vm, &e);
    Error(e.0, Some(v))
}

/// Whether `e` is a derived constructor's bad result's (`intrinsics::
/// derived_return`): thrown once the constructor's frame is gone, an
/// error of its caller's realm.
fn is_callers(e: &Error) -> bool {
    e.0 == crate::intrinsics::DERIVED_RESULT || e.0 == crate::intrinsics::DERIVED_THIS
}

/// [`own_error`] for the error a frame left in `vm.error`.
pub(crate) fn own_pending_error(vm: &mut Vm) {
    if let Some(e) = vm.error.take() {
        vm.error = Some(own_error(vm, e));
    }
}

/// A new realm, with its intrinsics: its number.
pub(crate) fn create(vm: &mut Vm) -> u32 {
    if vm.realms.is_empty() {
        // The first realm's slot (blank while it is current). Compiled code
        // would call other realms' functions without switching: no more.
        vm.realms.push(Box::new(Realm::blank()));
        vm.set_tier_back_edges(i64::MAX);
        vm.bump_code_epoch();
        for c in vm.codes() {
            c.calls_left.set(0);
        }
    }
    let r = vm.realms.len() as u32;
    vm.realms.push(Box::new(Realm::blank()));
    // (What the realms share, which installing would make anew.)
    let kept = (vm.proxy_codes, vm.to_primitive_defined, vm.has_instance_defined);
    let old = enter(vm, r);
    vm.init_realm();
    enter(vm, old);
    (vm.proxy_codes, vm.to_primitive_defined, vm.has_instance_defined) = kept;
    r
}

/// The value of `f` read in realm `r`.
pub(crate) fn intrinsic<T>(vm: &mut Vm, r: u32, f: impl FnOnce(&Vm) -> T) -> T {
    let old = enter(vm, r);
    let v = f(vm);
    enter(vm, old);
    v
}

/// GetFunctionRealm(f): the realm of function `f` (a bound function's,
/// a proxy's target's); the current one for anything else.
pub(crate) fn function_realm(vm: &mut Vm, f: Value) -> Result<u32> {
    if vm.realms.is_empty() || !f.is_cell() {
        return Ok(vm.realm);
    }
    let o = as_object(f);
    match unsafe { (*o).class } {
        Class::Function | Class::Native => {
            if crate::proxy::is_proxy(vm, o) {
                return function_realm(vm, crate::proxy::target(o)?);
            }
            if let Some(t) = vm.bound_target(f) {
                return function_realm(vm, t);
            }
            let r = unsafe { (*(*o).code).realm };
            Ok(if r == NONE { vm.realm } else { r })
        }
        Class::Proxy => function_realm(vm, crate::proxy::target(o)?),
        _ => Ok(vm.realm),
    }
}

/// Intrinsic `v` of some realm's, realm `r`'s same one (`v` if it is no
/// intrinsic: not a field of a realm's).
pub(crate) fn counterpart(vm: &mut Vm, v: Value, r: u32) -> Value {
    if vm.realms.is_empty() || r == NONE {
        return v;
    }
    let cur = vm.realm as usize;
    let mut found = None;
    unsafe {
        // (The current realm's intrinsics in its slot meanwhile.)
        let slot: *mut Realm = &mut *vm.realms[cur];
        swap(vm, &mut *slot);
        let to = &*vm.realms[r as usize];
        for (k, other) in vm.realms.iter().enumerate() {
            if k != r as usize && found.is_none() {
                other.pairs(to, &mut |a, b| {
                    if found.is_none() && a == v {
                        found = Some(b);
                    }
                });
            }
        }
        swap(vm, &mut *slot);
    }
    found.unwrap_or(v)
}

/// GetPrototypeFromConstructor's fallback: `default` (an intrinsic of the
/// current realm's) of constructor `c`'s realm.
pub(crate) fn default_proto(vm: &mut Vm, c: Value, default: Value) -> Result<Value> {
    if vm.realms.is_empty() {
        return Ok(default);
    }
    let r = function_realm(vm, c)?;
    Ok(if r == vm.realm { default } else { counterpart(vm, default, r) })
}

/// `$262.createRealm()`: a new realm's `$262`.
pub(crate) fn host_create_realm(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let r = create(vm);
    let a = vm.intern("$262");
    let g = intrinsic(vm, r, |vm| vm.global);
    vm.get(object_value(g), a)
}
