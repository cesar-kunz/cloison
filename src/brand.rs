//! Branded cells: Rust code holding a cell across a call that may collect
//! made a compile error, in gc-arena's manner (`Gc<'gc>`) as far as
//! cloison's collections allow.
//!
//! gc-arena collects only between `mutate` calls, so a `Gc<'gc>` never
//! sees a collection. Cloison collects within natives: a native calls JS
//! (a getter, a callback), and that JS reaches safepoints. So the brand
//! is a borrow of the VM, which ends at any call that may collect, and a
//! value kept across one is rooted:
//!
//! - [`Fresh<'a>`]: what a call that may collect returns (`vm.get_b`,
//!   `vm.to_object_b`, ...). Unrooted, it borrows the VM as the call did,
//!   so no other call comes before it is rooted ([`Fresh::root`]) or
//!   given on ([`Fresh::take`]).
//! - [`Gc<'a>`]: a cell made or read where nothing collects, through
//!   [`Mu<'a>`] (gc-arena's `Mutation<'gc>`), which holds the VM: no call
//!   that may collect while a `Gc` lives.
//! - [`Handle<'s>`]: a cell rooted in a [`Scope`], which doesn't borrow
//!   the VM: it lives across calls that collect, the collector updating
//!   it if the cell moves (it is a slot of `Vm::handles`), until the
//!   scope ends.
//!
//! A native's arguments and `this` are rooted already (the VM stack). A
//! raw [`Value`] in a Rust local is what this replaces: the code still
//! holding them is what CI's `CLOISON_GC_NO_NATIVE_ROOTS=drop` run of the
//! scripts checks, on the paths they take.
//!
//! The case this was made for, `String.raw`'s: what a getter returned,
//! held across a call that may collect.
//!
//! ```compile_fail,E0499
//! # use cloison::{brand::Scope, vm::Vm, value::Value};
//! # fn f(vm: &mut Vm, o: Value) -> cloison::vm::Result<f64> {
//! let raw = vm.get_b(o, "raw")?;
//! let n = vm.length_of_b(o)?; // another call while `raw` is unrooted
//! let m = vm.length_of_b(raw.take())?;
//! # Ok(n + m) }
//! ```
//!
//! Given on at once, or rooted, it is right:
//!
//! ```
//! # use cloison::{brand::Scope, vm::Vm, value::Value};
//! # fn f(vm: &mut Vm, o: Value) -> cloison::vm::Result<f64> {
//! let raw = vm.get_b(o, "raw")?.take();
//! vm.length_of_b(raw)
//! # }
//! ```
//!
//!
//! ```
//! # use cloison::{brand::Scope, vm::Vm, value::Value};
//! # fn f(vm: &mut Vm, o: Value) -> cloison::vm::Result<f64> {
//! let s = Scope::new(vm);
//! let raw = vm.get_b(o, "raw")?.root(&s);
//! let n = vm.length_of_b(o)?;
//! let m = vm.length_of_b(raw.v())?;
//! # Ok(n + m) }
//! ```
//!
//! A `Gc` held across a call that may collect:
//!
//! ```compile_fail,E0499
//! # use cloison::{brand::Scope, vm::Vm, value::Value};
//! # fn f(vm: &mut Vm, o: Value) -> cloison::vm::Result<f64> {
//! let mu = vm.mu();
//! let a = mu.new_array();
//! let n = vm.length_of_b(o)?;
//! let _ = a;
//! # Ok(n) }
//! ```
//!
//! ```
//! # use cloison::{brand::Scope, vm::Vm, value::Value};
//! # fn f(vm: &mut Vm, o: Value) -> cloison::vm::Result<f64> {
//! let s = Scope::new(vm);
//! let a = vm.mu().new_array().root(&s);
//! let n = vm.length_of_b(o)?;
//! let _ = a.v();
//! # Ok(n) }
//! ```
//!
//! A handle outliving its scope:
//!
//! ```compile_fail,E0597
//! # use cloison::{brand::Scope, vm::Vm, value::Value};
//! # fn f(vm: &mut Vm) {
//! let h;
//! {
//!     let s = Scope::new(vm);
//!     h = vm.mu().new_array().root(&s);
//! }
//! let _ = h.v();
//! # }
//! ```
use std::marker::PhantomData;

use crate::object::{Obj, object_value};
use crate::props::Desc;
use crate::value::Value;
use crate::vm::{Key, Result, Vm};

/// What a call that may collect returned (a value, a descriptor, a
/// list): unrooted, borrowing the VM as the call did. Root it
/// ([`Fresh::root`]) to keep it, or give it on to the next call at once
/// ([`Fresh::take`]), its callee's to root.
#[must_use = "a fresh value is to be rooted, or given on at once"]
pub struct Fresh<'a, T = Value> {
    v: T,
    _vm: PhantomData<&'a mut Vm>,
}

impl<'a, T: Rootable> Fresh<'a, T> {
    pub(crate) fn new(v: T) -> Fresh<'a, T> {
        Fresh { v, _vm: PhantomData }
    }

    /// Rooted in `s`, until it ends.
    pub fn root<'s>(self, s: &'s Scope) -> T::Rooted<'s> {
        self.v.root_in(s)
    }

    /// What it holds, for the call it is given to at once (an argument
    /// its callee roots), or not a cell (a number's).
    pub fn take(self) -> T {
        self.v
    }
}

impl<'a> Fresh<'a> {
    /// The value as a `Gc`, while nothing collects.
    pub fn gc(self) -> Gc<'a> {
        Gc { v: self.v, _vm: PhantomData }
    }
}

/// What holds cells, and its rooted form in a [`Scope`].
pub trait Rootable {
    type Rooted<'s>;
    fn root_in(self, s: &Scope) -> Self::Rooted<'_>;
}

impl Rootable for Value {
    type Rooted<'s> = Handle<'s>;
    fn root_in(self, s: &Scope) -> Handle<'_> {
        s.root(self)
    }
}

impl Rootable for Desc {
    type Rooted<'s> = RootedDesc<'s>;
    fn root_in(self, s: &Scope) -> RootedDesc<'_> {
        let h = |v: Option<Value>| v.map(|v| s.root(v));
        RootedDesc { value: h(self.value), get: h(self.get), set: h(self.set), d: self }
    }
}

impl<T: Rootable> Rootable for Option<T> {
    type Rooted<'s> = Option<T::Rooted<'s>>;
    fn root_in(self, s: &Scope) -> Option<T::Rooted<'_>> {
        self.map(|v| v.root_in(s))
    }
}

/// A list of values (CreateListFromArrayLike's), rooted only as it is
/// given at once to a call (which copies it onto the VM stack first).
impl Rootable for Vec<Value> {
    type Rooted<'s> = Vec<Handle<'s>>;
    fn root_in(self, s: &Scope) -> Vec<Handle<'_>> {
        self.into_iter().map(|v| s.root(v)).collect()
    }
}

/// A property descriptor, its value, getter and setter rooted.
pub struct RootedDesc<'s> {
    d: Desc,
    value: Option<Handle<'s>>,
    get: Option<Handle<'s>>,
    set: Option<Handle<'s>>,
}

impl RootedDesc<'_> {
    /// The descriptor now (its values' current addresses).
    pub(crate) fn desc(&self) -> Desc {
        Desc { value: self.value.map(Handle::v), get: self.get.map(Handle::v), set: self.set.map(Handle::v), ..self.d }
    }
}

/// A cell (or any value) while nothing collects: made or read through
/// [`Mu`], or fresh ([`Fresh::gc`]). It borrows the VM, so no call that
/// may collect comes while it lives.
#[derive(Clone, Copy)]
pub struct Gc<'a> {
    v: Value,
    _vm: PhantomData<&'a Vm>,
}

impl<'a> Gc<'a> {
    /// The value, for a call that doesn't collect.
    pub fn v(self) -> Value {
        self.v
    }

    /// Rooted in `s`, until it ends.
    pub fn root<'s>(self, s: &'s Scope) -> Handle<'s> {
        s.root(self.v)
    }
}

/// The VM where nothing collects (gc-arena's `Mutation<'gc>`): cells made
/// and read are [`Gc<'a>`]s, which can't outlive it, and it holds the VM
/// so that no call that may collect comes while they live.
pub struct Mu<'a> {
    vm: *mut Vm,
    _vm: PhantomData<&'a mut Vm>,
}

impl<'a> Mu<'a> {
    /// `f` with the VM: what makes and reads cells, never collecting.
    fn with<T>(&self, f: impl FnOnce(&mut Vm) -> T) -> T {
        // SAFETY: `Mu<'a>` holds the VM's exclusive borrow for `'a` (its
        // `PhantomData`), and lends it to one `f` at a time; a `Gc` is a
        // value, no borrow of the VM's.
        f(unsafe { &mut *self.vm })
    }

    /// A new array.
    pub fn new_array(&self) -> Gc<'a> {
        Gc { v: self.with(Vm::new_array), _vm: PhantomData }
    }

    /// A new string of `units`.
    pub fn new_string(&self, units: &[u16]) -> Gc<'a> {
        Gc { v: self.with(|vm| vm.new_string(units)), _vm: PhantomData }
    }

    /// FromPropertyDescriptor: a new object of `d`'s fields.
    pub(crate) fn from_descriptor(&self, d: &Desc) -> Gc<'a> {
        Gc { v: self.with(|vm| vm.from_descriptor(d)), _vm: PhantomData }
    }

    /// A key as a property key value: a string (an index's made) or a
    /// symbol.
    pub(crate) fn key_string(&self, key: Key) -> Gc<'a> {
        Gc {
            v: self.with(|vm| {
                let s = vm.key_value(key);
                if s.is_number() { vm.key_string(key) } else { s }
            }),
            _vm: PhantomData,
        }
    }
}

/// Values rooted until it ends: slots of `Vm::handles`, cut back as it
/// drops. It doesn't borrow the VM (calls that collect come meanwhile);
/// it must not outlive it.
#[must_use]
pub struct Scope {
    vm: *mut Vm,
    depth: usize,
}

impl Scope {
    /// A scope from `vm.handles`' current depth.
    pub fn new(vm: &mut Vm) -> Scope {
        Scope { depth: vm.handles.len(), vm }
    }

    fn root(&self, v: Value) -> Handle<'_> {
        // SAFETY: the scope doesn't outlive its VM, and nothing else
        // borrows `handles` while it is pushed to.
        let slot = unsafe {
            let h = &mut (*self.vm).handles;
            h.push(v);
            h.len() - 1
        };
        Handle { vm: self.vm, slot, _s: PhantomData }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        // SAFETY: as `root`.
        unsafe { (*self.vm).handles.truncate(self.depth) };
    }
}

/// A rooted value, in its [`Scope`]'s slot: its value now, whatever the
/// collections since (a moved cell's new address).
#[derive(Clone, Copy)]
pub struct Handle<'s> {
    vm: *const Vm,
    slot: usize,
    _s: PhantomData<&'s Scope>,
}

impl Handle<'_> {
    /// The value now.
    pub fn v(self) -> Value {
        // SAFETY: the slot is the scope's, which outlives the handle, and
        // the scope its VM.
        unsafe { (&(*self.vm).handles)[self.slot] }
    }
}

/// The calls that may collect, their results branded ([`Fresh`]).
impl Vm {
    /// The VM where nothing collects.
    pub fn mu(&mut self) -> Mu<'_> {
        Mu { vm: self, _vm: PhantomData }
    }

    /// `o[name]`.
    pub fn get_b(&mut self, o: Value, name: &str) -> Result<Fresh<'_>> {
        let a = self.intern_name(name);
        self.get(o, a).map(Fresh::new)
    }

    /// `o[key]`.
    pub fn get_keyed_b(&mut self, o: Value, key: Value) -> Result<Fresh<'_>> {
        self.get_keyed(o, key).map(Fresh::new)
    }

    /// ToObject.
    pub fn to_object_b(&mut self, v: Value) -> Result<Fresh<'_>> {
        self.to_object(v).map(|o| Fresh::new(crate::object::object_value(o.ptr())))
    }

    /// ToString.
    pub fn to_string_b(&mut self, v: Value) -> Result<Fresh<'_>> {
        crate::intrinsics::to_string(self, v).map(Fresh::new)
    }

    /// LengthOfArrayLike.
    pub fn length_of_b(&mut self, o: Value) -> Result<f64> {
        self.length_of(o)
    }

    /// Call `f` with `this` and `args`.
    pub fn call_b(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Fresh<'_>> {
        crate::interp::call_function(self, f, this, args).map(Fresh::new)
    }

    /// Construct `f` with `args`, `nt` its `new.target`.
    pub(crate) fn construct_b(&mut self, f: Value, args: &[Value], nt: Value) -> Result<Fresh<'_>> {
        crate::intrinsics::construct_with(self, f, args, nt).map(Fresh::new)
    }

    /// CreateListFromArrayLike of `v`, an object (rooted while it is made:
    /// its getters run).
    pub(crate) fn list_b(&mut self, v: Value) -> Result<Fresh<'_, Vec<Value>>> {
        self.handle_scope(|vm| crate::array::apply_list(vm, v)).map(Fresh::new)
    }

    /// ToPropertyDescriptor (its getters run).
    pub(crate) fn to_descriptor_b(&mut self, v: Value) -> Result<Fresh<'_, Desc>> {
        self.to_descriptor(v).map(Fresh::new)
    }

    /// `o.[[GetOwnProperty]](key)` (a proxy's trap runs).
    pub(crate) fn own_property_b(&mut self, o: Obj, key: Key) -> Result<Fresh<'_, Option<Desc>>> {
        crate::proxy::get_own_property(self, o, key).map(Fresh::new)
    }

    /// `o.[[GetPrototypeOf]]()`.
    pub(crate) fn prototype_of_b(&mut self, o: Obj) -> Result<Fresh<'_>> {
        crate::proxy::get_prototype_of(self, o).map(Fresh::new)
    }

    /// `o.[[Get]](key, receiver)`.
    pub(crate) fn get_from_b(&mut self, o: Obj, key: Key, receiver: Value) -> Result<Fresh<'_>> {
        let ov = object_value(o.ptr());
        if crate::proxy::chain_has_proxy(self, o) {
            crate::proxy::generic_get(self, ov, key, receiver)
        } else {
            crate::intrinsics::get_from(self, ov, key, receiver)
        }
        .map(Fresh::new)
    }
}
