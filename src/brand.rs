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

use crate::value::Value;
use crate::vm::{Result, Vm};

/// A value a call that may collect returned: unrooted, borrowing the VM
/// as the call did. Root it ([`Fresh::root`]) to keep it, or give it on
/// to the next call at once ([`Fresh::take`]), its callee's to root.
#[must_use = "a fresh value is to be rooted, or given on at once"]
pub struct Fresh<'a> {
    v: Value,
    _vm: PhantomData<&'a mut Vm>,
}

impl<'a> Fresh<'a> {
    pub(crate) fn new(v: Value) -> Fresh<'a> {
        Fresh { v, _vm: PhantomData }
    }

    /// Rooted in `s`, until it ends.
    pub fn root<'s>(self, s: &'s Scope) -> Handle<'s> {
        s.root(self.v)
    }

    /// The value, for the call it is given to at once (an argument
    /// rooted by its callee), or one not a cell (a number's).
    pub fn take(self) -> Value {
        self.v
    }

    /// The value as a `Gc`, while nothing collects.
    pub fn gc(self) -> Gc<'a> {
        Gc { v: self.v, _vm: PhantomData }
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
}
