//! Handle scopes: values the host holds rooted, across runs and
//! operations, until the scope ends (#369).
//!
//! A [`Local`] is a value valid until the VM's next run or operation
//! that may run JavaScript, checked at each entry by its tag (#359). A
//! [`Global`] keeps one for as long as the host likes. Between the two,
//! a [`HandleScope`] keeps every value it hands out, a
//! [`ScopedLocal`], for as long as the scope is open:
//!
//! ```
//! use cloison::object::Atom;
//! use cloison::vm::Vm;
//! let make = cloison::js::compile("({answer: 41})").unwrap();
//! let churn = cloison::js::compile("for (var i = 0; i < 1e5; i++) var x = {i: [i]}; $262.gc();").unwrap();
//! let mut vm = Vm::new();
//! let n = vm.with_handles(|s| {
//!     let object = s.run(&make).map_err(|e| s.owned_error(e))?;
//!     let key = s.string("answer").map_err(|e| s.owned_error(e))?;
//!     // JavaScript that allocates and collects: `object` and `key` are
//!     // roots of the scope's, updated as the collector moves them.
//!     s.run(&churn).map_err(|e| s.owned_error(e))?;
//!     let answer = s.get_keyed(object, key).map_err(|e| s.owned_error(e))?;
//!     s.to_number(answer).map_err(|e| s.owned_error(e))
//! });
//! assert_eq!(n.unwrap(), 41.0);
//! ```
//!
//! Which to use:
//! - a [`Local`] for a value used at once, read again after each
//!   operation (no root, no cost but a compare);
//! - a scope for the values of a sequence of operations, rooted until it
//!   ends: one slot each (their memory and their cells' kept until the
//!   scope ends, so a loop that makes many opens a scope per round, see
//!   [`HandleScope::scope`]);
//! - a [`Global`] for a value kept past the scope, across calls of the
//!   host's ([`HandleScope::persist`], [`HandleScope::local`]).
//!
//! The scope's identity is a lifetime, `'s`, that the closure
//! [`Vm::with_handles`] runs takes for any one: a brand. It is invariant,
//! so the `ScopedLocal<'s>` of one scope is of no other's type, nor can
//! it leave the closure (its result's type is chosen outside it). The
//! scope gives no `&mut Vm`: the VM can't be swapped or dropped under it.
//!
//! Each case that doesn't compile below fails with E0521 ("borrowed data
//! escapes outside of closure", the note naming `ScopedLocal<'s>` as
//! invariant over `'s`), and is followed by the same code made right,
//! which compiles: the brand is what each one breaks.
//!
//! A `ScopedLocal` escaping its scope:
//!
//! ```compile_fail,E0521
//! let mut vm = cloison::vm::Vm::new();
//! let mut kept = None;
//! vm.with_handles(|s| kept = Some(s.global_object()));
//! ```
//!
//! ```
//! let mut vm = cloison::vm::Vm::new();
//! let mut kept = None;
//! vm.with_handles(|s| {
//!     let g = s.global_object();
//!     kept = Some(s.persist(g));
//! });
//! ```
//!
//! Kept in a `static`:
//!
//! ```compile_fail,E0521
//! use cloison::handles::ScopedLocal;
//! static KEPT: std::sync::Mutex<Option<ScopedLocal<'static>>> = std::sync::Mutex::new(None);
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| *KEPT.lock().unwrap() = Some(s.global_object()));
//! ```
//!
//! ```
//! thread_local!(static KEPT: std::cell::RefCell<Option<cloison::vm::Global>> = Default::default());
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let g = s.global_object();
//!     KEPT.with(|k| *k.borrow_mut() = Some(s.persist(g)));
//! });
//! ```
//!
//! Given to the scope of another VM's, both open:
//!
//! ```compile_fail,E0521
//! let (mut a, mut b) = (cloison::vm::Vm::new(), cloison::vm::Vm::new());
//! a.with_handles(|sa| {
//!     let x = sa.global_object();
//!     b.with_handles(|sb| sb.display(x));
//! });
//! ```
//!
//! ```
//! let (mut a, mut b) = (cloison::vm::Vm::new(), cloison::vm::Vm::new());
//! a.with_handles(|sa| {
//!     let x = sa.global_object();
//!     b.with_handles(|sb| {
//!         let y = sb.global_object();
//!         sb.display(y)
//!     });
//!     sa.display(x)
//! });
//! ```
//!
//! Or to an inner scope of the same VM's without [`Lift`]:
//!
//! ```compile_fail,E0521
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let x = s.global_object();
//!     s.scope(|inner, _| inner.display(x));
//! });
//! ```
//!
//! ```
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let x = s.global_object();
//!     s.scope(|inner, up| inner.display(up.lift(x)));
//! });
//! ```
//!
//! A `ScopedLocal` made up (its fields are private), or a raw value
//! given for one, doesn't compile either:
//!
//! ```compile_fail,E0451
//! let l = cloison::handles::ScopedLocal { slot: 0, brand: std::marker::PhantomData };
//! ```
//!
//! ```compile_fail,E0308
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| s.display(cloison::value::Value::from_i32(1)));
//! ```
//!
//! A [`Lift`] kept past its inner scope, to be used in another:
//!
//! ```compile_fail,E0521
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let mut kept = None;
//!     s.scope(|_, up| kept = Some(up));
//! });
//! ```
//!
//! ```
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let x = s.global_object();
//!     for _ in 0..2 {
//!         s.scope(|inner, up| inner.display(up.lift(x)));
//!     }
//! });
//! ```
//!
//! A value of one inner scope used in its sibling:
//!
//! ```compile_fail,E0521
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let mut first = None;
//!     s.scope(|a, _| first = Some(a.global_object()));
//!     s.scope(|b, _| b.display(first.unwrap()));
//! });
//! ```
//!
//! ```
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let first = s.escape_scope(|a, _| Ok(a.global_object())).unwrap();
//!     s.scope(|b, up| b.display(up.lift(first)));
//! });
//! ```
//!
//! A string's units borrow the scope: they can't be read past the next
//! operation, which may move the string:
//!
//! ```compile_fail,E0502
//! let m = cloison::js::compile("'x'.repeat(64)").unwrap();
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let x = s.run(&m).unwrap();
//!     let units = s.string_units(x).unwrap();
//!     s.run(&m).unwrap();
//!     units[0]
//! });
//! ```
//!
//! ```
//! let m = cloison::js::compile("'x'.repeat(64)").unwrap();
//! let mut vm = cloison::vm::Vm::new();
//! vm.with_handles(|s| {
//!     let x = s.run(&m).unwrap();
//!     let first = s.string_units(x).unwrap()[0];
//!     s.run(&m).unwrap();
//!     assert_eq!(first, s.string_units(x).unwrap()[0]);
//! });
//! ```
//!
//! While several `ScopedLocal`s live across operations, each borrowing
//! the scope only for its call:
//!
//! ```
//! let mut vm = cloison::vm::Vm::new();
//! let shown = vm.with_handles(|s| {
//!     let a = s.run(&cloison::js::compile("[1, 2]").unwrap()).unwrap();
//!     let b = s.run(&cloison::js::compile("[3]").unwrap()).unwrap();
//!     let (concat, join, length) = (s.intern("concat"), s.intern("join"), s.intern("length"));
//!     let f = s.get(a, concat).unwrap();
//!     let c = s.call(f, a, &[b]).unwrap();
//!     let j = s.get(c, join).unwrap();
//!     let text = s.call(j, c, &[]).unwrap();
//!     let n = s.get(a, length).unwrap();
//!     (s.display(text), s.to_number(n).unwrap(), s.string_of(text))
//! });
//! assert_eq!(shown, ("1,2,3".into(), 2.0, Some("1,2,3".into())));
//! ```
//!
//! How the roots work: a scope's values are slots of the VM's `handles`,
//! from the depth the scope found as it opened (its watermark) to the
//! top, which its drop truncates back to, on return and on unwinding. A
//! `ScopedLocal` is a slot's index, read again at each use: the slots
//! are roots every collection marks and, moving their cells, updates
//! (`Vm::mark_roots`, `Vm::each_root_slot`), and an index survives the
//! vector's reallocation. Runs and operations truncate `handles` to the
//! depth they found, which is above the scope's. No cell is kept from
//! moving while a scope is open: only the host operations do, as they
//! did, while they run.

use std::borrow::Cow;
use std::marker::PhantomData;

use crate::object::Atom;
use crate::value::Value;
use crate::vm::{Error, Global, Local, Vm};

/// A scope's identity: `'s`, invariant.
type Brand<'s> = PhantomData<fn(&'s ()) -> &'s ()>;

/// A value of a [`HandleScope`]'s, rooted while it is open: valid across
/// runs and operations, moved with its cell, of that scope only.
#[derive(Clone, Copy, Debug)]
pub struct ScopedLocal<'s> {
    slot: usize,
    brand: Brand<'s>,
}

// A slot of a scope of this thread's VM's: of no use on another, and kept
// on this one as a value is (see `Vm`'s "Threads").
impl !Send for ScopedLocal<'_> {}
impl !Sync for ScopedLocal<'_> {}

/// What a scope's operation threw: the error, and its value, rooted in
/// the scope (an object thrown is reached, and read, as any value of the
/// scope's).
#[derive(Debug)]
pub struct ScopedError<'s> {
    error: Error,
    thrown: Option<ScopedLocal<'s>>,
}

impl<'s> ScopedError<'s> {
    /// The value thrown: none for the VM's own errors (see
    /// [`message`](Self::message)).
    pub fn thrown(&self) -> Option<ScopedLocal<'s>> {
        self.thrown
    }

    /// The error's own message, made as it left the VM without running
    /// JavaScript (see [`Vm::uncaught_message`]). The full one, an
    /// object's `toString` run, is [`HandleScope::error_message`]'s.
    pub fn message(&self) -> &str {
        &self.error.0
    }
}

impl std::fmt::Display for ScopedError<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error.0)
    }
}

/// What a scope's operations give: a value of the scope's, or what was
/// thrown.
pub type ScopedResult<'s, T = ScopedLocal<'s>> = std::result::Result<T, ScopedError<'s>>;

/// The values of an outer scope, `'s`, for a scope open in it, `'i`
/// (see [`HandleScope::scope`]): theirs are slots below the inner one's,
/// alive as long as it is.
#[derive(Clone, Copy)]
pub struct Lift<'s, 'i> {
    brand: PhantomData<(Brand<'s>, Brand<'i>)>,
}

impl<'s, 'i> Lift<'s, 'i> {
    /// `l`, of the outer scope, for the inner one.
    pub fn lift(self, l: ScopedLocal<'s>) -> ScopedLocal<'i> {
        ScopedLocal { slot: l.slot, brand: PhantomData }
    }
}

/// A scope of roots of a VM's for the host (see the [module](self)):
/// what its operations return stays valid until it ends.
pub struct HandleScope<'vm, 's> {
    vm: &'vm mut Vm,
    /// The depth of `vm.handles` it found, and leaves.
    base: usize,
    brand: Brand<'s>,
}

impl Drop for HandleScope<'_, '_> {
    fn drop(&mut self) {
        self.vm.handles.truncate(self.base);
        self.vm.host_slots = self.base;
    }
}

impl Vm {
    /// `f` given a new [`HandleScope`] of this VM's, its values dropped
    /// as it returns or unwinds.
    pub fn with_handles<R>(&mut self, f: impl for<'s> FnOnce(&mut HandleScope<'_, 's>) -> R) -> R {
        let base = self.handles.len();
        f(&mut HandleScope { vm: self, base, brand: PhantomData })
    }

    /// The roots the open handle scopes hold, with the VM's own (for
    /// tests and measurements).
    #[doc(hidden)]
    pub fn handle_depth(&self) -> usize {
        self.handles.len()
    }
}

impl<'vm, 's> HandleScope<'vm, 's> {
    /// `v`, of this VM's, rooted as a new slot of the scope's.
    ///
    /// No collection runs between the operation that gave `v` and this:
    /// it is live, and a root from here. (A slot taken while a concurrent
    /// cycle's marking is under way, paused between runs, is marked by its
    /// final pause, which marks every root anew: `Vm::mark_roots`.)
    fn root_value(&mut self, v: Value) -> ScopedLocal<'s> {
        let slot = self.vm.handles.len();
        self.vm.handles.push(v);
        // (The scopes' slots are a prefix: taken between operations, at
        // the top. See `Vm::host_slots`.)
        self.vm.host_slots = slot + 1;
        ScopedLocal { slot, brand: PhantomData }
    }

    /// The value in `l`'s slot now.
    #[inline]
    fn value(&self, l: ScopedLocal<'s>) -> Value {
        self.vm.handles[l.slot]
    }

    /// `l` as a [`Local`], valid until the next operation.
    #[inline]
    fn now(&self, l: ScopedLocal<'s>) -> Local {
        self.vm.local(self.value(l))
    }

    /// What an operation gave, rooted in the scope, or what it threw.
    fn publish(&mut self, r: crate::vm::Result<Local>) -> ScopedResult<'s> {
        match r {
            Ok(l) => Ok(self.root_value(l.value())),
            Err(e) => Err(self.error(e)),
        }
    }

    /// [`HandleScope::publish`] for an operation giving no value.
    fn done<T>(&mut self, r: crate::vm::Result<T>) -> ScopedResult<'s, T> {
        r.map_err(|e| self.error(e))
    }

    /// Error `e`, the VM's last, its value rooted in the scope.
    fn error(&mut self, mut e: Error) -> ScopedError<'s> {
        let thrown = self.vm.thrown(&e).map(|l| self.root_value(l.value()));
        // (Its value is the slot's from here.)
        e.1 = None;
        ScopedError { error: e, thrown }
    }

    /// The VM, to look at.
    pub fn vm(&self) -> &Vm {
        self.vm
    }

    /// [`Vm::intern`].
    pub fn intern(&mut self, name: &str) -> Atom {
        self.vm.intern(name)
    }

    /// `l`, a [`Local`] of this VM's still valid (a primitive, say:
    /// `Local::from(3)`), rooted in the scope.
    pub fn root(&mut self, l: Local) -> ScopedLocal<'s> {
        let v = self.vm.host_value(l, "HandleScope::root");
        self.root_value(v)
    }

    /// `l` as a [`Local`], for the API of those: valid until the VM's
    /// next run or operation, as any.
    pub fn unrooted(&self, l: ScopedLocal<'s>) -> Local {
        self.now(l)
    }

    /// `l` kept past the scope, as a [`Global`].
    pub fn persist(&mut self, l: ScopedLocal<'s>) -> Global {
        Global::new(self.vm, self.now(l))
    }

    /// `g`'s value, rooted in the scope. Panics if `g` is another VM's:
    /// two VMs' globals may have the same slot number, which tells
    /// nothing.
    pub fn local(&mut self, g: &Global) -> ScopedLocal<'s> {
        assert!(g.is_of(self.vm), "HandleScope::local: a Global of another VM");
        let v = g.local().value();
        self.root_value(v)
    }

    /// The global object.
    pub fn global_object(&mut self) -> ScopedLocal<'s> {
        let v = self.vm.global_object().value();
        self.root_value(v)
    }

    /// [`Vm::run`]: its completion value, or what it threw.
    pub fn run(&mut self, module: &crate::asm::Checked) -> ScopedResult<'s> {
        let r = self.vm.run(module);
        self.publish(r)
    }

    /// [`Vm::run_owned`].
    pub fn run_owned(&mut self, module: crate::asm::Checked) -> ScopedResult<'s> {
        let r = self.vm.run_owned(module);
        self.publish(r)
    }

    /// A new string of `s`, or a RangeError past the longest.
    pub fn string(&mut self, s: &str) -> ScopedResult<'s> {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.string_utf16(&units)
    }

    /// A new string of `units` (UTF-16 code units), or a RangeError past
    /// the longest.
    pub fn string_utf16(&mut self, units: &[u16]) -> ScopedResult<'s> {
        let r = Local::try_string(self.vm, units);
        self.publish(r)
    }

    /// A new plain object with prototype `proto` (an object or null).
    pub fn object(&mut self, proto: ScopedLocal<'s>) -> ScopedLocal<'s> {
        let o = Local::object(self.vm, self.now(proto));
        self.root_value(o.value())
    }

    /// A new empty array.
    pub fn array(&mut self) -> ScopedLocal<'s> {
        let a = Local::array(self.vm);
        self.root_value(a.value())
    }

    /// `o[name]`.
    pub fn get(&mut self, o: ScopedLocal<'s>, name: Atom) -> ScopedResult<'s> {
        let r = self.now(o).get(self.vm, name);
        self.publish(r)
    }

    /// `o[name] = value`.
    pub fn set(&mut self, o: ScopedLocal<'s>, name: Atom, value: ScopedLocal<'s>) -> ScopedResult<'s, ()> {
        let r = self.now(o).set(self.vm, name, self.now(value));
        self.done(r)
    }

    /// `o[key]`.
    pub fn get_keyed(&mut self, o: ScopedLocal<'s>, key: ScopedLocal<'s>) -> ScopedResult<'s> {
        let r = self.now(o).get_keyed(self.vm, self.now(key));
        self.publish(r)
    }

    /// `o[key] = value`.
    pub fn set_keyed(
        &mut self,
        o: ScopedLocal<'s>,
        key: ScopedLocal<'s>,
        value: ScopedLocal<'s>,
    ) -> ScopedResult<'s, ()> {
        let r = self.now(o).set_keyed(self.vm, self.now(key), self.now(value));
        self.done(r)
    }

    /// `key in o`.
    pub fn has_property(&mut self, o: ScopedLocal<'s>, key: ScopedLocal<'s>) -> ScopedResult<'s, bool> {
        let r = self.now(o).has_property(self.vm, self.now(key));
        self.done(r)
    }

    /// `delete o[key]`.
    pub fn delete_property(&mut self, o: ScopedLocal<'s>, key: ScopedLocal<'s>) -> ScopedResult<'s, bool> {
        let r = self.now(o).delete_property(self.vm, self.now(key));
        self.done(r)
    }

    /// `v instanceof f`.
    pub fn instance_of(&mut self, v: ScopedLocal<'s>, f: ScopedLocal<'s>) -> ScopedResult<'s, bool> {
        let r = self.now(v).instance_of(self.vm, self.now(f));
        self.done(r)
    }

    /// The keys a `for (k in o)` visits, as an array of strings.
    pub fn for_in_keys(&mut self, o: ScopedLocal<'s>) -> ScopedResult<'s> {
        let r = self.now(o).for_in_keys(self.vm);
        self.publish(r)
    }

    /// ECMAScript ToNumber.
    pub fn to_number(&mut self, v: ScopedLocal<'s>) -> ScopedResult<'s, f64> {
        let r = self.now(v).to_number(self.vm);
        self.done(r)
    }

    /// `typeof v`, a string.
    pub fn type_of(&mut self, v: ScopedLocal<'s>) -> ScopedLocal<'s> {
        let t = self.now(v).type_of(self.vm);
        self.root_value(t.value())
    }

    /// `f.call(this, ...args)` (see [`Local::call_other`]).
    pub fn call(&mut self, f: ScopedLocal<'s>, this: ScopedLocal<'s>, args: &[ScopedLocal<'s>]) -> ScopedResult<'s> {
        let (f, this) = (self.now(f), self.now(this));
        let r = self.with_args(args, |vm, args| f.call_other(vm, this, args));
        self.publish(r)
    }

    /// `new f(...args)` (see [`Local::construct_other`]).
    pub fn construct(&mut self, f: ScopedLocal<'s>, args: &[ScopedLocal<'s>]) -> ScopedResult<'s> {
        let f = self.now(f);
        let r = self.with_args(args, |vm, args| f.construct_other(vm, args));
        self.publish(r)
    }

    /// `g` given the [`Local`]s of `args` (on the stack up to 8).
    fn with_args<T>(&mut self, args: &[ScopedLocal<'s>], g: impl FnOnce(&mut Vm, &[Local]) -> T) -> T {
        let mut few = [Local::UNDEFINED; 8];
        if args.len() <= few.len() {
            for (to, &a) in few.iter_mut().zip(args) {
                *to = self.now(a);
            }
            return g(self.vm, &few[..args.len()]);
        }
        let many: Vec<Local> = args.iter().map(|&a| self.now(a)).collect();
        g(self.vm, &many)
    }

    /// `String(v)` without running JavaScript (see [`crate::vm::display`]).
    pub fn display(&self, v: ScopedLocal<'s>) -> String {
        crate::vm::display(self.now(v), self.vm)
    }

    /// `v`'s code units if it is a string, borrowed from the scope: until
    /// its next operation, which may move the string (see
    /// [`Local::string_units`]). [`HandleScope::string_of`] copies them.
    pub fn string_units(&self, v: ScopedLocal<'s>) -> Option<Cow<'_, [u16]>> {
        crate::vm::units_in_place(self.value(v))
    }

    /// `v`'s text if it is a string, copied out (lone surrogates
    /// replaced).
    pub fn string_of(&self, v: ScopedLocal<'s>) -> Option<String> {
        self.string_units(v).map(|u| String::from_utf16_lossy(&u))
    }

    /// `e`'s full message: an object thrown, its `toString` run (as an
    /// operation: the VM's last error stays its last, as for
    /// [`Vm::uncaught_message`]), else the error's own.
    pub fn error_message(&mut self, e: &ScopedError<'s>) -> String {
        match e.thrown {
            Some(t) => {
                let v = self.value(t);
                self.vm.message_of(v, &e.error.0)
            }
            None => e.error.0.clone(),
        }
    }

    /// `e` as an [`Error`] that outlives the scope: its full message (see
    /// [`HandleScope::error_message`]), no value.
    pub fn owned_error(&mut self, e: ScopedError<'s>) -> Error {
        let message = self.error_message(&e);
        Error(message, None, 0)
    }

    /// `f` given a scope open in this one, whose values are dropped as it
    /// returns or unwinds (for a loop that makes many: the roots of each
    /// round go with it), and `lift` for this one's values there.
    pub fn scope<R>(&mut self, f: impl for<'i> FnOnce(&mut HandleScope<'_, 'i>, Lift<'s, 'i>) -> R) -> R {
        let base = self.vm.handles.len();
        f(&mut HandleScope { vm: self.vm, base, brand: PhantomData }, Lift { brand: PhantomData })
    }

    /// [`HandleScope::scope`] whose result, a value or what was thrown,
    /// is rooted in this one as the inner scope ends: in a slot taken
    /// before it opened (below its watermark), copied there with no
    /// collection between.
    pub fn escape_scope(
        &mut self,
        f: impl for<'i> FnOnce(&mut HandleScope<'_, 'i>, Lift<'s, 'i>) -> ScopedResult<'i>,
    ) -> ScopedResult<'s> {
        let to = self.root_value(Value::UNDEFINED);
        let base = self.vm.handles.len();
        let mut inner = HandleScope { vm: &mut *self.vm, base, brand: PhantomData };
        let r = f(&mut inner, Lift { brand: PhantomData });
        let (from, r) = match r {
            Ok(l) => (Some(l.slot), Ok(())),
            Err(e) => (e.thrown.map(|t| t.slot), Err(e.error)),
        };
        if let Some(from) = from {
            inner.vm.handles[to.slot] = inner.vm.handles[from];
        }
        drop(inner);
        match r {
            Ok(()) => Ok(to),
            Err(error) => Err(ScopedError { error, thrown: from.map(|_| to) }),
        }
    }
}
