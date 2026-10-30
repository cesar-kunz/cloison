//! The VM: loaded code, intrinsics, and the runtime behind the interpreter's
//! slow paths (inline-cache misses, allocation, calls to natives).
//!
//! The interpreter reaches it through each frame's header (see
//! `src/interp.rs`), which holds the frame's [`Code`] and the `Vm`.

use std::cell::{Cell, RefCell, UnsafeCell};
// FxHash: interning the builtins' names at startup was mostly SipHash.
use crate::jit::HashMap;
use std::fmt;

use crate::asm::Module;
use crate::object::{Slots, 
    ACCESSOR, Atom, Class, DONT_DELETE, DONT_ENUM, FieldCell, FieldType, Object, PROTOTYPE, READ_ONLY,
    Shape,
    as_object, field_fits, object_value,
};
use crate::heap::Heap;
use crate::value::Value;

/// A JS exception, or a feature not implemented yet: its message, and for
/// a `throw`, the value thrown. There is no `try` yet, so every error ends
/// the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String, pub Option<Value>);

/// How running a module failed (see [`Vm::run_module`]).
#[derive(Debug)]
pub enum ModuleError {
    /// Before it ran: a syntax error, an import that can't be resolved.
    Early(String),
    /// What it threw.
    Thrown(Error),
}

impl Error {
    pub fn type_error(m: impl fmt::Display) -> Error {
        Error(format!("TypeError: {m}"), None)
    }
    pub fn reference_error(m: impl fmt::Display) -> Error {
        Error(format!("ReferenceError: {m}"), None)
    }
    pub fn new_syntax(m: impl fmt::Display) -> Error {
        Error(format!("SyntaxError: {m}"), None)
    }
    pub fn range_error(m: impl fmt::Display) -> Error {
        Error(format!("RangeError: {m}"), None)
    }
    pub fn unsupported(m: impl fmt::Display) -> Error {
        Error(format!("unsupported: {m}"), None)
    }
    /// `throw v`. (No heap object moves or is freed while it's held.)
    pub fn thrown(v: Value) -> Error {
        Error(display(v), Some(v))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub use crate::layout::Upvalue;

/// A [`Vm::lookup`] result for receivers of shape `shape` and property
/// `name`: the receiver's own slot `slot` (`holder` null), which its shape
/// alone fixes; or, while no prototype changes (`epoch`, the VM's
/// `proto_epoch` then), prototype `holder`'s slot, or none (`holder`
/// `PROTO_NONE`). Shapes never change their keys (a change is a new shape)
/// nor go away, so an entry is never another shape's. (V8's megamorphic
/// stub cache, for the runtime's lookups: TypeScript's syntax tree nodes,
/// of many shapes, read by name through it.)
#[derive(Clone, Copy)]
pub(crate) struct LookupEntry {
    shape: *const crate::object::Shape,
    name: u32,
    epoch: u32,
    holder: *mut Object,
    slot: u32,
}

impl LookupEntry {
    const EMPTY: LookupEntry = LookupEntry {
        shape: std::ptr::null(),
        name: 0,
        epoch: 0,
        holder: std::ptr::null_mut(),
        slot: 0,
    };
}

use crate::layout::{MEGAS, MegaEntry, MegaStore, mega_index, mega_pair};

/// [`LookupEntry::holder`] of a property no object on the chain has.
const PROTO_NONE: *mut Object = std::ptr::dangling_mut::<Object>();
/// Entries of `Vm::lookups` (a power of two).
const LOOKUPS: usize = 4096;

#[inline(always)]
fn lookup_index(shape: *const crate::object::Shape, name: Atom) -> usize {
    ((shape as usize >> 4) ^ (name.0 as usize).wrapping_mul(0x9E37_79B1)) & (LOOKUPS - 1)
}

/// A function implemented in Rust: `(vm, this, args)`.
pub type NativeFn = fn(&mut Vm, Value, &[Value]) -> Result<Value>;

/// One way of an inline-cache slot. A load hits when the receiver's shape is `shape`:
/// the value is at `loc` in the receiver, or, if `holder` is set, in that
/// prototype, provided no prototype has changed shape since (`epoch`). A
/// store hits on `shape` too, and moves the object to `new_shape` first if
/// set (a cached transition, always to an in-object slot), provided no
/// prototype has changed shape since (`epoch`: one may have gained a setter
/// or a read-only property of that name). For a store,
/// `holder` is instead the field's type, encoded by [`FieldType::encode`]:
/// the stored value must fit it, or the store takes the slow path, which
/// generalizes it. 32 bytes, so
/// indexing is a shift; the property name is kept apart, in
/// [`Code::ic_names`], as only misses need it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Ic {
    pub shape: *const Shape,
    pub holder: *mut Object,
    pub new_shape: *const Shape,
    pub loc: i32,
    pub epoch: u32,
}

const _: () = assert!(size_of::<Ic>() == 32);

/// Ways per inline-cache slot: a property access caches up to this many
/// receiver shapes (it is polymorphic), the first in way 0, which the
/// interpreter's hit path checks; a miss there checks the others before
/// the lookup. Other instructions use way 0 only. 128 bytes, so indexing
/// is a shift.
pub const IC_WAYS: usize = 4;

/// An elements' type code (`Shape::elems_code`) that no value fits.
pub use crate::layout::NO_ELEMENTS;

/// Record in keyed-access cache `ic` that it met `o`'s bytes (a string's,
/// a typed array's or a buffer's: `BYTES_LOC`), and for the JIT a typed
/// array's element kind plus one in `epoch` (`u32::MAX` once two kinds or
/// anything else came). A cache holding a shape keeps it.
///
/// # Safety
/// `ic` must be a live cache slot and `o` a live object.
pub(crate) unsafe fn note_bytes(ic: *mut Ic, o: *mut Object) {
    unsafe {
        // (A BigInt kind's as anything else's: its loads allocate.)
        let kind = if (*o).class == Class::TypedArray && crate::object::TypedKind::from_u8((*o).kind).compiled() {
            (*o).kind as u32 + 1
        } else {
            u32::MAX
        };
        if (*ic).shape.is_null() && ((*ic).loc != BYTES_LOC || (*ic).epoch != kind) {
            let epoch = if (*ic).loc != BYTES_LOC { kind } else { u32::MAX };
            *ic = Ic {
                loc: BYTES_LOC,
                epoch,
                ..Ic::EMPTY
            };
        }
    }
}

/// A property key: an element index or a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Key {
    Index(u32),
    Name(Atom),
}

/// `Ic::loc` of a call's cache whose callees are what `f.call(…)` called,
/// `f` being `Function.prototype.call` (see `interp::unwrap_call`): the JIT
/// compiles such a call as a call of them.
pub const VIA_CALL: i32 = 1;

/// `Ic::loc` of a `GetNamed` that read an array's `length`.
pub const LENGTH_LOC: i32 = i32::MIN;
/// The most elements an array keeps dense: past it, it is far (see
/// `far`).
pub(crate) const MAX_ELEMENTS: usize = 1 << 28;

/// The most object keys `Vm::key_cache` holds.
pub(crate) const KEY_CACHE: usize = 32;

/// How far past twice its elements' length an integer key of an object
/// that isn't an array makes it sparse (see `object::SPARSE`).
pub(crate) const SPARSE_GAP: usize = 1024;
/// `Ic::loc` of a `GetKeyed` that read a typed array's, a buffer's or a
/// string's elements (no shape: the interpreter's hit path never takes it;
/// compiled code reads them through the runtime).
pub const BYTES_LOC: i32 = i32::MIN + 1;
/// `Ic::loc` of a `SetKeyed` that stored at an array's length, growing it
/// (`a[a.length] = v`): the JIT compiles it to append (`jit::Prop::Append`).
pub const APPEND_LOC: i32 = i32::MIN + 2;
/// `Ic::loc` of a named access that found an accessor property in a
/// prototype, `holder`: each way's `new_shape` is a receiver's shape (way
/// 0's null once more came than there are ways), `epoch` the prototype
/// epoch. No shape, so the interpreter's hit path never takes it; the JIT
/// inlines the getter or setter (`jit::Lowering::accessor`).
pub const ACCESSOR_LOC: i32 = i32::MIN + 3;
const _: fn(u64, Value) -> bool = field_fits;

/// The first `new` of `code` returned: shrink the capacity of its objects'
/// shapes, all reached from its private root, to the most properties one
/// has (at least 1), and give later objects that. Every
/// slot of those objects is in the object (they had room for more), below
/// that count, so it stays where it is.
fn settle(code: &Code) {
    let root = code.ctor_shape.get();
    if root.is_null() {
        return;
    }
    // Exactly the most properties any has: objects that get more later
    // keep those out of line, but every object is smaller (with no
    // collector, memory is fresh pages the kernel zeroes).
    code.ctor_capacity.set(shrink_root(root));
}

/// Objects a shared constructor (`Code::ctor_shared`) makes with a
/// prototype before their capacity settles (see [`CtorProto`]).
const SLACK_OBJECTS: u32 = 8;

/// Slack tracking for the objects a shared constructor makes with one
/// prototype: as `Code`'s (see [`settle`]), but settled once
/// [`SLACK_OBJECTS`] have been made, so it counts the properties they got
/// after their constructor too (Raytrace's `IntersectionInfo`, whose
/// `intersect` adds five).
pub(crate) struct CtorProto {
    /// The capacity to give objects ([`MAX_INLINE_CAPACITY`] until settled).
    pub(crate) cap: Cell<u32>,
    /// The root shape last used.
    root: Cell<*const Shape>,
    /// Objects made, up to [`SLACK_OBJECTS`].
    made: Cell<u32>,
}

impl CtorProto {
    pub(crate) fn settled(&self) -> bool {
        self.made.get() >= SLACK_OBJECTS
    }
}

/// Shrink the capacity of the shapes reached from `root` to the most
/// properties one has (at least 1); that capacity.
fn shrink_root(root: *const Shape) -> u32 {
    fn walk(s: *const Shape, f: &mut dyn FnMut(&Shape)) {
        let s = unsafe { &*s };
        f(s);
        for &(_, _, t) in s.transitions.borrow().iter() {
            walk(t, f);
        }
        for &(_, t) in s.elem_transitions.borrow().iter() {
            walk(t, f);
        }
    }
    let mut most = 1;
    walk(root, &mut |s| most = most.max(s.keys().len() as u32));
    if most < unsafe { (*root).capacity.get() } {
        walk(root, &mut |s| s.capacity.set(most));
    }
    unsafe { (*root).capacity.get() }
}

/// `v` as stored in a field of type `ft`: in one typed `Num`, an int32 as
/// a double (see `FieldType::Num`).
#[inline(always)]
pub(crate) fn as_field(ft: FieldType, v: Value) -> Value {
    if ft == FieldType::Num && v.is_int32() {
        Value::from_f64(v.as_i32_unchecked() as f64)
    } else {
        v
    }
}

/// Cache `e` in the slot whose way 0 is at `ic`: in the way caching its
/// shape if one does (it may be stale: a field generalized since, or a
/// prototype changed), else in the first free way. With every way taken
/// (the site is megamorphic), `e` replaces way 0, and the result is true.
unsafe fn install(ic: *mut Ic, e: Ic) -> bool {
    unsafe {
        // (Nor a dictionary shape, changed in place: see `Shape::dictionary`.)
        if !e.shape.is_null() && (*e.shape).dictionary {
            return false;
        }
        for k in 0..IC_WAYS {
            let w = &mut *ic.add(k);
            if w.shape.is_null() || w.shape == e.shape {
                *w = e;
                return false;
            }
        }
        *ic = e;
        true
    }
}

/// Record in named-access cache `ic` that a receiver of shape `shape` found
/// an accessor in prototype `holder` (see [`ACCESSOR_LOC`]), unless the
/// cache holds shapes.
///
/// # Safety
/// `ic` must be a live cache slot.
unsafe fn note_accessor(ic: *mut Ic, shape: *const Shape, holder: *mut Object, epoch: u32) {
    unsafe {
        let ours = |w: &Ic| w.loc == ACCESSOR_LOC && w.epoch == epoch && w.holder == holder;
        if (*shape).dictionary || !(*ic).shape.is_null() || ours(&*ic) && (*ic).new_shape.is_null() {
            return;
        }
        let e = Ic {
            holder,
            new_shape: shape,
            loc: ACCESSOR_LOC,
            epoch,
            ..Ic::EMPTY
        };
        if !ours(&*ic) {
            for k in 0..IC_WAYS {
                *ic.add(k) = Ic::EMPTY;
            }
            *ic = e;
            return;
        }
        for k in 0..IC_WAYS {
            let w = &mut *ic.add(k);
            if w.new_shape == shape && ours(w) {
                return;
            }
            if !ours(w) {
                *w = e;
                return;
            }
        }
        (*ic).new_shape = std::ptr::null();
    }
}

/// Before a global load's miss: its cache's first way made free, where
/// `install` then puts the global object's current shape. The interpreter's
/// hit path and the JIT read only that way (there is one global object);
/// left holding a shape the global had before, it sent the shape it has now
/// to the second way, and every load after that missed.
unsafe fn global_way(code: &Code, ic: u16) {
    unsafe { (*code.ic(ic).0).shape = std::ptr::null() }
}

impl Ic {
    const EMPTY: Ic = Ic {
        shape: std::ptr::null(),
        holder: std::ptr::null_mut(),
        new_shape: std::ptr::null(),
        loc: 0,
        epoch: 0,
    };
}

/// Keep `code` for good (see `Code::pinned`).
pub(crate) fn pin(code: *const Code) {
    unsafe { (*code).pinned.set(true) }
}

/// Over the process: functions loaded with bytecode (compiled up front or
/// at their first call), their bytecode's bytes, and functions loaded as
/// stubs not compiled yet (`cloison` prints them with `CLOISON_SIZES` set).
pub static FUNCTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static BYTECODE_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static STUBS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Entries of [`Vm::short_strings`], and the longest string kept there.
const SHORT_STRINGS: usize = 1024;
const SHORT_STRING_UNITS: usize = 32;

/// A function's code, shared by its closures.
pub struct Code {
    pub name: String,
    /// Where it starts in its source, 1-based line and column, for
    /// `CLOISON_JIT_LOG` (see `asm::FunctionDef::line`).
    pub line: u32,
    pub col: u32,
    pub bytecode: Vec<u8>,
    pub nparams: u32,
    /// Its `length` property.
    pub length: u32,
    /// 0: an ordinary function; 1: an arrow function or a method (no
    /// constructor); 2: a class constructor (which only `new` calls); 3:
    /// a derived class's; 4: a generator function; 5: an async function;
    /// 6: an async generator function.
    pub kind: u8,
    /// Its code starts with `LdaNewTarget`.
    pub uses_new_target: bool,
    /// Its source text (see `asm::FunctionDef::text`).
    pub text: Option<(std::rc::Rc<str>, u32, u32)>,
    /// Register count, `r0` (`this`) and the parameters included.
    pub nregs: u32,
    /// Its inline caches, one slot per `ic_names` entry, made when first
    /// asked for (see [`Code::ics`]): most functions loaded never run. A
    /// script's are dropped once it has run (see [`Code::release`]).
    ics: UnsafeCell<std::cell::OnceCell<Box<[UnsafeCell<[Ic; IC_WAYS]>]>>>,
    /// The property name of each inline-cache slot.
    pub ic_names: Box<[Atom]>,
    /// The `SetNamed` slots that define their property, an object
    /// literal's: what a prototype's setter or read-only property of the
    /// name doesn't change (see `Vm::set_named_miss`).
    pub define_ics: Box<[u16]>,
    /// The nested functions, by `CreateClosure` operand.
    pub closures: Vec<*const Code>,
    /// `LdaConstant`'s values.
    pub constants: Box<[Value]>,
    /// Exception handlers (see `FunctionDef::handlers`). A function with
    /// any runs only in the interpreter, which unwinds into them
    /// (`interp::unwind`): it isn't compiled, inlined or called directly.
    pub handlers: Box<[(u32, u32, u32)]>,
    /// See `FunctionDef::compound_keys`.
    pub compound_keys: Box<[(u32, u32)]>,
    /// Where its upvalues come from (see `FunctionDef::upvalues`).
    pub upvalues: Box<[(bool, u8)]>,
    /// Runs only in the interpreter: it has exception handlers or variables
    /// its nested functions use (frames the interpreter manages: unwinding,
    /// closing upvalues). (One with upvalues is compiled: see
    /// `jit::Prop::Upvalue`.)
    pub interpreted: bool,
    /// The function's own epoch: bumped when a field type its compiled
    /// code took generalizes (see `object::FieldCell`), or anything all
    /// compiled code may rely on changes (`Vm::bump_code_epoch`). Added to
    /// `Vm::field_epoch` for its entries; its code checks it after calls.
    pub field_deps: Cell<u32>,
    /// Inline-cache misses in this function's interpreted frames: the
    /// JIT waits for its feedback to settle before compiling it again
    /// (see `jit::VmState::cool`).
    pub ic_misses: Cell<u32>,
    pub native: Option<NativeFn>,
    /// A native that `new` may call (`Array`, `Object`): it makes its own
    /// object.
    pub constructs: bool,
    /// Strict mode code: its `this` is the receiver as is, `undefined`
    /// for a plain call (see `interp::push_frame`).
    pub strict: bool,
    /// For `new`: the in-object capacity to give new objects, and the root
    /// shape last used. Slack tracking: until the first `new` returns (the
    /// capacity is not `ctor_settled`), objects get a root of their own
    /// with ample room ([`MAX_INLINE_CAPACITY`]), whose shapes then shrink
    /// in place to the most properties any has (at least 1), so later
    /// objects share them (see
    /// [`Vm::construct_done`]). The capacity grows afterwards if a
    /// constructed object outgrows it, with a new root.
    pub ctor_capacity: Cell<u32>,
    pub ctor_shape: Cell<*const Shape>,
    pub ctor_settled: Cell<bool>,
    /// The shape of the objects `new` has given, once constructed: null if
    /// none yet, [`MANY_SHAPES`] if not always the same. Compiled code
    /// predicts it for the result of a `new` through the runtime.
    pub(crate) ctor_result: Cell<*const Shape>,
    /// The prototype of the objects the first `new` made, and whether a
    /// later one made objects with another: one code for several classes
    /// (Prototype's `Class.create` constructors), whose slack tracking is
    /// then each prototype's ([`CtorProto`]): else every class's objects
    /// had room for the largest one's fields (Raytrace's `Color`, 3, for 7).
    pub(crate) ctor_proto: Cell<u64>,
    pub(crate) ctor_shared: Cell<bool>,
    /// Compiled code, once a loop or the function got hot (see
    /// `jit::vm_run`).
    pub(crate) jit: RefCell<Option<Box<crate::jit::JitFunction>>>,
    /// Calls left before calls enter compiled code (below zero: they do;
    /// `i32::MAX`: never).
    pub(crate) calls_left: Cell<i32>,
    /// Calls never enter compiled code, although loops may: its start is
    /// refused for good (see `jit::vm_run`).
    pub(crate) calls_interpret: Cell<bool>,
    /// Closures made of it, up to 2: past one, it is made anew (by each
    /// call of the function making it), and calls bind to the code, not a
    /// closure (see `jit::CallSite::by_code`).
    pub(crate) closures_made: Cell<u8>,
    /// Compiled code of it took the upvalues of its only closure as
    /// constants (`jit::Lowering::constant_upvalue`): a second closure,
    /// with cells of its own, drops that code.
    pub(crate) upvalues_folded: Cell<bool>,
    /// Loop back edges left before the interpreter tiers up: a frame of
    /// the function starts with it and gives back what it didn't use when
    /// it returns, so loops tier up after that many back edges over all
    /// calls, however few each call runs.
    pub(crate) loop_budget: Cell<i64>,
    /// What calls to the function are predicted to give, by its latest
    /// compilation, or the one in progress: a direct call to it from a
    /// function compiled meanwhile (mutual recursion) has no version of it
    /// to predict from (see `jit::JitCode::compile`).
    pub(crate) ret_hint: Cell<crate::jit::Ty>,
    /// The collection epoch (`gc::CODE_EPOCH`) it was last found live in:
    /// a closure of it traced or made, it on a stack, loaded or the
    /// nested function of a live one. One not found live in a full
    /// marking is freed (see `Vm::sweep_codes`); the marker thread writes
    /// it too.
    pub(crate) mark: std::sync::atomic::AtomicU32,
    /// Never freed: a native's, or one the JIT took (compiled, inlined,
    /// bound to a call site), which compiled code and its tables refer to
    /// by address.
    pub(crate) pinned: Cell<bool>,
    /// Not compiled yet: its bytecode is `CompileLazy`, which compiles
    /// it from this (see `Vm::compile_lazy`).
    pub(crate) lazy: Option<Box<crate::asm::LazyFn>>,
    /// The realm it was made in (see `realm`): its calls run there.
    /// `realm::NONE` for code that runs in its caller's (a proxy's).
    pub(crate) realm: u32,
}

impl Code {
    /// Whether `new` may call it.
    pub fn constructor(&self) -> bool {
        !matches!(self.kind, 1 | 4 | 5 | 6)
    }

    /// The inline-cache slot `i` (its way 0, the others following), and
    /// the name it looks up.
    pub fn ic(&self, i: u16) -> (*mut Ic, Atom) {
        (self.ics()[i as usize].get().cast(), self.ic_names[i as usize])
    }

    /// Its inline caches, made (empty) the first time.
    pub fn ics(&self) -> &[UnsafeCell<[Ic; IC_WAYS]>] {
        unsafe { &*self.ics.get() }.get_or_init(|| {
            crate::gc::code_bytes(self.ic_names.len() * size_of::<[Ic; IC_WAYS]>());
            (0..self.ic_names.len())
                .map(|_| UnsafeCell::new([Ic::EMPTY; IC_WAYS]))
                .collect()
        })
    }

    /// About the bytes it takes, but for compiled code: the collector
    /// starts a full marking, which frees dead code, when code grows (see
    /// `gc::code_bytes`).
    pub(crate) fn footprint(&self) -> usize {
        // (A native's never dies, so it isn't room for code that may.)
        if self.native.is_some() {
            return 0;
        }
        size_of::<Code>()
            + self.bytecode.len()
            + self.ic_names.len() * size_of::<Atom>()
            + self.made_ics().len() * size_of::<[Ic; IC_WAYS]>()
            + self.constants.len() * size_of::<Value>()
            + self.closures.len() * size_of::<*const Code>()
    }

    /// Drop its inline caches and compiled code: a script's, which has run
    /// and won't again (each `eval` loads its code anew; no function
    /// inlines a script). CodeLoad's evaluated scripts' caches took 20 MiB.
    ///
    /// # Safety
    /// No frame of it may be running, nor anything hold its caches'
    /// addresses but `heap`'s log (its own compiled code's are dropped
    /// with it).
    pub(crate) unsafe fn release(&self, heap: &mut crate::heap::Heap) {
        let ics = self.made_ics();
        let from = ics.as_ptr() as usize;
        heap.forget_slots(from, from + size_of_val(ics));
        drop(unsafe { (*self.ics.get()).take() });
        drop(self.jit.borrow_mut().take());
    }

    /// Its inline caches if it has made them (for the collector: those it
    /// hasn't hold nothing).
    pub(crate) fn made_ics(&self) -> &[UnsafeCell<[Ic; IC_WAYS]>] {
        unsafe { &*self.ics.get() }.get().map_or(&[], |b| b)
    }
}

/// Where an `eval`'s code runs: what compiling it depends on but its
/// source (see `Vm::eval_cache`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EvalPlace {
    /// A direct one's, in a function (see `intrinsics::direct_eval`):
    /// whether the caller is strict, and the `with` statements around it.
    Direct { strict: bool, withs: usize },
    /// A direct one's compiled for its site's bindings (see
    /// `intrinsics::direct_eval_bound`): as `Direct`, the site (a number
    /// of its own: its bindings' names, kinds and upvalues are the code's)
    /// and whether it is in parameters' defaults.
    Bound { strict: bool, withs: usize, site: u64, params: bool },
    /// In the global scope (see `builtins::eval_fn`): an indirect one's,
    /// or a strict script's own direct one's (`strict`, see
    /// `intrinsics::strict_eval`).
    Global { strict: bool },
}

/// An `eval`'s code, kept to run again (see `Vm::eval_cache`).
pub(crate) struct EvalCode {
    /// Its source, and where it runs.
    pub(crate) source: Box<[u16]>,
    pub(crate) place: EvalPlace,
    pub(crate) code: *const Code,
    /// Its code is strict of its own (a `"use strict"` of its own, or the
    /// caller's): a direct one's `var`s are in a scope object of its own.
    pub(crate) own_strict: bool,
    /// Its `var`s and functions (sloppy code's): a direct one's made in
    /// the caller's scope object at each run, as `Module::declared`.
    pub(crate) declared: std::rc::Rc<[Atom]>,
    /// A global one's declarations (`Module::declared`, `function_names`;
    /// no functions): made globals at each run (see
    /// `Vm::global_declarations`).
    pub(crate) globals: Option<std::rc::Rc<Module>>,
    /// `Vm::eval_uses` at its last run.
    pub(crate) used: u64,
    /// Run since the last full marking began.
    pub(crate) recent: bool,
}

/// Direct `eval`s' code kept (see `Vm::eval_cache`).
pub(crate) const EVAL_CACHE_MAX: usize = 64;

/// The longest source of an `eval` whose code is kept (UTF-16 units).
pub(crate) const EVAL_CACHE_TEXT_MAX: usize = 1 << 16;

/// `eval`s' keys remembered as compiled once (see `Vm::eval_seen`).
const EVAL_SEEN_MAX: usize = 1 << 12;

/// Element transitions of a shape to elements of objects of one shape
/// (`FieldType::Obj`) at most: one for each shape stored in arrays of
/// it would make every store search them all, and keep each one.
const MAX_ELEM_SHAPES: usize = 8;

/// Transitions a shape has before they are also kept in a table (see
/// `Vm::wide_transitions`).
const WIDE_TRANSITIONS: usize = 16;

/// Zombie shapes kept before a sweep looks for those it can free (see
/// `Vm::free_zombies`): about 6 MB.
const ZOMBIES_MIN: usize = 1 << 14;

const INITIAL_CTOR_CAPACITY: u32 = 4;
/// `Code::ctor_result` when constructed objects have had several shapes.
pub(crate) const MANY_SHAPES: *const Shape = std::ptr::dangling::<Shape>();
/// Loop back edges before a frame tiers up, and calls before a function's
/// calls enter compiled code (as `jit::Tiering::default`).
pub(crate) const TIER_BACK_EDGES: i64 = 1000;
pub(crate) const TIER_CALLS: i32 = 10;
pub const MAX_INLINE_CAPACITY: u32 = 64;

/// The global object's room for properties in itself (the builtins take
/// about 70; Octane's emscripten benchmarks define thousands).
const GLOBAL_CAPACITY: u32 = 1024;
/// Every closure's room for properties in itself (`prototype` and one
/// more; the rest out of line), its shapes' whatever its kind: its upvalues
/// come after, at a fixed offset (see [`Vm::upvalues`]).
pub(crate) const FUNCTION_CAPACITY: u32 = 2;
/// The byte offset of a closure's upvalues.
pub(crate) const UPVALUES_OFF: usize = crate::layout::slots_offset(Class::Function) + 8 * FUNCTION_CAPACITY as usize;
/// The byte offset of a flat string's units, after its `code` (see
/// `layout::Class::String`).
const STRING_UNITS: usize = crate::layout::slots_offset(Class::String);
/// Stack size in values (frame headers included).
const STACK_VALUES: usize = 1 << 18;

// Shapes (in slabs: `object::ShapeBox`) and code are boxed so their
// addresses stay put as the vectors grow.
#[allow(clippy::vec_box)]
pub struct Vm {
    pub heap: Heap,
    shapes: Vec<crate::object::ShapeBox>,
    /// Shapes a full marking found dead (see [`Vm::sweep_shapes`]) that a
    /// cache or the JIT may still compare with or read: kept, so that
    /// their addresses aren't reused (see [`Vm::free_zombies`]).
    zombies: Vec<crate::object::ShapeBox>,
    /// Zombies nothing held at a full marking's end, freed once the sweep
    /// that frees their objects is done (as `condemned` codes).
    pub(crate) condemned_shapes: Vec<crate::object::ShapeBox>,
    /// Shapes made since a full marking last collected them.
    shapes_made: usize,
    /// Field cells made since then.
    cells_made: usize,
    /// The field cells, each of the slot tables that have it (see
    /// `free_condemned_shapes`). (After the shapes: dropped after their
    /// tables.)
    field_cells: crate::object::CellSlab,
    /// The field cell of every dictionary shape's slots (`Any`, mutable,
    /// held for good: see `Shape::dictionary`); null till the first.
    dictionary_cell: *const FieldCell,
    roots: HashMap<(Class, u64, u32), *const Shape>,
    /// The transitions of shapes with [`WIDE_TRANSITIONS`] or more, by
    /// shape, property and attributes, as their `transitions` lists them
    /// (see `Vm::find_transition`): an object given each of many names
    /// in turn (`o["p" + i] = 1`) made adding one search them all.
    wide_transitions: crate::jit::HashMap<(usize, u32, u8), *const Shape>,
    /// Each atom's name, once (the map's key shares it).
    atoms: Vec<std::rc::Rc<str>>,
    atom_ids: HashMap<std::rc::Rc<str>, Atom>,
    pub(crate) codes: Vec<Box<Code>>,
    /// Fields that went from `Int` to `Num` whose int32s haven't become
    /// doubles yet (see `int_fields_to_doubles`).
    int_fields: Vec<*const FieldCell>,
    /// The addresses of `codes`, to find them among a stack's words.
    pub(crate) code_set: crate::jit::HashSet<usize>,
    /// Codes found dead, freed once the sweep that frees their closures
    /// (which `heap::cell_size` reads them for) is done.
    pub(crate) condemned: Vec<Box<Code>>,
    pub global: *mut Object,
    pub object_prototype: *mut Object,
    pub function_prototype: *mut Object,
    pub array_prototype: *mut Object,
    pub string_prototype: *mut Object,
    /// An object whose one slot always holds `undefined`: a cached miss
    /// (a property found nowhere on the chain) is a prototype load from it.
    missing_holder: *mut Object,
    pub(crate) string_shape: *const Shape,
    /// String constants by contents, so equal literals are one string.
    strings: HashMap<Vec<u16>, Value>,
    /// Bumped whenever a prototype changes shape; see [`Ic`].
    pub proto_epoch: u32,
    /// [`Vm::lookup`]'s results, by the receiver's shape and the name (see
    /// [`LookupEntry`]).
    lookups: Box<[std::cell::Cell<LookupEntry>]>,
    /// See [`MegaEntry`].
    megas: Box<[MegaEntry]>,
    /// See [`MegaStore`].
    mega_stores: Box<[MegaStore]>,
    /// Named-access cache slots (way 0's address) that met more receiver
    /// shapes than they have ways: compiled through `megas`, not a
    /// dispatch on the shapes the ways hold at the time.
    pub(crate) megamorphic: crate::jit::HashSet<usize>,
    /// Prototype slots whose values compiled code folded (`jit::rely_on_slot`):
    /// a store of another value to one bumps `proto_epoch`; to another
    /// prototype slot, nothing (compiled code reads those when it runs).
    /// Cleared with each bump, which drops all compiled code anyway.
    pub(crate) relied_slots: crate::jit::HashSet<usize>,
    /// The functions the JIT has state for (see `bump_code_epoch`).
    pub(crate) jitted: Vec<*const Code>,
    /// By field cell (its address), the functions whose compiled code took
    /// its type (see `object::FieldCell::relied`).
    pub(crate) field_deps: HashMap<usize, Vec<usize>>,
    /// By function, the field cells in `field_deps` it is listed for.
    pub(crate) relied_cells: HashMap<usize, Vec<usize>>,
    /// By field cell, the functions whose compiled code calls straight
    /// into one listed for it in `field_deps`, with that callee (see
    /// [`Vm::rely_through`]).
    pub(crate) through_deps: HashMap<usize, Vec<(usize, usize)>>,
    /// Bumped whenever a field's type generalizes (see [`FieldType`]).
    pub field_epoch: u32,
    /// Bumped with anything compiled code may rely on changing (see
    /// `bump_code_epoch`), and with a field type it took generalizing
    /// (`field_generalized`): stores through the runtime check it, as one
    /// may have invalidated what the code storing assumes.
    pub code_epoch: u32,
    /// Some object has had an indexed accessor or a read-only element
    /// (see `protect_indices`).
    pub(crate) indices_protected: bool,
    /// When arrays go far, and back (see `far::Density`).
    pub(crate) density: crate::far::Density,
    /// The far keys `far::next_key` searched last.
    pub(crate) far_keys: crate::far::SortedKeys,
    /// A fixed-length buffer has been detached (see `typed::detach`): its
    /// views' lengths, which nothing brought up to date, may be stale.
    pub(crate) views_detached: bool,
    /// Some typed array has had another kind's prototype (see
    /// `typed::note_proto`): a shape no longer says the kind of its typed
    /// arrays' elements.
    pub(crate) typed_protos_crossed: bool,
    /// A proxy has been made: property operations look for them on
    /// prototype chains (see `proxy`).
    pub(crate) proxies_exist: bool,
    /// The codes of callable proxies (a non-constructor's, a
    /// constructor's) and of a revocable proxy's `revoke`.
    pub(crate) proxy_codes: [*const Code; 3],
    /// The `WeakMap`s and `WeakSet`s, live or not yet found dead by a
    /// collection (not roots: see `Vm::ephemerons`).
    pub(crate) weak_tables: Vec<*mut Object>,
    /// The targets of the `WeakRef`s made or dereferenced in the current
    /// job, kept alive until it ends (the spec's KeepDuringJob).
    pub(crate) kept_alive: Vec<Value>,
    pub(crate) error: Option<Error>,
    pub(crate) stack: Box<[Value]>,
    pub(crate) stack_end: *mut Value,
    /// Loop back edges a new frame runs in the interpreter before tiering
    /// up into the JIT: `CLOISON_TIER` if set, `i64::MAX` (never) with
    /// `CLOISON_NO_JIT` set.
    pub(crate) frame_budget: i64,
    /// Functions are compiled warm first (see `jit::Tier`): off by
    /// default.
    pub(crate) warm_tier: bool,
    /// Calls of a function before calls to it enter compiled code (see
    /// `Code::calls_left`); `i32::MAX` with `CLOISON_NO_JIT`.
    pub(crate) tier_calls: i32,
    /// The frame that left the interpreter to tier up.
    pub(crate) osr_regs: *mut Value,
    /// The native stack address below which compiled code makes no more
    /// calls on the native stack (see `interp::native_stack_low`).
    pub(crate) native_limit: usize,
    /// A call from compiled code found no room for another nested native
    /// call: the code leaves, and the interpreter makes the call instead
    /// (see `interp::call_from_jit`).
    pub(crate) redo_call: bool,
    /// A conversion (ToPrimitive) needed a bytecode `valueOf` or
    /// `toString` where nothing could call it (`native_top` null): the
    /// operation is redone where something can (see
    /// `interp::to_primitive_hint`).
    pub(crate) conv_redo: bool,
    /// What a `valueOf` or `toString` a conversion called threw, for the
    /// operation converting to throw (conversions can't fail themselves);
    /// `conv_thrown` says there is one (a flag: cheaper to test).
    pub(crate) conv_error: Option<Error>,
    pub(crate) conv_thrown: bool,
    /// A store just did nothing, as it can't happen (to a read-only
    /// property, a non-extensible object, a primitive, an accessor without
    /// a setter): strict code's store throws then (see
    /// `interp::strict_store_check`). Cleared before the stores that check.
    pub(crate) store_failed: bool,
    /// The open upvalues (see [`Upvalue`]), by the address of the register
    /// they point to, lowest first.
    pub(crate) open_upvalues: Vec<*mut Upvalue>,
    /// `typeof`'s results, once made (see [`Vm::type_of`]).
    type_names: [Option<Value>; 8],
    /// Values Rust code holds in heap containers across a call into JS
    /// (see [`Vm::handle_scope`]): roots.
    pub(crate) handles: Vec<Value>,
    /// The most values a frame spans from its registers: its own and, for
    /// compiled code, its inlined frames' (the collector scans the VM stack
    /// that far past the deepest frame's registers).
    pub(crate) max_frame: usize,
    pub(crate) function_shape: *const Shape,
    /// The shape of closures that aren't constructors (arrows, methods):
    /// `function_shape` without `prototype`.
    pub(crate) method_shape: *const Shape,
    /// Generator functions' (`function_shape` from `%GeneratorFunction.
    /// prototype%`) and async functions' (`method_shape` from
    /// `%AsyncFunction.prototype%`), once made (see `generator::install`).
    pub(crate) generator_function_shape: *const Shape,
    pub(crate) async_function_shape: *const Shape,
    pub(crate) async_generator_function_shape: *const Shape,
    /// The root shape of arrays (cached: `root_shape` hashes).
    pub(crate) array_shape: *const Shape,
    pub(crate) proto_object_shape: *const Shape,
    /// `Function.prototype.call`'s code: a call through it is a call of its
    /// receiver (see `interp::unwrap_call`).
    pub(crate) function_call: *const Code,
    /// `Function.prototype.call` itself.
    pub(crate) function_call_fn: *mut Object,
    /// `Function.prototype.apply`'s code (see `interp::unwrap_apply`).
    pub(crate) function_apply: *const Code,
    /// The arguments of a call the runtime makes from compiled code, for
    /// the callee's `CreateArguments` (its frame has no call instruction to
    /// find them by).
    pub(crate) pending_args: (*const Value, usize),
    /// The VM stack's free top while runtime code that may call a
    /// function runs (null elsewhere): see `interp::call_function`.
    pub(crate) native_top: *mut Value,
    /// The native being called is called by `new` (see
    /// [`Vm::construct_other`]): a constructor takes it at entry
    /// ([`Vm::is_construct`]).
    pub(crate) constructing: bool,
    /// The native being called (set by every call of one): a native with
    /// hidden slots of its own reads them through it, first thing (see
    /// `promise::closure`).
    pub(crate) callee: *mut Object,
    /// The `Array` constructor: compiled code makes `new Array()` itself.
    pub(crate) array_fn: *mut Object,
    pub(crate) date_prototype: *mut Object,
    pub(crate) number_prototype: *mut Object,
    pub(crate) bigint_prototype: *mut Object,
    /// Each typed array kind's prototype (`TypedKind` order), and
    /// `ArrayBuffer.prototype`.
    pub(crate) typed_prototypes: Vec<*mut Object>,
    /// The typed array kinds' constructors, then `ArrayBuffer` (see `typed`).
    pub(crate) typed_constructors: Vec<*mut Object>,
    /// `Map.prototype`, `Set.prototype`, `WeakMap.prototype`,
    /// `WeakSet.prototype`, `DataView.prototype`, then the map and set
    /// iterator prototypes (see `collections::Proto`).
    pub(crate) collection_prototypes: Vec<*mut Object>,
    /// `Promise.prototype`, `Promise`, the job queue, … (see
    /// `promise::Root`).
    pub(crate) promise_roots: Vec<*mut Object>,
    /// `Iterator`, `%IteratorHelperPrototype%` and
    /// `%WrapForValidIteratorPrototype%` (see `iterator`).
    pub(crate) iterator_roots: Vec<*mut Object>,
    /// The builtin constructor a subclass's constructor constructs
    /// through (`new.target` another), while it runs (see
    /// `intrinsics::construct_with`); `undefined` otherwise.
    pub(crate) derived_construct: Value,
    /// Tagged templates' strings arrays, by site (see
    /// `intrinsics::template_object`).
    pub(crate) template_cache: crate::jit::HashMap<u64, *mut Object>,
    /// Symbols, by their key atoms (see `symbol`).
    pub(crate) symbols: crate::jit::HashMap<Atom, *mut Object>,
    /// `Symbol.for`'s registry, the agent's (every realm's): its symbols
    /// never die (see `registered_symbols`).
    pub(crate) symbol_registry: crate::jit::HashMap<Vec<u16>, *mut Object>,
    pub(crate) symbol_count: u64,
    /// `Symbol.prototype`, the iterator prototypes, … (see `symbol::Root`).
    pub(crate) symbol_roots: Vec<*mut Object>,
    pub(crate) a_sym_iterator: Atom,
    pub(crate) a_sym_species: Atom,
    pub(crate) a_sym_to_string_tag: Atom,
    pub(crate) a_sym_to_primitive: Atom,
    /// Some object has had a `Symbol.toPrimitive` property, which
    /// conversions then look for (see `interp::to_primitive_hint`).
    pub(crate) to_primitive_defined: bool,
    /// Some object but `Function.prototype` has had a `Symbol.hasInstance`
    /// property, which `instance_of` then looks for.
    pub(crate) has_instance_defined: bool,
    pub(crate) a_sym_has_instance: Atom,
    /// `Function.prototype[Symbol.hasInstance]` (see `instance_of`).
    pub(crate) has_instance_fn: *mut Object,
    /// The `new.target` of the frame about to start (set by
    /// `intrinsics::construct_with` for a function whose first instruction
    /// is `LdaNewTarget`, which takes it); `EMPTY` otherwise.
    pub(crate) new_target: Value,
    /// A native constructor called with another `new.target` from the
    /// runtime (`Reflect.construct`, `super()`), and that target: for a
    /// constructor that reads its prototype itself, before allocating
    /// (see [`Vm::take_new_target`]).
    pub(crate) native_new_target: (Value, Value),
    /// The scope objects of the direct `eval`s running (see
    /// `intrinsics::direct_eval`), innermost last: the caller's, the
    /// eval's own, then the `with` objects around it.
    pub(crate) eval_envs: Vec<Vec<Value>>,
    /// Those of `eval_envs` in use (the rest kept for their room).
    pub(crate) eval_depth: usize,
    /// `eval`s' code, by their source and where they run: what it
    /// compiles to depends on nothing else (see `Vm::cached_eval`). By
    /// their hash ([`Vm::eval_hash`]: found from the source string's
    /// units, with nothing made). At most `EVAL_CACHE_MAX` (`eval_kept`),
    /// the least recently used out; those unused since the last full
    /// marking go at the next (see `Vm::age_eval_cache`).
    pub(crate) eval_cache: crate::jit::HashMap<u64, Vec<EvalCode>>,
    pub(crate) eval_kept: usize,
    /// `eval`s run from the cache: its clock.
    pub(crate) eval_uses: u64,
    /// The keys of `eval`s compiled once, hashed: code is kept for one seen
    /// again only (most texts run once; code-load's, made unique, all do).
    /// Cleared past `EVAL_SEEN_MAX`.
    pub(crate) eval_seen: crate::jit::HashSet<u64>,
    /// `%ThrowTypeError%`.
    pub(crate) thrower: *mut Object,
    /// The mapped `arguments` objects of the frames running, innermost
    /// last: each index mapped to its parameter's register (see
    /// `Vm::mapped_get`), until the frame ends.
    pub(crate) mapped_args: Vec<MappedArgs>,
    /// Sloppy functions' `arguments` objects' shape (see
    /// `builtins::arguments_object_of`).
    pub(crate) arguments_shape: *const Shape,
    /// Their shape when mapped (see `builtins::mapped_arguments_object`).
    pub(crate) mapped_arguments_shape: *const Shape,
    /// The object keys read-modify-writes' `GetKeyed`s converted, what to,
    /// in which frame (its registers) and for the `SetKeyed` at which offset
    /// (`Code::compound_keys`): that one takes its key instead of converting
    /// it again (see `interp::slow_set_keyed`), no other. One per such
    /// assignment under way (nested in calls in another's value); one an
    /// exception left goes once there are `KEY_CACHE` more.
    pub(crate) key_cache: Vec<(Value, Value, *mut Value, u32)>,
    /// The global `eval` (what a direct `eval` calls: see
    /// `intrinsics::direct_eval`).
    pub(crate) eval_function: *mut Object,
    pub(crate) regexp_prototype: *mut Object,
    /// `RegExp.prototype.exec` and `[Symbol.match]`, `[Symbol.matchAll]`,
    /// `[Symbol.replace]`, `[Symbol.search]`, `[Symbol.split]` as made
    /// (see `builtins::pristine`).
    pub(crate) regexp_fns: [*mut Object; 6],
    /// Their keys: `exec`, `Symbol.match`, … (as `regexp_fns`).
    pub(crate) regexp_atoms: [Atom; 6],
    /// `RegExp.prototype`'s shape when `regexp_fns` were last found its
    /// properties, and their slots' locations there (see
    /// `builtins::pristine`).
    pub(crate) regexp_proto_shape: *const crate::object::Shape,
    pub(crate) regexp_slots: [i32; 6],
    pub(crate) boolean_prototype: *mut Object,
    /// Loaded scripts' `var` and function names (see `Module::declared`).
    pub(crate) declared: crate::jit::HashSet<Atom>,
    /// Loaded scripts' block-scoped names (see `Module::lexical`).
    pub(crate) lexical_declared: crate::jit::HashSet<Atom>,
    /// Scripts' (not `eval`s') `var` and function names: their globals
    /// can't be deleted.
    pub(crate) script_declared: crate::jit::HashSet<Atom>,
    /// `Function.prototype.bind`'s JS helper, once made.
    pub(crate) bind_helper: Value,
    /// `Array.fromAsync`'s JS helper, once made.
    pub(crate) from_async_helper: Value,
    /// `dispose`'s prototypes and helpers (see `dispose::Root`).
    pub(crate) dispose_roots: Vec<Value>,
    /// The modules loaded (see `module`).
    pub(crate) modules: crate::module::Modules,
    /// Compiled regular expressions, by pattern and flags (a literal makes
    /// a new object each time it runs, sharing one).
    pub(crate) regexps: HashMap<(String, String), Box<crate::regexp::Regex>>,
    /// The same, by a literal's pattern and flags constants (a `Value`'s
    /// bits each, kept by its `Code::constants`).
    pub(crate) regexp_literals: HashMap<(u64, u64), *const crate::regexp::Regex>,
    /// Captures for `exec`, `test`, `match` and `replace` to match into
    /// (taken while in use).
    pub(crate) caps_buf: crate::regexp::Captures,
    /// `exec` arrays' shapes, by their elements' type (a few): a new
    /// array's with elements of it, then with `index` and with `input`
    /// added.
    pub(crate) match_shapes: Vec<(FieldType, *const Shape, *const Shape, *const Shape, *const Shape)>,
    /// A `RegExp` object's shape when made, and with its five properties
    /// (see `builtins::regexp_object`).
    pub(crate) regexp_shapes: (*const Shape, *const Shape),
    /// Names of `RegExp` objects' and `exec` results' properties.
    pub(crate) a_last_index: Atom,
    /// A bound function's target, a hidden key (see `function_bind`).
    pub(crate) a_bound_target: Atom,
    pub(crate) a_source: Atom,
    pub(crate) a_global: Atom,
    pub(crate) a_ignore_case: Atom,
    pub(crate) a_multiline: Atom,
    pub(crate) a_index: Atom,
    pub(crate) a_input: Atom,
    pub(crate) array_buffer_prototype: *mut Object,
    /// The legacy static properties of `RegExp` (`RegExp.$1`, …): the last
    /// match's input and groups, `input` (set apart), and whether they are
    /// valid (not after a subclass's match). See `builtins::note_legacy`.
    pub(crate) legacy_match: (Value, crate::regexp::Captures),
    pub(crate) legacy_input: Value,
    pub(crate) legacy_valid: bool,
    /// `%RegExp%`.
    pub(crate) regexp_constructor: *mut Object,
    /// The shared memory's state: `SharedArrayBuffer`s' blocks, async
    /// waiters (see `shared`).
    pub(crate) shared: crate::shared::State,
    pub(crate) shared_atomics: *mut Object,
    /// Whether `Atomics.wait` may block this agent.
    pub(crate) can_block: bool,
    /// Temporal's state: its prototypes, the time zones in use.
    pub(crate) temporal: crate::temporal::State,
    /// `Intl`'s prototypes and the like (empty without the `intl`
    /// feature: see `intl::Root`).
    pub(crate) intl_roots: Vec<*mut Object>,
    /// `Intl`'s state: its collators (see `intl`).
    #[cfg(feature = "intl")]
    pub(crate) intl: crate::intl::State,
    /// The agent's part, if it is one (see `agent`).
    pub(crate) agent: crate::agent::State,
    /// `Error.prototype` and its kinds' (see `builtins::ERROR_KINDS`).
    pub(crate) error_prototypes: Vec<*mut Object>,
    /// Strings of one code unit below 256, made on first use.
    single_chars: Vec<Value>,
    /// Short strings substrings made, by a hash of their units, to give
    /// an equal one again (see [`Vm::short_string`]). Emptied as each
    /// collection starts, so it holds no string the collector has found
    /// dead nor one a concurrent marking hasn't seen.
    pub(crate) short_strings: Vec<Value>,
    pub(crate) a_to_string: Atom,
    pub(crate) a_value_of: Atom,
    /// `Array.prototype.push` and `pop`, which the JIT compiles inline.
    pub(crate) array_push_fn: *mut Object,
    pub(crate) array_pop_fn: *mut Object,
    /// `String.prototype.charCodeAt` and `String.fromCharCode`, which
    /// compiled code inlines (see `jit::Insn::native`).
    pub(crate) char_code_at_fn: *mut Object,
    pub(crate) from_char_code_fn: *mut Object,
    /// `Math.sqrt`, `Math.pow`, `Math.max` and `Math.min`, which compiled
    /// code computes itself.
    pub(crate) math_sqrt_fn: *mut Object,
    pub(crate) math_pow_fn: *mut Object,
    pub(crate) math_max_fn: *mut Object,
    pub(crate) math_min_fn: *mut Object,
    /// The slack tracking of `new` for each prototype, for constructors
    /// that have made objects with several (see `Code::ctor_shared`).
    ctor_protos: HashMap<u64, Box<CtorProto>>,
    pub(crate) a_prototype: Atom,
    pub(crate) a_constructor: Atom,
    pub(crate) a_length: Atom,
    pub(crate) a_name: Atom,
    pub(crate) a_value: Atom,
    pub(crate) a_writable: Atom,
    pub(crate) a_enumerable: Atom,
    pub(crate) a_configurable: Atom,
    pub(crate) a_get: Atom,
    pub(crate) a_set: Atom,
    /// What the JIT worked out about functions' bytecode, for every
    /// function's compilations.
    pub(crate) jit_caches: crate::jit::Caches,
    /// The current realm (see `realm`), and every realm's intrinsics but
    /// the current's (whose slot is blank): empty until a second realm is
    /// made.
    pub(crate) realm: u32,
    pub(crate) realms: Vec<Box<crate::realm::Realm>>,
    /// The promise job queue, which the realms share.
    pub(crate) job_queue: *mut Object,
    /// `ShadowRealm.prototype`, … (see `shadow::Root`).
    pub(crate) shadow_roots: Vec<*mut Object>,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    pub fn new() -> Vm {
        // Zeroed, so fresh pages from the kernel, touched only as frames
        // reach them (a frame sets its registers when pushed); filling it
        // faulted in all 2 MiB at startup.
        let mut stack = vec![Value::from_bits(0); STACK_VALUES].into_boxed_slice();
        let stack_end = unsafe { stack.as_mut_ptr().add(stack.len()) };
        let mut vm = Vm {
            heap: Heap::new(),
            shapes: Vec::new(),
            zombies: Vec::new(),
            condemned_shapes: Vec::new(),
            shapes_made: 0,
            cells_made: 0,
            field_cells: Default::default(),
            dictionary_cell: std::ptr::null(),
            roots: HashMap::default(),
            wide_transitions: Default::default(),
            atoms: Vec::new(),
            atom_ids: HashMap::default(),
            codes: Vec::new(),
            int_fields: Vec::new(),
            code_set: Default::default(),
            condemned: Vec::new(),
            jit_caches: Default::default(),
            global: std::ptr::null_mut(),
            object_prototype: std::ptr::null_mut(),
            function_prototype: std::ptr::null_mut(),
            array_prototype: std::ptr::null_mut(),
            string_prototype: std::ptr::null_mut(),
            missing_holder: std::ptr::null_mut(),
            string_shape: std::ptr::null(),
            strings: HashMap::default(),
            proto_epoch: 0,
            lookups: vec![std::cell::Cell::new(LookupEntry::EMPTY); LOOKUPS].into_boxed_slice(),
            megas: vec![MegaEntry::EMPTY; 1 + MEGAS].into_boxed_slice(),
            mega_stores: vec![MegaStore::EMPTY; 1 + MEGAS].into_boxed_slice(),
            megamorphic: Default::default(),
            relied_slots: Default::default(),
            jitted: Vec::new(),
            field_deps: HashMap::default(),
            relied_cells: HashMap::default(),
            through_deps: HashMap::default(),
            field_epoch: 0,
            code_epoch: 0,
            indices_protected: false,
            density: crate::far::Density::from_env(),
            far_keys: Default::default(),
            views_detached: false,
            typed_protos_crossed: false,
            proxies_exist: false,
            proxy_codes: [std::ptr::null(); 3],
            weak_tables: Vec::new(),
            kept_alive: Vec::new(),
            error: None,
            stack,
            stack_end,
            warm_tier: std::env::var_os("CLOISON_WARM").is_some(),
            frame_budget: if std::env::var_os("CLOISON_NO_JIT").is_some() {
                i64::MAX
            } else {
                std::env::var("CLOISON_TIER")
                    .ok()
                    .and_then(|n| n.parse::<i64>().ok())
                    .map_or(TIER_BACK_EDGES, |n| n.max(1))
            },
            tier_calls: 0,
            osr_regs: std::ptr::null_mut(),
            native_limit: 0,
            redo_call: false,
            conv_redo: false,
            conv_error: None,
            conv_thrown: false,
            store_failed: false,
            open_upvalues: Vec::new(),
            type_names: [None; 8],
            handles: Vec::new(),
            max_frame: crate::interp::HEADER,
            function_shape: std::ptr::null(),
            method_shape: std::ptr::null(),
            generator_function_shape: std::ptr::null(),
            async_function_shape: std::ptr::null(),
            async_generator_function_shape: std::ptr::null(),
            array_shape: std::ptr::null(),
            proto_object_shape: std::ptr::null(),
            function_call: std::ptr::null(),
            function_call_fn: std::ptr::null_mut(),
            function_apply: std::ptr::null(),
            pending_args: (std::ptr::null(), 0),
            native_top: std::ptr::null_mut(),
            constructing: false,
            callee: std::ptr::null_mut(),
            array_fn: std::ptr::null_mut(),
            date_prototype: std::ptr::null_mut(),
            number_prototype: std::ptr::null_mut(),
            bigint_prototype: std::ptr::null_mut(),
            error_prototypes: Vec::new(),
            typed_prototypes: Vec::new(),
            typed_constructors: Vec::new(),
            collection_prototypes: Vec::new(),
            promise_roots: Vec::new(),
            iterator_roots: Vec::new(),
            derived_construct: Value::UNDEFINED,
            template_cache: Default::default(),
            symbols: Default::default(),
            symbol_registry: Default::default(),
            symbol_count: 0,
            symbol_roots: Vec::new(),
            a_sym_iterator: Atom(0),
            a_sym_species: Atom(0),
            a_sym_to_string_tag: Atom(0),
            a_sym_to_primitive: Atom(0),
            to_primitive_defined: false,
            has_instance_defined: false,
            a_sym_has_instance: Atom(0),
            has_instance_fn: std::ptr::null_mut(),
            new_target: Value::EMPTY,
            native_new_target: (Value::EMPTY, Value::EMPTY),
            eval_envs: Vec::new(),
            eval_depth: 0,
            eval_cache: Default::default(),
            eval_kept: 0,
            eval_uses: 0,
            eval_seen: Default::default(),
            thrower: std::ptr::null_mut(),
            mapped_args: Vec::new(),
            arguments_shape: std::ptr::null(),
            mapped_arguments_shape: std::ptr::null(),
            key_cache: Vec::new(),
            eval_function: std::ptr::null_mut(),
            regexp_prototype: std::ptr::null_mut(),
            regexp_fns: [std::ptr::null_mut(); 6],
            regexp_atoms: [Atom(0); 6],
            regexp_proto_shape: std::ptr::null(),
            regexp_slots: [0; 6],
            boolean_prototype: std::ptr::null_mut(),
            bind_helper: Value::UNDEFINED,
            from_async_helper: Value::UNDEFINED,
            dispose_roots: Vec::new(),
            modules: Default::default(),
            declared: Default::default(),
            lexical_declared: Default::default(),
            script_declared: Default::default(),
            regexps: HashMap::default(),
            regexp_literals: HashMap::default(),
            caps_buf: Vec::new(),
            match_shapes: Vec::new(),
            regexp_shapes: (std::ptr::null(), std::ptr::null()),
            a_last_index: Atom(0),
            a_bound_target: Atom(0),
            a_source: Atom(0),
            a_global: Atom(0),
            a_ignore_case: Atom(0),
            a_multiline: Atom(0),
            a_index: Atom(0),
            a_input: Atom(0),
            array_buffer_prototype: std::ptr::null_mut(),
            legacy_match: (Value::EMPTY, Vec::new()),
            legacy_input: Value::EMPTY,
            legacy_valid: true,
            regexp_constructor: std::ptr::null_mut(),
            shared: crate::shared::State::default(),
            shared_atomics: std::ptr::null_mut(),
            can_block: true,
            agent: crate::agent::State::default(),
            temporal: crate::temporal::State::default(),
            intl_roots: Vec::new(),
            #[cfg(feature = "intl")]
            intl: crate::intl::State::default(),
            single_chars: vec![Value::EMPTY; 256],
            short_strings: vec![Value::EMPTY; SHORT_STRINGS],
            a_to_string: Atom(0),
            a_value_of: Atom(0),
            array_push_fn: std::ptr::null_mut(),
            array_pop_fn: std::ptr::null_mut(),
            char_code_at_fn: std::ptr::null_mut(),
            from_char_code_fn: std::ptr::null_mut(),
            math_sqrt_fn: std::ptr::null_mut(),
            math_pow_fn: std::ptr::null_mut(),
            math_max_fn: std::ptr::null_mut(),
            math_min_fn: std::ptr::null_mut(),
            ctor_protos: HashMap::default(),
            a_prototype: Atom(0),
            a_constructor: Atom(0),
            a_length: Atom(0),
            a_name: Atom(0),
            a_value: Atom(0),
            a_writable: Atom(0),
            a_enumerable: Atom(0),
            a_configurable: Atom(0),
            a_get: Atom(0),
            a_set: Atom(0),
            realm: 0,
            realms: Vec::new(),
            job_queue: std::ptr::null_mut(),
            shadow_roots: Vec::new(),
        };
        let budget = vm.frame_budget;
        vm.set_tier_back_edges(budget);
        vm.a_prototype = vm.intern("prototype");
        vm.a_constructor = vm.intern("constructor");
        vm.a_length = vm.intern("length");
        vm.a_name = vm.intern("name");
        vm.a_value = vm.intern("value");
        vm.a_writable = vm.intern("writable");
        vm.a_enumerable = vm.intern("enumerable");
        vm.a_configurable = vm.intern("configurable");
        vm.a_get = vm.intern("get");
        vm.a_set = vm.intern("set");
        vm.a_to_string = vm.intern("toString");
        vm.a_value_of = vm.intern("valueOf");
        vm.a_last_index = vm.intern("lastIndex");
        vm.a_bound_target = vm.intern("\u{FDD0}BoundTarget");
        vm.a_source = vm.intern("source");
        vm.a_global = vm.intern("global");
        vm.a_ignore_case = vm.intern("ignoreCase");
        vm.a_multiline = vm.intern("multiline");
        vm.a_index = vm.intern("index");
        vm.a_input = vm.intern("input");
        vm.missing_holder = vm.new_object_with(Class::Plain, Value::NULL, 1);
        vm.init_realm();
        vm
    }

    /// Make the current realm's intrinsics: its global object, the
    /// builtins.
    pub(crate) fn init_realm(&mut self) {
        let vm = self;
        vm.object_prototype = vm.new_object_with(Class::Plain, Value::NULL, 4);
        let op = object_value(vm.object_prototype);
        // `Function.prototype` is itself a function (returning `undefined`).
        let fp = vm.new_object_with(Class::Native, op, 0);
        unsafe { (*fp).code = vm.native_code("", function_prototype_fn, 0, crate::jit::Ty::Any, false) };
        vm.function_prototype = fp;
        vm.array_prototype = vm.new_object(op);
        // `String.prototype` is a string object (of `""`, set below) and
        // `Number.prototype` a number object (of 0).
        vm.string_prototype = vm.new_object_with(Class::Wrapper, op, 4);
        // Room for the builtins and the script's globals in the object
        // itself: out-of-line globals cost compiled code a load each, and
        // aren't folded as constants. (64 slots, the builtins took them
        // all: DeltaBlue +20% instructions.)
        vm.global = vm.new_object_with(Class::Plain, op, GLOBAL_CAPACITY);

        let fp = object_value(vm.function_prototype);
        let root = vm.root_shape(Class::Function, fp, FUNCTION_CAPACITY);
        // Functions' `prototype` isn't enumerable nor configurable;
        // prototypes' `constructor` isn't enumerable.
        vm.function_shape = vm.transition_with(root, vm.a_prototype, DONT_ENUM | DONT_DELETE);
        // (Arrows' and methods': no `prototype`, the same capacity, so
        // upvalues are where compiled code reads them.)
        vm.method_shape = root;
        let root = vm.root_shape(Class::Plain, op, INITIAL_CTOR_CAPACITY);
        vm.proto_object_shape = vm.transition_with(root, vm.a_constructor, DONT_ENUM);

        vm.array_shape = vm.root_shape(Class::Array, object_value(vm.array_prototype), 0);
        // (Strings, primitives, share the first realm's shape: property
        // reads on one find the current realm's `String.prototype`.)
        if vm.string_shape.is_null() {
            vm.string_shape = vm.root_shape(Class::String, object_value(vm.string_prototype), 0);
            // No value fits a string's "elements" (its code units): element
            // stores' fast paths never take one.
            unsafe { (*vm.string_shape.cast_mut()).elems_code = NO_ELEMENTS };
        }
        let empty = vm.new_string(&[]);
        unsafe { (*vm.string_prototype).code = empty.to_bits() as *const Code };
        let ap = vm.array_prototype;
        vm.array_push_fn = vm.define_native(ap, "push", array_push_method, 1, crate::jit::Ty::I32, false);
        vm.array_pop_fn = vm.define_native(ap, "pop", crate::array::array_pop, 0, crate::jit::Ty::Any, false);
        let fp = vm.function_prototype;
        let call = vm.define_native(fp, "call", function_call, 1, crate::jit::Ty::Any, false);
        vm.function_call = unsafe { (*call).code };
        vm.function_call_fn = call;
        // The constructors, with `prototype` and `constructor` links.
        let g = vm.global;
        for (name, v) in [("NaN", Value::from_f64(f64::NAN)), ("Infinity", Value::from_f64(f64::INFINITY)), ("undefined", Value::UNDEFINED)] {
            let a = vm.intern(name);
            vm.add_property_with(
                g,
                a,
                v,
                READ_ONLY | DONT_ENUM | DONT_DELETE,
            );
        }
        vm.number_prototype = vm.new_object_with(Class::Wrapper, op, 4);
        unsafe { (*vm.number_prototype).code = Value::from_i32(0).to_bits() as *const Code };
        let np = vm.number_prototype;
        vm.define_native(
            np,
            "toFixed",
            number_to_fixed,
            1,
            crate::jit::Ty::Any,
            false,
        );
        vm.define_native(
            np,
            "toPrecision",
            number_to_precision,
            1,
            crate::jit::Ty::Any,
            false,
        );
        vm.define_native(
            np,
            "toString",
            number_to_string_method,
            1,
            crate::jit::Ty::Any,
            false,
        );
        vm.define_native(ap, "indexOf", crate::array::array_index_of, 1, crate::jit::Ty::I32, false);
        let math = vm.new_object_with(Class::Plain, op, 16);
        let a = vm.intern("Math");
        vm.add_property_with(g, a, object_value(math), DONT_ENUM);
        for (name, v) in [
            ("E", std::f64::consts::E),
            ("PI", std::f64::consts::PI),
            ("LN2", std::f64::consts::LN_2),
            ("LN10", std::f64::consts::LN_10),
            ("LOG2E", std::f64::consts::LOG2_E),
            ("LOG10E", std::f64::consts::LOG10_E),
            ("SQRT2", std::f64::consts::SQRT_2),
        ] {
            let a = vm.intern(name);
            vm.add_property_with(
                math,
                a,
                Value::from_f64(v),
                READ_ONLY | DONT_ENUM | DONT_DELETE,
            );
        }
        // Results compiled code takes for doubles (checked, and unboxed:
        // `1 / Math.sqrt(x)` divides doubles), for int32s (`Math.floor`'s
        // and the like: as doubles they would make the fields and elements
        // they go to doubles, Crypto's digits; a call giving another is
        // predicted no more), or of any type (`abs`, `min`, `max`, as
        // often of doubles).
        use crate::jit::Ty::{Any, F64, I32};
        for (name, f, n, ty) in [
            ("log", math_log as NativeFn, 1, F64),
            ("exp", math_exp, 1, F64),
            ("sqrt", math_sqrt, 1, F64),
            ("abs", math_abs, 1, Any),
            ("floor", math_floor, 1, I32),
            ("ceil", math_ceil, 1, I32),
            ("round", math_round, 1, I32),
            ("sin", math_sin, 1, F64),
            ("cos", math_cos, 1, F64),
            ("pow", math_pow, 2, F64),
            ("min", math_min, 2, Any),
            ("max", math_max, 2, Any),
            ("random", math_random, 0, F64),
        ] {
            let fo = vm.define_native(math, name, f, n, ty, false);
            match name {
                "sqrt" => vm.math_sqrt_fn = fo,
                "pow" => vm.math_pow_fn = fo,
                "max" => vm.math_max_fn = fo,
                "min" => vm.math_min_fn = fo,
                _ => {}
            }
        }
        vm.date_prototype = vm.new_object(op);
        for (name, f, proto) in [
            ("Array", array_constructor as NativeFn, vm.array_prototype),
            (
                "Object",
                object_constructor as NativeFn,
                vm.object_prototype,
            ),
        ] {
            let c = vm.define_native(g, name, f, 1, crate::jit::Ty::Any, true);
            if name == "Array" {
                vm.array_fn = c;
            }
            let (a_p, a_c) = (vm.a_prototype, vm.a_constructor);
            vm.add_property_with(
                c,
                a_p,
                object_value(proto),
                READ_ONLY | DONT_ENUM | DONT_DELETE,
            );
            // Not enumerable, as the builtins' properties.
            vm.add_property_with(proto, a_c, object_value(c), DONT_ENUM);
        }
        crate::builtins::install(vm);
    }

    // ---- Names ----

    pub fn intern(&mut self, s: &str) -> Atom {
        if let Some(&a) = self.atom_ids.get(s) {
            return a;
        }
        let a = Atom(self.atoms.len() as u32);
        let name: std::rc::Rc<str> = s.into();
        self.atoms.push(name.clone());
        self.atom_ids.insert(name, a);
        a
    }

    pub fn atom_name(&self, a: Atom) -> &str {
        &self.atoms[a.0 as usize]
    }

    // ---- Shapes ----

    /// Shapes made so far and the bytes they take with their slot vectors,
    /// transitions and field cells; codes, the bytes they take, and those
    /// of them pinned; zombie shapes (`CLOISON_SIZES`).
    pub fn memory_stats(&self) -> [usize; 6] {
        // (Each slot table once.)
        let mut tables: crate::jit::HashSet<usize> = Default::default();
        let shape_bytes: usize = self
            .shapes
            .iter()
            .map(|s| {
                let t = s.slots.table();
                size_of::<Shape>()
                    + if tables.insert(t as *const _ as usize) { t.bytes() } else { 0 }
                    + s.transitions.borrow().capacity() * size_of::<(Atom, u8, *const Shape)>()
                    + s.elem_transitions.borrow().capacity() * size_of::<(FieldType, *const Shape)>()
            })
            .sum::<usize>()
            + self.field_cells.len() * size_of::<FieldCell>();
        let code_bytes = self.codes.iter().map(|c| c.footprint()).sum();
        let pinned = self.codes.iter().filter(|c| c.pinned.get()).count();
        [self.shapes.len(), shape_bytes, self.codes.len(), code_bytes, pinned, self.zombies.len()]
    }

    /// The shapes the VM's tables hold: each realm's intrinsics'.
    pub(crate) fn fixed_shapes(&self) -> Vec<*const Shape> {
        let mut out = vec![self.string_shape];
        crate::realm::current_shapes(self, &mut out);
        for r in &self.realms {
            r.shapes(&mut out);
        }
        out.retain(|s| !s.is_null());
        out
    }

    /// The shapes made, by index (for the collector).
    pub(crate) fn shape_at(&self, i: usize) -> &Shape {
        &self.shapes[i]
    }

    pub(crate) fn shape_count(&self) -> usize {
        self.shapes.len()
    }

    /// Shapes made since a full marking last collected them (see
    /// `gc::SHAPE_SWEEP_MIN`).
    pub(crate) fn shapes_made(&self) -> usize {
        self.shapes_made
    }

    pub(crate) fn reset_shapes_made(&mut self) {
        self.shapes_made = 0;
        self.cells_made = 0;
    }

    /// Field cells made since a full marking last collected shapes.
    pub(crate) fn cells_made(&self) -> usize {
        self.cells_made
    }

    pub(crate) fn cell_count(&self) -> usize {
        self.field_cells.len()
    }

    /// A new field cell, for a slot table to have.
    fn new_field_cell(&mut self) -> *const FieldCell {
        let p = self.field_cells.alloc();
        self.cells_made += 1;
        if self.cells_made % 4096 == 0 {
            self.shapes_made_check();
        }
        p
    }

    /// Shapes and cells are outside the heap: a program that makes them
    /// and little else would rarely collect. Once enough are made, a
    /// collection at the next safepoint (see `gc::SHAPE_FULL_MIN`); a
    /// minor one, if the last full marking condemned shapes, which it frees
    /// once the sweep is over (else they wait for the next marking, while
    /// as many more are made: del.js's peak).
    fn shapes_made_check(&mut self) {
        if self.shapes_full() || !self.condemned_shapes.is_empty() && !self.heap.gc.marking {
            self.heap.requested.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// After a full marking: the constructor tracking for prototypes it
    /// left unmarked goes (their addresses may be reused), the others'
    /// root shapes are live.
    pub(crate) fn sweep_ctor_protos(&mut self, live: impl Fn(usize) -> bool) {
        let epoch = crate::gc::code_epoch();
        self.ctor_protos.retain(|&p, e| {
            if !live(p as usize) {
                return false;
            }
            let s = e.root.get();
            if !s.is_null() {
                unsafe { (*s).mark.store(epoch, std::sync::atomic::Ordering::Relaxed) };
            }
            true
        });
    }

    /// After a full marking, the shapes it didn't find live, nor were made
    /// or given to an object since it began, nor are pinned: no object has
    /// one or can be given one (no live shape has a transition to it, no
    /// cache or table holds it, compiled code doesn't make it). Each becomes
    /// a zombie: its transitions, push cache and prototype dropped (the
    /// prototype may die), its capacity impossible, kept so that its
    /// address isn't reused while compiled code, caches or the JIT's
    /// records may still compare a shape with it (never equal now) or read
    /// its slots: then freed (`free_zombies`).
    pub(crate) fn sweep_shapes(&mut self, from: u32) {
        use std::sync::atomic::Ordering::Relaxed;
        let mut t = std::time::Instant::now();
        let live = |s: *const Shape| unsafe { (*s).pinned.get() || (*s).mark.load(Relaxed) >= from };
        let (keep, dead): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.shapes).into_iter().partition(|s| live(&**s));
        self.shapes = keep;
        let ph = &mut self.heap.gc.shape_phases;
        (ph.dead, ph.shapes_left, ph.any) = (dead.len(), self.shapes.len(), true);
        if dead.is_empty() {
            self.heap.gc.shape_phases.sweep = crate::gc::lap(&mut t);
            self.free_zombies();
            return;
        }
        for s in &self.shapes {
            s.transitions.borrow_mut().retain(|&(_, _, t)| live(t));
            s.elem_transitions.borrow_mut().retain(|&(_, t)| live(t));
            // Compiled code moves arrays to the shapes a push cache names
            // (a zombie's prototype is null): those of the dead go, as do
            // the entries of dead keys.
            let p = s.push.get();
            if !p.is_null() {
                let c = unsafe { &mut *p };
                for e in c.to.iter_mut() {
                    let key_dead = e.0 > crate::layout::FIELD_NUM && !live(e.0 as *const Shape);
                    if e.1 != 0 && (key_dead || !live(e.1 as *const Shape)) {
                        *e = (0, 0);
                    }
                }
                if c.any != 0 && !live(c.any as *const Shape) {
                    c.any = 0;
                }
            }
        }
        // And the megamorphic store table's transitions (entry 0 holds the
        // prototype epoch's address).
        for e in self.mega_stores[1..].iter_mut() {
            if e.shape != 0 && (!live(e.shape as *const Shape) || e.new_shape != 0 && !live(e.new_shape as *const Shape)) {
                *e = MegaStore::EMPTY;
            }
        }
        // (A dead shape is a zombie still: its address may be read.)
        self.wide_transitions.retain(|&(s, _, _), t| live(s as *const Shape) && live(*t));
        self.roots.retain(|_, s| live(*s));
        for mut z in dead {
            z.proto = Value::NULL;
            // (`Shape::is_zombie`.)
            z.capacity.set(u32::MAX);
            z.transitions.take();
            z.elem_transitions.take();
            let p = z.push.replace(std::ptr::null_mut());
            if !p.is_null() {
                drop(unsafe { Box::from_raw(p) });
            }
            self.zombies.push(z);
        }
        self.heap.gc.shape_phases.sweep = crate::gc::lap(&mut t);
        self.free_zombies();
    }

    /// Condemn the zombies nothing may read or compare a shape with any
    /// more (see `condemned_shapes`): those no inline cache, lookup table,
    /// push cache, live shape's field or elements, constructor tracking or
    /// state of the JIT holds (`jit::shape_words`), nor compiled code
    /// (pinned: see `jit`'s relocations), nor a zombie kept (its parent,
    /// family, fields: the JIT reads a kept one's).
    fn free_zombies(&mut self) {
        // (Its scan of the caches and the JIT's state, once for many: at
        // every sweep, it took code-load 0.75% more instructions.)
        if self.zombies.len() < ZOMBIES_MIN {
            return;
        }
        let mut t = std::time::Instant::now();
        /// The zombies found held, those whose own references are still to
        /// be followed, and the slot tables whose fields were (each once,
        /// all of them: a lineage's shapes share one).
        struct Held {
            held: crate::jit::HashSet<usize>,
            work: Vec<usize>,
            tables: crate::jit::HashSet<usize>,
        }
        impl Held {
            fn hold(&mut self, w: u64) {
                let a = w as usize;
                let zombie = || crate::object::is_shape_address(a) && unsafe { (*(a as *const Shape)).is_zombie() };
                if a != 0 && zombie() && self.held.insert(a) {
                    self.work.push(a);
                }
            }
            fn refs(&mut self, s: &Shape) {
                self.hold(s.parent as u64);
                self.hold(s.family as u64);
                let t = s.slots.table();
                if self.tables.insert(t as *const _ as usize) {
                    for &c in t.all_fields() {
                        if let FieldType::Obj(t) = unsafe { (*c).get() } {
                            self.hold(t as u64);
                        }
                    }
                }
                if let FieldType::Obj(t) = s.elems {
                    self.hold(t as u64);
                }
                for &(k, _) in s.elem_transitions.borrow().iter() {
                    if let FieldType::Obj(t) = k {
                        self.hold(t as u64);
                    }
                }
                let p = s.push.get();
                if !p.is_null() {
                    let c = unsafe { &*p };
                    for &(k, t) in &c.to {
                        self.hold(k);
                        self.hold(t);
                    }
                    self.hold(c.any);
                }
            }
        }
        let mut h = Held {
            held: Default::default(),
            work: Vec::new(),
            tables: Default::default(),
        };
        if !crate::jit::shape_words(self, &mut |w| h.hold(w)) {
            self.heap.gc.shape_phases.zombies = crate::gc::lap(&mut t);
            return;
        }
        for z in &self.zombies {
            if z.pinned.get() {
                h.hold(&**z as *const Shape as u64);
            }
        }
        for c in self.codes.iter().chain(&self.condemned) {
            for ways in c.made_ics() {
                for ic in unsafe { &*ways.get() } {
                    h.hold(ic.shape as u64);
                    h.hold(ic.new_shape as u64);
                }
            }
            h.hold(c.ctor_shape.get() as u64);
            h.hold(c.ctor_result.get() as u64);
        }
        for e in self.lookups.iter() {
            h.hold(e.get().shape as u64);
        }
        for e in self.megas.iter() {
            h.hold(e.shape);
        }
        for e in self.mega_stores.iter() {
            h.hold(e.shape);
            h.hold(e.ft);
            h.hold(e.new_shape);
        }
        for e in self.ctor_protos.values() {
            h.hold(e.root.get() as u64);
        }
        for s in &self.shapes {
            h.refs(s);
        }
        while let Some(a) = h.work.pop() {
            h.refs(unsafe { &*(a as *const Shape) });
        }
        let held = h.held;
        let (keep, free): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.zombies).into_iter().partition(|z| held.contains(&(&**z as *const Shape as usize)));
        self.zombies = keep;
        let ph = &mut self.heap.gc.shape_phases;
        (ph.condemned, ph.zombies_left) = (free.len(), self.zombies.len());
        self.condemned_shapes.extend(free);
        self.heap.gc.shape_phases.zombies = crate::gc::lap(&mut t);
    }

    /// The hash of an `eval` of `source` in `place`, for [`Vm::cached_eval`]
    /// and [`Vm::keep_eval`] (`None`: a long source's, not kept): of its
    /// length and its first and last 64 units, as entries are told apart
    /// by their whole source. (Hashing it all took 0.7% of code-load, whose
    /// 1,700 sources of 9 KB are all distinct.)
    pub(crate) fn eval_hash(source: &[u16], place: EvalPlace) -> Option<u64> {
        use std::hash::{BuildHasher, BuildHasherDefault};
        let n = source.len();
        let ends = (&source[..n.min(64)], &source[n.saturating_sub(64)..]);
        (n <= EVAL_CACHE_TEXT_MAX).then(|| BuildHasherDefault::<crate::jit::FxHasher>::default().hash_one((n, ends, place)))
    }

    /// Push a direct `eval`'s scope objects on `eval_envs` (see
    /// `intrinsics::eval_env`), into the room one before left.
    pub(crate) fn push_eval_envs(&mut self, envs: impl IntoIterator<Item = Value>) {
        if self.eval_depth == self.eval_envs.len() {
            self.eval_envs.push(Vec::new());
        }
        let e = &mut self.eval_envs[self.eval_depth];
        e.clear();
        e.extend(envs);
        self.eval_depth += 1;
    }

    pub(crate) fn pop_eval_envs(&mut self) {
        self.eval_depth -= 1;
    }

    /// The kept code of an `eval` of `source` in `place` (`hash` their
    /// hash), now used. (Not another realm's: code runs in the realm it was
    /// loaded in.)
    pub(crate) fn cached_eval(&mut self, source: &[u16], place: EvalPlace, hash: Option<u64>) -> Option<&EvalCode> {
        // (One not seen is not kept: no lookup.)
        if !self.eval_seen.contains(&hash?) {
            return None;
        }
        self.eval_uses += 1;
        let (uses, realm) = (self.eval_uses, self.realm);
        let e = self.eval_cache.get_mut(&hash?)?.iter_mut().find(|e| {
            e.place == place && *e.source == *source && unsafe { (*e.code).realm } == realm
        })?;
        e.used = uses;
        e.recent = true;
        Some(e)
    }

    /// Keep `code` (`hash` its source's and place's hash), an `eval`'s, for
    /// it to run again: whether it is kept. Not the first time (see
    /// `Vm::eval_seen`), nor a long one's (run once, likely), nor one with
    /// a template, whose tagged templates' strings arrays are each
    /// evaluation's own (a new parse of it: `template_object`'s sites are
    /// the code's).
    pub(crate) fn keep_eval(&mut self, hash: Option<u64>, code: EvalCode) -> bool {
        let Some(hash) = hash.filter(|_| !code.source.contains(&(b'`' as u16))) else {
            return false;
        };
        if self.eval_seen.len() >= EVAL_SEEN_MAX {
            self.eval_seen.clear();
        }
        if self.eval_seen.insert(hash) {
            return false;
        }
        if self.eval_kept >= EVAL_CACHE_MAX
            && let Some((h, i)) = self
                .eval_cache
                .iter()
                .flat_map(|(&h, es)| es.iter().enumerate().map(move |(i, e)| (h, i, e.used)))
                .min_by_key(|&(_, _, used)| used)
                .map(|(h, i, _)| (h, i))
        {
            let es = self.eval_cache.get_mut(&h).unwrap();
            es.swap_remove(i);
            if es.is_empty() {
                self.eval_cache.remove(&h);
            }
            self.eval_kept -= 1;
        }
        // (The same source and place of another realm's: replaced.)
        let es = self.eval_cache.entry(hash).or_default();
        if let Some(i) = es.iter().position(|e| e.place == code.place && e.source == code.source) {
            es.swap_remove(i);
            self.eval_kept -= 1;
        }
        let used = self.eval_uses;
        es.push(EvalCode { used, recent: true, ..code });
        self.eval_kept += 1;
        true
    }

    /// As a full marking begins: the `eval`s' code run since the
    /// last one began is live (kept), the rest goes, to be freed if
    /// nothing else has it.
    pub(crate) fn age_eval_cache(&mut self) {
        for es in self.eval_cache.values_mut() {
            es.retain_mut(|e| std::mem::take(&mut e.recent));
        }
        self.eval_cache.retain(|_, es| !es.is_empty());
        self.eval_kept = self.eval_cache.values().map(Vec::len).sum();
        for e in self.eval_cache.values().flatten() {
            crate::gc::mark_code(e.code);
        }
    }

    /// Drop the condemned shapes, and free the field cells no slot table
    /// has any more (a unique shape's, `delete`'s, and a direct `eval`'s
    /// scopes' made a cell for each field): those of the tables of shapes
    /// no live shape descends from, left unused as they drop. The
    /// functions whose code relied on a cell's type are forgotten with it,
    /// its address then another's.
    pub(crate) fn free_condemned_shapes(&mut self) {
        if self.condemned_shapes.is_empty() {
            return;
        }
        let mut t = std::time::Instant::now();
        let ph = &mut self.heap.gc.shape_phases;
        (ph.dropped, ph.any) = (self.condemned_shapes.len(), true);
        let condemned = &mut self.condemned_shapes;
        let freed = crate::object::cells_left_unused(|| condemned.clear());
        self.heap.gc.shape_phases.drop = crate::gc::lap(&mut t);
        if !freed.is_empty() {
            // (The cells these hold are not freed yet: those unused are
            // being freed. Far fewer than those freed, not hashed.)
            let unused = |c: usize| unsafe { (*(c as *const FieldCell)).unused() };
            self.field_deps.retain(|&c, _| !unused(c));
            self.through_deps.retain(|&c, _| !unused(c));
            self.int_fields.retain(|&c| !unused(c as usize));
            for cells in self.relied_cells.values_mut() {
                cells.retain(|&c| !unused(c));
            }
            for &c in &freed {
                unsafe { self.field_cells.free(c) };
            }
        }
        let ph = &mut self.heap.gc.shape_phases;
        (ph.cells_freed, ph.cells_left) = (freed.len(), self.field_cells.len());
        self.heap.gc.shape_phases.cells = crate::gc::lap(&mut t);
    }

    pub(crate) fn add_shape(&mut self, s: Shape) -> *const Shape {
        // (A Rust structure: its prototype is logged if young.)
        self.heap.note(s.proto);
        let mut b = crate::object::ShapeBox::new(s);
        let p: *const Shape = &*b;
        if b.family.is_null() {
            b.family = p;
        }
        // (Live in a full marking under way: no object it finds has it yet.)
        b.mark.store(crate::gc::code_epoch(), std::sync::atomic::Ordering::Relaxed);
        self.shapes.push(b);
        self.shapes_made += 1;
        if self.shapes_made % 4096 == 0 {
            self.shapes_made_check();
        }
        p
    }

    /// The shared root shape for `class`, `proto` and `capacity`. Marks
    /// `proto` as a prototype.
    pub(crate) fn root_shape(&mut self, class: Class, proto: Value, capacity: u32) -> *const Shape {
        let key = (class, proto.to_bits(), capacity);
        if let Some(&s) = self.roots.get(&key) {
            // (Live in a full marking under way: it may go to a cache, not
            // an object, as `CreateObject`'s.)
            unsafe { crate::object::mark_shape(s) };
            return s;
        }
        if proto.is_cell() {
            self.make_prototype(as_object(proto));
        }
        let primitive = matches!(class, Class::BigInt | Class::Symbol);
        let s = self.add_shape(Shape {
            class,
            proto,
            capacity: Cell::new(capacity),
            slots: Slots::empty(),
            // (BigInts and symbols are made not extensible, sharing this
            // shape: elements of any type, so that no store generalizes
            // them, as `Vm::unique_shape`'s with `no_elements`.)
            elems: if primitive { FieldType::Any } else { FieldType::None },
            // (No value fits a typed array's or buffer's bytes, so stores'
            // fast paths never take them.)
            elems_code: if primitive || matches!(class, Class::Array | Class::TypedArray) || class.no_elements() {
                NO_ELEMENTS
            } else {
                0
            },
            family: std::ptr::null(),
            push: Cell::new(std::ptr::null_mut()),
            unique: false,
            fixed: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
            left: Cell::new(false),
            lineage_relied: Cell::new(false),
            parent: std::ptr::null(),
            mark: Default::default(),
            traced: Cell::new(0),
            pinned: Cell::new(false),
            dictionary: false,
        });
        self.roots.insert(key, s);
        s
    }

    /// A root shape for `proto` and `capacity` that no other gets (see
    /// `Code::ctor_settled`). Marks `proto` as a prototype.
    fn private_root(&mut self, proto: Value, capacity: u32) -> *const Shape {
        if proto.is_cell() {
            self.make_prototype(as_object(proto));
        }
        self.add_shape(Shape {
            class: Class::Plain,
            proto,
            capacity: Cell::new(capacity),
            slots: Slots::empty(),
            elems: FieldType::None,
            elems_code: 0,
            family: std::ptr::null(),
            push: Cell::new(std::ptr::null_mut()),
            unique: false,
            fixed: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
            left: Cell::new(false),
            lineage_relied: Cell::new(false),
            parent: std::ptr::null(),
            mark: Default::default(),
            traced: Cell::new(0),
            pinned: Cell::new(false),
            dictionary: false,
        })
    }

    /// `shape` plus property `name` with attributes `attrs`. Shared shapes
    /// share transitions; unique shapes get a fresh unique successor. Its
    /// slots extend `shape`'s table in place if `shape` has all of it (see
    /// `Slots::with`).
    pub(crate) fn transition_with(
        &mut self,
        shape: *const Shape,
        name: Atom,
        attrs: u8,
    ) -> *const Shape {
        let s = unsafe { &*shape };
        if !s.unique
            && !s.fixed
            && let Some(t) = self.find_transition(s, name, attrs)
        {
            return t;
        }
        if name == self.a_sym_to_primitive {
            self.to_primitive_defined = true;
        }
        if name == self.a_sym_has_instance {
            self.has_instance_defined = true;
        }
        // A dictionary shape gets the property in place.
        if s.dictionary {
            let cell = self.dictionary_cell();
            let slots = s.slots.with(name, attrs, cell);
            unsafe { Shape::dictionary_mut(shape).slots = slots };
            return shape;
        }
        let cell = self.new_field_cell();
        let slots = s.slots.with(name, attrs, cell);
        let t = self.add_shape(Shape {
            class: s.class,
            proto: s.proto,
            capacity: Cell::new(s.capacity.get()),
            slots,
            elems: s.elems,
            elems_code: s.elems_code,
            family: std::ptr::null(),
            push: Cell::new(std::ptr::null_mut()),
            // (A fixed shape's successors are unique: see `Shape::fixed`.)
            unique: s.unique || s.fixed,
            fixed: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
            left: Cell::new(false),
            lineage_relied: Cell::new(false),
            parent: s,
            mark: Default::default(),
            traced: Cell::new(0),
            pinned: Cell::new(false),
            dictionary: false,
        });
        if !s.unique {
            self.shape_left_stable(s);
            if !s.fixed {
                self.add_transition(s, name, attrs, t);
            }
        }
        t
    }

    /// Shared shape `s`'s transition for property `name` with attributes
    /// `attrs`, if it has one: from [`Vm::wide_transitions`] if it has
    /// many.
    pub(crate) fn find_transition(&self, s: &Shape, name: Atom, attrs: u8) -> Option<*const Shape> {
        let ts = s.transitions.borrow();
        if ts.len() < WIDE_TRANSITIONS {
            ts.iter().find(|&&(n, a, _)| n == name && a == attrs).map(|&(_, _, t)| t)
        } else {
            self.wide_transitions.get(&(s as *const Shape as usize, name.0, attrs)).copied()
        }
    }

    /// Record shared shape `s`'s transition to `t` (see `find_transition`).
    pub(crate) fn add_transition(&mut self, s: &Shape, name: Atom, attrs: u8, t: *const Shape) {
        let mut ts = s.transitions.borrow_mut();
        ts.push((name, attrs, t));
        let key = s as *const Shape as usize;
        if ts.len() == WIDE_TRANSITIONS {
            for &(n, a, t) in ts.iter() {
                self.wide_transitions.insert((key, n.0, a), t);
            }
        } else if ts.len() > WIDE_TRANSITIONS {
            self.wide_transitions.insert((key, name.0, attrs), t);
        }
    }

    /// Flag `o` as a prototype and give it a unique shape, so that no
    /// inline cache can treat a property added to it as an ordinary
    /// transition (which would skip the epoch bump).
    pub(crate) fn make_prototype(&mut self, o: *mut Object) {
        unsafe {
            if (*o).flags & PROTOTYPE != 0 {
                return;
            }
            // (V8 makes a prototype fast again: `ReoptimizeIfPrototype`.)
            self.leave_dictionary(o);
            (*o).flags |= PROTOTYPE;
            let s = &*(*o).shape;
            // The object leaves `s` (for a copy of its own).
            self.shape_left_stable(s);
            let u = Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                slots: s.slots.clone(),
                elems: s.elems,
                elems_code: s.elems_code,
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: true,
                fixed: false,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
            relied: Cell::new(false),
            left: Cell::new(false),
            lineage_relied: Cell::new(false),
            parent: s,
            mark: Default::default(),
            traced: Cell::new(0),
            pinned: Cell::new(false),
            dictionary: false,
            };
            crate::object::set_shape(o, self.add_shape(u));
        }
    }

    // ---- Objects ----

    pub(crate) fn new_object_with(&mut self, class: Class, proto: Value, capacity: u32) -> *mut Object {
        let s = self.root_shape(class, proto, capacity);
        self.heap.alloc_object(unsafe { &*s })
    }

    /// A plain object with prototype `proto`.
    pub fn new_object(&mut self, proto: Value) -> *mut Object {
        self.new_object_with(Class::Plain, proto, 4)
    }

    pub fn new_array(&mut self) -> Value {
        object_value(self.heap.alloc_object(unsafe { &*self.array_shape }))
    }

    /// Add property `name` (which `o` must not have) with `value`, and
    /// return its slot.
    pub(crate) fn add_property(&mut self, o: *mut Object, name: Atom, value: Value) -> u32 {
        self.add_property_with(o, name, value, 0)
    }

    /// `add_property` with attributes `attrs`.
    pub(crate) fn add_property_with(
        &mut self,
        o: *mut Object,
        name: Atom,
        value: Value,
        attrs: u8,
    ) -> u32 {
        let new = self.transition_with(unsafe { (*o).shape }, name, attrs);
        self.add_property_shaped(o, new, value)
    }

    /// Room in `o`'s out-of-line slots (`props`) for slot `i`: doubled
    /// (at least 4) if there is none.
    #[inline]
    pub(crate) fn reserve_props(&mut self, o: *mut Object, i: u32) {
        unsafe {
            let old = Object::props_cap(o);
            if i >= old {
                let cap = (old * 2).max(4).max(i + 1);
                let p = self.heap.alloc_values(cap as usize);
                if !(*o).props.is_null() {
                    std::ptr::copy_nonoverlapping((*o).props, p, old as usize);
                }
                (*o).props = p;
                crate::heap::barrier_young(o.cast(), &raw const (*o).props);
            }
        }
    }

    /// Add the property that shape `new`, a transition from `o`'s, adds,
    /// with `value`: its slot.
    pub(crate) fn add_property_shaped(&mut self, o: *mut Object, new: *const Shape, value: Value) -> u32 {
        unsafe {
            let ns = &*new;
            let slot = ns.keys().len() as u32 - 1;
            if slot >= ns.capacity.get() {
                self.reserve_props(o, slot - ns.capacity.get());
            }
            crate::object::set_shape(o, new);
            self.store_field(o, slot, value);
            if (*o).flags & PROTOTYPE != 0 {
                self.proto_epoch = self.proto_epoch.wrapping_add(1);
                self.bump_code_epoch();
            }
            slot
        }
    }

    /// Whether an inline cache may cache stores to slot `slot` of `o`'s
    /// shape: not while the field is constant on the global object or a
    /// function, whose constant fields compiled code folds; on other
    /// objects, once the field is made mutable (see `FieldCell`).
    fn may_cache_store(&mut self, o: *mut Object, slot: u32) -> bool {
        let cell = unsafe { (*(*o).shape).field_cell(slot) };
        if !cell.is_constant() {
            return true;
        }
        if o == self.global || unsafe { (*o).class } == Class::Function {
            return false;
        }
        self.field_made_mutable(cell);
        true
    }

    /// Something all compiled code may rely on changed: the own epoch
    /// (`Code::field_deps`) of each function the JIT has state for
    /// (`jitted`) moves, which its code, once a call returns, and its next
    /// entry notice. (Others compare the VM's epochs when they compile.)
    pub(crate) fn bump_code_epoch(&mut self) {
        self.code_epoch = self.code_epoch.wrapping_add(1);
        for &c in &self.jitted {
            let c = unsafe { &*c };
            c.field_deps.set(c.field_deps.get().wrapping_add(1));
        }
    }

    /// Shape `s` gets a transition: drop compiled code that relied on it
    /// being stable (see `Shape::relied`).
    pub(crate) fn shape_left_stable(&mut self, s: &Shape) {
        if s.relied.replace(false) {
            self.field_epoch = self.field_epoch.wrapping_add(1);
            self.bump_code_epoch();
        }
    }

    /// An object of shape `s` moves to one without some of its slots (or
    /// another prototype): `s` and the shapes it was made from are no longer
    /// stable, now or later, and compiled code that took an object of at
    /// least one of them to have its slots is dropped (see `Shape::left`).
    pub(crate) fn shape_left(&mut self, s: &Shape) {
        self.shape_left_stable(s);
        let mut p: *const Shape = s;
        while !p.is_null() && unsafe { !(*p).left.get() } {
            let shape = unsafe { &*p };
            shape.left.set(true);
            if shape.lineage_relied.replace(false) {
                self.field_epoch = self.field_epoch.wrapping_add(1);
                self.bump_code_epoch();
            }
            p = shape.parent;
        }
    }

    /// Make constant field `cell` mutable, dropping compiled code that
    /// folded it (see `FieldCell`).
    pub(crate) fn field_made_mutable(&mut self, cell: &FieldCell) {
        if cell.make_mutable() {
            self.field_epoch = self.field_epoch.wrapping_add(1);
            self.bump_code_epoch();
        }
    }

    /// Write `value` to slot `slot` of `o`, generalizing the field's type
    /// first if `value` doesn't fit it. Every store to a field that doesn't
    /// go through an inline cache whose check passed must use this.
    pub(crate) fn store_field(&mut self, o: *mut Object, slot: u32, value: Value) {
        unsafe {
            let shape = &*(*o).shape;
            let cell = &*shape.fields()[slot as usize];
            let at = Object::slot(o, shape.location(slot));
            let value = as_field(cell.get(), value);
            // The common case, calling nothing: the type holds, and the
            // field isn't constant or a prototype's, or keeps its value.
            let unchanged = *at == value;
            if cell.get().join(FieldType::of(value)) == cell.get()
                && (unchanged || !cell.is_constant() || *at == Value::UNDEFINED)
                && (unchanged || (*o).flags & PROTOTYPE == 0)
            {
                *at = value;
                crate::heap::barrier(o.cast(), at, value);
                return;
            }
            self.store_field_slow(o, slot, value);
        }
    }

    #[cold]
    #[inline(never)]
    fn store_field_slow(&mut self, o: *mut Object, slot: u32, value: Value) {
        unsafe {
            let shape = &*(*o).shape;
            let cell = &*shape.fields()[slot as usize];
            let old = cell.get();
            let new = old.join(FieldType::of(value));
            if new != old {
                cell.set(new);
                // Its int32s become doubles once compiled code would read
                // them as such (`int_fields_to_doubles`).
                if old == FieldType::Int && new == FieldType::Num {
                    cell.set_ints(true);
                    self.int_fields.push(cell);
                }
                if cell.take_relied() {
                    self.field_generalized(cell, old);
                }
            }
            let value = as_field(new, value);
            let at = Object::slot(o, shape.location(slot));
            // Constant while stores fill the field or store what it holds
            // (see `FieldCell`).
            if cell.is_constant() && *at != value && *at != Value::UNDEFINED {
                self.field_made_mutable(cell);
            }
            // A prototype's slots whose values compiled code folded hold
            // them while the prototype epoch holds (`relied_slots`); every
            // store to one comes here (inline caches don't cache them).
            if (*o).flags & PROTOTYPE != 0 && *at != value {
                self.prototype_slot_changed(at);
            }
            *at = value;
            crate::heap::barrier(o.cast(), at, value);
        }
    }

    /// Field `cell`, whose type compiled code took, generalized from
    /// `old`: the functions whose code did recompile (code running notices
    /// after its calls), and other functions' direct calls into them are
    /// unbound; no code relies on an empty field's.
    #[cold]
    #[inline(never)]
    fn field_generalized(&mut self, cell: &FieldCell, old: FieldType) {
        let key = cell as *const FieldCell as usize;
        let deps = self.field_deps.remove(&key);
        let through = self.through_deps.remove(&key);
        if old != FieldType::None {
            // (Compiled code that stored through the runtime goes on only
            // if nothing it relies on changed: `jit::cloison_rt_set_named`.)
            self.code_epoch = self.code_epoch.wrapping_add(1);
            for code in deps.into_iter().flatten() {
                let c = unsafe { &*(code as *const Code) };
                c.field_deps.set(c.field_deps.get().wrapping_add(1));
            }
            for (caller, callee) in through.into_iter().flatten() {
                unsafe { crate::jit::unbind_calls(caller as *const Code, callee as *const Code) };
            }
        }
    }

    /// The fields in `int_fields`, which went from `Int` to `Num`, hold
    /// doubles only from here on in objects' own memory (see
    /// `FieldType::Num`): their int32s there become doubles, in every
    /// object in the heap, live or not, in one walk of it. Out of line
    /// they stay: a dead object's `props` may be some other cell's by now.
    /// The JIT calls this before it compiles a load that takes a double
    /// from one of them.
    #[cold]
    #[inline(never)]
    pub(crate) fn int_fields_to_doubles(&mut self) {
        use crate::heap::{cell_size, meta, COLOR, GRANULE, YOUNG};
        if self.int_fields.is_empty() {
            return;
        }
        let cells: std::collections::HashSet<*const FieldCell> =
            self.int_fields.drain(..).collect();
        for &c in &cells {
            unsafe { (*c).set_ints(false) };
        }
        // Stores the interpreter's caches took for the int32s the field
        // held check for a number now (see `FieldType::encode`).
        let int = FieldType::Int.encode() as *mut Object;
        for code in &self.codes {
            for ways in code.made_ics() {
                for w in unsafe { &mut *ways.get() } {
                    if w.holder != int || w.shape.is_null() {
                        continue;
                    }
                    let s = unsafe { &*if w.new_shape.is_null() { w.shape } else { w.new_shape } };
                    // (The slot at `w.loc`, inverting `Shape::location`: not a
                    // search of every field, which shapes of objects with
                    // hundreds of properties out of line made slow.)
                    let cap = s.capacity.get() as i64;
                    let slot = if w.loc >= 0 {
                        (w.loc as i64 - crate::layout::slots_offset(s.class) as i64) / 8
                    } else {
                        cap + !w.loc as i64
                    };
                    if (0..s.fields().len() as i64).contains(&slot)
                        && s.location(slot as u32) == w.loc
                        && cells.contains(&s.fields()[slot as usize])
                    {
                        w.holder = FieldType::Num.encode() as *mut Object;
                    }
                }
            }
        }
        // Per shape, its slots of `cells` in objects' own memory, as offsets.
        // (Objects of a shape mostly come together: the last one's first.)
        let mut slots: HashMap<*const Shape, std::rc::Rc<[i32]>> = HashMap::default();
        let mut last: (*const Shape, std::rc::Rc<[i32]>) = (std::ptr::null(), std::rc::Rc::from([]));
        for (start, end) in self.heap.block_ranges() {
            let mut p = start;
            while p < end {
                if unsafe { *meta(p) } & (YOUNG | COLOR) == 0 {
                    // Free space: eight granules at a time while their
                    // metadata is all 0.
                    while p + 8 * GRANULE <= end
                        && unsafe { (meta(p) as *const u64).read_unaligned() } == 0
                    {
                        p += 8 * GRANULE;
                    }
                    if p < end && unsafe { *meta(p) } & (YOUNG | COLOR) == 0 {
                        p += GRANULE;
                    }
                    continue;
                }
                unsafe {
                    if *(p as *const u64) & 1 == 0 {
                        let o = p as *mut Object;
                        let shape = (*o).shape;
                        if shape != last.0 {
                            let at = slots.entry(shape).or_insert_with(|| {
                                let s = &*shape;
                                (0..s.fields().len() as u32)
                                    .filter(|&i| cells.contains(&s.fields()[i as usize]))
                                    .map(|i| s.location(i))
                                    .filter(|&l| l >= 0)
                                    .collect()
                            });
                            last = (shape, at.clone());
                        }
                        for &l in last.1.iter() {
                            let v = Object::slot(o, l);
                            if (*v).is_int32() {
                                *v = Value::from_f64((*v).as_i32_unchecked() as f64);
                            }
                        }
                    }
                    p += cell_size(p as *const u8);
                }
            }
        }
    }

    /// Function `caller`'s compiled code calls straight into `callee`'s
    /// (a bound direct call), whose code took field types: generalizing
    /// one unbinds those calls (`jit::unbind_calls`), which bind again to
    /// the callee's next code. `caller`'s own code stays. (Mandreel's
    /// `malloc` took global `tlsf_ptr` for an int32; a double stored there
    /// made it recompile, but a loop calling it kept calling the old code,
    /// which read the double's low half.)
    pub(crate) fn rely_through(&mut self, caller: usize, callee: usize) {
        if caller == callee || caller == 0 {
            return;
        }
        let Some(cells) = self.relied_cells.get(&callee) else {
            return;
        };
        for &cell in cells {
            if self.field_deps.get(&cell).is_some_and(|deps| deps.contains(&callee)) {
                let through = self.through_deps.entry(cell).or_default();
                if !through.contains(&(caller, callee)) {
                    through.push((caller, callee));
                }
            }
        }
    }

    /// A prototype's slot `at` gets another value: compiled code that
    /// folded it is dropped (see `relied_slots`).
    #[cold]
    #[inline(never)]
    fn prototype_slot_changed(&mut self, at: *mut Value) {
        if self.relied_slots.contains(&(at as usize)) {
            self.relied_slots.clear();
            self.proto_epoch = self.proto_epoch.wrapping_add(1);
            self.bump_code_epoch();
        }
    }

    /// `shape` with elements of type `elems`. Shared shapes share these
    /// transitions; unique ones get a fresh unique successor. Past
    /// [`MAX_ELEM_SHAPES`] objects' shapes, elements of objects of another
    /// are of any type.
    pub(crate) fn elements_transition(
        &mut self,
        shape: *const Shape,
        elems: FieldType,
    ) -> *const Shape {
        let s = unsafe { &*shape };
        // (A dictionary shape's elements are `Any`: none.)
        debug_assert!(!s.dictionary);
        if !s.unique && !s.fixed {
            let ts = s.elem_transitions.borrow();
            if let Some(&(_, t)) = ts.iter().find(|(e, _)| *e == elems) {
                return t;
            }
            if matches!(elems, FieldType::Obj(_))
                && ts.iter().filter(|(e, _)| matches!(e, FieldType::Obj(_))).count() >= MAX_ELEM_SHAPES
            {
                drop(ts);
                return self.elements_transition(shape, FieldType::Any);
            }
        }
        let t = self.add_shape(Shape {
            class: s.class,
            proto: s.proto,
            capacity: Cell::new(s.capacity.get()),
            slots: s.slots.clone(),
            elems,
            elems_code: elems.encode(),
            family: s.family,
            push: Cell::new(std::ptr::null_mut()),
            // (A fixed shape's successors are unique: see `Shape::fixed`.)
            unique: s.unique || s.fixed,
            fixed: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
            left: Cell::new(false),
            lineage_relied: Cell::new(false),
            parent: s,
            mark: Default::default(),
            traced: Cell::new(0),
            pinned: Cell::new(false),
            dictionary: false,
        });
        if !s.unique {
            self.shape_left_stable(s);
            if !s.fixed {
                s.elem_transitions.borrow_mut().push((elems, t));
            }
        }
        t
    }

    /// Write element `i` (within capacity) of `o`, first moving `o` to a
    /// shape whose elements' type `value` fits (see `Shape::elems`).
    pub(crate) fn store_element(&mut self, o: *mut Object, i: usize, value: Value) {
        unsafe {
            let s = &*(*o).shape;
            let elems = s.elems.join(FieldType::of(value));
            if elems != s.elems {
                let t = self.elements_transition((*o).shape, elems);
                // For the `array_push` stencils (see `Shape::push_key`).
                if !s.unique && s.class == crate::object::Class::Array {
                    if s.elems == FieldType::None {
                        s.with_push_cache(|c| {
                            c.to.rotate_right(1);
                            c.to[0] = (crate::object::push_key(value), t as u64);
                        });
                    } else if elems == FieldType::Any {
                        s.with_push_cache(|c| c.any = t as u64);
                    }
                }
                crate::object::set_shape(o, t);
            }
            *(*o).elems.add(i) = value;
            crate::heap::barrier((*o).elems.cast::<u8>().sub(8), (*o).elems.add(i), value);
        }
    }

    /// Write the `n` values at `from` (holes as they are) to `o`'s
    /// elements `at` on (within capacity; `from` may be in them): each
    /// value that fits `o`'s elements' type is only written.
    pub(crate) fn store_elements(&mut self, o: *mut Object, at: usize, from: *const Value, n: usize) {
        unsafe {
            let e = (*o).elems;
            let fwd = from >= e.add(at).cast_const();
            for k in 0..n {
                let i = if fwd { k } else { n - 1 - k };
                let v = *from.add(i);
                let s = &*(*o).shape;
                if v == Value::EMPTY || s.elems.join(FieldType::of(v)) == s.elems {
                    *e.add(at + i) = v;
                    crate::heap::barrier(e.cast::<u8>().sub(8), e.add(at + i), v);
                } else {
                    self.store_element(o, at + i, v);
                }
            }
        }
    }

    /// The object holding `name` on `o`'s prototype chain (`o` included),
    /// and its slot there.
    pub(crate) fn lookup(&self, o: *mut Object, name: Atom) -> Option<(*mut Object, u32)> {
        let shape = unsafe { (*o).shape };
        if unsafe { (*shape).dictionary } {
            return self.lookup_uncached(o, name);
        }
        let e = &self.lookups[lookup_index(shape, name)];
        let c = e.get();
        if c.shape == shape && c.name == name.0 {
            if c.holder.is_null() {
                return Some((o, c.slot));
            }
            if c.epoch == self.proto_epoch {
                return (c.slot != u32::MAX).then_some((c.holder, c.slot));
            }
        }
        let found = self.lookup_uncached(o, name);
        e.set(LookupEntry {
            shape,
            name: name.0,
            epoch: self.proto_epoch,
            holder: match found {
                Some((h, _)) if h == o => std::ptr::null_mut(),
                Some((h, _)) => h,
                None => PROTO_NONE,
            },
            slot: found.map_or(u32::MAX, |f| f.1),
        });
        found
    }

    /// The megamorphic store table's entries (the first holds the
    /// prototype epoch's address in `new_shape`).
    pub(crate) fn mega_store_entries(&self) -> &[MegaStore] {
        &self.mega_stores[1..]
    }

    /// The address of [`MegaStore`] table `Vm::mega_stores`'s entry 1,
    /// past which `mega_index`'s entries are (the stencils add the hash's
    /// entry to it in one instruction; entry 0 is just before).
    pub(crate) fn mega_stores_addr(&self) -> u64 {
        self.mega_stores[1..].as_ptr() as u64
    }

    /// Note in `Vm::mega_stores` how `o.name` was just stored, `o` of
    /// shape `old` before: as `set_named_miss` would cache it, for a plain
    /// object (not a prototype, nor the global object), in place or by a
    /// transition `old` made the new shape by.
    pub(crate) fn mega_store_note(&mut self, o: Value, old: *const Shape, name: Atom) {
        if !o.is_cell() || self.proxies_exist || !self.realms.is_empty() {
            return;
        }
        let o = as_object(o);
        unsafe {
            if (*o).class != Class::Plain || (*o).flags & PROTOTYPE != 0 || o == self.global || (*(*o).shape).dictionary {
                return;
            }
            let new = (*o).shape;
            let e = if new == old {
                let Some(slot) = (*old).lookup(name) else { return };
                if (*old).field_cell(slot).is_constant() {
                    return;
                }
                MegaStore {
                    shape: old as u64,
                    name: name.0,
                    ft: (*old).field_type(slot).encode(),
                    loc: (*old).location(slot),
                    ..MegaStore::EMPTY
                }
            } else {
                if (*old).unique || (*new).unique || (*new).parent != old {
                    return;
                }
                let Some(slot) = (*new).lookup(name) else { return };
                MegaStore {
                    shape: old as u64,
                    name: name.0,
                    epoch: self.proto_epoch,
                    ft: (*new).field_type(slot).encode(),
                    new_shape: new as u64,
                    loc: (*new).location(slot),
                    _pad: 0,
                }
            };
            self.mega_stores[0].new_shape = &raw const self.proto_epoch as u64;
            // Two ways: the entry's own, and its neighbour's (`mega_pair`),
            // which the entry there moves to (as JSC's megamorphic cache
            // moves its primary entry to its secondary table).
            let k = mega_index(old as u64, name.0);
            let here = self.mega_stores[k];
            if here.shape != 0 && (here.shape, here.name) != (e.shape, e.name) {
                self.mega_stores[mega_pair(k)] = here;
            }
            self.mega_stores[k] = e;
        }
    }

    /// The address of [`MegaEntry`] table `Vm::megas`'s entry 1, as
    /// [`Vm::mega_stores_addr`].
    pub(crate) fn megas_addr(&self) -> u64 {
        self.megas[1..].as_ptr() as u64
    }

    /// Note in `Vm::megas` where `o.name` is, if a data property's slot
    /// is what `Vm::get` reads (not a proxy's, a typed array's, a
    /// `length`, an accessor, another realm's).
    pub(crate) fn mega_note(&mut self, o: Value, name: Atom) {
        if !o.is_cell() || name == self.a_length || self.proxies_exist || !self.realms.is_empty() {
            return;
        }
        let o = as_object(o);
        if unsafe { (*o).class == Class::TypedArray || (*(*o).shape).dictionary } {
            return;
        }
        let Some((h, slot)) = self.lookup(o, name) else { return };
        if self.is_accessor(h, slot) {
            return;
        }
        let shape = unsafe { (*o).shape } as u64;
        let loc = unsafe { (*(*h).shape).location(slot) };
        let (off, addr) = if h == o {
            (if loc >= 0 { loc } else { !(!loc * 8) }, 0)
        } else {
            (0, unsafe { Object::slot(h, loc) } as u64)
        };
        self.megas[0].addr = &raw const self.proto_epoch as u64;
        self.megas[mega_index(shape, name.0)] = MegaEntry {
            shape,
            name: name.0,
            epoch: self.proto_epoch,
            off,
            _pad: 0,
            addr,
        };
    }

    /// [`Vm::lookup`], without its cache.
    fn lookup_uncached(&self, o: *mut Object, name: Atom) -> Option<(*mut Object, u32)> {
        let mut o = o;
        loop {
            let s = unsafe { &*(*o).shape };
            if let Some(slot) = s.lookup(name) {
                return Some((o, slot));
            }
            if !s.proto.is_cell() {
                return None;
            }
            o = as_object(s.proto);
        }
    }

    /// `obj[name]`.
    pub fn get(&mut self, obj: Value, name: Atom) -> Result<Value> {
        if !obj.is_cell() {
            return self.get_primitive(obj, name);
        }
        let o = as_object(obj);
        if !self.realms.is_empty()
            && name != self.a_length
            && let Some(p) = self.realm_proto(o)
        {
            return self.get_through(p, name, obj);
        }
        if crate::proxy::chain_has_proxy(self, o) {
            return crate::proxy::generic_get(self, obj, Key::Name(name), obj);
        }
        unsafe {
            if name == self.a_length
                && matches!((*o).class, Class::Array | Class::String | Class::TypedArray)
            {
                return Ok(Value::number(Self::length_field(o) as f64));
            }
            if self.typed_numeric(o, name) {
                return Ok(Value::UNDEFINED);
            }
            Ok(match self.lookup(o, name) {
                Some((h, slot)) if self.is_accessor(h, slot) => {
                    let pair = *Object::slot(h, (*(*h).shape).location(slot));
                    return self.call_getter(pair, obj);
                }
                Some((h, slot)) => *Object::slot(h, (*(*h).shape).location(slot)),
                None => return Ok(self.virtual_get(o, name)),
            })
        }
    }

    /// `o.name` for a property without a slot (a function's `length` and
    /// `name`, a string object's or an array's `length`: see
    /// `Vm::own_property`), `o`'s or a prototype's, else `undefined`.
    /// Whether `name` is a canonical numeric string and `o` a typed array
    /// (see `typed::canonical_numeric`).
    pub(crate) fn typed_numeric(&self, o: *mut Object, name: Atom) -> bool {
        (unsafe { (*o).class == Class::TypedArray }) && crate::typed::canonical_numeric(self.atom_name(name))
    }

    /// An array's, a string's or a typed array's `length` (a typed array's
    /// 0 once its buffer is detached: see `typed::ta_length`; a far array's
    /// its store's: see `far`).
    fn length_field(o: *mut Object) -> u32 {
        unsafe {
            if (*o).class == Class::TypedArray {
                return crate::typed::ta_length(o).unwrap_or(0) as u32;
            }
            crate::far::array_length(o)
        }
    }

    #[cold]
    fn virtual_get(&mut self, o: *mut Object, name: Atom) -> Value {
        if name != self.a_length && name != self.a_name {
            return Value::UNDEFINED;
        }
        let mut p = o;
        loop {
            unsafe {
                let class = (*p).class;
                if name == self.a_length && matches!(class, Class::Array | Class::String | Class::TypedArray) {
                    return Value::number(Self::length_field(p) as f64);
                }
                if matches!(class, Class::Function | Class::Native | Class::Wrapper)
                    && let Some(d) = self.own_property(p, Key::Name(name))
                {
                    return d.value.unwrap_or(Value::UNDEFINED);
                }
                let proto = (*(*p).shape).proto;
                if !proto.is_cell() {
                    return Value::UNDEFINED;
                }
                p = as_object(proto);
            }
        }
    }

    /// Whether `o` or a prototype has `name` with no slot, as
    /// `virtual_get` reads it: an array's or a string wrapper's `length`, a
    /// function's `length` and `name`.
    fn virtual_has(&mut self, o: *mut Object, name: Atom) -> bool {
        if name != self.a_length && name != self.a_name {
            return false;
        }
        let mut p = o;
        loop {
            unsafe {
                let class = (*p).class;
                if name == self.a_length && matches!(class, Class::Array | Class::String | Class::TypedArray) {
                    return true;
                }
                if matches!(class, Class::Function | Class::Native | Class::Wrapper)
                    && self.own_property(p, Key::Name(name)).is_some()
                {
                    return true;
                }
                let proto = (*(*p).shape).proto;
                if !proto.is_cell() {
                    return false;
                }
                p = as_object(proto);
            }
        }
    }

    /// A wrapper object for primitive `v` (see `Class::Wrapper`).
    pub(crate) fn new_wrapper(&mut self, v: Value) -> Value {
        let proto = if v.is_number() {
            self.number_prototype
        } else if v.is_bool() {
            self.boolean_prototype
        } else if crate::symbol::is_symbol(v) {
            self.symbol_roots[crate::symbol::Root::SymbolPrototype as usize]
        } else if crate::bigint::is_bigint(v) {
            self.bigint_prototype
        } else {
            self.string_prototype
        };
        let o = self.new_object_with(Class::Wrapper, object_value(proto), 0);
        unsafe { (*o).code = v.to_bits() as *const Code };
        object_value(o)
    }

    /// An uncaught error's message: a thrown error object's `name` and
    /// `message`, else the error's own.
    pub fn uncaught_message(&mut self, e: &Error) -> String {
        if let Some(v) = e.1
            && crate::props::is_object(v)
        {
            // The run is over: its `toString` can run at the stack's base.
            // (The conversion reaches the VM through `interp::current_vm`,
            // not `self`: the stores must not move across it.)
            let vm: *mut Vm = self;
            unsafe {
                let saved = (*vm).native_top;
                std::ptr::write_volatile(&raw mut (*vm).native_top, (*vm).stack.as_mut_ptr());
                let s = crate::interp::to_string_units(v);
                std::ptr::write_volatile(&raw mut (*vm).native_top, saved);
                std::ptr::write_volatile(&raw mut (*vm).conv_error, None);
                std::ptr::write_volatile(&raw mut (*vm).conv_thrown, false);
                return String::from_utf16_lossy(&s);
            }
        }
        e.0.clone()
    }

    fn is_accessor(&self, h: *mut Object, slot: u32) -> bool {
        unsafe { (&(*(*h).shape).attrs())[slot as usize] & ACCESSOR != 0 }
    }

    /// The accessor property `name` of `o` or its prototypes, if that is
    /// what `o.name` finds: its getter-setter pair.
    fn accessor(&self, o: *mut Object, name: Atom) -> Option<Value> {
        let (h, slot) = self.lookup(o, name)?;
        self.is_accessor(h, slot)
            .then(|| unsafe { *Object::slot(h, (*(*h).shape).location(slot)) })
    }

    /// `this.name` through an accessor `pair`'s getter (`undefined`
    /// without one).
    fn call_getter(&mut self, pair: Value, this: Value) -> Result<Value> {
        let g = unsafe { *(*as_object(pair)).elems };
        if g.is_undefined() {
            return Ok(Value::UNDEFINED);
        }
        crate::interp::call_function(self, g, this, &[])
    }

    /// `this.name = value` through an accessor `pair`'s setter (nothing
    /// without one: sloppy mode).
    fn call_setter(&mut self, pair: Value, this: Value, value: Value) -> Result<()> {
        let s = unsafe { *(*as_object(pair)).elems.add(1) };
        if s.is_undefined() {
            self.store_failed = true;
            return Ok(());
        }
        crate::interp::call_function(self, s, this, &[value]).map(|_| ())
    }

    /// `Object.defineProperty(o, name, {get, set, …})`: an accessor
    /// property with attributes `attrs` (`ACCESSOR` added). An existing
    /// accessor keeps the function a descriptor leaves out; an existing
    /// data property becomes an accessor.
    pub(crate) fn define_accessor(
        &mut self,
        o: *mut Object,
        name: Atom,
        get: Option<Value>,
        set: Option<Value>,
        attrs: u8,
    ) {
        unsafe {
            let s = &*(*o).shape;
            let old = s.lookup(name).filter(|&k| s.attrs()[k as usize] & ACCESSOR != 0);
            let (mut g, mut st) = (Value::UNDEFINED, Value::UNDEFINED);
            if let Some(k) = old {
                let pair = as_object(*Object::slot(o, s.location(k)));
                (g, st) = (*(*pair).elems, *(*pair).elems.add(1));
            }
            let pair = self.new_array();
            crate::vm::array_push(self, pair, &[get.unwrap_or(g), set.unwrap_or(st)])
                .expect("an array");
            self.define_property(o, name, Some(pair), attrs | ACCESSOR);
        }
    }

    /// For a primitive cell (a string, a symbol, a bigint) whose prototype
    /// is another realm's, the current realm's (whose properties a
    /// primitive has).
    fn realm_proto(&self, o: *mut Object) -> Option<*mut Object> {
        let p = match unsafe { (*o).class } {
            Class::String => self.string_prototype,
            Class::Symbol => self.symbol_roots[crate::symbol::Root::SymbolPrototype as usize],
            Class::BigInt => self.bigint_prototype,
            _ => return None,
        };
        (unsafe { (*(*o).shape).proto } != object_value(p)).then_some(p)
    }

    /// `p.[[Get]](name, receiver)`.
    fn get_through(&mut self, p: *mut Object, name: Atom, receiver: Value) -> Result<Value> {
        if crate::proxy::chain_has_proxy(self, p) {
            return crate::proxy::generic_get(self, object_value(p), Key::Name(name), receiver);
        }
        match self.lookup(p, name) {
            // (A getter's `this` the primitive.)
            Some((h, slot)) if self.is_accessor(h, slot) => {
                let pair = unsafe { *Object::slot(h, (*(*h).shape).location(slot)) };
                self.call_getter(pair, receiver)
            }
            Some((h, slot)) => Ok(unsafe { *Object::slot(h, (*(*h).shape).location(slot)) }),
            None => Ok(self.virtual_get(p, name)),
        }
    }

    /// `p.[[Set]](name, value, receiver)` for a primitive `receiver`:
    /// an inherited setter's call (or a proxy's trap), or nothing.
    fn set_through(&mut self, p: *mut Object, name: Atom, value: Value, receiver: Value) -> Result<()> {
        if crate::proxy::chain_has_proxy(self, p) {
            if !crate::proxy::generic_set(self, object_value(p), Key::Name(name), value, receiver)? {
                self.store_failed = true;
            }
            return Ok(());
        }
        if let Some(pair) = self.accessor(p, name) {
            return self.call_setter(pair, receiver, value);
        }
        self.store_failed = true;
        Ok(())
    }

    fn get_primitive(&mut self, obj: Value, name: Atom) -> Result<Value> {
        if obj.is_undefined() || obj.is_null() {
            return Err(Error::type_error(format!(
                "Cannot read properties of {obj:?} (reading '{}')",
                self.atom_name(name)
            )));
        }
        // A number's properties are `Number.prototype`'s, a boolean's
        // `Boolean.prototype`'s.
        if obj.is_number() || obj.is_bool() {
            let o = if obj.is_number() { self.number_prototype } else { self.boolean_prototype };
            return match self.lookup(o, name) {
                // (A getter's `this` the primitive.)
                Some((h, slot)) if self.is_accessor(h, slot) => {
                    let pair = unsafe { *Object::slot(h, (*(*h).shape).location(slot)) };
                    self.call_getter(pair, obj)
                }
                Some((h, slot)) => Ok(unsafe { *Object::slot(h, (*(*h).shape).location(slot)) }),
                None => Ok(Value::UNDEFINED),
            };
        }
        Ok(Value::UNDEFINED)
    }

    /// `obj[name] = value`.
    pub fn set(&mut self, obj: Value, name: Atom, value: Value) -> Result<()> {
        if !obj.is_cell() {
            return self.set_primitive(obj, name, value);
        }
        let o = as_object(obj);
        if !self.realms.is_empty()
            && name != self.a_length
            && let Some(p) = self.realm_proto(o)
        {
            return self.set_through(p, name, value, obj);
        }
        if crate::proxy::chain_has_proxy(self, o) {
            if !crate::proxy::generic_set(self, obj, Key::Name(name), value, obj)? {
                self.store_failed = true;
            }
            return Ok(());
        }
        unsafe {
            if (*o).class == Class::Array && name == self.a_length {
                return self.set_array_length(o, value);
            }
            // A string is a primitive: stores to it do nothing, but call
            // setters.
            if (*o).class == Class::String {
                return self.set_string(obj, o, name, value);
            }
            // (A typed array's numeric property that is no index: the
            // value converted, then nothing.)
            if self.typed_numeric(o, name) {
                let kind = crate::object::TypedKind::from_u8((*o).kind);
                crate::typed::to_element(self, kind, value)?;
                return Ok(());
            }
            if self.read_only(o, name) {
                self.store_failed = true;
                return Ok(());
            }
            if let Some(pair) = self.accessor(o, name) {
                return self.call_setter(pair, obj, value);
            }
            match (*(*o).shape).lookup(name) {
                Some(slot) => self.store_field(o, slot, value),
                None => {
                    if (*o).flags & crate::object::NOT_EXTENSIBLE != 0 {
                        self.store_failed = true;
                        return Ok(());
                    }
                    self.add_property(o, name, value);
                }
            }
        }
        Ok(())
    }

    fn set_primitive(&mut self, obj: Value, name: Atom, value: Value) -> Result<()> {
        if obj.is_undefined() || obj.is_null() {
            return Err(Error::type_error(format!(
                "Cannot set properties of {obj:?} (setting '{}')",
                self.atom_name(name)
            )));
        }
        // (An inherited setter's `this` the primitive.)
        let o = if obj.is_number() { self.number_prototype } else { self.boolean_prototype };
        self.set_through(o, name, value, obj)
    }

    /// A store to string `o`'s property `name`: an inherited setter's
    /// call, or nothing (a primitive's own properties can't be set).
    fn set_string(&mut self, obj: Value, o: *mut Object, name: Atom, value: Value) -> Result<()> {
        if name != self.a_length
            && let Some(pair) = self.accessor(o, name)
        {
            return self.call_setter(pair, obj, value);
        }
        self.store_failed = true;
        Ok(())
    }

    // ---- Inline-cache misses ----

    /// `GetNamed` miss: look `obj.name` up and cache how, if possible.
    pub fn get_named_miss(&mut self, code: &Code, ic: u16, obj: Value) -> Result<Value> {
        code.ic_misses.set(code.ic_misses.get().wrapping_add(1));
        let (ic, name) = code.ic(ic);
        if !obj.is_cell() {
            return self.get_primitive(obj, name);
        }
        let o = as_object(obj);
        // (A primitive of another realm's: never cached.)
        if !self.realms.is_empty()
            && name != self.a_length
            && let Some(p) = self.realm_proto(o)
        {
            return self.get_through(p, name, obj);
        }
        // (Never cached through a proxy.)
        if crate::proxy::chain_has_proxy(self, o) {
            return crate::proxy::generic_get(self, obj, Key::Name(name), obj);
        }
        unsafe {
            if name == self.a_length
                && matches!((*o).class, Class::Array | Class::String | Class::TypedArray)
            {
                // Recorded for the JIT (`jit::Prop::Length`); no shape, so
                // the interpreter's hit path never takes it.
                *ic = Ic {
                    loc: LENGTH_LOC,
                    ..Ic::EMPTY
                };
                return Ok(Value::number(Self::length_field(o) as f64));
            }
            // Absent: cached as a load from `missing_holder`'s `undefined`
            // slot, valid (like a prototype's property) while no prototype
            // changes shape.
            let Some((h, slot)) = self.lookup(o, name) else {
                // (A virtual `length` or `name`, maybe a prototype's: not
                // cached.)
                if name == self.a_length || name == self.a_name {
                    let v = self.virtual_get(o, name);
                    if !v.is_undefined() || matches!((*o).class, Class::Function | Class::Native | Class::Wrapper) {
                        return Ok(v);
                    }
                }
                let e = Ic {
                    shape: (*o).shape,
                    holder: self.missing_holder,
                    new_shape: std::ptr::null(),
                    loc: crate::object::HEADER_BYTES as i32,
                    epoch: self.proto_epoch,
                };
                if install(ic, e) {
                    self.megamorphic.insert(ic as usize);
                }
                return Ok(Value::UNDEFINED);
            };
            let loc = (*(*h).shape).location(slot);
            // A getter's: called, never cached (noted for the JIT).
            if self.is_accessor(h, slot) {
                if h != o {
                    self.heap.note(object_value(h));
                    note_accessor(ic, (*o).shape, h, self.proto_epoch);
                }
                return self.call_getter(*Object::slot(h, loc), obj);
            }
            self.heap.note(object_value(h));
            let e = Ic {
                shape: (*o).shape,
                holder: if h == o { std::ptr::null_mut() } else { h },
                new_shape: std::ptr::null(),
                loc,
                epoch: self.proto_epoch,
            };
            if install(ic, e) {
                self.megamorphic.insert(ic as usize);
            }
            Ok(*Object::slot(h, loc))
        }
    }

    /// `SetNamed` miss: store `obj.name = value` and cache how, if possible.
    pub fn set_named_miss(&mut self, code: &Code, k: u16, obj: Value, value: Value) -> Result<()> {
        let (ic, name) = code.ic(k);
        // (Not a miss where the cache has the transition, out of line, and
        // only the object's `props` had no room: the store grows them. As
        // misses, Box2D's initializers kept cooling in the interpreter, see
        // `jit::MAX_SETTLE`.)
        let room_only = obj.is_cell()
            && unsafe {
                (*ic).shape == (*as_object(obj)).shape
                    && !(*ic).new_shape.is_null()
                    && (*ic).loc < 0
                    && (*ic).epoch == self.proto_epoch
            };
        if !room_only {
            code.ic_misses.set(code.ic_misses.get().wrapping_add(1));
        }
        if !obj.is_cell() {
            return self.set_primitive(obj, name, value);
        }
        let o = as_object(obj);
        // An object literal's property past a proxy or another realm's
        // prototype: defined, not that prototype's [[Set]].
        if (!self.realms.is_empty() || crate::proxy::chain_has_proxy(self, o)) && code.define_ics.contains(&k) {
            return self.create_data_property(obj, Key::Name(name), value);
        }
        if !self.realms.is_empty()
            && name != self.a_length
            && let Some(p) = self.realm_proto(o)
        {
            return self.set_through(p, name, value, obj);
        }
        if crate::proxy::chain_has_proxy(self, o) {
            if !crate::proxy::generic_set(self, obj, Key::Name(name), value, obj)? {
                self.store_failed = true;
            }
            return Ok(());
        }
        unsafe {
            let old = (*o).shape;
            if (*o).class == Class::Array && name == self.a_length {
                return self.set_array_length(o, value);
            }
            // A store to a read-only property, own or inherited, does
            // nothing (and isn't cached, so it stays slow); strict code's
            // throws (see `store_failed`).
            if (*o).class == Class::String {
                return self.set_string(obj, o, name, value);
            }
            // An object literal's property past a prototype's setter or
            // read-only property: defined, not cached.
            let defines = || code.define_ics.contains(&k);
            if self.read_only(o, name) {
                if defines() {
                    return self.create_data_property(obj, Key::Name(name), value);
                }
                self.store_failed = true;
                return Ok(());
            }
            // A setter's: called, never cached (noted for the JIT).
            if let Some((h, slot)) = self.lookup(o, name)
                && self.is_accessor(h, slot)
            {
                if defines() {
                    return self.create_data_property(obj, Key::Name(name), value);
                }
                if h != o {
                    self.heap.note(object_value(h));
                    note_accessor(ic, old, h, self.proto_epoch);
                }
                return self.call_setter(*Object::slot(h, (*(*h).shape).location(slot)), obj, value);
            }
            if let Some(slot) = (*old).lookup(name) {
                self.store_field(o, slot, value);
                // Stores to a prototype stay on this path, which bumps the
                // epoch (see `store_field`), as do those to a constant field
                // of a function's.
                if (*o).flags & PROTOTYPE != 0 || !self.may_cache_store(o, slot) {
                    return Ok(());
                }
                let e = Ic {
                    shape: old,
                    holder: (*old).field_type(slot).encode() as *mut Object,
                    loc: (*old).location(slot),
                    ..Ic::EMPTY
                };
                if install(ic, e) {
                    self.megamorphic.insert(ic as usize);
                }
                return Ok(());
            }
            if (*o).flags & crate::object::NOT_EXTENSIBLE != 0 {
                self.store_failed = true;
                return Ok(());
            }
            let slot = self.add_property(o, name, value);
            let new = (*o).shape;
            // (Out of line too: the fast paths check there is room, see
            // `interp::has_room`. Without, an initializer adding properties
            // past the constructor's slots (Box2D's `b2Contact.Reset`) missed
            // at every store, and compiled code left at each.)
            if !(*old).unique {
                let e = Ic {
                    shape: old,
                    holder: (*new).field_type(slot).encode() as *mut Object,
                    new_shape: new,
                    loc: (*new).location(slot),
                    epoch: self.proto_epoch,
                };
                if install(ic, e) {
                    self.megamorphic.insert(ic as usize);
                }
            }
        }
        Ok(())
    }

    /// Whether a store of `name` to `o` is blocked by a read-only property
    /// of that name: `o`'s own, or else the nearest on its prototype chain.
    pub(crate) fn read_only(&self, o: *mut Object, name: Atom) -> bool {
        let Some((h, slot)) = self.lookup(o, name) else {
            return self.virtual_read_only(o, name, std::ptr::null_mut());
        };
        // (A slot-less one nearer than the slot shadows it.)
        unsafe { self.virtual_read_only(o, name, h) || (&(*(*h).shape).attrs())[slot as usize] & READ_ONLY != 0 }
    }

    /// Whether `o` or a prototype of it before `until` has `name` with no
    /// slot, which is read-only: a function's `length` and `name`, a string
    /// object's `length` (see `props`).
    fn virtual_read_only(&self, mut o: *mut Object, name: Atom, until: *mut Object) -> bool {
        if name != self.a_length && name != self.a_name {
            return false;
        }
        unsafe {
            while o != until {
                // (Not one a slot of its own redefined.)
                let own = (*(*o).shape).lookup(name).is_none()
                    && match (*o).class {
                        Class::Function | Class::Native => {
                            name == self.a_length && (*o).flags & crate::object::FN_LENGTH_DELETED == 0
                                || name == self.a_name && (*o).flags & crate::object::FN_NAME_DELETED == 0
                        }
                        Class::Wrapper => name == self.a_length && wrapped(object_value(o)).and_then(string_units).is_some(),
                        _ => false,
                    };
                if own {
                    return true;
                }
                let proto = (*(*o).shape).proto;
                if !proto.is_cell() {
                    return false;
                }
                o = as_object(proto);
            }
        }
        false
    }

    /// `Object.defineProperty(o, name, …)` of a data property: `value` (if
    /// given) with attributes `attrs`. An existing property keeps its value
    /// if none is given, and changes its attributes: `o` gets a shape of
    /// its own (no shared transition leads to it).
    pub(crate) fn define_property(
        &mut self,
        o: *mut Object,
        name: Atom,
        value: Option<Value>,
        attrs: u8,
    ) {
        unsafe {
            let s = &*(*o).shape;
            let Some(slot) = s.lookup(name) else {
                self.add_property_with(o, name, value.unwrap_or(Value::UNDEFINED), attrs);
                return;
            };
            if s.attrs()[slot as usize] != attrs {
                self.shape_left(s);
                let mut slot_attrs = s.attrs().to_vec();
                slot_attrs[slot as usize] = attrs;
                let u = Shape {
                    class: s.class,
                    proto: s.proto,
                    capacity: Cell::new(s.capacity.get()),
                    slots: Slots::new(s.keys(), &slot_attrs, s.fields()),
                    elems: s.elems,
                    elems_code: s.elems_code,
                    family: std::ptr::null(),
                    push: Cell::new(std::ptr::null_mut()),
                    unique: true,
                    fixed: false,
                    transitions: RefCell::default(),
                    elem_transitions: RefCell::default(),
            relied: Cell::new(false),
            left: Cell::new(false),
            lineage_relied: Cell::new(false),
            parent: std::ptr::null(),
            mark: Default::default(),
            traced: Cell::new(0),
            pinned: Cell::new(false),
            dictionary: false,
                };
                crate::object::set_shape(o, self.add_shape(u));
                self.bump_code_epoch();
                if (*o).flags & PROTOTYPE != 0 {
                    self.proto_epoch = self.proto_epoch.wrapping_add(1);
                }
            }
            if let Some(v) = value {
                self.store_field(o, slot, v);
            }
        }
    }

    /// Whether global `name` exists (HasBinding): on the global object or
    /// its prototypes, a proxy among them asked (its `has` trap).
    fn has_global(&mut self, name: Atom) -> Result<bool> {
        let g = self.global;
        if crate::proxy::chain_has_proxy(self, g) {
            return crate::proxy::generic_has(self, object_value(g), Key::Name(name));
        }
        Ok(self.lookup(g, name).is_some())
    }

    /// `LdaGlobal` miss: a ReferenceError if the global doesn't exist.
    pub fn get_global_miss(&mut self, code: &Code, ic: u16) -> Result<Value> {
        let (_, name) = code.ic(ic);
        let g = self.global;
        if !self.has_global(name)? {
            // (A script's `var`s are made before it runs: see
            // `global_declarations`.)
            return Err(Error::reference_error(format!(
                "{} is not defined",
                self.atom_name(name)
            )));
        }
        unsafe { global_way(code, ic) };
        self.get_named_miss(code, ic, object_value(g))
    }

    /// `LdaGlobalInsideTypeof` miss: as `LdaGlobal`'s, but `undefined` for a
    /// missing global.
    pub fn get_global_typeof_miss(&mut self, code: &Code, ic: u16) -> Result<Value> {
        let (_, name) = code.ic(ic);
        if !self.has_global(name)? {
            return Ok(Value::UNDEFINED);
        }
        unsafe { global_way(code, ic) };
        self.get_named_miss(code, ic, object_value(self.global))
    }

    /// `typeof v`, a string constant (each made once: EarleyBoyer's
    /// `sc_isNumber` and others spent 3% encoding and looking them up).
    pub fn type_of(&mut self, v: Value) -> Value {
        const NAMES: [&str; 8] = ["number", "undefined", "boolean", "string", "function", "object", "symbol", "bigint"];
        let k = if v.is_number() {
            0
        } else if v.is_undefined() {
            1
        } else if v.is_bool() {
            2
        } else if v.is_cell() {
            match unsafe { (*as_object(v)).class } {
                Class::String => 3,
                Class::Function | Class::Native => 4,
                Class::Symbol => 6,
                Class::BigInt => 7,
                _ => 5,
            }
        } else {
            5
        };
        if let Some(s) = self.type_names[k] {
            return s;
        }
        let units: Vec<u16> = NAMES[k].encode_utf16().collect();
        let s = self.string_constant(&units);
        self.type_names[k] = Some(s);
        s
    }

    /// `StaGlobal` miss (sloppy mode: creates the global if missing). Only
    /// stores to existing globals are cached.
    pub fn set_global_miss(&mut self, code: &Code, ic: u16, value: Value) -> Result<()> {
        let (ic, name) = code.ic(ic);
        let g = self.global;
        // Not the global's own, past a proxy on its prototype chain: the
        // proxy's `has` says if it exists, the store is [[Set]] on the global
        // (its `set` trap).
        if unsafe { (*(*g).shape).lookup(name).is_none() } && crate::proxy::chain_has_proxy(self, g) {
            if code.strict && !self.has_global(name)? {
                return Err(Error::reference_error(format!("{} is not defined", self.atom_name(name))));
            }
            let gv = object_value(g);
            if !crate::proxy::generic_set(self, gv, Key::Name(name), value, gv)? {
                self.store_failed = true;
            }
            return Ok(());
        }
        if self.read_only(g, name) {
            // (In strict code a TypeError: `interp::strict_store_check`.)
            self.store_failed = true;
            return Ok(());
        }
        unsafe {
            let shape = (*g).shape;
            match (*shape).lookup(name) {
                Some(slot) if self.is_accessor(g, slot) => {
                    let pair = *Object::slot(g, (*shape).location(slot));
                    return self.call_setter(pair, object_value(g), value);
                }
                Some(slot) => {
                    self.store_field(g, slot, value);
                    if !self.may_cache_store(g, slot) {
                        return Ok(());
                    }
                    *ic = Ic {
                        shape,
                        holder: (*shape).field_type(slot).encode() as *mut Object,
                        loc: (*shape).location(slot),
                        ..Ic::EMPTY
                    };
                }
                // An inherited setter, else a new global (a ReferenceError
                // in strict code).
                None => {
                    if let Some(pair) = self.accessor(g, name) {
                        return self.call_setter(pair, object_value(g), value);
                    }
                    // (A script's `var` or function: not deletable; an
                    // eval's is.)
                    if self.script_declared.contains(&name) {
                        self.add_property_with(g, name, value, crate::object::DONT_DELETE);
                        return Ok(());
                    }
                    if code.strict {
                        return Err(Error::reference_error(format!("{} is not defined", self.atom_name(name))));
                    }
                    self.add_property(g, name, value);
                }
            }
        }
        Ok(())
    }

    /// `v instanceof f` (InstanceofOperator): `f[Symbol.hasInstance](v)`,
    /// which for a function is `Function.prototype`'s (it can't be changed
    /// there: not looked up for a plain function, of `function_shape`, nor
    /// until some other object has had one), OrdinaryHasInstance.
    pub fn instance_of(&mut self, v: Value, f: Value) -> Result<bool> {
        let function = f.is_cell() && matches!(unsafe { (*as_object(f)).class }, Class::Function | Class::Native);
        if !function || self.has_instance_defined && unsafe { (*as_object(f)).shape } != self.function_shape {
            return self.instance_of_generic(v, f);
        }
        self.ordinary_has_instance(v, f)
    }

    #[cold]
    #[inline(never)]
    fn instance_of_generic(&mut self, v: Value, f: Value) -> Result<bool> {
        if !crate::props::is_object(f) {
            return Err(Error::type_error("Right-hand side of 'instanceof' is not an object"));
        }
        let h = self.get(f, self.a_sym_has_instance)?;
        if !h.is_undefined() && !h.is_null() && h != object_value(self.has_instance_fn) {
            if !crate::props::is_callable(h) {
                return Err(Error::type_error("Symbol.hasInstance is not a function"));
            }
            let r = crate::interp::call_function(self, h, f, &[v])?;
            return Ok(crate::interp::to_boolean(r));
        }
        if h.is_undefined() || h.is_null() {
            if !crate::props::is_callable(f) {
                return Err(Error::type_error("Right-hand side of 'instanceof' is not callable"));
            }
        }
        self.ordinary_has_instance(v, f)
    }

    /// OrdinaryHasInstance(f, v): whether `f.prototype` is on `v`'s
    /// prototype chain (a bound function's target's instances, through its
    /// `instanceof`).
    #[inline(always)]
    pub(crate) fn ordinary_has_instance(&mut self, v: Value, f: Value) -> Result<bool> {
        let callable = f.is_cell()
            && matches!(unsafe { (*as_object(f)).class }, Class::Function | Class::Native);
        if !callable {
            return Ok(false);
        }
        if let Some(t) = self.bound_target(f) {
            return self.instance_of(v, t);
        }
        // Strings (and symbols) are primitives.
        if !crate::props::is_object(v) {
            return Ok(false);
        }
        let p = self.get(f, self.a_prototype)?;
        if !crate::props::is_object(p) {
            return Err(Error::type_error(
                "Function has non-object prototype in instanceof check",
            ));
        }
        let mut o = as_object(v);
        loop {
            let proto = if self.proxies_exist {
                crate::proxy::get_prototype_of(self, o)?
            } else {
                unsafe { (*(*o).shape).proto }
            };
            if !proto.is_cell() {
                return Ok(false);
            }
            if proto == p {
                return Ok(true);
            }
            o = as_object(proto);
        }
    }

    /// The function bound function `f` calls, if `f` is one.
    pub(crate) fn bound_target(&self, f: Value) -> Option<Value> {
        let o = as_object(f);
        let s = unsafe { &*(*o).shape };
        let slot = s.lookup(self.a_bound_target)?;
        Some(unsafe { *Object::slot(o, s.location(slot)) })
    }

    /// Whether `o` has element `i` of its own: within the length, and not
    /// a hole (strings and typed arrays have none; buffers no elements).
    /// (A sparse object's far keys are named properties: [`Vm::sparse_get`].)
    pub(crate) fn has_element(o: *mut Object, i: u32) -> bool {
        unsafe {
            match (*o).class {
                Class::String => i < (*o).elems_len,
                Class::TypedArray => (i as usize) < crate::typed::ta_length(o).unwrap_or(0),
                c if c.no_elements() => false,
                _ => i < (*o).elems_len && *(*o).elems.add(i as usize) != Value::EMPTY,
            }
        }
    }

    /// A sparse object's own key `i`, stored as a named property (see
    /// `object::SPARSE`), if it has it: a getter's gets `receiver` (`o`, or
    /// an object `o` is a prototype of).
    fn sparse_get(&mut self, o: *mut Object, i: u32, receiver: Value) -> Result<Option<Value>> {
        let a = self.intern(&i.to_string());
        unsafe {
            let Some(slot) = (*(*o).shape).lookup(a) else {
                return Ok(None);
            };
            let v = *Object::slot(o, (*(*o).shape).location(slot));
            if self.is_accessor(o, slot) {
                return self.call_getter(v, receiver).map(Some);
            }
            Ok(Some(v))
        }
    }

    /// Whether `o` has key `i` of its own: an element, or a sparse one.
    pub(crate) fn has_own_index(&mut self, o: *mut Object, i: u32) -> bool {
        Self::has_element(o, i)
            || unsafe { (*o).class == Class::Wrapper }
                && string_units(wrapped(object_value(o)).unwrap()).is_some_and(|u| (i as usize) < u.len())
            || crate::far::is_far(o) && crate::far::has(o, i)
            || unsafe { (*o).flags & crate::object::SPARSE != 0 && {
                let a = self.intern(&i.to_string());
                (*(*o).shape).lookup(a).is_some()
            } }
    }

    /// `key in obj`: whether `obj` or its prototypes have the property or
    /// element `key`.
    pub fn has_property(&mut self, obj: Value, key: Value) -> Result<bool> {
        if !obj.is_cell() || string_units(obj).is_some() {
            return Err(Error::type_error(format!(
                "Cannot use 'in' operator to search for '{}' in {}",
                display(key),
                display(obj)
            )));
        }
        let mut o = as_object(obj);
        if crate::proxy::chain_has_proxy(self, o) {
            let k = self.property_key(key);
            return crate::proxy::generic_has(self, obj, k);
        }
        match self.property_key(key) {
            Key::Name(a) if self.typed_numeric(o, a) => Ok(false),
            Key::Name(a) => Ok(self.lookup(o, a).is_some() || self.virtual_has(o, a)),
            Key::Index(i) => loop {
                unsafe {
                    if (*o).class != Class::String && self.has_own_index(o, i) {
                        return Ok(true);
                    }
                    // (A typed array's indices are its own, or none.)
                    if (*o).class == Class::TypedArray {
                        return Ok(false);
                    }
                    let proto = (*(*o).shape).proto;
                    if !proto.is_cell() {
                        return Ok(false);
                    }
                    o = as_object(proto);
                }
            },
        }
    }

    /// `delete obj[key]`: an element becomes a hole; a property is
    /// removed, the object getting a shape of its own without it (see
    /// [`Vm::delete_named`]). False for what can't be deleted.
    pub fn delete_property(&mut self, obj: Value, key: Value) -> Result<bool> {
        if !obj.is_cell() {
            if obj.is_undefined() || obj.is_null() {
                return Err(Error::type_error(format!(
                    "Cannot convert {} to object",
                    display(obj)
                )));
            }
            return Ok(true);
        }
        let o = as_object(obj);
        let key = self.property_key(key);
        if self.proxies_exist {
            return crate::proxy::delete(self, o, key);
        }
        Ok(self.delete_own(o, key))
    }

    /// Remove property `name` of `o`, unless it can't be deleted. The
    /// object gets a shape of its own (as a prototype's), with the other
    /// properties' slots moved down. Compiled code that folded `o`'s fields
    /// or relied on its shape is dropped: every field of its old shape is
    /// made mutable (they no longer describe `o`'s), and its shape left
    /// stable.
    /// `delete name` of a global: false for a script's `var` or function
    /// (made before it runs, not deletable: see `global_declarations`).
    pub(crate) fn delete_global(&mut self, name: Atom) -> bool {
        let g = self.global;
        if unsafe { (*(*g).shape).lookup(name).is_none() } && self.script_declared.contains(&name) {
            return false;
        }
        self.delete_named(g, name)
    }

    pub(crate) fn delete_named(&mut self, o: *mut Object, name: Atom) -> bool {
        self.remove_named(o, name, false)
    }

    /// Remove property `name` of `o` even if not configurable (as a bound
    /// function's `prototype`, which it shouldn't have).
    pub(crate) fn force_delete_named(&mut self, o: *mut Object, name: Atom) -> bool {
        self.remove_named(o, name, true)
    }

    fn remove_named(&mut self, o: *mut Object, name: Atom, force: bool) -> bool {
        unsafe {
            let s = &*(*o).shape;
            let Some(slot) = s.lookup(name) else {
                return !(name == self.a_length
                    && matches!((*o).class, Class::Array | Class::String));
            };
            if !force && s.attrs()[slot as usize] & DONT_DELETE != 0 {
                return false;
            }
            // An ordinary object deleting again (its shape unique: a
            // delete's copy, or a property added since) goes to dictionary
            // mode, where its deletes and adds change its shape in place
            // (V8 normalizes at the first: `LookupIterator::Delete`). At its
            // first delete it gets a unique shape of its own, which caches
            // still hold: objects that delete once and are read after keep
            // their reads cached (100 such objects read 2M times took 5.6
            // times as long in dictionary mode). Other objects get a unique
            // shape for each delete.
            if !s.dictionary && s.unique && self.may_be_dictionary(o) {
                self.to_dictionary(o);
            }
            if (*(*o).shape).dictionary {
                self.dict_remove(o, slot);
                return true;
            }
            let n = s.keys().len() as u32;
            let kept: Vec<u32> = (0..n).filter(|&k| k != slot).collect();
            let values: Vec<Value> = kept
                .iter()
                .map(|&k| *Object::slot(o, s.location(k)))
                .collect();
            self.shape_left(s);
            for &cell in s.fields() {
                self.field_made_mutable(&*cell);
            }
            let mut fields = Vec::with_capacity(kept.len());
            for &k in &kept {
                let cell = self.new_field_cell();
                (*cell).set((*s.fields()[k as usize]).get());
                fields.push(cell);
            }
            let u = Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                slots: Slots::new(
                    &kept.iter().map(|&k| s.keys()[k as usize]).collect::<Vec<_>>(),
                    &kept.iter().map(|&k| s.attrs()[k as usize]).collect::<Vec<_>>(),
                    &fields,
                ),
                elems: s.elems,
                elems_code: s.elems_code,
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: true,
                fixed: false,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
                relied: Cell::new(false),
                left: Cell::new(false),
                lineage_relied: Cell::new(false),
                parent: std::ptr::null(),
                mark: Default::default(),
                traced: Cell::new(0),
                pinned: Cell::new(false),
                dictionary: false,
            };
            let new = self.add_shape(u);
            crate::object::set_shape(o, new);
            for (k, v) in values.into_iter().enumerate() {
                let at = Object::slot(o, (*new).location(k as u32));
                // (Out of line, a `Num` field may still hold an int32.)
                let v = as_field((*new).field_type(k as u32), v);
                *at = v;
                crate::heap::barrier(o.cast(), at, v);
            }
            if (*o).flags & PROTOTYPE != 0 {
                self.proto_epoch = self.proto_epoch.wrapping_add(1);
                self.bump_code_epoch();
            }
            true
        }
    }

    /// Whether `o` may go to dictionary mode: a plain object, not a
    /// prototype (lookups through prototypes cache their shapes, V8 keeps
    /// them fast too), nor the global object, nor with flags (not
    /// extensible, sealed, frozen, elements with attributes: rare, and
    /// their shapes are made by `props`).
    fn may_be_dictionary(&self, o: *mut Object) -> bool {
        unsafe { (*o).class == Class::Plain && (*o).flags & !crate::object::EXTERNAL == 0 && o != self.global }
    }

    /// The field cell of dictionary shapes' slots.
    fn dictionary_cell(&mut self) -> *const FieldCell {
        if self.dictionary_cell.is_null() {
            let c = self.field_cells.alloc();
            unsafe {
                (*c).set(FieldType::Any);
                (*c).make_mutable();
                (*c).hold();
            }
            self.dictionary_cell = c;
        }
        self.dictionary_cell
    }

    /// Give `o` a dictionary shape (see `Shape::dictionary`) with the slots
    /// it has: its values stay where they are.
    fn to_dictionary(&mut self, o: *mut Object) {
        unsafe {
            let s = &*(*o).shape;
            self.shape_left(s);
            for &cell in s.fields() {
                self.field_made_mutable(&*cell);
            }
            let cell = self.dictionary_cell();
            let fields = vec![cell; s.keys().len()];
            let d = self.add_shape(Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                slots: Slots::new(s.keys(), s.attrs(), &fields),
                // (Elements of any type: no transition, which would copy
                // the shape.)
                elems: FieldType::Any,
                elems_code: FieldType::Any.encode(),
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: true,
                fixed: false,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
                relied: Cell::new(false),
                left: Cell::new(true),
                lineage_relied: Cell::new(false),
                parent: std::ptr::null(),
                mark: Default::default(),
                traced: Cell::new(0),
                pinned: Cell::new(false),
                dictionary: true,
            });
            crate::object::set_shape(o, d);
        }
    }

    /// Remove slot `k` of `o`, of a dictionary shape, in place: the values
    /// of the slots after it move down one.
    fn dict_remove(&mut self, o: *mut Object, k: u32) {
        unsafe {
            let s = (*o).shape;
            let n = (*s).keys().len() as u32;
            for j in k + 1..n {
                let v = *Object::slot(o, (*s).location(j));
                let at = Object::slot(o, (*s).location(j - 1));
                *at = v;
                crate::heap::barrier(o.cast(), at, v);
            }
            *Object::slot(o, (*s).location(n - 1)) = Value::UNDEFINED;
            Shape::dictionary_mut(s).slots.remove(k);
        }
    }

    /// Before `o`'s shape is copied (attributes, the prototype, extensibility
    /// change, it becomes a prototype): out of dictionary mode, to a unique
    /// shape with a table and cells of its own (`Any`), so that the copies
    /// don't share a table that changes.
    pub(crate) fn leave_dictionary(&mut self, o: *mut Object) {
        unsafe {
            let s = &*(*o).shape;
            if !s.dictionary {
                return;
            }
            let fields: Vec<*const FieldCell> = (0..s.keys().len())
                .map(|_| {
                    let c = self.new_field_cell();
                    (*c).set(FieldType::Any);
                    (*c).make_mutable();
                    c
                })
                .collect();
            let u = self.add_shape(Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                slots: Slots::new(s.keys(), s.attrs(), &fields),
                elems: s.elems,
                elems_code: s.elems_code,
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: true,
                fixed: false,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
                relied: Cell::new(false),
                left: Cell::new(false),
                lineage_relied: Cell::new(false),
                parent: std::ptr::null(),
                mark: Default::default(),
                traced: Cell::new(0),
                pinned: Cell::new(false),
                dictionary: false,
            });
            crate::object::set_shape(o, u);
        }
    }

    // ---- Elements ----

    /// What `o[key]` accesses: an element, for an array index (a number,
    /// or its canonical string, below 2^32 - 1), else the property named
    /// by the key's string.
    pub(crate) fn property_key(&mut self, key: Value) -> Key {
        if key.is_int32() && key.as_i32_unchecked() >= 0 {
            return Key::Index(key.as_i32_unchecked() as u32);
        }
        if crate::symbol::is_symbol(key) {
            return Key::Name(crate::symbol::symbol_atom(self, key));
        }
        if let Some(d) = key.as_number()
            && d >= 0.0
            && d < u32::MAX as f64
            && d.fract() == 0.0
            && !(d == 0.0 && d.is_sign_negative())
        {
            return Key::Index(d as u32);
        }
        // A string's atom, once found, is kept in it (`Object::string_atom`,
        // a flat string's): a key looked up again isn't converted and hashed
        // again (PdfJS's `map[command]`).
        let flat = |key: Value| string_len(key).is_some() && unsafe { !(*as_object(key)).elems.is_null() };
        if flat(key) {
            let cached = unsafe { *Object::string_atom(as_object(key)) };
            if cached != 0 {
                return Key::Name(Atom(cached - 1));
            }
        }
        let units = crate::interp::to_string_units(key);
        if let Some(i) = canonical_index(&units) {
            return Key::Index(i);
        }
        // (A lone surrogate kept, as the front end keeps it in a literal:
        // `js::lex::units_to_string`.)
        let name = crate::js::lex::units_to_string(&units);
        let a = self.intern(&name);
        // (A cons string flattened by `to_string_units` too.)
        if flat(key) && a.0 < u32::MAX {
            unsafe { *Object::string_atom(as_object(key)) = a.0 + 1 };
        }
        Key::Name(a)
    }

    /// `GetKeyed` miss: remember the receiver's shape (for the JIT: its
    /// elements' type), then `obj[key]`.
    pub fn get_keyed_miss(
        &mut self,
        code: &Code,
        ic: u16,
        obj: Value,
        key: Value,
    ) -> Result<Value> {
        code.ic_misses.set(code.ic_misses.get().wrapping_add(1));
        // A string's elements are code units, not values, and a typed
        // array's or buffer's bytes: recorded as such (`note_bytes`).
        if obj.is_cell()
            && unsafe {
                let c = (*as_object(obj)).class;
                matches!(c, Class::String | Class::TypedArray) || c.no_elements()
            }
        {
            let (ic, _) = code.ic(ic);
            unsafe { note_bytes(ic, as_object(obj)) };
        } else if obj.is_cell()
            // (Not a mapped `arguments` object's: its elements' values are
            // the parameters', see `mapped_get`.)
            && !self.mapped_args.iter().any(|m| m.o == as_object(obj))
        {
            let (ic, _) = code.ic(ic);
            unsafe {
                install(
                    ic,
                    Ic {
                        shape: (*as_object(obj)).shape,
                        ..Ic::EMPTY
                    },
                );
            }
        }
        self.get_keyed(obj, key)
    }

    /// `obj[key]`.
    pub fn get_keyed(&mut self, obj: Value, key: Value) -> Result<Value> {
        if !obj.is_cell() {
            if obj.is_undefined() || obj.is_null() {
                return Err(Error::type_error(format!(
                    "Cannot read properties of {obj:?}"
                )));
            }
            // A number's or a boolean's properties are `Number.prototype`'s
            // or `Boolean.prototype`'s, the primitive the receiver (as
            // `get_primitive`'s).
            let i = match self.property_key(key) {
                Key::Index(i) => i,
                Key::Name(a) => return self.get(obj, a),
            };
            let proto = if obj.is_number() { self.number_prototype } else { self.boolean_prototype };
            return self.get_index_from(proto, i, obj);
        }
        if crate::proxy::chain_has_proxy(self, as_object(obj)) {
            let k = self.property_key(key);
            return crate::proxy::generic_get(self, obj, k, obj);
        }
        let i = match self.property_key(key) {
            Key::Index(i) => i,
            Key::Name(a) => return self.get(obj, a),
        };
        let mut o = as_object(obj);
        if let Some(v) = self.mapped_get(o, i) {
            return Ok(v);
        }
        // A string's index is its code unit there, as a string.
        if let Some(u) = string_units(obj) {
            if (i as usize) < u.len() {
                return Ok(self.single_char(u[i as usize]));
            }
            o = self.string_prototype;
        } else if let Some(u) = wrapped(obj).and_then(string_units)
            && (i as usize) < u.len()
        {
            return Ok(self.single_char(u[i as usize]));
        } else if crate::symbol::is_symbol(obj) {
            // (A symbol's or a bigint's indices are its prototype's.)
            o = self.symbol_roots[crate::symbol::Root::SymbolPrototype as usize];
        } else if crate::bigint::is_bigint(obj) {
            o = self.bigint_prototype;
        }
        self.get_index_from(o, i, obj)
    }

    /// Index `i` of `o` or the first of its prototypes to have it, for
    /// receiver `obj`.
    fn get_index_from(&mut self, mut o: *mut Object, i: u32, obj: Value) -> Result<Value> {
        loop {
            unsafe {
                // A string wrapper's code units, as its own elements.
                if let Some(u) = wrapped(object_value(o)).and_then(string_units)
                    && (i as usize) < u.len()
                {
                    return Ok(self.single_char(u[i as usize]));
                }
                // A typed array's elements are all there is at an index (no
                // holes, nothing inherited), on the receiver or on its chain;
                // a buffer has none. Their `elems` are bytes.
                match (*o).class {
                    Class::TypedArray => {
                        return Ok(if (i as usize) < crate::typed::ta_length(o).unwrap_or(0) {
                            let kind = crate::object::TypedKind::from_u8((*o).kind);
                            kind.load((*o).elems.cast(), i as usize)
                        } else {
                            Value::UNDEFINED
                        });
                    }
                    c if c.no_elements() => return Ok(Value::UNDEFINED),
                    _ => {}
                }
                if i < (*o).elems_len && (*o).class != Class::String {
                    let v = *(*o).elems.add(i as usize);
                    if v != Value::EMPTY {
                        return Ok(v);
                    }
                }
                if crate::far::is_far(o)
                    && let Some(v) = crate::far::get(o, i)
                {
                    return Ok(v);
                }
                if (*o).flags & crate::object::SPARSE != 0
                    && let Some(v) = self.sparse_get(o, i, obj)?
                {
                    return Ok(v);
                }
                let proto = (*(*o).shape).proto;
                if !proto.is_cell() {
                    return Ok(Value::UNDEFINED);
                }
                o = as_object(proto);
            }
        }
    }

    /// `obj[key] = value`.
    /// `obj[key] = value` for compiled code's keyed stores through the
    /// runtime, where `key` names a property: a data property of a plain
    /// object or an array that is neither a prototype nor the global
    /// object, with no accessor of that name on its chain (a store that
    /// runs no code). `None` where the interpreter must do it.
    pub(crate) fn set_named_plain(&mut self, obj: Value, key: Value, value: Value) -> Option<()> {
        let Key::Name(a) = self.property_key(key) else {
            return None;
        };
        self.set_named_rt(obj, a, value)
    }

    /// `obj.name = value` for compiled code's runtime path: on a plain
    /// object or an array, not a prototype or the global object (stores
    /// there move epochs compiled code relies on), and not through a
    /// setter; else nothing (`None`: the interpreter stores).
    pub(crate) fn set_named_rt(&mut self, obj: Value, name: Atom, value: Value) -> Option<()> {
        if !obj.is_cell() {
            return None;
        }
        let o = as_object(obj);
        unsafe {
            // (A function's too: its constant fields, which stores
            // don't cache, come here once given up. Box2D's never set
            // `b2Distance.b2_gjkCalls`, `++`ed: `NaN`, stored at every
            // call, kept it constant, and each call exited there.)
            if !matches!((*o).class, Class::Plain | Class::Array | Class::Function)
                || (*o).flags & crate::object::PROTOTYPE != 0
                || o == self.global
            {
                return None;
            }
        }
        if self.accessor(o, name).is_some() {
            return None;
        }
        self.set(obj, name, value).ok()
    }

    /// `obj.name` for compiled code's runtime path: `None` where it would
    /// throw or call a bytecode getter (the interpreter does). A native
    /// getter (a typed array's `byteLength`) is called.
    pub(crate) fn get_named_rt(&mut self, obj: Value, name: Atom) -> Option<Value> {
        // A number's or a boolean's: its prototype's (PdfJS's
        // `command.charAt`, a number's or a string's, left at each number).
        let holder = if obj.is_cell() {
            as_object(obj)
        } else if obj.is_number() {
            self.number_prototype
        } else if obj.is_bool() {
            self.boolean_prototype
        } else {
            return None;
        };
        if let Some(pair) = self.accessor(holder, name) {
            let g = unsafe { *(*as_object(pair)).elems };
            if g.is_cell() && unsafe { (*as_object(g)).class } != Class::Native {
                return None;
            }
        }
        self.get(obj, name).ok()
    }

    pub fn set_keyed(&mut self, obj: Value, key: Value, value: Value) -> Result<()> {
        if !obj.is_cell() {
            if obj.is_undefined() || obj.is_null() {
                return Err(Error::type_error(format!(
                    "Cannot set properties of {obj:?}"
                )));
            }
            if let Key::Name(a) = self.property_key(key) {
                return self.set_primitive(obj, a, value);
            }
            self.store_failed = true;
            return Ok(());
        }
        if crate::proxy::chain_has_proxy(self, as_object(obj)) {
            let k = self.property_key(key);
            if !crate::proxy::generic_set(self, obj, k, value, obj)? {
                self.store_failed = true;
            }
            return Ok(());
        }
        let i = match self.property_key(key) {
            Key::Index(i) => i,
            Key::Name(a) => return self.set(obj, a, value),
        };
        let o = as_object(obj);
        // (A mapped `arguments` element: its parameter too.)
        if self.mapped_get(o, i).is_some() && self.writable_element(o, i) {
            self.mapped_set(o, i, value);
        }
        unsafe {
            // A string's characters can't be changed; a typed array stores
            // within its length only (a buffer has no elements).
            match (*o).class {
                Class::String => {
                    self.store_failed = true;
                    return Ok(());
                }
                c if c.no_elements() => return Ok(()),
                Class::TypedArray => {
                    // (An immutable buffer's element can't be set.)
                    if crate::typed::is_immutable_view(o) {
                        self.store_failed = true;
                        return Ok(());
                    }
                    // (Converted first: the conversion may detach or
                    // shrink its buffer.)
                    let kind = crate::object::TypedKind::from_u8((*o).kind);
                    let v = crate::typed::to_element(self, kind, value)?;
                    if (i as usize) < crate::typed::ta_length(o).unwrap_or(0) {
                        kind.store_value((*o).elems.cast(), i as usize, v);
                    }
                    return Ok(());
                }
                // (A string object's characters are read-only.)
                Class::Wrapper if wrapped(obj).and_then(string_units).is_some_and(|u| (i as usize) < u.len()) => {
                    self.store_failed = true;
                    return Ok(());
                }
                _ => {}
            }
            // Far past the elements of an object that isn't an array (a
            // map keyed by numbers): a named property, not as many holes.
            if (*o).class != Class::Array && self.sparse_key(o, i) {
                (*o).flags |= crate::object::SPARSE;
                let a = self.intern(&i.to_string());
                return self.set(obj, a, value);
            }
            // (A new element: a prototype's setter or read-only one may
            // take the store.)
            if self.indices_protected && !Self::has_element(o, i) && self.set_new_element(obj, o, i, value)? {
                return Ok(());
            }
            // Past an array's dense elements: far if it is, or if the
            // store would leave it too sparse (see `far::Density`).
            if (*o).class == Class::Array
                && i >= (*o).elems_len
                && (crate::far::is_far(o) || crate::far::goes_far(self, o, i as usize + 1, 1))
            {
                return self.set_far(obj, o, i, value);
            }
            if (*o).flags & !(crate::object::EXTERNAL | PROTOTYPE) != 0 {
                return self.set_keyed_flagged(obj, o, i, value);
            }
            if i >= (*o).elems_len {
                self.reserve_elems(o, i as usize + 1);
                for j in (*o).elems_len..i {
                    *(*o).elems.add(j as usize) = Value::EMPTY;
                }
                (*o).elems_len = i + 1;
            }
            self.store_element(o, i as usize, value);
        }
        Ok(())
    }

    /// `obj[i] = value` where `obj` (object `o`) has no element `i`, once
    /// indices are protected (see `protect_indices`): whether the store
    /// is done, by a setter on the prototype chain, or failed on a
    /// read-only element there (else `obj` gets the element).
    #[cold]
    fn set_new_element(&mut self, obj: Value, o: *mut Object, i: u32, value: Value) -> Result<bool> {
        if self.own_property(o, Key::Index(i)).is_some() {
            return Ok(false);
        }
        let mut p = unsafe { (*(*o).shape).proto };
        while p.is_cell() {
            let po = as_object(p);
            if let Some(d) = self.own_property(po, Key::Index(i)) {
                if d.is_accessor() {
                    match d.set.filter(|f| !f.is_undefined()) {
                        Some(f) => {
                            crate::interp::call_function(self, f, obj, &[value])?;
                        }
                        None => self.store_failed = true,
                    }
                    return Ok(true);
                }
                if d.writable == Some(false) {
                    self.store_failed = true;
                    return Ok(true);
                }
                return Ok(false);
            }
            p = unsafe { (*(*po).shape).proto };
        }
        Ok(false)
    }

    /// `obj[i] = value` on array `o` past its dense elements, where it is
    /// far or goes far (see `far`): an element with attributes (a named
    /// property: `SPARSE`) as such, else a plain one, unless the array's
    /// flags forbid it (sloppy mode: nothing done).
    #[cold]
    fn set_far(&mut self, obj: Value, o: *mut Object, i: u32, value: Value) -> Result<()> {
        use crate::object::{FROZEN, LENGTH_READ_ONLY, NOT_EXTENSIBLE, SPARSE};
        unsafe {
            let flags = (*o).flags;
            if flags & SPARSE != 0 {
                let a = self.intern(&i.to_string());
                if (*(*o).shape).lookup(a).is_some() {
                    return self.set(obj, a, value);
                }
            }
            let far = crate::far::is_far(o);
            if far && crate::far::has(o, i) {
                if flags & FROZEN != 0 {
                    self.store_failed = true;
                } else {
                    crate::far::insert(self, o, i, value);
                }
                return Ok(());
            }
            if flags & NOT_EXTENSIBLE != 0 || i >= crate::far::array_length(o) && flags & LENGTH_READ_ONLY != 0 {
                self.store_failed = true;
                return Ok(());
            }
            if !far {
                crate::far::make_far(self, o, (*o).elems_len);
            }
            crate::far::set(self, o, i, value);
        }
        Ok(())
    }

    /// `obj[i] = value` on an object with elements that have attributes,
    /// a frozen or sealed one, or one that isn't extensible (see
    /// `props`): the store does nothing where it can't happen (sloppy
    /// mode); a wrapped string's characters can't be set.
    #[cold]
    fn set_keyed_flagged(&mut self, obj: Value, o: *mut Object, i: u32, value: Value) -> Result<()> {
        use crate::object::{FROZEN, LENGTH_READ_ONLY, NOT_EXTENSIBLE, SPARSE};
        unsafe {
            let flags = (*o).flags;
            if (*o).class == Class::Wrapper
                && string_units(wrapped(obj).unwrap()).is_some_and(|u| (i as usize) < u.len())
            {
                self.store_failed = true;
                return Ok(());
            }
            if flags & SPARSE != 0 {
                let a = self.intern(&i.to_string());
                if (*(*o).shape).lookup(a).is_some() {
                    return self.set(obj, a, value);
                }
            }
            if Self::has_element(o, i) {
                if flags & FROZEN != 0 || !self.writable_element(o, i) {
                    self.store_failed = true;
                    return Ok(());
                }
                self.store_element(o, i as usize, value);
                return Ok(());
            }
            if flags & NOT_EXTENSIBLE != 0 {
                self.store_failed = true;
                return Ok(());
            }
            if i >= (*o).elems_len {
                if (*o).class == Class::Array && flags & LENGTH_READ_ONLY != 0 {
                    self.store_failed = true;
                    return Ok(());
                }
                self.reserve_elems(o, i as usize + 1);
                for j in (*o).elems_len..i {
                    *(*o).elems.add(j as usize) = Value::EMPTY;
                }
                (*o).elems_len = i + 1;
            }
            self.store_element(o, i as usize, value);
        }
        Ok(())
    }

    /// `a.length = value`: truncated (the elements past it removed) or
    /// grown with holes.
    fn set_array_length(&mut self, o: *mut Object, value: Value) -> Result<()> {
        // A read-only length fails before anything (OrdinarySet: the
        // property isn't writable), even set to what it is, and without
        // converting the value.
        if unsafe { (*o).flags } & crate::object::LENGTH_READ_ONLY != 0 {
            self.store_failed = true;
            return Ok(());
        }
        let d = crate::interp::to_number(value);
        if !(d >= 0.0 && d <= u32::MAX as f64 && d.fract() == 0.0) {
            return Err(Error::range_error("Invalid array length"));
        }
        if unsafe { (*o).flags } & !(crate::object::EXTERNAL | PROTOTYPE) != 0 {
            let d = crate::props::Desc { value: Some(value), ..Default::default() };
            // (Short of what it was set to: a non-configurable element
            // stops it, a TypeError in strict code.)
            if !self.define_array_length(o, &d)? {
                self.store_failed = true;
            }
            return Ok(());
        }
        let n = d as usize;
        unsafe {
            let len = (*o).elems_len as usize;
            if n > len && crate::far::goes_far(self, o, n, 0) {
                crate::far::make_far(self, o, n as u32);
                return Ok(());
            }
            if n > len {
                self.reserve_elems(o, n);
            }
            for i in n.min(len)..n.max(len) {
                *(*o).elems.add(i) = Value::EMPTY;
            }
            (*o).elems_len = n as u32;
        }
        Ok(())
    }

    /// Reallocate `o`'s elements to just their count (room for 4 at
    /// least): what was past them freed at the next collection.
    pub(crate) fn shrink_elems(&mut self, o: *mut Object) {
        unsafe {
            let len = (*o).elems_len as usize;
            self.move_elems(o, len.max(4), len);
        }
    }

    /// Move `o`'s first `len` elements to a new cell with room for `cap`
    /// (and a far array's store, in the slot after: see `far`).
    fn move_elems(&mut self, o: *mut Object, cap: usize, len: usize) {
        unsafe {
            let far = crate::far::is_far(o);
            let e = self.heap.alloc_values_from(cap + far as usize, len);
            if len > 0 {
                std::ptr::copy_nonoverlapping((*o).elems, e, len);
            }
            if far {
                *e.add(cap) = *crate::far::store_slot(o);
            }
            (*o).elems = e;
            crate::heap::barrier_young(o.cast(), &raw const (*o).elems);
        }
    }

    /// Make room for `n` elements.
    /// Whether element `i` of object `o`, not an array, is kept as a named
    /// property: far past its elements, or where storing it would leave
    /// them too sparse (as an array goes far: see `far::goes_far`). A
    /// TypeScript enum's flags (`NodeFlags[1 << 24] = "…"`, each key twice
    /// the last) doubled the elements up to 2^30, out of memory.
    pub(crate) fn sparse_key(&self, o: *mut Object, i: u32) -> bool {
        let len = unsafe { (*o).elems_len } as usize;
        let i = i as usize;
        i >= len && (i > len * 2 + SPARSE_GAP || crate::far::goes_far(self, o, i + 1, 1))
    }

    pub(crate) fn reserve_elems(&mut self, o: *mut Object, n: usize) {
        unsafe {
            let old = Object::elems_cap(o) as usize;
            if n <= old {
                return;
            }
            let cap = n.max(old * 2).max(4);
            u32::try_from(cap + 1).expect("array too large");
            let len = if (*o).elems.is_null() { 0 } else { (*o).elems_len as usize };
            self.move_elems(o, cap, len);
        }
    }

    /// `CreateObject n`: a plain object with room for `n` properties in
    /// itself, its root shape cached in inline-cache slot `ic`.
    pub fn create_object(&mut self, code: &Code, ic: u16, n: u32) -> Value {
        let (ic, _) = code.ic(ic);
        unsafe {
            // (Or made again if no object had it at a full collection:
            // `Vm::sweep_shapes` left it a zombie.)
            if (*ic).shape.is_null() || (*(*ic).shape).is_zombie() {
                let op = object_value(self.object_prototype);
                (*ic).shape = self.root_shape(Class::Plain, op, n);
            }
            // (Given to an object a full marking under way may not trace.)
            crate::object::mark_shape((*ic).shape);
            object_value(self.heap.alloc_object(&*(*ic).shape))
        }
    }

    /// `for … in`'s keys of `v`, as an array of strings: indices (of an
    /// array's elements or a string's code units) in order, then each
    /// object's enumerable properties in insertion order, from `v` up its
    /// prototype chain. A name seen once (even not enumerable) hides it
    /// further up. Null and undefined have none.
    pub fn for_in_keys(&mut self, v: Value) -> Value {
        let mut names: Vec<Vec<u16>> = Vec::new();
        // (Through a proxy's traps: what they throw, a conversion's error.)
        if v.is_cell() && crate::proxy::chain_has_proxy(self, as_object(v)) {
            match crate::proxy::for_in_names(self, as_object(v)) {
                Ok(n) => names = n,
                Err(e) => {
                    self.conv_error.get_or_insert(e);
                    self.conv_thrown = true;
                }
            }
        } else if v.is_cell() {
            let mut seen: Vec<Key> = Vec::new();
            let mut o = as_object(v);
            loop {
                for (key, enumerable) in self.own_keys(o) {
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.push(key);
                    if enumerable {
                        let mut u = Vec::new();
                        match key {
                            Key::Index(i) => u.extend(i.to_string().encode_utf16()),
                            Key::Name(a) => crate::js::lex::string_units(self.atom_name(a), &mut u),
                        }
                        names.push(u);
                    }
                }
                let proto = unsafe { (*(*o).shape).proto };
                if !proto.is_cell() {
                    break;
                }
                o = as_object(proto);
            }
        }
        let a = self.new_array();
        let o = as_object(a);
        unsafe {
            self.reserve_elems(o, names.len());
            for (i, n) in names.iter().enumerate() {
                let s = self.string_constant(n);
                self.store_element(o, i, s);
            }
            (*o).elems_len = names.len() as u32;
        }
        a
    }

    // ---- Strings ----

    /// A new string of `units` (UTF-16 code units), stored after its header.
    pub fn new_string(&mut self, units: &[u16]) -> Value {
        self.new_string_with(units.len(), |out| out.copy_from_slice(units))
    }

    /// A new string of `len` code units, with `words` words after its
    /// header (at least the units'), which `fill` writes from their start.
    pub(crate) fn new_string_raw(
        &mut self,
        len: usize,
        words: usize,
        fill: impl FnOnce(*mut u16),
    ) -> Value {
        unsafe {
            let o = self.alloc_string(words);
            let data = o.cast::<u8>().add(STRING_UNITS).cast::<u16>();
            fill(data);
            // The room past the units zero, not what memory held.
            debug_assert!(4 * words >= len);
            std::ptr::write_bytes(data.add(len), 0, 4 * words - len);
            (*o).elems = data.cast();
            (*o).elems_len = u32::try_from(len).expect("string too long");
            object_value(o)
        }
    }

    /// A string cell with `words` words of units after its `code`, which
    /// records them (`Object::string_words`, see `heap::cell_size`).
    fn alloc_string(&mut self, words: usize) -> *mut Object {
        let o = self
            .heap
            .alloc(crate::heap::object_bytes(1 + words))
            .cast::<Object>();
        unsafe {
            o.cast::<[u64; crate::layout::HEADER_WORDS]>()
                .write(crate::object::object_header(&*self.string_shape));
            Object::set_string_words(o, words);
        }
        o
    }

    /// A new string of `len` code units, which `fill` writes.
    pub fn new_string_with(&mut self, len: usize, fill: impl FnOnce(&mut [u16])) -> Value {
        unsafe {
            let words = len.div_ceil(4);
            let o = self.alloc_string(words);
            let data = o.cast::<u8>().add(STRING_UNITS).cast::<u16>();
            fill(std::slice::from_raw_parts_mut(data, len));
            // The room past the units zero, not what memory held.
            std::ptr::write_bytes(data.add(len), 0, 4 * words - len);
            (*o).elems = data.cast();
            (*o).elems_len = u32::try_from(len).expect("string too long");
            object_value(o)
        }
    }

    /// A string of `units`: one made from equal units since the last
    /// collection began, if [`Vm::short_strings`] still has it, else a new
    /// one. For substrings: a scanner's identifiers repeat (TypeScript's
    /// live strings were 223,000 of 7,000 distinct contents).
    pub(crate) fn short_string(&mut self, units: &[u16]) -> Value {
        if !(2..=SHORT_STRING_UNITS).contains(&units.len()) {
            return self.new_string(units);
        }
        let mut h = units.len() as u64;
        for &c in units {
            h = (h.rotate_left(5) ^ c as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
        let slot = (h >> 54) as usize % SHORT_STRINGS;
        let v = self.short_strings[slot];
        if v != Value::EMPTY && string_units(v) == Some(units) {
            return v;
        }
        let v = self.new_string(units);
        self.short_strings[slot] = v;
        v
    }

    /// The string of the one code unit `c` (below 256, the string
    /// constant: shared).
    pub(crate) fn single_char(&mut self, c: u16) -> Value {
        if c >= 256 {
            return self.new_string(&[c]);
        }
        if self.single_chars[c as usize] == Value::EMPTY {
            // (The constant `"c"` itself: equal to a literal's by identity,
            // which compiled `==` tells without the runtime.)
            self.single_chars[c as usize] = self.string_constant(&[c]);
        }
        self.single_chars[c as usize]
    }

    /// The table of [`Vm::single_char`]'s strings (`EMPTY` where not made
    /// yet), 256 values that stay where they are: compiled code reads it.
    pub(crate) fn single_chars_at(&self) -> u64 {
        self.single_chars.as_ptr() as u64
    }

    /// A cons string of strings `a` and `b` (V8's): no units of its own
    /// (`elems` null) until something reads them (`string_units` flattens
    /// it), `a` and `b` in `props` and `code`, its length the sum. So
    /// building a long string piece by piece costs memory linear in its
    /// length, not quadratic (pdfjs: 138 GB).
    pub(crate) fn new_cons(&mut self, a: Value, b: Value) -> Value {
        let len = string_len(a).unwrap() + string_len(b).unwrap();
        // Past the longest string (as `repeat`'s): a RangeError, left for
        // the operation to throw as a conversion's (compiled code's redone
        // in the interpreter, see `jit::converting`).
        if len > MAX_STRING_LEN {
            self.conv_error.get_or_insert_with(|| Error::range_error("Invalid string length"));
            self.conv_thrown = true;
            self.conv_redo = true;
            return a;
        }
        unsafe {
            let o = self.alloc_string(0);
            (*o).elems = std::ptr::null_mut();
            (*o).elems_len = u32::try_from(len).expect("string too long");
            (*o).props = as_object(a).cast();
            (*o).code = as_object(b).cast();
            object_value(o)
        }
    }

    /// The string constant with `units`, shared by equal ones.
    pub fn string_constant(&mut self, units: &[u16]) -> Value {
        if let Some(&v) = self.strings.get(units) {
            return v;
        }
        let v = self.new_string(units);
        self.heap.note(v);
        self.strings.insert(units.to_vec(), v);
        v
    }

    // ---- Functions ----

    /// A new closure of `code`'s nested function `i`, with a fresh
    /// `prototype` object.
    pub fn create_closure(&mut self, code: &Code, i: u16) -> Value {
        self.new_closure(code.closures[i as usize])
    }

    /// A closure of `callee`, which has no upvalues.
    pub(crate) fn new_closure(&mut self, callee: *const Code) -> Value {
        unsafe {
            let f = self.heap.alloc_object(&*self.closure_shape(callee));
            (*f).code = callee;
            self.closure_prototype(f)
        }
    }

    /// `CreateClosure` in frame `regs` of function `current`: the closure's
    /// upvalues (after its in-object slots: see [`Vm::upvalues`]) are open
    /// ones for `regs`' registers, or `current`'s own.
    pub(crate) fn create_closure_in(
        &mut self,
        code: &Code,
        i: u16,
        regs: *mut Value,
        current: *const Object,
    ) -> Value {
        self.closure_of(code.closures[i as usize], regs, current)
    }

    /// A new closure of `callee` in frame `regs` of function `current`
    /// (see [`Vm::create_closure_in`]).
    pub(crate) fn closure_of(
        &mut self,
        callee: *const Code,
        regs: *mut Value,
        current: *const Object,
    ) -> Value {
        unsafe {
            let shape = &*self.closure_shape(callee);
            let ups = &(*callee).upvalues;
            let f = if ups.is_empty() {
                self.heap.alloc_object(shape)
            } else {
                let o = self.heap.alloc_object_extra(shape, ups.len());
                let out = Self::upvalues(o);
                for (k, &(local, idx)) in ups.iter().enumerate() {
                    *out.add(k) = if local {
                        self.open_upvalue(regs.add(idx as usize))
                    } else {
                        *Self::upvalues(current).add(idx as usize)
                    };
                }
                o
            };
            (*f).code = callee;
            self.closure_prototype(f)
        }
    }

    /// The shape a closure of `callee` gets.
    fn closure_shape(&self, callee: *const Code) -> *const Shape {
        match unsafe { (*callee).kind } {
            4 if !self.generator_function_shape.is_null() => self.generator_function_shape,
            6 if !self.async_generator_function_shape.is_null() => self.async_generator_function_shape,
            5 if !self.async_function_shape.is_null() => self.async_function_shape,
            1 | 5 => self.method_shape,
            _ => self.function_shape,
        }
    }

    /// Give new closure `f` its `prototype` object; `f` as a value.
    fn closure_prototype(&mut self, f: *mut Object) -> Value {
        // (Live in a marking under way: the closure may be allocated
        // black, untraced.)
        crate::gc::mark_code(unsafe { (*f).code });
        let made = unsafe { &(*(*f).code).closures_made };
        made.set(made.get().saturating_add(1).min(2));
        // A second closure: compiled code that took the first one's
        // upvalues for its own (as its only closure's) mustn't run for it.
        // (TypeScript's parser appended a lambda's parameters to an
        // earlier lambda's list, through `translateBinExOperand`'s folded
        // `formals`.) Only that code is lowered again: calls straight into
        // it check for the first closure (`jit::CallSite::closure`); sites
        // bound to the code come once there are two.
        let code = unsafe { &*(*f).code };
        if made.get() == 2 && code.upvalues_folded.replace(false) {
            code.field_deps.set(code.field_deps.get().wrapping_add(1));
            // And the calls bound to that code already: those to any
            // closure of it (`jit::CallSite::by_code`) don't look at the
            // epoch again. (An arrow capturing an object, called from a
            // function compiled on its first call's loop: its next calls'
            // arrows read the first one's object, cloison-fuzz's C02.)
            for &c in &self.jitted {
                unsafe { crate::jit::unbind_calls(c, code) };
            }
        }
        // (An arrow's, a method's or an async function's: none.)
        if matches!(unsafe { (*(*f).code).kind }, 1 | 5) {
            return object_value(f);
        }
        // (A generator function's: a `%GeneratorPrototype%` object; an
        // async one's, an `%AsyncGeneratorPrototype%` one.)
        let kind = unsafe { (*(*f).code).kind };
        if kind == 4 || kind == 6 {
            let root = if kind == 6 { crate::symbol::Root::AsyncGeneratorPrototype } else { crate::symbol::Root::GeneratorPrototype };
            let gp = self.symbol_roots.get(root as usize).copied();
            let proto = gp.map_or(Value::NULL, object_value);
            let p = self.new_object(proto);
            self.store_field(f, 0, object_value(p));
            return object_value(f);
        }
        let p = self.heap.alloc_object(unsafe { &*self.proto_object_shape });
        self.store_field(p, 0, object_value(f));
        self.store_field(f, 0, object_value(p));
        object_value(f)
    }

    /// A closure's upvalues: after its in-object slots, of which every
    /// closure has [`FUNCTION_CAPACITY`] (a function's shapes keep the
    /// capacity they were created with), at a fixed offset.
    #[inline(always)]
    pub(crate) fn upvalues(f: *const Object) -> *mut *mut Upvalue {
        debug_assert_eq!(unsafe { Object::inline_cap(f) }, FUNCTION_CAPACITY as usize);
        unsafe { f.cast::<u8>().add(UPVALUES_OFF).cast_mut().cast() }
    }

    /// The open upvalue of register `at`, made if there is none.
    fn open_upvalue(&mut self, at: *mut Value) -> *mut Upvalue {
        // The registers captured are usually the newest frame's, at or
        // near the end of the list: probe from there first (in deep
        // recursion the list is long, and every probe is a cell's load).
        let ups = &self.open_upvalues;
        let mut k = ups.len();
        let near = k.saturating_sub(8);
        while k > near && unsafe { (*ups[k - 1]).at } >= at {
            k -= 1;
        }
        if k == near && k > 0 {
            k = ups[..k].partition_point(|&u| unsafe { (*u).at } < at);
        }
        if let Some(&u) = self.open_upvalues.get(k)
            && unsafe { (*u).at } == at
        {
            return u;
        }
        let u = self.heap.alloc_upvalue().cast::<Upvalue>();
        unsafe {
            u.write(Upvalue {
                at,
                closed: Value::UNDEFINED,
            })
        };
        self.open_upvalues.insert(k, u);
        u
    }

    /// The value of parameter `i` of mapped `arguments` object `o`, if
    /// mapped: in its register, or in its function's scope object (see
    /// [`MappedArgs::scope`]).
    pub(crate) fn mapped_get(&self, o: *mut Object, i: u32) -> Option<Value> {
        if self.mapped_args.is_empty() {
            return None;
        }
        let m = self.mapped_args.iter().rev().find(|m| m.o == o)?;
        (m.mapped.get(i as usize) == Some(&true)).then(|| unsafe { m.value(i as usize) })
    }

    /// Set parameter `i` of mapped `arguments` object `o`, if mapped.
    pub(crate) fn mapped_set(&mut self, o: *mut Object, i: u32, v: Value) {
        let Some(m) = self.mapped_args.iter().rev().find(|m| m.o == o) else {
            return;
        };
        if m.mapped.get(i as usize) != Some(&true) {
            return;
        }
        match m.scope_slot(i as usize) {
            Some((env, slot)) => self.store_field(env, slot, v),
            None => unsafe { *m.regs.add(1 + i as usize) = v },
        }
    }

    /// Index `i` of `o` no longer mapped: its element keeps the value.
    pub(crate) fn unmap_argument(&mut self, o: *mut Object, i: u32) {
        if let Some(m) = self.mapped_args.iter_mut().rev().find(|m| m.o == o)
            && let Some(on) = m.mapped.get_mut(i as usize)
        {
            *on = false;
        }
    }

    /// `o` frozen: if a mapped `arguments` object, each element still
    /// mapped gets its parameter's value and is unmapped, as a non-writable
    /// definition of it does (`define_own_property`; the arguments object's
    /// [[DefineOwnProperty]]).
    pub(crate) fn unmap_arguments(&mut self, o: *mut Object) {
        let Some(m) = self.mapped_args.iter_mut().rev().find(|m| m.o == o) else {
            return;
        };
        for i in 0..m.mapped.len() {
            if m.mapped[i] && (i as u32) < unsafe { (*o).elems_len } {
                unsafe {
                    let v = m.value(i);
                    let at = (*o).elems.add(i);
                    *at = v;
                    crate::heap::barrier(o.cast(), at, v);
                }
            }
            m.mapped[i] = false;
        }
    }

    /// The mapped `arguments` objects of frames at or past `level`, ended:
    /// their elements get the parameters' values.
    fn release_mapped(&mut self, level: *mut Value) {
        while let Some(m) = self.mapped_args.last()
            && m.regs >= level
        {
            let m = self.mapped_args.pop().unwrap();
            for (i, &on) in m.mapped.iter().enumerate() {
                if on && (i as u32) < unsafe { (*m.o).elems_len } {
                    unsafe {
                        let v = m.value(i);
                        let at = (*m.o).elems.add(i);
                        *at = v;
                        crate::heap::barrier(m.o.cast(), at, v);
                    }
                }
            }
        }
    }

    /// Close the open upvalue of register `at`, if there is one.
    pub(crate) fn close_upvalue(&mut self, at: *mut Value) {
        if let Some(k) = self.open_upvalues.iter().rposition(|&u| unsafe { (*u).at } == at) {
            let u = self.open_upvalues.remove(k);
            unsafe {
                (*u).closed = *(*u).at;
                (*u).at = &raw mut (*u).closed;
                crate::heap::barrier(u.cast::<u8>().sub(8), &raw const (*u).closed, (*u).closed);
            }
        }
    }

    /// Close the open upvalues of registers at or past `level` (a frame
    /// ending): their values move into them. (And its mapped `arguments`
    /// keep its parameters' values.)
    pub(crate) fn close_upvalues(&mut self, level: *mut Value) {
        if !self.mapped_args.is_empty() {
            self.release_mapped(level);
        }
        while let Some(&u) = self.open_upvalues.last()
            && unsafe { (*u).at } >= level
        {
            unsafe {
                (*u).closed = *(*u).at;
                (*u).at = &raw mut (*u).closed;
                crate::heap::barrier(u.cast::<u8>().sub(8), &raw const (*u).closed, (*u).closed);
            }
            self.open_upvalues.pop();
        }
    }

    /// Define `obj.name` as a native function, whose results are always of
    /// type `ret` (compiled code calling it predicts them).
    pub(crate) fn define_native(
        &mut self,
        obj: *mut Object,
        name: &str,
        f: NativeFn,
        nparams: u32,
        ret: crate::jit::Ty,
        constructs: bool,
    ) -> *mut Object {
        let fo = self.native_function(name, f, nparams, ret, constructs);
        let a = self.intern(name);
        // Builtins' methods aren't enumerable.
        self.add_property_with(obj, a, object_value(fo), DONT_ENUM);
        fo
    }

    /// Define natives `fs` (name, function, parameters) on `obj`, not
    /// enumerable, their results of any type: one shape for them all (see
    /// [`Vm::add_properties`]).
    pub(crate) fn define_natives(&mut self, obj: *mut Object, fs: &[(&str, NativeFn, u32)]) {
        let mut names = Vec::with_capacity(fs.len());
        let mut values = Vec::with_capacity(fs.len());
        for &(name, f, n) in fs {
            let fo = self.native_function(name, f, n, crate::jit::Ty::Any, false);
            names.push(self.intern(name));
            values.push(object_value(fo));
        }
        self.add_properties(obj, &names, &values, DONT_ENUM);
    }

    /// A native function object.
    pub(crate) fn native_function(
        &mut self,
        name: &str,
        f: NativeFn,
        nparams: u32,
        ret: crate::jit::Ty,
        constructs: bool,
    ) -> *mut Object {
        let code = self.native_code(name, f, nparams, ret, constructs);
        let fp = object_value(self.function_prototype);
        let fo = self.new_object_with(Class::Native, fp, 0);
        unsafe { (*fo).code = code };
        fo
    }

    /// A native function's code.
    fn native_code(
        &mut self,
        name: &str,
        f: NativeFn,
        nparams: u32,
        ret: crate::jit::Ty,
        constructs: bool,
    ) -> *const Code {
        self.add_code(Code {
            name: name.to_string(),
            line: 0,
            col: 0,
            bytecode: Vec::new(),
            nparams,
            length: nparams,
            kind: 0,
            uses_new_target: false,
            text: None,
            nregs: 1 + nparams,
            ics: Default::default(),
            ic_names: Box::new([]),
            define_ics: Box::new([]),
            closures: Vec::new(),
            constants: Box::new([]),
            handlers: Box::new([]),
            compound_keys: Box::new([]),
            upvalues: Box::new([]),
            interpreted: false,
            field_deps: Cell::new(0),
            ic_misses: Cell::new(0),
            native: Some(f),
            constructs,
            strict: false,
            ctor_capacity: Cell::new(0),
            ctor_shape: Cell::new(std::ptr::null()),
            ctor_settled: Cell::new(true),
            ctor_result: Cell::new(std::ptr::null()),
            ctor_proto: Cell::new(0),
            ctor_shared: Cell::new(false),
            jit: RefCell::new(None),
            calls_left: Cell::new(i32::MAX),
            calls_interpret: Cell::new(false),
            closures_made: Cell::new(0),
            upvalues_folded: Cell::new(false),
            loop_budget: Cell::new(i64::MAX),
            ret_hint: Cell::new(ret),
            mark: Default::default(),
            pinned: Cell::new(true),
            lazy: None,
            realm: 0,
        })
    }

    /// Add properties `names` (none of which `o` has) with `values` and
    /// attributes `attrs`, as `add_property_with` would one by one, but
    /// through one new shape (no transition leads to it; startup made one
    /// shape per builtin, and copied its keys each time).
    pub(crate) fn add_properties(&mut self, o: *mut Object, names: &[Atom], values: &[Value], attrs: u8) {
        self.leave_dictionary(o);
        unsafe {
            let s = &*(*o).shape;
            let n = s.keys().len() + names.len();
            if names.contains(&self.a_sym_has_instance) {
                self.has_instance_defined = true;
            }
            if names.contains(&self.a_sym_to_primitive) {
                self.to_primitive_defined = true;
            }
            let mut keys = Vec::with_capacity(n);
            keys.extend_from_slice(s.keys());
            keys.extend_from_slice(names);
            let mut slot_attrs = Vec::with_capacity(n);
            slot_attrs.extend_from_slice(s.attrs());
            slot_attrs.resize(n, attrs);
            let mut fields = Vec::with_capacity(n);
            fields.extend_from_slice(s.fields());
            for _ in names {
                fields.push(self.new_field_cell());
            }
            self.shape_left_stable(s);
            let new = self.add_shape(Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                slots: Slots::new(&keys, &slot_attrs, &fields),
                elems: s.elems,
                elems_code: s.elems_code,
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: s.unique,
                fixed: s.fixed,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
                relied: Cell::new(false),
                left: Cell::new(false),
                lineage_relied: Cell::new(false),
                parent: s,
                mark: Default::default(),
                traced: Cell::new(0),
                pinned: Cell::new(false),
                dictionary: false,
            });
            // Out-of-line room for the slots past the object's capacity.
            let cap = (*new).capacity.get() as usize;
            let old = Object::props_cap(o) as usize;
            if n > cap && n - cap > old {
                let want = (n - cap).max(old * 2).max(4);
                let p = self.heap.alloc_values(want);
                if !(*o).props.is_null() {
                    std::ptr::copy_nonoverlapping((*o).props, p, old);
                }
                (*o).props = p;
                crate::heap::barrier_young(o.cast(), &raw const (*o).props);
            }
            crate::object::set_shape(o, new);
            let first = n - names.len();
            for (k, &v) in values.iter().enumerate() {
                self.store_field(o, (first + k) as u32, v);
            }
            if (*o).flags & PROTOTYPE != 0 {
                self.proto_epoch = self.proto_epoch.wrapping_add(1);
                self.bump_code_epoch();
            }
        }
    }

    /// The receiver for `new f`: a new object whose prototype is
    /// `f.prototype` (or `Object.prototype` if that is not an object).
    pub(crate) fn construct_this(&mut self, f: *mut Object) -> Value {
        let s = self.ctor_root(f);
        // (Given to an object a full marking under way may not trace.)
        unsafe { crate::object::mark_shape(s) };
        object_value(self.heap.alloc_object(unsafe { &*s }))
    }

    /// The shape `new f` gives `this` now: the root for `f.prototype` and
    /// `f`'s in-object capacity.
    pub(crate) fn ctor_root(&mut self, f: *mut Object) -> *const Shape {
        unsafe {
            debug_assert_eq!((*f).class, Class::Function);
            let code = &*(*f).code;
            // A bytecode function's slot 0 is always `prototype`.
            let p = *f.cast::<u8>().add(crate::layout::slots_offset(Class::Function)).cast::<Value>();
            let proto = if p.is_cell() {
                p
            } else if !self.realms.is_empty() && code.realm != self.realm {
                // (`f`'s realm's.)
                crate::realm::intrinsic(self, code.realm, |vm| object_value(vm.object_prototype))
            } else {
                object_value(self.object_prototype)
            };
            if let Some(e) = self.ctor_proto(code, proto) {
                let e = &*e;
                let mut s = e.root.get();
                if s.is_null() || (*s).capacity.get() != e.cap.get() {
                    s = if e.settled() {
                        self.root_shape(Class::Plain, proto, e.cap.get())
                    } else {
                        self.private_root(proto, MAX_INLINE_CAPACITY)
                    };
                    e.root.set(s);
                }
                return s;
            }
            let cap = code.ctor_capacity.get();
            let mut s = code.ctor_shape.get();
            if s.is_null() || (*s).proto != proto || (*s).capacity.get() != cap {
                s = if code.ctor_settled.get() {
                    self.root_shape(Class::Plain, proto, cap)
                } else {
                    self.private_root(proto, cap)
                };
                code.ctor_shape.set(s);
            }
            s
        }
    }

    /// The slack tracking of `new` of `code` for objects with prototype
    /// `proto`, once `code` has made objects with another prototype than
    /// its first's (see `Code::ctor_shared`); `None` before: the code's.
    /// The prototypes constructor tracking knows by address (codes' first
    /// and `ctor_protos`' keys), for `CLOISON_GC_FRAG`.
    pub(crate) fn ctor_protos_keys(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.ctor_protos.keys().map(|&k| k as usize).collect();
        v.extend(self.codes().map(|c| c.ctor_proto.get() as usize).filter(|&p| p != 0));
        v
    }

    pub(crate) fn ctor_proto(&mut self, code: &Code, proto: Value) -> Option<*const CtorProto> {
        let p = proto.to_bits();
        if !code.ctor_shared.get() {
            if code.ctor_proto.get() == 0 {
                code.ctor_proto.set(p);
            } else if code.ctor_proto.get() != p {
                code.ctor_shared.set(true);
            }
        }
        if !code.ctor_shared.get() {
            return None;
        }
        let e = self.ctor_protos.entry(p).or_insert_with(|| {
            // (The first prototype's tracking was the code's: settled, if
            // a `new` returned.)
            let first = code.ctor_proto.get() == p && code.ctor_settled.get();
            let root = code.ctor_shape.get();
            Box::new(CtorProto {
                cap: Cell::new(if first { code.ctor_capacity.get() } else { MAX_INLINE_CAPACITY }),
                root: Cell::new(if first && !root.is_null() && unsafe { (*root).proto } == proto {
                    root
                } else {
                    std::ptr::null()
                }),
                made: Cell::new(if first { SLACK_OBJECTS } else { 0 }),
            })
        });
        Some(&**e)
    }

    /// After `new` returned `this`: grow the in-object capacity for the next
    /// objects if this one outgrew it.
    pub(crate) fn construct_done(code: &Code, this: *mut Object) {
        if !code.ctor_settled.replace(true) {
            settle(code);
        }
        let shape = unsafe { (*this).shape };
        let n = unsafe { (*shape).keys().len() } as u32;
        let cap = match Self::shared_proto(code, this) {
            Some(e) if !e.settled() => {
                e.made.set(e.made.get() + 1);
                if e.settled() && !e.root.get().is_null() {
                    e.cap.set(shrink_root(e.root.get()));
                }
                &e.cap
            }
            Some(e) => &e.cap,
            None => &code.ctor_capacity,
        };
        if n > cap.get() {
            cap.set(n.min(MAX_INLINE_CAPACITY));
        }
        let seen = code.ctor_result.get();
        // (A dictionary shape changes in place: as many.)
        if unsafe { (*shape).dictionary } {
            code.ctor_result.set(MANY_SHAPES);
        } else if seen.is_null() {
            code.ctor_result.set(shape);
        } else if seen != shape {
            code.ctor_result.set(MANY_SHAPES);
        }
    }

    /// The slack tracking of `this`'s prototype, made by `new` of `code`,
    /// if that is shared (see [`Vm::ctor_proto`]).
    pub(crate) fn shared_proto<'a>(code: &Code, this: *mut Object) -> Option<&'a CtorProto> {
        if !code.ctor_shared.get() {
            return None;
        }
        unsafe {
            let vm = &mut *crate::interp::current_vm();
            vm.ctor_proto(code, (*(*this).shape).proto).map(|e| &*e)
        }
    }

    /// `new.target`'s prototype, if constructor `f` (a native) was called
    /// with another target, and Get it: OrdinaryCreateFromConstructor's
    /// read (which the runtime leaves to `f` then: see
    /// `intrinsics::construct_with`).
    pub(crate) fn take_new_target(&mut self, f: *mut Object, default: Value) -> Result<Value> {
        if self.native_new_target.0 != object_value(f) {
            return Ok(default);
        }
        let nt = std::mem::replace(&mut self.native_new_target, (Value::EMPTY, Value::EMPTY)).1;
        let a = self.a_prototype;
        let p = self.get(nt, a)?;
        if crate::props::is_object(p) {
            return Ok(p);
        }
        crate::realm::default_proto(self, nt, default)
    }

    /// Whether the native now called was called by `new`, taken: a
    /// constructor reads it first thing (it is cleared by any call made
    /// after).
    pub(crate) fn is_construct(&mut self) -> bool {
        std::mem::replace(&mut self.constructing, false)
    }

    /// Whether a conversion threw (`conv_thrown`), read from memory: the
    /// conversions set it through the current VM's pointer, not this
    /// reference, so a plain read may be one from before them.
    pub(crate) fn conv_failed(&self) -> bool {
        unsafe { std::ptr::read_volatile(&raw const self.conv_thrown) }
    }

    /// What a conversion threw (see `conv_error`), taken.
    #[cold]
    #[inline(never)]
    pub(crate) fn take_conv_error(&mut self) -> Error {
        self.conv_thrown = false;
        self.conv_error.take().expect("a conversion's error")
    }

    /// Call `f` when it isn't a bytecode function (those the interpreter
    /// enters itself).
    #[inline]
    pub fn call_other(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Value> {
        if f.is_cell() {
            let o = as_object(f);
            unsafe {
                if (*o).class == Class::Native {
                    // (A native calling natives, as iterator helpers' chains
                    // or a proxy's of proxies do, recurses natively alone.)
                    crate::interp::check_native_stack(self)?;
                    let native = (*(*o).code).native.unwrap();
                    if !self.realms.is_empty() && (*(*o).code).realm != self.realm {
                        return crate::realm::run_in(self, (*(*o).code).realm, |vm| {
                            vm.constructing = false;
                            vm.callee = o;
                            native(vm, this, args)
                        });
                    }
                    self.constructing = false;
                    self.callee = o;
                    return native(self, this, args);
                }
            }
        }
        Err(Error::type_error(format!("{} is not a function", display(f))))
    }

    /// `new f(…args)` when `f` isn't a bytecode function: a native that
    /// constructs (`Array`, `Object`) makes its object.
    pub fn construct_other(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        if f.is_cell() {
            let o = as_object(f);
            unsafe {
                if (*o).class == Class::Native && (*(*o).code).constructs {
                    crate::interp::check_native_stack(self)?;
                    let native = (*(*o).code).native.unwrap();
                    if !self.realms.is_empty() && (*(*o).code).realm != self.realm {
                        return crate::realm::run_in(self, (*(*o).code).realm, |vm| {
                            vm.constructing = true;
                            vm.callee = o;
                            let r = native(vm, Value::UNDEFINED, args);
                            vm.constructing = false;
                            r
                        });
                    }
                    self.constructing = true;
                    self.callee = o;
                    let r = native(self, Value::UNDEFINED, args);
                    self.constructing = false;
                    return r;
                }
            }
        }
        Err(Error::type_error(format!("{} is not a constructor", display(f))))
    }

    // ---- Loading and running ----

    fn add_code(&mut self, mut c: Code) -> *const Code {
        c.realm = self.realm;
        let b = Box::new(c);
        let p: *const Code = &*b;
        self.codes.push(b);
        self.code_set.insert(p as usize);
        crate::gc::code_bytes(unsafe { (*p).footprint() });
        if self.heap.gc.code_full() {
            self.heap.requested.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        p
    }

    /// `%ThrowTypeError%`, as a value.
    pub(crate) fn throw_type_error_fn(&self) -> Value {
        object_value(self.thrower)
    }

    /// A script's global declarations: a block-scoped name may not be
    /// another script's (any kind), nor a `var` an earlier script's
    /// block-scoped one (a SyntaxError, before it runs).
    pub(crate) fn declare_script(&mut self, module: &Module) -> Result<()> {
        for n in &module.lexical {
            let a = self.intern(n);
            if self.declared.contains(&a) || self.lexical_declared.contains(&a) {
                return Err(Error(format!("SyntaxError: Identifier '{n}' has already been declared"), None));
            }
            if let Some(d) = self.own_property(self.global, Key::Name(a))
                && d.configurable == Some(false)
            {
                return Err(Error(format!("SyntaxError: Identifier '{n}' has already been declared"), None));
            }
        }
        for n in &module.declared {
            let a = self.intern(n);
            if self.lexical_declared.contains(&a) {
                return Err(Error(format!("SyntaxError: Identifier '{n}' has already been declared"), None));
            }
        }
        self.global_declarations(module, false)?;
        for n in &module.lexical {
            let a = self.intern(n);
            self.lexical_declared.insert(a);
        }
        for n in &module.declared {
            let a = self.intern(n);
            self.script_declared.insert(a);
        }
        Ok(())
    }

    /// GlobalDeclarationInstantiation's (or an eval's, `eval`) checks for
    /// a script's `var`s and functions, a `TypeError` where the global
    /// object can't take one; a function's global made a writable,
    /// enumerable data property (configurable only by an eval) if it was a
    /// configurable property already; and the globals the script declares
    /// that don't exist yet made, functions' then `var`s' (see
    /// [`Vm::declare_global`]).
    pub(crate) fn global_declarations(&mut self, module: &Module, eval: bool) -> Result<()> {
        let g = self.global;
        let extensible = Self::is_extensible(g);
        for n in &module.declared {
            let a = self.intern(n);
            let function = module.function_names.contains(n);
            let ok = match self.own_property(g, Key::Name(a)) {
                None => extensible,
                Some(_) if !function => true,
                Some(d) => {
                    d.configurable == Some(true)
                        || d.is_data() && d.writable == Some(true) && d.enumerable == Some(true)
                }
            };
            if !ok {
                let what = if function { "function" } else { "variable" };
                return Err(Error::type_error(format!("Cannot declare global {what} '{n}'")));
            }
        }
        // A `var` the global's prototypes have (`__proto__`'s accessor, a
        // read-only property): its own property now, for stores not to
        // reach theirs.
        for n in &module.declared {
            let a = self.intern(n);
            if !module.function_names.contains(n)
                && unsafe { (*(*g).shape).lookup(a).is_none() }
                && self.lookup(g, a).is_some()
                && extensible
            {
                let d = crate::props::Desc::data(Value::UNDEFINED, true, true, eval);
                self.define_own_property(g, Key::Name(a), &d)?;
            }
        }
        for n in &module.function_names {
            let a = self.intern(n);
            if let Some(d) = self.own_property(g, Key::Name(a))
                && d.configurable == Some(true)
            {
                let v = d.value.unwrap_or(Value::UNDEFINED);
                let d = crate::props::Desc::data(v, true, true, eval);
                self.define_own_property(g, Key::Name(a), &d)?;
            }
        }
        if extensible {
            let attrs = if eval { 0 } else { DONT_DELETE };
            let functions = module.function_names.iter();
            let vars = module.declared.iter().filter(|n| !module.function_names.contains(n));
            for n in functions.chain(vars) {
                let a = self.intern(n);
                if unsafe { (*(*g).shape).lookup(a).is_none() } {
                    self.declare_global(a, attrs);
                }
            }
        }
        Ok(())
    }

    /// Global `name` made `undefined` before the script declaring it runs,
    /// its field left as though nothing were stored yet: its first store
    /// sets the field's type and constant value, as when the store made
    /// the global (an int, not `undefined`, for compiled code to read; see
    /// `FieldCell`), and compiled code folds no `undefined`.
    fn declare_global(&mut self, name: Atom, attrs: u8) {
        let g = self.global;
        unsafe {
            let new = self.transition_with((*g).shape, name, attrs);
            let ns = &*new;
            let slot = ns.keys().len() as u32 - 1;
            if slot >= ns.capacity.get() {
                self.reserve_props(g, slot - ns.capacity.get());
            }
            crate::object::set_shape(g, new);
            if (*ns.fields()[slot as usize]).get() == FieldType::None {
                *Object::slot(g, ns.location(slot)) = Value::UNDEFINED;
            } else {
                // (A field another global's shape typed: a realm's.)
                self.store_field(g, slot, Value::UNDEFINED);
            }
            if (*g).flags & PROTOTYPE != 0 {
                self.proto_epoch = self.proto_epoch.wrapping_add(1);
                self.bump_code_epoch();
            }
        }
    }

    /// Load `module`'s functions; returns its top-level code.
    pub fn load(&mut self, module: Module) -> *const Code {
        for n in &module.declared {
            let a = self.intern(n);
            self.declared.insert(a);
        }
        let script = module.script;
        self.load_functions(module, None)[script]
    }

    /// Compile the function of stub `code` (see `asm::LazyFn`) into it:
    /// a syntax error (or what the code generator doesn't support) is
    /// thrown at its call, and it stays a stub.
    pub(crate) fn compile_lazy(&mut self, code: *const Code) -> Result<()> {
        let c = unsafe { &*code };
        let mut module = crate::js::compile_lazy(c.lazy.as_ref().expect("a stub")).map_err(|e| {
            if e.contains("unsupported") {
                Error(format!("unsupported: in eval'd code: {e}"), None)
            } else {
                Error(format!("SyntaxError: {e}"), None)
            }
        })?;
        debug_assert_eq!(module.script, 0);
        module.functions[0].name = c.name.clone();
        self.load_functions(module, Some(code));
        Ok(())
    }

    /// Load `module`'s functions, the first into `into` if given (a stub
    /// compiled: see [`Vm::compile_lazy`]).
    fn load_functions(&mut self, module: Module, into: Option<*const Code>) -> Vec<*const Code> {
        let recursive = recursive_functions(&module);
        static QUICKEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let quicken = *QUICKEN.get_or_init(|| std::env::var_os("CLOISON_NO_QUICKEN").is_none());
        let mut units = Vec::new();
        // Sized ahead: each growth hashes every key again, the longest
        // literal's too (TypeScript's 1.28 MB input: loading -10%).
        self.strings.reserve(module.functions.iter().map(|f| f.constants.len()).sum());
        let mut closures = Vec::with_capacity(module.functions.len());
        let codes: Vec<*const Code> = module
            .functions
            .into_iter()
            .zip(recursive)
            .enumerate()
            .map(|(i, (f, recursive))| {
                let scan = Scan::of(&f);
                // Not inlined nor called directly from compiled code (whose
                // frames an exception unwinds without their handlers):
                // `Code::interpreted`. With handlers, though, it is compiled,
                // entered from the runtime and at its loops: an exception
                // there unwinds from where its compiled code left, its
                // handlers included (`interp::drive_from`). (GameBoy's
                // `fromTypedArray`, a copy loop in a `try`, ran interpreted
                // at each run.)
                let interpreted = !f.handlers.is_empty() || scan.callee || f.lazy.is_some();
                let only_interpreted = scan.callee || f.lazy.is_some();
                let ic_names: Vec<Atom> = f.ics.iter().map(|n| self.intern(n)).collect();
                let constants: Box<[Value]> = f
                    .constants
                    .iter()
                    .map(|c| match c {
                        crate::asm::Constant::Number(d) => Value::number(*d),
                        crate::asm::Constant::String(t) => {
                            units.clear();
                            crate::js::lex::string_units(t, &mut units);
                            self.string_constant(&units)
                        }
                    })
                    .collect();
                let mut bytecode = f.code;
                let uses_new_target = bytecode.first() == Some(&(crate::bytecode::Op::LdaNewTarget as u8));
                // (Without the slack it grew with: it lives as long as the VM.)
                bytecode.shrink_to_fit();
                let counter = if f.lazy.is_some() { &STUBS } else { &FUNCTIONS };
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if f.lazy.is_none() {
                    BYTECODE_BYTES.fetch_add(bytecode.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
                if quicken {
                    crate::bytecode::quicken(&mut bytecode);
                }
                self.max_frame = self.max_frame.max(f.nregs as usize + crate::interp::HEADER);
                closures.push(f.closures);
                let calls_left = match (scan.has_loop, recursive || scan.ops >= ENTRY_COMPILE_SIZE) {
                    // (Several realms: every call checks the callee's.)
                    _ if !self.realms.is_empty() => 0,
                    _ if only_interpreted => i32::MAX,
                    _ if f.captures && scan.ops > CAPTURING_ENTRY_SIZE => i32::MAX,
                    (true, _) => self.tier_calls,
                    (false, true) => self.tier_calls.saturating_mul(LOOP_FREE_CALLS),
                    // (One whose registers closures capture isn't inlined:
                    // compiled callers call it through the runtime, into
                    // the interpreter, unless it is compiled. EarleyBoyer's
                    // `deriv_trees`, recursive through its closures, ran
                    // interpreted for 12 million calls from `loop2`.)
                    (false, false) if f.captures => self.tier_calls.saturating_mul(LOOP_FREE_CALLS),
                    (false, false) => i32::MAX,
                };
                let loop_budget = if !only_interpreted { self.frame_budget } else { i64::MAX };
                if let Some(into) = into.filter(|_| i == 0) {
                    // The stub's runtime state stays (its constructor's
                    // slack tracking, its liveness): it has never run.
                    let c = unsafe { &mut *into.cast_mut() };
                    debug_assert_eq!(c.nparams, f.nparams);
                    // Its upvalues in the stub's order: the names its
                    // closures captured, declared again in that order (see
                    // `js::compile_lazy`). The stub's sources, where they
                    // are in its parent, stay; the new ones are where they
                    // are in the wrapper declaring them.
                    debug_assert_eq!(c.upvalues.len(), f.upvalues.len());
                    debug_assert_eq!(c.lazy.as_ref().map(|l| l.upvalues.len()), Some(f.upvalues.len()));
                    let before = c.footprint();
                    c.bytecode = bytecode;
                    c.nregs = f.nregs;
                    c.ics = Default::default();
                    c.ic_names = ic_names.into_boxed_slice();
                    c.define_ics = f.defines.into_boxed_slice();
                    c.constants = constants;
                    c.handlers = f.handlers.into_boxed_slice();
                    c.compound_keys = f.compound_keys.into_boxed_slice();
                    c.interpreted = interpreted;
                    c.calls_left.set(calls_left);
                    c.loop_budget.set(loop_budget);
                    c.lazy = None;
                    crate::gc::code_bytes(c.footprint().saturating_sub(before));
                    return into;
                }
                self.add_code(Code {
                    name: f.name,
                    line: f.line,
                    col: f.col,
                    bytecode,
                    strict: f.strict,
                    nparams: f.nparams,
                    length: f.length,
                    kind: f.kind,
                    uses_new_target,
                    text: f.text.clone(),
                    nregs: f.nregs,
                    ics: Default::default(),
                    ic_names: ic_names.into_boxed_slice(),
                    define_ics: f.defines.into_boxed_slice(),
                    closures: Vec::new(),
                    constants,
                    handlers: f.handlers.into_boxed_slice(),
                    compound_keys: f.compound_keys.into_boxed_slice(),
                    upvalues: f.upvalues.into_boxed_slice(),
                    interpreted,
                    field_deps: Cell::new(0),
                    ic_misses: Cell::new(0),
                    native: None,
                    constructs: false,
                    ctor_capacity: Cell::new(MAX_INLINE_CAPACITY),
                    ctor_shape: Cell::new(std::ptr::null()),
                    ctor_settled: Cell::new(false),
                    ctor_result: Cell::new(std::ptr::null()),
                    ctor_proto: Cell::new(0),
                    ctor_shared: Cell::new(false),
                    jit: RefCell::new(None),
                    ret_hint: Cell::new(crate::jit::Ty::Any),
                    calls_interpret: Cell::new(false),
                    closures_made: Cell::new(0),
                    upvalues_folded: Cell::new(false),
                    calls_left: Cell::new(calls_left),
                    loop_budget: Cell::new(loop_budget),
                    mark: crate::gc::code_epoch().into(),
                    pinned: Cell::new(false),
                    lazy: f.lazy.map(Box::new),
                    realm: 0,
                })
            })
            .collect();
        for (nested, &c) in closures.into_iter().zip(&codes) {
            let closures = nested.iter().map(|&i| codes[i]).collect();
            unsafe { (*c.cast_mut()).closures = closures };
        }
        codes
    }

    /// Load and run `module`; returns its completion value. The module
    /// is `js::compile`'s or `asm::parse`'s, or one `asm::verify` accepts:
    /// its bytecode is run unchecked.
    pub fn run(&mut self, module: &Module) -> Result<Value> {
        crate::interp::CURRENT_VM.with(|v| v.set(self));
        self.declare_script(module)?;
        let code = self.load(module.clone());
        let here = 0u8;
        self.native_limit = (&raw const here as usize).saturating_sub(crate::interp::NATIVE_STACK);
        // Roots on the native stack are below here (see `gc`).
        let outer = self.heap.gc.stack_base == 0;
        if outer {
            self.heap.gc.stack_base = &raw const here as usize & !7;
        }
        let r = unsafe { crate::interp::run_script(self, code) };
        // The script done, the promise jobs it queued run (and theirs),
        // and the outermost's event loop until no async waiter is left.
        let r = r.and_then(|v| if outer { self.run_event_loop() } else { self.run_jobs() }.map(|()| v));
        self.kept_alive.clear();
        if outer {
            self.heap.gc.stack_base = 0;
            self.pause_marker();
        }
        r
    }

    /// Run `f` from the host, as `run` runs a script (then the event
    /// loop): an agent's broadcast callback (see `agent`).
    pub(crate) fn enter(&mut self, f: impl FnOnce(&mut Vm) -> Result<Value>) -> Result<Value> {
        crate::interp::CURRENT_VM.with(|v| v.set(self));
        let here = 0u8;
        self.native_limit = (&raw const here as usize).saturating_sub(crate::interp::NATIVE_STACK);
        let outer = self.heap.gc.stack_base == 0;
        if outer {
            self.heap.gc.stack_base = &raw const here as usize & !7;
        }
        let r = self.handle_scope(|vm| {
            let saved = vm.native_top;
            vm.native_top = vm.stack.as_mut_ptr();
            let r = f(vm);
            vm.native_top = saved;
            r
        });
        let r = r.and_then(|v| self.run_event_loop().map(|()| v));
        self.kept_alive.clear();
        if outer {
            self.heap.gc.stack_base = 0;
            self.pause_marker();
        }
        r
    }

    /// Whether `Atomics.wait` may block (test262's `CanBlockIsTrue`, the
    /// default; `CanBlockIsFalse`, as a browser's main thread).
    pub fn set_can_block(&mut self, on: bool) {
        self.can_block = on;
    }

    /// The script's file: its `import()`s resolve from its directory.
    pub fn set_script_path(&mut self, path: &str) {
        self.modules.script_path = Some(path.to_string());
    }

    /// Run module `path` (see `module`) and the modules it imports, then
    /// the promise jobs queued. Fails with an early error (a syntax error,
    /// an import that can't be resolved), or what its evaluation threw.
    pub fn run_module(&mut self, path: &str) -> std::result::Result<(), ModuleError> {
        crate::interp::CURRENT_VM.with(|v| v.set(self));
        let here = 0u8;
        self.native_limit = (&raw const here as usize).saturating_sub(crate::interp::NATIVE_STACK);
        let outer = self.heap.gc.stack_base == 0;
        if outer {
            self.heap.gc.stack_base = &raw const here as usize & !7;
        }
        let r = self.handle_scope(|vm| {
            let saved = vm.native_top;
            vm.native_top = vm.stack.as_mut_ptr();
            let r = crate::module::run_main(vm, path);
            vm.native_top = saved;
            match r {
                Ok(p) => {
                    vm.handles.push(p);
                    if outer { vm.run_event_loop() } else { vm.run_jobs() }.map_err(ModuleError::Thrown)?;
                    match crate::promise::state(p) {
                        (2, v) => Err(ModuleError::Thrown(Error::thrown(v))),
                        _ => Ok(()),
                    }
                }
                Err(Ok(m)) => Err(ModuleError::Early(m)),
                Err(Err(e)) => Err(ModuleError::Thrown(e)),
            }
        });
        self.kept_alive.clear();
        if outer {
            self.heap.gc.stack_base = 0;
            self.pause_marker();
        }
        r
    }

    /// Run the promise jobs queued (see `promise::run_jobs`): the VM stack
    /// is free, so they run from its base.
    fn run_jobs(&mut self) -> Result<()> {
        let saved = self.native_top;
        self.native_top = self.stack.as_mut_ptr();
        let r = crate::promise::run_jobs(self);
        self.native_top = saved;
        r
    }

    /// Run the promise jobs queued, then wait for the async waiters
    /// (`Atomics.waitAsync`'s) to be notified or time out, running the
    /// jobs their promises queue, until none is left (see `shared`).
    pub(crate) fn run_event_loop(&mut self) -> Result<()> {
        loop {
            self.run_jobs()?;
            if !self.shared.waiting() {
                return Ok(());
            }
            crate::shared::idle(self);
            let saved = self.native_top;
            self.native_top = self.stack.as_mut_ptr();
            let r = crate::shared::poll(self);
            self.native_top = saved;
            r?;
        }
    }

    /// Run `f` with the values `vm.handles` gets meanwhile rooted: Rust
    /// code that keeps values in a heap container (a `Vec`) across a call
    /// into JS pushes them there.
    pub(crate) fn handle_scope<T>(&mut self, f: impl FnOnce(&mut Vm) -> T) -> T {
        let depth = self.handles.len();
        let r = f(self);
        self.handles.truncate(depth);
        r
    }

    /// The cells the VM refers to itself: the global object, the builtins'
    /// prototypes and functions, `single_chars`, a pending error's value.
    pub(crate) fn named_roots(&self) -> Vec<Value> {
        let mut v: Vec<Value> = [
            self.global,
            self.object_prototype,
            self.function_prototype,
            self.array_prototype,
            self.string_prototype,
            self.missing_holder,
            self.function_call_fn,
            self.array_fn,
            self.date_prototype,
            self.number_prototype,
            self.bigint_prototype,
            self.regexp_prototype,
            self.boolean_prototype,
            self.array_buffer_prototype,
            self.shared_atomics,
            self.regexp_constructor,
            self.array_push_fn,
            self.array_pop_fn,
        ]
        .into_iter()
        .chain(self.typed_prototypes.iter().copied())
        .chain(self.typed_constructors.iter().copied())
        .chain(self.collection_prototypes.iter().copied())
        .chain(self.promise_roots.iter().copied())
        .chain(self.iterator_roots.iter().copied())
        .chain(self.template_cache.values().copied())
        .chain(self.symbols.values().copied())
        .chain(self.symbol_roots.iter().copied())
        .chain(std::iter::once(self.thrower))
        .chain(self.mapped_args.iter().map(|m| m.o))
        .chain(self.mapped_args.iter().map(|m| m.scope.0))
        .chain(std::iter::once(self.eval_function))
        .chain(self.error_prototypes.iter().copied())
        .chain(self.shadow_roots.iter().copied())
        .chain(self.intl_roots.iter().copied())
        .filter(|o| !o.is_null())
        .map(object_value)
        .collect();
        v.push(self.bind_helper);
        v.push(self.from_async_helper);
        v.extend_from_slice(&self.dispose_roots);
        v.extend(self.modules.roots());
        v.extend(self.shared.roots());
        v.extend([self.legacy_match.0, self.legacy_input].into_iter().filter(|v| v.is_cell()));
        v.extend(self.agent.roots());
        v.extend(self.temporal.roots());
        v.extend_from_slice(&self.single_chars);
        for r in &self.realms {
            r.trace(&mut v);
        }
        if !self.job_queue.is_null() {
            v.push(object_value(self.job_queue));
        }
        if let Some(Error(_, Some(e))) = &self.error {
            v.push(*e);
        }
        v
    }

    /// Every shape.
    pub(crate) fn shapes(&self) -> impl Iterator<Item = &Shape> {
        self.shapes.iter().map(|s| &**s)
    }

    /// Every function's code.
    pub(crate) fn codes(&self) -> impl Iterator<Item = &Code> {
        self.codes.iter().map(|c| &**c)
    }

    /// The string constants (see [`Vm::string_constant`]).
    pub(crate) fn string_constants(&self) -> Vec<Value> {
        self.strings.values().copied().collect()
    }

    /// `Symbol.for`'s symbols, roots for as long as the agent lives (a
    /// young one is also in the log when registered: see `symbol_for`).
    pub(crate) fn registered_symbols(&self) -> Vec<Value> {
        self.symbol_registry.values().map(|&o| object_value(o)).collect()
    }

    /// Whether functions are compiled warm first, then hot, or hot from
    /// the start (the default; `CLOISON_WARM` set: warm first). See
    /// `jit::Tier`.
    pub fn set_warm_tier(&mut self, on: bool) {
        self.warm_tier = on;
    }

    /// The least code (in bytes) loaded before a full collection looks for
    /// dead code (for tests; see `gc::CODE_MIN`).
    pub fn set_code_room(&mut self, bytes: usize) {
        self.heap.gc.code_min = bytes;
        self.heap.gc.code_room = bytes;
    }

    /// Collect every `bytes` allocated, checking references and poisoning
    /// what is freed, as `CLOISON_GC_VERIFY` does (for tests).
    pub fn set_gc_verify(&mut self, bytes: usize) {
        self.heap.gc.verify = bytes;
        self.heap.budget = self.heap.gc.budget();
    }

    /// The functions' codes loaded and not freed (for tests).
    pub fn code_count(&self) -> usize {
        self.codes.len() + self.condemned.len()
    }

    /// The entries the weak collections (`WeakMap`s, `WeakSet`s) not yet
    /// found dead hold (for tests of the ephemeron pass).
    pub fn weak_entry_count(&self) -> usize {
        self.weak_tables.iter().map(|&t| crate::collections::live_entries(t)).sum()
    }

    /// Loop back edges a frame runs in the interpreter before tiering up
    /// into the JIT, and (up to `TIER_CALLS`) calls before a function's
    /// calls enter compiled code; `i64::MAX` disables the JIT.
    pub fn set_tier_back_edges(&mut self, n: i64) {
        self.frame_budget = n.max(1);
        self.tier_calls = if n == i64::MAX {
            i32::MAX
        } else {
            n.clamp(1, TIER_CALLS as i64) as i32
        };
    }

    /// How often `function`'s inline caches missed in the interpreter (for
    /// tests).
    pub fn ic_misses(&self, function: &str) -> u32 {
        self.codes.iter().filter(|c| c.name == function).map(|c| c.ic_misses.get()).sum()
    }

    /// How many of `function`'s calls go straight into their callees'
    /// compiled code (for tests).
    pub fn jit_bound_direct_calls(&self, function: &str) -> usize {
        let Some(code) = self.codes.iter().find(|c| c.name == function) else {
            return 0;
        };
        let jit = code.jit.borrow();
        jit.as_ref().map_or(0, |f| f.bound_direct_calls())
    }

    /// `function`'s JIT counters, if it was compiled (for tests).
    pub fn jit_stats(&self, function: &str) -> Option<crate::jit::JitStats> {
        let code = self.codes.iter().find(|c| c.name == function)?;
        Some(code.jit.borrow().as_ref()?.stats)
    }

    /// The stencils of `function`'s most recent compiled code, if any (for
    /// tests).
    pub fn jit_stencils(&self, function: &str) -> Option<Vec<String>> {
        // (Not a native of that name, `Set.prototype.add` say.)
        let code = self.codes.iter().find(|c| c.name == function && c.native.is_none())?;
        let jit = code.jit.borrow();
        let names = jit
            .as_ref()?
            .last_code()?
            .stencils()
            .map(String::from)
            .collect();
        Some(names)
    }

    /// Words of in-object storage in `o` (for tests).
    pub fn inline_capacity(&self, o: Value) -> u32 {
        unsafe { (*(*as_object(o)).shape).capacity.get() }
    }
}


/// What `load` needs of a function's code, in one pass: whether it has a
/// loop ([`Scan::has_loop`]), whether it reads `arguments.callee` (see
/// `Code::interpreted`), and its instruction count (large: see
/// [`ENTRY_COMPILE_SIZE`]).
struct Scan {
    /// Whether calls to a function should enter its compiled code once it is
    /// hot: if it has a loop (whose back-edge budget restarts with each frame,
    /// so a short loop in a function called often never tiers up by itself),
    /// or at least [`ENTRY_COMPILE_SIZE`] instructions ([`Scan::ops`]), or if it
    /// looks recursive (see [`recursive_functions`]): its compiled code can
    /// then call itself, or the functions it recurses through, directly.
    /// Entering compiled code costs a round trip through the driver, which a
    /// small loop-free function doesn't win back; when compiled code calls it,
    /// it is inlined instead. A loop-free one is entered after
    /// [`LOOP_FREE_CALLS`] times as many calls: compiled sooner, its callers'
    /// feedback is still growing and it gets compiled again (91: f and g
    /// three and two times; DeltaBlue's methods).
    has_loop: bool,
    callee: bool,
    ops: usize,
}

impl Scan {
    fn of(f: &crate::asm::FunctionDef) -> Scan {
        let mut s = Scan {
            has_loop: false,
            callee: false,
            ops: 0,
        };
        for op in ops(f) {
            s.has_loop |= op.is_loop();
            // (And what the JIT doesn't compile, newer syntax's.)
            s.callee |= matches!(
                op,
                crate::bytecode::Op::LdaCallee
                    | crate::bytecode::Op::Intrinsic
                    | crate::bytecode::Op::CreateRest
                    | crate::bytecode::Op::CloseUpvalue
                    | crate::bytecode::Op::LdaNewTarget
                    | crate::bytecode::Op::InitialYield
                    | crate::bytecode::Op::CreateMappedArguments
                    | crate::bytecode::Op::AsyncStart
                    | crate::bytecode::Op::Yield
            );
            s.ops += 1;
        }
        s
    }
}

/// The opcodes of `f`'s code.
fn ops(f: &crate::asm::FunctionDef) -> impl Iterator<Item = crate::bytecode::Op> + '_ {
    let mut pc = 0;
    std::iter::from_fn(move || {
        let op = crate::bytecode::Op::from_u8(*f.code.get(pc)?).expect("valid bytecode");
        pc += op.size();
        Some(op)
    })
}

/// See [`Scan::has_loop`]. Measured (perf.sh --ab, against entering loop-free
/// functions after `TIER_CALLS` too): 91 -10%, 90 -5%, 87 -5%; 10 does the
/// same. A higher threshold for functions with loops too cost up to +70%
/// (61, 66, 80, 82: loops in functions called a few hundred times).
const LOOP_FREE_CALLS: i32 = 5;

/// Instructions past which a loop-free function is entered compiled as
/// one with a loop is, after more calls (see [`Scan::has_loop`]).
const ENTRY_COMPILE_SIZE: usize = 64;

/// A function whose registers its closures capture and of more
/// instructions than this: calls never enter compiled code (its loops
/// may). Its compiled code costs more (captured registers in memory,
/// nothing hoisted), and such a function is mostly a driver making
/// closures: TypeScript's checkers (100 to 4,000 instructions), each
/// called a few thousand times, compiled ~10 times each and ran compiled
/// hardly at all; EarleyBoyer's hot ones (`loop2`, `deriv_trees`) have
/// fewer than 100.
const CAPTURING_ENTRY_SIZE: usize = 100;

/// Which of `module`'s functions look recursive: they reach themselves
/// through the names their inline caches look up (`LdaGlobal fib` in
/// `fib`; `isOdd` naming `isEven`, which names `isOdd`). A global loaded
/// only to read a property of it (`LdaGlobal C; Star r; GetNamed r, …`:
/// `C.parent.call(this)` in constructor `C`) isn't a call of it.
fn recursive_functions(module: &Module) -> Vec<bool> {
    use crate::bytecode::Op;
    let fs = &module.functions;
    let by_name: HashMap<&str, usize> = fs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();
    // The inline-cache slots of `f`'s global loads that only feed a
    // property read.
    let property_bases = |f: &crate::asm::FunctionDef| {
        let (code, mut pc, mut bases) = (&f.code, 0, Vec::new());
        let op_at = |pc: usize| code.get(pc).and_then(|&b| Op::from_u8(b));
        while let Some(op) = op_at(pc) {
            let star = pc + op.size();
            if op == Op::LdaGlobal
                && op_at(star) == Some(Op::Star)
                && op_at(star + Op::Star.size()) == Some(Op::GetNamed)
                && code[star + 1] == code[star + Op::Star.size() + 1]
            {
                bases.push(u16::from_le_bytes([code[pc + 1], code[pc + 2]]) as usize);
            }
            pc = star;
        }
        bases
    };
    let edges: Vec<Vec<usize>> = fs
        .iter()
        .map(|f| {
            let bases = property_bases(f);
            (f.ics.iter().enumerate())
                .filter(|(k, _)| !bases.contains(k))
                .filter_map(|(_, n)| by_name.get(&**n).copied())
                .collect()
        })
        .collect();
    (0..fs.len())
        .map(|start| {
            let mut seen = vec![false; fs.len()];
            let mut work = edges[start].clone();
            while let Some(i) = work.pop() {
                if i == start {
                    return true;
                }
                if !std::mem::replace(&mut seen[i], true) {
                    work.extend(&edges[i]);
                }
            }
            false
        })
        .collect()
}

/// `Array(…)` and `new Array(…)`: `Array(n)` has length `n` (holes), any
/// other arguments are the elements.
fn array_constructor(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value> {
    let a = vm.new_array();
    let o = as_object(a);
    let elems: Vec<Value> = match args {
        [n] if n.is_number() => {
            let len = n.as_number().unwrap();
            if len < 0.0 || len.fract() != 0.0 || len > u32::MAX as f64 {
                return Err(Error::range_error("Invalid array length"));
            }
            if vm.density.goes_far(0, len as usize, false) {
                crate::far::make_far(vm, o, len as u32);
                return Ok(a);
            }
            vec![Value::EMPTY; len as usize]
        }
        _ => args.to_vec(),
    };
    unsafe {
        vm.reserve_elems(o, elems.len());
        for (i, &v) in elems.iter().enumerate() {
            // Holes don't type the elements.
            if v == Value::EMPTY {
                *(*o).elems.add(i) = v;
            } else {
                vm.store_element(o, i, v);
            }
        }
        (*o).elems_len = elems.len() as u32;
    }
    Ok(a)
}

/// `Object(…)` and `new Object(…)`: an object argument itself, else a new
/// plain object (there are no wrapper objects yet).
/// Argument `i` as a number (ToNumber), `NaN` if missing.
fn num_arg(args: &[Value], i: usize) -> f64 {
    args.get(i)
        .map_or(f64::NAN, |&v| crate::interp::to_number(v))
}

fn math_log(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(crate::math::log(num_arg(a, 0))))
}
fn math_exp(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(crate::math::exp(num_arg(a, 0))))
}
fn math_sqrt(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(num_arg(a, 0).sqrt()))
}
fn math_abs(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(num_arg(a, 0).abs()))
}
fn math_floor(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(num_arg(a, 0).floor()))
}
fn math_ceil(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(num_arg(a, 0).ceil()))
}
/// `Math.round`: halves round up (`-2.5` is `-2`), unlike Rust's `round`.
fn math_round(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = num_arg(a, 0);
    // (Not `(x + 0.5).floor()`: 0.49999999999999994 + 0.5 rounds to 1.)
    let f = x.floor();
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    // (-0.5 rounds to -0; x + 0.5 is inexact past 2^52, where x is
    // integral already.)
    Ok(Value::number(if x.abs() >= 4503599627370496.0 {
        x
    } else if r == 0.0 && x.is_sign_negative() {
        -0.0
    } else {
        r
    }))
}
fn math_sin(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(crate::math::sin(num_arg(a, 0))))
}
fn math_cos(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(crate::math::cos(num_arg(a, 0))))
}
/// `Math.pow` (fdlibm's, as V8's: `pow(1, NaN)` and `pow(±1, ±∞)` are NaN).
fn math_pow(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::number(crate::math::pow(num_arg(a, 0), num_arg(a, 1))))
}
/// `Math.min`/`max` of all the arguments (NaN if any is; no arguments:
/// `Infinity`/`-Infinity`).
fn math_min(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let mut r = f64::INFINITY;
    let mut nan = false;
    for i in 0..a.len() {
        let x = num_arg(a, i);
        // (Not past one that throws.)
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
        if x.is_nan() {
            // (The rest still converted, for their `valueOf`s' effects.)
            nan = true;
        }
        if x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative()) {
            r = x;
        }
    }
    Ok(Value::number(if nan { f64::NAN } else { r }))
}
fn math_max(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let mut r = f64::NEG_INFINITY;
    let mut nan = false;
    for i in 0..a.len() {
        let x = num_arg(a, i);
        // (Not past one that throws.)
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
        if x.is_nan() {
            // (The rest still converted, for their `valueOf`s' effects.)
            nan = true;
        }
        if x > r || (x == 0.0 && r == 0.0 && r.is_sign_negative()) {
            r = x;
        }
    }
    Ok(Value::number(if nan { f64::NAN } else { r }))
}
/// `Math.random`: xorshift64*, seeded from the clock.
fn math_random(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    thread_local! {
        static STATE: Cell<u64> = Cell::new(
            (crate::date::now_ms() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
        );
    }
    let x = STATE.with(|s| {
        let mut x = s.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    });
    Ok(Value::from_f64((x >> 11) as f64 / (1u64 << 53) as f64))
}

/// `this` as a number for `Number.prototype` methods.
fn this_number(this: Value) -> Result<f64> {
    let n = wrapped(this).filter(|v| v.is_number()).unwrap_or(this);
    n.as_number()
        .ok_or_else(|| Error::type_error("Number.prototype method called on a non-number"))
}

/// `Number.prototype.toFixed(digits)`: rounded to `digits` (0 to 100)
/// decimals, halves away from zero; `ToString` past 1e21.
fn number_to_fixed(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let x = this_number(this)?;
    let f = to_integer_arg(vm, a.first().copied())?;
    if !(0.0..=100.0).contains(&f) {
        return Err(Error::range_error(
            "toFixed() digits argument must be between 0 and 100",
        ));
    }
    let f = f as usize;
    let s = if !x.is_finite() || x.abs() >= 1e21 {
        number_to_string(x)
    } else {
        // n: x × 10^f rounded, ties up (the spec's "larger n"). `{:.*}` is
        // exact but rounds ties to even: at a tie one digit more is exact,
        // and rounds up here.
        let tie = is_tie(x.abs(), -(f as i32));
        let d = digit_values(&format!("{:.*}", f + tie as usize, x.abs()));
        let n = round_digits(&d, d.len() as i32 - tie as i32);
        let digits = format!("{:0>width$}", n, width = f + 1);
        let (int, frac) = digits.split_at(digits.len() - f);
        // (The spec's sign: of x, even when it rounds to 0; -0 has none.)
        let sign = if x < 0.0 { "-" } else { "" };
        if f == 0 {
            format!("{sign}{int}")
        } else {
            format!("{sign}{int}.{frac}")
        }
    };
    let units: Vec<u16> = s.encode_utf16().collect();
    Ok(vm.new_string(&units))
}

/// ToIntegerOrInfinity of an optional argument (its conversion's errors
/// thrown).
fn to_integer_arg(vm: &mut Vm, v: Option<Value>) -> Result<f64> {
    let d = v.map_or(0.0, crate::interp::to_number);
    if vm.conv_thrown {
        return Err(vm.take_conv_error());
    }
    Ok(if d.is_nan() { 0.0 } else { d.trunc() })
}

/// Whether `x` (finite, ≥ 0) is halfway between two multiples of 10^k
/// (2x / 10^k an odd integer): there rounding half up, which `toFixed`,
/// `toPrecision` and `toExponential` want, and Rust's `{:.*}` and `{:.*e}`,
/// exact but ties to even, differ.
pub(crate) fn is_tie(x: f64, k: i32) -> bool {
    let bits = x.to_bits();
    let (exp, frac) = ((bits >> 52) as i32 & 0x7ff, bits & ((1 << 52) - 1));
    // x = m × 2^e
    let (m, e) = if exp == 0 { (frac, -1074) } else { (frac | 1 << 52, exp - 1075) };
    if m == 0 {
        // (0 is a multiple of every 10^k; and `m >> 64` would overflow.)
        return false;
    }
    // 2x = odd × 2^b; 2x × 10^-k = odd × 5^-k × 2^(b - k), odd at b = k
    // when k ≤ 0, and for k > 0 if also 5^k divides odd.
    let t = m.trailing_zeros() as i32;
    let (odd, b) = (m >> t, e + t + 1);
    b == k && (k <= 0 || 5u64.checked_pow(k as u32).is_some_and(|q| odd % q == 0))
}

/// The digits of a formatted number, as values (signs, points and exponents
/// left out: only for the mantissa).
fn digit_values(s: &str) -> Vec<u8> {
    s.bytes().filter(u8::is_ascii_digit).map(|c| c - b'0').collect()
}

/// The integer of `d`'s first `keep` digits, rounded at the next one
/// half up, as a decimal string (`keep` ≤ 0: 0 or 1).
pub(crate) fn round_digits(d: &[u8], keep: i32) -> String {
    if keep < 0 {
        return "0".into();
    }
    let k = keep as usize;
    let mut n: Vec<u8> = (0..k).map(|i| d.get(i).copied().unwrap_or(0)).collect();
    if d.get(k).is_some_and(|&c| c >= 5) {
        let mut i = n.len();
        loop {
            if i == 0 {
                n.insert(0, 1);
                break;
            }
            i -= 1;
            if n[i] == 9 {
                n[i] = 0;
            } else {
                n[i] += 1;
                break;
            }
        }
    }
    let s: String = n.iter().map(|&c| (b'0' + c) as char).collect();
    let s = s.trim_start_matches('0');
    if s.is_empty() { "0".into() } else { s.into() }
}

/// `x`'s `p` significant digits, rounded half up (`x` > 0): the digits and
/// the exponent of the first.
pub(crate) fn significant(x: f64, p: usize) -> (String, i32) {
    let digits = |k: usize| {
        let s = format!("{x:.*e}", k - 1);
        let (m, e) = s.split_once('e').unwrap();
        (m.replace('.', ""), e.parse::<i32>().unwrap())
    };
    let (n, e) = digits(p);
    // A tie is at x's p-th digit, which is the 10^(e - p + 1)s, or the
    // 10^(e - p)s if rounding carried (9.96 to 1.0e1).
    let p32 = p as i32;
    if is_tie(x, e - p32 + 1) || is_tie(x, e - p32) {
        // `p + 1` digits are exact at a tie, and give its exponent.
        let (d, e) = digits(p + 1);
        if is_tie(x, e - p32 + 1) {
            let mut n = round_digits(&digit_values(&d), p32);
            let mut e = e;
            if n.len() > p {
                // (A carry: 9.99… to 10.0….)
                n.truncate(p);
                e += 1;
            }
            return (n, e);
        }
    }
    (n, e)
}

/// `Number.prototype.toPrecision(p)`: `p` (1 to 100) significant digits,
/// in exponent form when the exponent is below -6 or at least `p`.
fn number_to_precision(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let x = this_number(this)?;
    let s = match a.first() {
        None => number_to_string(x),
        Some(&v) if v.is_undefined() => number_to_string(x),
        Some(&v) => {
            let p = to_integer_arg(vm, Some(v))?;
            if !x.is_finite() {
                number_to_string(x)
            } else {
                if !(1.0..=100.0).contains(&p) {
                    return Err(Error::range_error(
                        "toPrecision() argument must be between 1 and 100",
                    ));
                }
                let p = p as usize;
                if x == 0.0 {
                    if p == 1 {
                        "0".into()
                    } else {
                        format!("0.{}", "0".repeat(p - 1))
                    }
                } else {
                    let (n, exp) = significant(x.abs(), p);
                    let sign = if x < 0.0 { "-" } else { "" };
                    if exp < -6 || exp >= p as i32 {
                        let es = if exp >= 0 { format!("+{exp}") } else { exp.to_string() };
                        let m = if p == 1 { n.clone() } else { format!("{}.{}", &n[..1], &n[1..]) };
                        format!("{sign}{m}e{es}")
                    } else if exp >= 0 {
                        let (int, frac) = n.split_at(exp as usize + 1);
                        if frac.is_empty() { format!("{sign}{int}") } else { format!("{sign}{int}.{frac}") }
                    } else {
                        format!("{sign}0.{}{n}", "0".repeat((-exp - 1) as usize))
                    }
                }
            }
        }
    };
    let units: Vec<u16> = s.encode_utf16().collect();
    Ok(vm.new_string(&units))
}

/// `Number.prototype.toString(radix)` (2 to 36, 10 by default).
fn number_to_string_method(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let radix = match a.first() {
        Some(r) if !r.is_undefined() => crate::interp::to_number(*r).trunc(),
        _ => 10.0,
    };
    if !(2.0..=36.0).contains(&radix) {
        return Err(Error::range_error("toString() radix must be between 2 and 36"));
    }
    let units: Vec<u16> = crate::builtins::radix_string(this_number(this)?, radix as u32)
        .encode_utf16()
        .collect();
    Ok(vm.new_string(&units))
}

fn object_constructor(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value> {
    vm.is_construct();
    match args.first() {
        Some(&v) if !v.is_undefined() && !v.is_null() => Ok(object_value(vm.to_object(v)?)),
        _ => {
            let op = object_value(vm.object_prototype);
            Ok(object_value(vm.new_object(op)))
        }
    }
}

/// `Function.prototype` called: `undefined`.
fn function_prototype_fn(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::UNDEFINED)
}

/// The primitive `v` wraps, if it is a wrapper object.
pub(crate) fn wrapped(v: Value) -> Option<Value> {
    if !v.is_cell() {
        return None;
    }
    let o = as_object(v);
    unsafe { ((*o).class == Class::Wrapper).then(|| Value::from_bits((*o).code as u64)) }
}

/// A mapped `arguments` object (see `Vm::mapped_args`).
pub(crate) struct MappedArgs {
    pub(crate) o: *mut Object,
    /// Its frame's registers (`r1…` its parameters).
    pub(crate) regs: *mut Value,
    pub(crate) mapped: Vec<bool>,
    /// A function with a direct `eval` keeps its parameters in its scope
    /// object, by name: that object (null if they are in registers) and
    /// the parameters' names.
    pub(crate) scope: (*mut Object, Vec<Atom>),
}

impl MappedArgs {
    /// Where parameter `i` is in the scope object: it and the slot.
    fn scope_slot(&self, i: usize) -> Option<(*mut Object, u32)> {
        let env = self.scope.0;
        if env.is_null() {
            return None;
        }
        let slot = unsafe { (*(*env).shape).lookup(*self.scope.1.get(i)?) }?;
        Some((env, slot))
    }

    unsafe fn value(&self, i: usize) -> Value {
        unsafe {
            match self.scope_slot(i) {
                Some((env, slot)) => *Object::slot(env, (*(*env).shape).location(slot)),
                None => *self.regs.add(1 + i),
            }
        }
    }
}

/// `Function.prototype.call` on a native: a call through it to a bytecode
/// function is unwrapped where it is made (`interp::unwrap_call`).
fn function_call(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    let (that, rest) = match args {
        [] => (Value::UNDEFINED, args),
        [t, rest @ ..] => (*t, rest),
    };
    if !crate::props::is_callable(this) {
        return Err(Error::type_error("Function.prototype.call called on a non-function"));
    }
    crate::interp::call_function(vm, this, that, rest)
}

/// `this[length++] = v` for each argument (a failed store throws), as
/// `push` does on an array-like object.
fn array_push_generic(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    let this = object_value(vm.to_object(this)?);
    let a_length = vm.a_length;
    let n = crate::interp::to_number(vm.get(this, a_length)?);
    let mut n = if n.is_nan() || n < 0.0 { 0.0 } else { n.trunc().min(9007199254740991.0) };
    if n + args.len() as f64 > 9007199254740991.0 {
        return Err(Error::type_error("Pushing past the largest array-like length"));
    }
    for &v in args {
        let k = vm.property_key(Value::number(n));
        vm.put_key(this, k, v)?;
        n += 1.0;
    }
    vm.put_key(this, Key::Name(a_length), Value::number(n))?;
    Ok(Value::number(n))
}

/// `Array.prototype.push`: [`array_push`], but each element stored with
/// `Set` once a prototype may have a setter or read-only element there
/// (see `Vm::protect_indices`).
fn array_push_method(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    if vm.indices_protected {
        return array_push_generic(vm, this, args);
    }
    array_push(vm, this, args)
}

/// Push onto array `this` (as array literals do: elements defined, not
/// stored through prototypes).
pub(crate) fn array_push(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    if !this.is_cell() || unsafe { (*as_object(this)).class } != Class::Array {
        return array_push_generic(vm, this, args);
    }
    let o = as_object(this);
    unsafe {
        if (*o).flags & !(crate::object::EXTERNAL | PROTOTYPE) != 0 {
            return array_push_flagged(vm, this, args);
        }
        let len = (*o).elems_len as usize;
        vm.reserve_elems(o, len + args.len());
        for (i, &v) in args.iter().enumerate() {
            vm.store_element(o, len + i, v);
        }
        (*o).elems_len = (len + args.len()) as u32;
        Ok(Value::from_i32((*o).elems_len as i32))
    }
}

/// `push` onto an array with elements that have attributes, or that
/// isn't extensible: through the generic stores, which throw where they
/// can't store.
#[cold]
fn array_push_flagged(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    let o = as_object(this);
    for &v in args {
        let n = crate::far::array_length(o);
        vm.put_key(this, Key::Index(n), v)?;
    }
    let n = crate::far::array_length(o);
    let a_length = vm.a_length;
    let len = Value::number(n as f64);
    vm.put_key(this, Key::Name(a_length), len)?;
    Ok(len)
}

/// `String(v)` for the values that exist so far.
pub fn display(v: Value) -> String {
    if v.is_int32() {
        return v.as_i32_unchecked().to_string();
    }
    if let Some(d) = v.as_number() {
        return number_to_string(d);
    }
    if v.is_undefined() {
        "undefined".into()
    } else if v.is_null() {
        "null".into()
    } else if v.is_bool() {
        (v == Value::TRUE).to_string()
    } else if let Some(s) = string_units(v) {
        String::from_utf16_lossy(s)
    } else if crate::symbol::is_symbol(v) {
        String::from_utf16_lossy(&crate::symbol::symbol_string(v))
    } else if crate::bigint::is_bigint(v) {
        crate::bigint::to_string(v)
    } else if v.is_cell() {
        "[object Object]".into()
    } else {
        format!("{v:?}")
    }
}

/// `v`'s code units if it is a string.
pub fn string_units<'a>(v: Value) -> Option<&'a [u16]> {
    if !v.is_cell() {
        return None;
    }
    let o = as_object(v);
    unsafe {
        if (*o).class != Class::String {
            return None;
        }
        if (*o).elems.is_null() {
            flatten(o);
        }
        Some(std::slice::from_raw_parts((*o).elems.cast::<u16>(), (*o).elems_len as usize))
    }
}

/// The array index `units` spell canonically (`"7"`, not `"07"`), below
/// 2^32 - 1.
fn canonical_index(units: &[u16]) -> Option<u32> {
    if units.is_empty() || units.len() > 10 || (units[0] == b'0' as u16 && units.len() > 1) {
        return None;
    }
    let mut n: u64 = 0;
    for &c in units {
        if !(b'0' as u16..=b'9' as u16).contains(&c) {
            return None;
        }
        n = n * 10 + (c - b'0' as u16) as u64;
    }
    (n < u32::MAX as u64).then_some(n as u32)
}

/// A string's length without flattening it (see [`Vm::new_cons`]).
pub(crate) fn string_len(v: Value) -> Option<usize> {
    if !v.is_cell() {
        return None;
    }
    let o = as_object(v);
    unsafe { ((*o).class == Class::String).then(|| (*o).elems_len as usize) }
}

/// The most code units a string made by concatenation has.
pub(crate) const MAX_STRING_LEN: usize = 1 << 30;

/// Make the cons string `o` (see [`Vm::new_cons`]) flat: its units copied
/// out of its pieces (iteratively: `s += c` makes a chain as long as the
/// string), the pieces dropped.
#[cold]
#[inline(never)]
unsafe fn flatten(o: *mut Object) {
    unsafe {
        let vm = &mut *crate::interp::current_vm();
        let len = (*o).elems_len as usize;
        // Padded to whole words, and one more (copies read past the end).
        let buf = vm.heap.alloc_bytes(8 * (len.div_ceil(4) + 1)).cast::<u16>();
        let mut at = 0;
        let mut stack: Vec<*mut Object> = vec![(*o).code as *mut Object, (*o).props.cast::<Object>()];
        while let Some(s) = stack.pop() {
            if (*s).elems.is_null() {
                stack.push((*s).code as *mut Object);
                stack.push((*s).props.cast::<Object>());
            } else {
                let n = (*s).elems_len as usize;
                std::ptr::copy_nonoverlapping((*s).elems.cast::<u16>(), buf.add(at), n);
                at += n;
            }
        }
        debug_assert_eq!(at, len);
        (*o).elems = buf.cast();
        crate::heap::barrier_young(o.cast(), &raw const (*o).elems);
        (*o).props = std::ptr::null_mut();
        (*o).code = std::ptr::null();
    }
}

/// Code units as source text for the front end or the regular expression
/// parser, which take Rust strings: a lone surrogate as the escape
/// `\uXXXX`, which means it in string literals, template literals' cooked
/// values and patterns (not U+FFFD, as a lossy conversion would make it).
pub(crate) fn source_text(u: &[u16]) -> String {
    let mut s = String::with_capacity(u.len());
    for c in char::decode_utf16(u.iter().copied()) {
        match c {
            Ok(c) => s.push(c),
            Err(e) => s.push_str(&format!("\\u{:04X}", e.unpaired_surrogate())),
        }
    }
    s
}

/// Number::toString(10): the shortest digits that round-trip, laid out as
/// ECMA-262 says (no exponent from 1e-6 up to 1e21).
pub fn number_to_string(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d == 0.0 {
        return "0".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // An integer below 2^53: its own digits are the shortest.
    if d.fract() == 0.0 && d.abs() < 9007199254740992.0 {
        return (d as i64).to_string();
    }
    if d < 0.0 {
        return format!("-{}", number_to_string(-d));
    }
    // `{:e}` gives the shortest round-trip digits, `d1.d2…dke<x>`, but
    // where two k-digit strings are equally close to `d` (2^-25 is
    // 2.98023223876953125e-8 exactly) it rounds up, and ECMA-262 wants the
    // even one. `{:.*e}` rounds `d` to k digits exactly, ties to even: the
    // closest k-digit string. It is the one to print if it round-trips;
    // it may not (2^-24 is 5.9604644775390625e-8, and only ...063e-8 reads
    // back as it: the interval of a power of two is narrower below), and
    // then the shortest string is the closest that does.
    let shortest = format!("{d:e}");
    let k = shortest.split_once('e').unwrap().0.chars().filter(|c| c.is_ascii_digit()).count();
    let closest = format!("{d:.*e}", k - 1);
    let e = if closest != shortest && closest.parse::<f64>() == Ok(d) { closest } else { shortest };
    let (mantissa, exp) = e.split_once('e').unwrap();
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0').to_owned();
    let k = digits.len() as i32;
    // d = 0.d1…dk × 10^n
    let n = exp.parse::<i32>().unwrap() + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let rest = if k > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!("{}{rest}e{sign}{}", &digits[..1], (n - 1).abs())
    }
}
