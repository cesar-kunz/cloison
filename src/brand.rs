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
//! # Scope::with(vm, |vm, s| {
//! let raw = vm.get_b(o, "raw")?.root(s);
//! let n = vm.length_of_b(o)?;
//! let m = vm.length_of_b(raw.v())?;
//! # Ok(n + m) }) }
//! ```
//!
//! (The crate's code makes its scopes with `let s = Scope::new(vm)`;
//! outside it, [`Scope::with`] gives one for a call.)
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
//! # Scope::with(vm, |vm, s| {
//! let a = vm.mu().new_array().root(s);
//! let n = vm.length_of_b(o)?;
//! let _ = a.v();
//! # Ok(n) }) }
//! ```
//!
//! A native called with arguments no frame roots (they go through
//! `Vm::with_args`, which puts them in the VM stack):
//!
//! ```compile_fail,E0308
//! # use cloison::{brand::Args, vm::Vm, value::Value};
//! fn native(_: &mut Vm, _: Value, a: Args<'_>) -> cloison::vm::Result<Value> {
//!     Ok(a.first().copied().unwrap_or(Value::UNDEFINED))
//! }
//! # fn f(vm: &mut Vm, x: Value) -> cloison::vm::Result<Value> {
//! native(vm, Value::UNDEFINED, &[x])
//! # }
//! ```
//!
//! A handle outliving its scope:
//!
//! ```compile_fail,E0521
//! # use cloison::{brand::Scope, vm::Vm, value::Value};
//! # fn f(vm: &mut Vm) {
//! let mut h = None;
//! Scope::with(vm, |vm, s| h = Some(vm.mu().new_array().root(s)));
//! let _ = h.map(|h| h.v());
//! # }
//! ```
// (Mu is where the VM's allocators are called: see clippy.toml.)
#![allow(clippy::disallowed_methods)]
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
    #[inline(always)]
    pub(crate) fn new(v: T) -> Fresh<'a, T> {
        Fresh { v, _vm: PhantomData }
    }

    /// Rooted in `s`, until it ends.
    #[inline(always)]
    pub fn root<'s>(self, s: &'s Scope) -> T::Rooted<'s> {
        self.v.root_in(s)
    }

    /// What it holds, for the call it is given to at once (an argument
    /// its callee roots), or not a cell (a number's).
    #[inline(always)]
    pub fn take(self) -> T {
        self.v
    }
}

impl<'a, T: Rootable + Copy> Fresh<'a, T> {
    /// The value, the fresh one still unrooted: it keeps borrowing the VM
    /// while it is used, so no call that may collect comes between.
    #[inline(always)]
    pub fn peek(&self) -> T {
        self.v
    }

    /// The value, raw, where nothing collects until its last use (see
    /// [`Raw`]).
    #[inline(always)]
    #[track_caller]
    pub(crate) fn raw(self) -> Raw<T> {
        Raw::new(self.v)
    }
}

impl<'a, T: Rootable> Fresh<'a, Option<T>> {
    /// The fresh value, if any, itself fresh.
    #[inline(always)]
    pub fn transpose(self) -> Option<Fresh<'a, T>> {
        self.v.map(Fresh::new)
    }
}

impl<'a> Fresh<'a, (Value, Value)> {
    /// The two fresh values, each fresh.
    #[inline(always)]
    pub fn split(self) -> (Fresh<'a>, Fresh<'a>) {
        (Fresh::new(self.v.0), Fresh::new(self.v.1))
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

/// A list of what holds cells (CreateListFromArrayLike's values, a
/// properties' descriptors), each rooted.
impl<T: Rootable> Rootable for Vec<T> {
    type Rooted<'s> = Vec<T::Rooted<'s>>;
    fn root_in(self, s: &Scope) -> Vec<T::Rooted<'_>> {
        self.into_iter().map(|v| v.root_in(s)).collect()
    }
}

/// A property's key (its atom held while the native runs) and descriptor.
impl Rootable for (Key, Desc) {
    type Rooted<'s> = (Key, RootedDesc<'s>);
    fn root_in(self, s: &Scope) -> (Key, RootedDesc<'_>) {
        (self.0, self.1.root_in(s))
    }
}

/// An object, rooted as its value.
impl Rootable for Obj {
    type Rooted<'s> = Handle<'s>;
    fn root_in(self, s: &Scope) -> Handle<'_> {
        s.root(object_value(self.ptr()))
    }
}

/// An element's read: whether it was there, and its value.
impl Rootable for (bool, Value) {
    type Rooted<'s> = (bool, Handle<'s>);
    fn root_in(self, s: &Scope) -> (bool, Handle<'_>) {
        (self.0, s.root(self.1))
    }
}

/// Two values (a pair's).
impl Rootable for (Value, Value) {
    type Rooted<'s> = (Handle<'s>, Handle<'s>);
    fn root_in(self, s: &Scope) -> (Handle<'_>, Handle<'_>) {
        (s.root(self.0), s.root(self.1))
    }
}

/// An object and its length.
impl Rootable for (Value, f64) {
    type Rooted<'s> = (Handle<'s>, f64);
    fn root_in(self, s: &Scope) -> (Handle<'_>, f64) {
        (s.root(self.0), self.1)
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
    #[inline(always)]
    pub fn v(self) -> Value {
        self.v
    }

    /// The object, for a call that doesn't collect.
    #[inline(always)]
    pub(crate) fn obj(self) -> Obj {
        Obj::of(self.v)
    }

    /// Rooted in `s`, until it ends.
    pub fn root<'s>(self, s: &'s Scope) -> Handle<'s> {
        s.root(self.v)
    }

    /// The value, raw, where nothing collects until its last use (see
    /// [`Raw`]).
    #[inline(always)]
    #[track_caller]
    pub(crate) fn raw(self) -> Raw {
        Raw::new(self.v)
    }

    /// The object, raw, where nothing collects until its last use.
    #[inline(always)]
    #[track_caller]
    pub(crate) fn raw_obj(self) -> Raw<Obj> {
        Raw::new(Obj::of(self.v))
    }
}

/// A value taken raw from a [`Fresh`] or a [`Gc`] where the code after
/// it, up to its last use, doesn't collect: what the borrow checker
/// can't follow (the calls between are the VM's own, not [`Mu`]'s). With
/// the `gc-check` feature it is checked: each use ([`Raw::v`]) panics if
/// a point that may collect passed since it was taken (a safepoint, JS
/// run, a collection; see [`may_collect`]).
#[derive(Clone, Copy)]
pub struct Raw<T = Value> {
    v: T,
    /// (`gc-check`: the points that may collect so far when it was taken,
    /// and where.)
    #[cfg(feature = "gc-check")]
    at: u64,
    #[cfg(feature = "gc-check")]
    from: &'static std::panic::Location<'static>,
}

impl<T: Copy> Raw<T> {
    #[inline(always)]
    #[track_caller]
    fn new(v: T) -> Raw<T> {
        Raw {
            v,
            #[cfg(feature = "gc-check")]
            at: collect_points(),
            #[cfg(feature = "gc-check")]
            from: std::panic::Location::caller(),
        }
    }

    /// The value, nothing having collected since it was taken (checked
    /// with `gc-check`).
    #[inline(always)]
    #[track_caller]
    pub fn v(self) -> T {
        #[cfg(feature = "gc-check")]
        if collect_points() != self.at {
            raw_used_late(self.from, std::panic::Location::caller());
        }
        self.v
    }
}

/// (`gc-check`: this thread's points that may collect so far, its VMs'.)
#[cfg(feature = "gc-check")]
fn collect_points() -> u64 {
    COLLECT_POINTS.with(|c| c.get())
}

#[cfg(feature = "gc-check")]
thread_local! {
    static COLLECT_POINTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "gc-check")]
#[cold]
#[inline(never)]
fn raw_used_late(from: &std::panic::Location<'_>, at: &std::panic::Location<'_>) {
    if !std::thread::panicking() {
        panic!("a Raw value taken at {from} used at {at}, after a point that may collect");
    }
}

/// `gc-check`: a point that may collect (a safepoint, JS about to run, a
/// collection asked for): counted, for [`Raw`]'s check, and never inside a
/// [`Mu`]'s own work.
#[cfg(feature = "gc-check")]
#[inline(always)]
pub(crate) fn may_collect(vm: &mut Vm) {
    COLLECT_POINTS.with(|c| c.set(c.get() + 1));
    if vm.no_collect != 0 {
        collects_in_mu();
    }
}

#[cfg(feature = "gc-check")]
#[cold]
#[inline(never)]
fn collects_in_mu() {
    if !std::thread::panicking() {
        panic!("a point that may collect, inside a Mu's work (where nothing collects)");
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
    #[inline(always)]
    fn with<T>(&self, f: impl FnOnce(&mut Vm) -> T) -> T {
        // SAFETY: `Mu<'a>` holds the VM's exclusive borrow for `'a` (its
        // `PhantomData`), and lends it to one `f` at a time; a `Gc` is a
        // value, no borrow of the VM's.
        let vm = unsafe { &mut *self.vm };
        // (`gc-check`: no point that may collect while `f` runs.)
        // (An unwinding, out of memory, leaves it counted up: the run's end
        // resets it, see `RunState`.)
        #[cfg(feature = "gc-check")]
        {
            vm.no_collect += 1;
            let r = f(vm);
            vm.no_collect -= 1;
            r
        }
        #[cfg(not(feature = "gc-check"))]
        f(vm)
    }

    /// A new object of `class` inheriting from `proto`, room for
    /// `capacity` slots.
    #[inline(always)]
    pub(crate) fn new_object_with(&self, class: crate::object::Class, proto: Value, capacity: u32) -> Gc<'a> {
        let o = self.with(|vm| vm.new_object_with(class, proto, capacity));
        Gc { v: object_value(o.ptr()), _vm: PhantomData }
    }

    /// What `f`, which allocates but never collects (a module's helper
    /// making a closure or a promise), makes.
    #[inline(always)]
    pub(crate) fn make(&self, f: impl FnOnce(&mut Vm) -> Value) -> Gc<'a> {
        Gc { v: self.with(f), _vm: PhantomData }
    }

    /// A new array.
    #[inline(always)]
    pub fn new_array(&self) -> Gc<'a> {
        Gc { v: self.with(Vm::new_array), _vm: PhantomData }
    }

    /// A new string of `units`.
    #[inline(always)]
    pub fn new_string(&self, units: &[u16]) -> Gc<'a> {
        Gc { v: self.with(|vm| vm.new_string(units)), _vm: PhantomData }
    }

    /// A new string of `len` units, `fill` writing them.
    #[inline(always)]
    pub(crate) fn new_string_with(&self, len: usize, fill: impl FnOnce(&mut [u16])) -> Gc<'a> {
        Gc { v: self.with(|vm| vm.new_string_with(len, fill)), _vm: PhantomData }
    }

    /// A new plain object inheriting from `proto`.
    #[inline(always)]
    pub(crate) fn new_plain(&self, proto: Value) -> Gc<'a> {
        Gc { v: object_value(self.with(|vm| vm.new_plain(proto)).ptr()), _vm: PhantomData }
    }

    /// The wrapper object of primitive `v` (ToObject's).
    #[inline(always)]
    pub(crate) fn new_wrapper(&self, v: Value) -> Gc<'a> {
        Gc { v: self.with(|vm| vm.new_wrapper(v)), _vm: PhantomData }
    }

    /// A new array of `n` elements, `at` making each: it allocates, never
    /// collects (as [`Mu::make`]'s).
    #[inline(always)]
    pub(crate) fn array_of_with(&self, n: usize, at: impl FnMut(&mut Vm, usize) -> Value) -> Gc<'a> {
        Gc { v: self.with(|vm| vm.array_of_with(n, at)), _vm: PhantomData }
    }

    /// FromPropertyDescriptor: a new object of `d`'s fields.
    #[inline(always)]
    pub(crate) fn from_descriptor(&self, d: &Desc) -> Gc<'a> {
        Gc { v: self.with(|vm| vm.from_descriptor(d)), _vm: PhantomData }
    }

    /// The atom of name `s` (a new one interned: its string made).
    #[inline(always)]
    pub(crate) fn intern_name(&self, s: &str) -> crate::object::Atom {
        self.with(|vm| vm.intern_name(s))
    }

    /// `o`, a cell made here, given property `name` (not one it has),
    /// `value`: an object filled where nothing collects (no setter
    /// consulted, its own property added).
    #[inline(always)]
    pub(crate) fn add_property(&self, o: Gc<'a>, name: crate::object::Atom, value: Value) {
        self.with(|vm| vm.add_property(o.obj(), name, value));
    }

    /// [`Mu::add_property`] with attributes `attrs`.
    #[inline(always)]
    pub(crate) fn add_property_with(&self, o: Gc<'a>, name: crate::object::Atom, value: Value, attrs: u8) {
        self.with(|vm| vm.add_property_with(o.obj(), name, value, attrs));
    }

    /// [`Mu::add_property_with`] of each name with its value.
    #[inline(always)]
    pub(crate) fn add_properties(&self, o: Gc<'a>, names: &[crate::object::Atom], values: &[Value], attrs: u8) {
        self.with(|vm| vm.add_properties(o.obj(), names, values, attrs));
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
/// drops. It doesn't borrow the VM (calls that collect come meanwhile),
/// so it must not outlive it, which nothing checks: the crate's code
/// makes one with [`Scope::new`], and code outside the crate gets one
/// only for a call, from [`Scope::with`] (a `Scope` kept past its VM had
/// it write freed memory as it dropped).
#[must_use]
pub struct Scope {
    vm: *mut Vm,
    /// `vm.handles`' depth where it began; with the `gc-check` feature, with
    /// `vm.scope_roots` then in its high half (the struct no bigger: deep
    /// recursions' frames as release's).
    depth: usize,
}

/// (`gc-check`: `Scope::depth`'s low half, the handles' depth.)
#[cfg(feature = "gc-check")]
const LOW: usize = u32::MAX as usize;

impl Scope {
    /// `f` given `vm` and a scope open for the call, which ends as `f`
    /// returns or unwinds: neither the scope nor its handles can leave
    /// `f` (`T` is chosen outside it).
    ///
    /// ```
    /// # use cloison::{brand::Scope, vm::Vm};
    /// let mut vm = Vm::new();
    /// let n = Scope::with(&mut vm, |vm, s| {
    ///     let a = vm.mu().new_array().root(s);
    ///     vm.length_of_b(a.v())
    /// });
    /// assert_eq!(n.unwrap(), 0.0);
    /// ```
    ///
    /// [`Scope::new`] is the crate's: a scope kept past its VM.
    ///
    /// ```compile_fail,E0624
    /// let s = {
    ///     let mut vm = cloison::vm::Vm::new();
    ///     cloison::brand::Scope::new(&mut vm)
    /// };
    /// drop(s);
    /// ```
    #[inline]
    pub fn with<T>(vm: &mut Vm, f: impl FnOnce(&mut Vm, &Scope) -> T) -> T {
        let s = Scope::new(vm);
        f(vm, &s)
    }

    /// A scope from `vm.handles`' current depth; the caller keeps it from
    /// outliving `vm` (see [`Scope`]).
    #[inline(always)]
    pub(crate) fn new(vm: &mut Vm) -> Scope {
        #[cfg(feature = "gc-check")]
        let depth = {
            debug_assert!(vm.handles.len() <= LOW && vm.scope_roots <= LOW);
            vm.handles.len() | vm.scope_roots << 32
        };
        #[cfg(not(feature = "gc-check"))]
        let depth = vm.handles.len();
        Scope { depth, vm }
    }

    /// The handles' depth where it began.
    #[inline(always)]
    fn base(&self) -> usize {
        #[cfg(feature = "gc-check")]
        return self.depth & LOW;
        #[cfg(not(feature = "gc-check"))]
        self.depth
    }

    /// `vs` rooted until the scope ends, at once (one growth check).
    #[inline(always)]
    pub fn roots<const N: usize>(&self, vs: [Value; N]) -> [Handle<'_>; N] {
        // SAFETY: as `root`.
        let at = unsafe {
            #[cfg(feature = "gc-check")]
            {
                (*self.vm).scope_roots += N;
            }
            let h = &mut (*self.vm).handles;
            h.extend_from_slice(&vs);
            h.len() - N
        };
        std::array::from_fn(|i| Handle { vm: self.vm, slot: at + i, _s: PhantomData })
    }

    /// What `x` holds rooted until the scope ends: what a caller passed
    /// (a record of values), held across a call that may collect.
    #[inline(always)]
    pub fn hold<T: Rootable>(&self, x: T) -> T::Rooted<'_> {
        x.root_in(self)
    }

    /// `v` rooted until the scope ends: a value a caller passed, which
    /// this code holds across a call that may collect.
    #[inline(always)]
    pub fn root(&self, v: Value) -> Handle<'_> {
        // SAFETY: the scope doesn't outlive its VM, and nothing else
        // borrows `handles` while it is pushed to.
        let slot = unsafe {
            #[cfg(feature = "gc-check")]
            {
                (*self.vm).scope_roots += 1;
            }
            let h = &mut (*self.vm).handles;
            h.push(v);
            h.len() - 1
        };
        Handle { vm: self.vm, slot, _s: PhantomData }
    }

    /// With `gc-check`, as it ends: only its own roots (and its inner
    /// scopes', ended) above where it began. A value pushed to
    /// `vm.handles` by hand after it began would be dropped with them, its
    /// root gone while its pusher still counts on it.
    #[cfg(feature = "gc-check")]
    #[inline(always)]
    fn check_own(&self, vm: &mut Vm) {
        let then = self.depth >> 32;
        let own = vm.scope_roots - then;
        vm.scope_roots = then;
        if vm.handles.len() != self.base() + own {
            unrooted_pushes(vm.handles.len() - (self.base() + own));
        }
    }
}

/// [`Scope::check_own`]'s failure.
#[cfg(feature = "gc-check")]
#[cold]
#[inline(never)]
fn unrooted_pushes(n: usize) {
    if !std::thread::panicking() {
        panic!("a Scope ends dropping {n} handle(s) it didn't root (pushed to vm.handles by hand)");
    }
}

impl Drop for Scope {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: as `root`.
        let vm = unsafe { &mut *self.vm };
        #[cfg(feature = "gc-check")]
        self.check_own(vm);
        vm.handles.truncate(self.base());
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
    #[inline(always)]
    pub fn v(self) -> Value {
        // SAFETY: the slot is the scope's, which outlives the handle, and
        // the scope its VM; `handles` is cut back only to a scope's depth,
        // no deeper than this slot while its scope lives.
        unsafe { *(&(*self.vm).handles).get_unchecked(self.slot) }
    }

    /// The value now (as [`Fresh::peek`]: a binding read either way,
    /// rooted or not).
    #[inline(always)]
    pub fn peek(self) -> Value {
        self.v()
    }

    /// Root `v` in this slot instead (a loop's current value).
    #[inline(always)]
    pub fn set(self, v: Value) {
        // SAFETY: as `v`.
        unsafe { *(&mut (*self.vm.cast_mut()).handles).get_unchecked_mut(self.slot) = v }
    }

    /// The object, as now.
    #[inline(always)]
    pub(crate) fn obj(self) -> Obj {
        Obj::of(self.v())
    }
}

/// The calls that may collect, their results branded ([`Fresh`]).
/// A native's arguments: rooted while it runs, in the VM stack (where
/// the dispatch put them) or in `vm.handles` with no cell moving. So they
/// are safe across any call the native makes, and passed on as they are.
/// Only the dispatch and [`Vm::with_args`] make them: a Rust caller's
/// `&[x, y]` given to a native directly doesn't compile, where it was
/// once a slice no collection saw.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Args<'s>(&'s [Value]);

impl<'s> Args<'s> {
    /// No arguments.
    pub const NONE: Args<'static> = Args(&[]);

    /// `a` as a native's arguments.
    ///
    /// # Safety
    ///
    /// Every value of `a` stays rooted for `'s`: `a` lies in the VM stack
    /// (a frame's arguments), or its values are in `vm.handles` with no
    /// cell moving meanwhile, or none is a cell.
    #[inline(always)]
    pub unsafe fn rooted(a: &'s [Value]) -> Args<'s> {
        Args(a)
    }

    /// The arguments as a slice, for as long as they are rooted.
    #[inline(always)]
    pub fn slice(self) -> &'s [Value] {
        self.0
    }

    /// Arguments `from..` (`apply`'s rest, a bound function's): rooted as
    /// these are.
    #[inline(always)]
    pub fn from(self, from: usize) -> Args<'s> {
        Args(self.0.get(from..).unwrap_or(&[]))
    }
}

impl<'s> IntoIterator for Args<'s> {
    type Item = &'s Value;
    type IntoIter = std::slice::Iter<'s, Value>;
    #[inline(always)]
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl std::ops::Deref for Args<'_> {
    type Target = [Value];
    #[inline(always)]
    fn deref(&self) -> &[Value] {
        self.0
    }
}

impl Vm {
    /// Run `f` with `args` rooted as a native's arguments ([`Args`]):
    /// copied to the VM stack's free top, unless they are there already
    /// (see [`crate::interp::with_args_on_stack`]). What a Rust caller
    /// gives a native it calls directly.
    #[inline(always)]
    pub(crate) fn with_args(
        &mut self,
        args: &[Value],
        f: impl FnOnce(&mut Vm, Args<'_>) -> Result<Value>,
    ) -> Result<Value> {
        // SAFETY: `with_args_on_stack` gives `f` its arguments in the VM
        // stack, or in `vm.handles` with no cell moving.
        crate::interp::with_args_on_stack(self, args, |vm, a| f(vm, unsafe { Args::rooted(a) }))
    }

    /// What `f`, a call that may collect, returns, branded: a module's own
    /// calls, which have no `*_b` form here.
    #[inline(always)]
    pub(crate) fn fresh<T: Rootable>(&mut self, f: impl FnOnce(&mut Vm) -> Result<T>) -> Result<Fresh<'_, T>> {
        f(self).map(Fresh::new)
    }

    /// The VM where nothing collects.
    #[inline(always)]
    pub fn mu(&mut self) -> Mu<'_> {
        Mu { vm: self, _vm: PhantomData }
    }

    /// `o[name]`.
    #[inline(always)]
    pub fn get_b(&mut self, o: Value, name: &str) -> Result<Fresh<'_>> {
        let a = self.intern_name(name);
        self.get(o, a).map(Fresh::new)
    }

    /// `o[a]`, an atom's name.
    #[inline(always)]
    pub(crate) fn get_atom_b(&mut self, o: Value, a: crate::object::Atom) -> Result<Fresh<'_>> {
        self.get(o, a).map(Fresh::new)
    }

    /// `o[key]`.
    #[inline(always)]
    pub fn get_keyed_b(&mut self, o: Value, key: Value) -> Result<Fresh<'_>> {
        self.get_keyed(o, key).map(Fresh::new)
    }

    /// ToObject, as the object.
    #[inline(always)]
    pub(crate) fn to_obj_b(&mut self, v: Value) -> Result<Fresh<'_, Obj>> {
        self.to_object(v).map(Fresh::new)
    }

    /// `o[key]`.
    #[inline(always)]
    pub(crate) fn get_key_b(&mut self, o: Value, key: Key) -> Result<Fresh<'_>> {
        self.get_key(o, key).map(Fresh::new)
    }

    /// ToObject.
    #[inline(always)]
    pub fn to_object_b(&mut self, v: Value) -> Result<Fresh<'_>> {
        self.to_object(v).map(|o| Fresh::new(crate::object::object_value(o.ptr())))
    }

    /// ToString.
    #[inline(always)]
    pub fn to_string_b(&mut self, v: Value) -> Result<Fresh<'_>> {
        crate::intrinsics::to_string(self, v).map(Fresh::new)
    }

    /// LengthOfArrayLike.
    #[inline(always)]
    pub fn length_of_b(&mut self, o: Value) -> Result<f64> {
        self.length_of(o)
    }

    /// Call `f` with `this` and `args`.
    #[inline(always)]
    pub fn call_b(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Fresh<'_>> {
        crate::interp::call_function(self, f, this, args).map(Fresh::new)
    }

    /// Construct `f` with `args`, `nt` its `new.target`.
    #[inline(always)]
    pub(crate) fn construct_b(&mut self, f: Value, args: &[Value], nt: Value) -> Result<Fresh<'_>> {
        crate::intrinsics::construct_with(self, f, args, nt).map(Fresh::new)
    }

    /// CreateListFromArrayLike of `v`, an object (rooted while it is made:
    /// its getters run).
    #[inline(always)]
    pub(crate) fn list_b(&mut self, v: Value) -> Result<Fresh<'_, Vec<Value>>> {
        self.handle_scope(|vm| crate::array::apply_list(vm, v)).map(Fresh::new)
    }

    /// ToPropertyDescriptor (its getters run).
    #[inline(always)]
    pub(crate) fn to_descriptor_b(&mut self, v: Value) -> Result<Fresh<'_, Desc>> {
        self.to_descriptor(v).map(Fresh::new)
    }

    /// `o.[[GetOwnProperty]](key)` (a proxy's trap runs).
    #[inline(always)]
    pub(crate) fn own_property_b(&mut self, o: Obj, key: Key) -> Result<Fresh<'_, Option<Desc>>> {
        crate::proxy::get_own_property(self, o, key).map(Fresh::new)
    }

    /// `o.[[GetPrototypeOf]]()`.
    #[inline(always)]
    pub(crate) fn prototype_of_b(&mut self, o: Obj) -> Result<Fresh<'_>> {
        crate::proxy::get_prototype_of(self, o).map(Fresh::new)
    }

    /// GetIterator(`v`): its iterator, an object.
    #[inline(always)]
    pub(crate) fn get_iterator_b(&mut self, v: Value) -> Result<Fresh<'_>> {
        crate::symbol::get_iterator(self, v).map(Fresh::new)
    }

    /// IteratorStep of `it` with its `next`: the next value, or `None`.
    #[inline(always)]
    pub(crate) fn step_b(&mut self, it: Value, next: Value) -> Result<Fresh<'_, Option<Value>>> {
        crate::symbol::step_with(self, it, next).map(Fresh::new)
    }

    /// `o.[[Get]](key, receiver)`.
    #[inline(always)]
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
