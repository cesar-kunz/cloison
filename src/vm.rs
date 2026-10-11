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
use crate::object::{
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

/// An enclosing function's variable as a closure sees it (Lua's design):
/// while that function's frame is live, `at` points to its register
/// (open: the function itself reads and writes the register as any
/// other); when the frame ends, the value moves to `closed` and `at`
/// points there. Closures of one frame share it. It is the payload of an
/// upvalue cell (`heap::UPVALUE`): a pointer to it points past the cell's
/// header word.
#[repr(C)]
pub struct Upvalue {
    pub at: *mut Value,
    pub closed: Value,
}

/// A function implemented in Rust: `(vm, this, args)`.
pub type NativeFn = fn(&mut Vm, Value, &[Value]) -> Result<Value>;

/// One way of an inline-cache slot. A load hits when the receiver's shape is `shape`:
/// the value is at `loc` in the receiver, or, if `holder` is set, in that
/// prototype, provided no prototype has changed shape since (`epoch`). A
/// store hits on `shape` too, and moves the object to `new_shape` first if
/// set (a cached transition, always to an in-object slot). For a store,
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

/// An elements' type code (`Shape::elems_code`) that no value fits: not a
/// type code (0–2) nor a shape's address.
pub const NO_ELEMENTS: u64 = 3;

/// Record in keyed-access cache `ic` that it met `o`'s bytes (a string's,
/// a typed array's or a buffer's: `BYTES_LOC`), and for the JIT a typed
/// array's element kind plus one in `epoch` (`u32::MAX` once two kinds or
/// anything else came). A cache holding a shape keeps it.
///
/// # Safety
/// `ic` must be a live cache slot and `o` a live object.
pub(crate) unsafe fn note_bytes(ic: *mut Ic, o: *mut Object) {
    unsafe {
        let kind = if (*o).class == Class::TypedArray {
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
enum Key {
    Index(u32),
    Name(Atom),
}

/// `Ic::loc` of a call's cache whose callees are what `f.call(…)` called,
/// `f` being `Function.prototype.call` (see `interp::unwrap_call`): the JIT
/// compiles such a call as a call of them.
pub const VIA_CALL: i32 = 1;

/// `Ic::loc` of a `GetNamed` that read an array's `length`.
pub const LENGTH_LOC: i32 = i32::MIN;
/// How far past twice its elements' length an integer key of an object
/// that isn't an array makes it sparse (see `object::SPARSE`).
const SPARSE_GAP: usize = 1024;
/// `Ic::loc` of a `GetKeyed` that read a typed array's, a buffer's or a
/// string's elements (no shape: the interpreter's hit path never takes it;
/// compiled code reads them through the runtime).
pub const BYTES_LOC: i32 = i32::MIN + 1;
const _: fn(u64, Value) -> bool = field_fits;

/// The first `new` of `code` returned: shrink the capacity of its objects'
/// shapes, all reached from its private root, to the most properties one
/// has (at least 1), and give later objects that. Every
/// slot of those objects is in the object (they had room for more), below
/// that count, so it stays where it is.
fn settle(code: &Code) {
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
    let root = code.ctor_shape.get();
    if root.is_null() {
        return;
    }
    // Exactly the most properties any has: objects that get more later
    // keep those out of line, but every object is smaller (with no
    // collector, memory is fresh pages the kernel zeroes).
    let mut most = 1;
    walk(root, &mut |s| most = most.max(s.keys.len() as u32));
    if most < unsafe { (*root).capacity.get() } {
        walk(root, &mut |s| s.capacity.set(most));
    }
    code.ctor_capacity.set(unsafe { (*root).capacity.get() });
}

/// Cache `e` in the slot whose way 0 is at `ic`: in the way caching its
/// shape if one does (it may be stale: a field generalized since, or a
/// prototype changed), else in the first free way. With every way taken
/// (the site is megamorphic), `e` replaces way 0.
unsafe fn install(ic: *mut Ic, e: Ic) {
    unsafe {
        for k in 0..IC_WAYS {
            let w = &mut *ic.add(k);
            if w.shape.is_null() || w.shape == e.shape {
                *w = e;
                return;
            }
        }
        *ic = e;
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

/// A function's code, shared by its closures.
pub struct Code {
    pub name: String,
    pub bytecode: Vec<u8>,
    pub nparams: u32,
    /// Register count, `r0` (`this`) and the parameters included.
    pub nregs: u32,
    pub ics: Box<[UnsafeCell<[Ic; IC_WAYS]>]>,
    /// The property name of each inline-cache slot.
    pub ic_names: Box<[Atom]>,
    /// The nested functions, by `CreateClosure` operand.
    pub closures: Vec<*const Code>,
    /// `LdaConstant`'s values.
    pub constants: Box<[Value]>,
    /// Exception handlers (see `FunctionDef::handlers`). A function with
    /// any runs only in the interpreter, which unwinds into them
    /// (`interp::unwind`): it isn't compiled, inlined or called directly.
    pub handlers: Box<[(u32, u32, u32)]>,
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
    /// Compiled code, once a loop or the function got hot (see
    /// `jit::vm_run`).
    pub(crate) jit: RefCell<Option<Box<crate::jit::JitFunction>>>,
    /// Calls left before calls enter compiled code (below zero: they do;
    /// `i32::MAX`: never).
    pub(crate) calls_left: Cell<i32>,
    /// Calls never enter compiled code, although loops may: its start is
    /// refused for good (see `jit::vm_run`).
    pub(crate) calls_interpret: Cell<bool>,
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
}

impl Code {
    /// The inline-cache slot `i` (its way 0, the others following), and
    /// the name it looks up.
    pub fn ic(&self, i: u16) -> (*mut Ic, Atom) {
        (self.ics[i as usize].get().cast(), self.ic_names[i as usize])
    }
}

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
/// Stack size in values (frame headers included).
const STACK_VALUES: usize = 1 << 18;

// Shapes and code are boxed so their addresses stay put as the vectors grow.
#[allow(clippy::vec_box)]
pub struct Vm {
    pub heap: Heap,
    shapes: Vec<Box<Shape>>,
    field_cells: Vec<Box<FieldCell>>,
    roots: HashMap<(Class, u64, u32), *const Shape>,
    /// Each atom's name, once (the map's key shares it).
    atoms: Vec<std::rc::Rc<str>>,
    atom_ids: HashMap<std::rc::Rc<str>, Atom>,
    codes: Vec<Box<Code>>,
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
    /// Bumped whenever a field's type generalizes (see [`FieldType`]).
    pub field_epoch: u32,
    /// Bumped with either: compiled code checks it after calling out, as
    /// the callee may have invalidated what it assumes.
    pub code_epoch: u32,
    pub(crate) error: Option<Error>,
    pub(crate) stack: Box<[Value]>,
    pub(crate) stack_end: *mut Value,
    /// Loop back edges a new frame runs in the interpreter before tiering
    /// up into the JIT: `TOI_TIER` if set, `i64::MAX` (never) with
    /// `TOI_NO_JIT` set.
    pub(crate) frame_budget: i64,
    /// Functions are compiled warm first (see `jit::Tier`): off by
    /// default.
    pub(crate) warm_tier: bool,
    /// Calls of a function before calls to it enter compiled code (see
    /// `Code::calls_left`); `i32::MAX` with `TOI_NO_JIT`.
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
    /// The open upvalues (see [`Upvalue`]), by the address of the register
    /// they point to, lowest first.
    pub(crate) open_upvalues: Vec<*mut Upvalue>,
    /// Values Rust code holds in heap containers across a call into JS
    /// (see [`Vm::handle_scope`]): roots.
    pub(crate) handles: Vec<Value>,
    /// The most values a frame spans from its registers: its own and, for
    /// compiled code, its inlined frames' (the collector scans the VM stack
    /// that far past the deepest frame's registers).
    pub(crate) max_frame: usize,
    pub(crate) function_shape: *const Shape,
    /// The root shape of arrays (cached: `root_shape` hashes).
    pub(crate) array_shape: *const Shape,
    proto_object_shape: *const Shape,
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
    /// The `Array` constructor: compiled code makes `new Array()` itself.
    pub(crate) array_fn: *mut Object,
    date_prototype: *mut Object,
    pub(crate) number_prototype: *mut Object,
    /// Each typed array kind's prototype (`TypedKind` order), and
    /// `ArrayBuffer.prototype`.
    pub(crate) typed_prototypes: Vec<*mut Object>,
    pub(crate) regexp_prototype: *mut Object,
    pub(crate) boolean_prototype: *mut Object,
    /// Loaded scripts' `var` and function names (see `Module::declared`).
    declared: std::collections::HashSet<Atom>,
    /// `Function.prototype.bind`'s JS helper, once made.
    pub(crate) bind_helper: Value,
    /// Compiled regular expressions, by pattern and flags (a literal makes
    /// a new object each time it runs, sharing one).
    pub(crate) regexps: HashMap<(String, String), Box<crate::regexp::Regex>>,
    /// The same, by a literal's pattern and flags constants (a `Value`'s
    /// bits each, kept by its `Code::constants`).
    pub(crate) regexp_literals: HashMap<(u64, u64), *const crate::regexp::Regex>,
    /// Captures for `exec`, `test`, `match` and `replace` to match into
    /// (taken while in use).
    pub(crate) caps_buf: crate::regexp::Captures,
    /// `exec` arrays' shapes: from an array's, with `index` and with
    /// `input` added (a few, by the elements' kind).
    pub(crate) match_shapes: Vec<(*const Shape, *const Shape, *const Shape)>,
    /// Names of `RegExp` objects' and `exec` results' properties.
    pub(crate) a_last_index: Atom,
    pub(crate) a_source: Atom,
    pub(crate) a_global: Atom,
    pub(crate) a_ignore_case: Atom,
    pub(crate) a_multiline: Atom,
    pub(crate) a_index: Atom,
    pub(crate) a_input: Atom,
    pub(crate) array_buffer_prototype: *mut Object,
    /// `Error.prototype` and its kinds' (see `builtins::ERROR_KINDS`).
    pub(crate) error_prototypes: Vec<*mut Object>,
    /// Strings of one code unit below 256, made on first use.
    single_chars: Vec<Value>,
    pub(crate) a_to_string: Atom,
    pub(crate) a_value_of: Atom,
    /// `Array.prototype.push` and `pop`, which the JIT compiles inline.
    pub(crate) array_push_fn: *mut Object,
    pub(crate) array_pop_fn: *mut Object,
    a_prototype: Atom,
    a_constructor: Atom,
    a_length: Atom,
    /// What the JIT worked out about functions' bytecode, for every
    /// function's compilations.
    pub(crate) jit_caches: crate::jit::Caches,
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
            field_cells: Vec::new(),
            roots: HashMap::default(),
            atoms: Vec::new(),
            atom_ids: HashMap::default(),
            codes: Vec::new(),
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
            relied_slots: Default::default(),
            jitted: Vec::new(),
            field_deps: HashMap::default(),
            field_epoch: 0,
            code_epoch: 0,
            error: None,
            stack,
            stack_end,
            warm_tier: std::env::var_os("TOI_WARM").is_some(),
            frame_budget: if std::env::var_os("TOI_NO_JIT").is_some() {
                i64::MAX
            } else {
                std::env::var("TOI_TIER")
                    .ok()
                    .and_then(|n| n.parse::<i64>().ok())
                    .map_or(TIER_BACK_EDGES, |n| n.max(1))
            },
            tier_calls: 0,
            osr_regs: std::ptr::null_mut(),
            native_limit: 0,
            redo_call: false,
            open_upvalues: Vec::new(),
            handles: Vec::new(),
            max_frame: crate::interp::HEADER,
            function_shape: std::ptr::null(),
            array_shape: std::ptr::null(),
            proto_object_shape: std::ptr::null(),
            function_call: std::ptr::null(),
            function_call_fn: std::ptr::null_mut(),
            function_apply: std::ptr::null(),
            pending_args: (std::ptr::null(), 0),
            native_top: std::ptr::null_mut(),
            array_fn: std::ptr::null_mut(),
            date_prototype: std::ptr::null_mut(),
            number_prototype: std::ptr::null_mut(),
            error_prototypes: Vec::new(),
            typed_prototypes: Vec::new(),
            regexp_prototype: std::ptr::null_mut(),
            boolean_prototype: std::ptr::null_mut(),
            bind_helper: Value::UNDEFINED,
            declared: Default::default(),
            regexps: HashMap::default(),
            regexp_literals: HashMap::default(),
            caps_buf: Vec::new(),
            match_shapes: Vec::new(),
            a_last_index: Atom(0),
            a_source: Atom(0),
            a_global: Atom(0),
            a_ignore_case: Atom(0),
            a_multiline: Atom(0),
            a_index: Atom(0),
            a_input: Atom(0),
            array_buffer_prototype: std::ptr::null_mut(),
            single_chars: Vec::new(),
            a_to_string: Atom(0),
            a_value_of: Atom(0),
            array_push_fn: std::ptr::null_mut(),
            array_pop_fn: std::ptr::null_mut(),
            a_prototype: Atom(0),
            a_constructor: Atom(0),
            a_length: Atom(0),
        };
        let budget = vm.frame_budget;
        vm.set_tier_back_edges(budget);
        vm.a_prototype = vm.intern("prototype");
        vm.a_constructor = vm.intern("constructor");
        vm.a_length = vm.intern("length");
        vm.a_to_string = vm.intern("toString");
        vm.a_value_of = vm.intern("valueOf");
        vm.a_last_index = vm.intern("lastIndex");
        vm.a_source = vm.intern("source");
        vm.a_global = vm.intern("global");
        vm.a_ignore_case = vm.intern("ignoreCase");
        vm.a_multiline = vm.intern("multiline");
        vm.a_index = vm.intern("index");
        vm.a_input = vm.intern("input");

        vm.object_prototype = vm.new_object_with(Class::Plain, Value::NULL, 4);
        let op = object_value(vm.object_prototype);
        vm.function_prototype = vm.new_object(op);
        vm.array_prototype = vm.new_object(op);
        vm.string_prototype = vm.new_object(op);
        vm.missing_holder = vm.new_object_with(Class::Plain, Value::NULL, 1);
        // Room for the builtins and the script's globals in the object
        // itself: out-of-line globals cost compiled code a load each, and
        // aren't folded as constants. (64 slots, the builtins took them
        // all: DeltaBlue +20% instructions.)
        vm.global = vm.new_object_with(Class::Plain, op, GLOBAL_CAPACITY);

        let fp = object_value(vm.function_prototype);
        let root = vm.root_shape(Class::Function, fp, 2);
        // Functions' `prototype` and prototypes' `constructor` aren't
        // enumerable.
        vm.function_shape = vm.transition_with(root, vm.a_prototype, DONT_ENUM);
        let root = vm.root_shape(Class::Plain, op, INITIAL_CTOR_CAPACITY);
        vm.proto_object_shape = vm.transition_with(root, vm.a_constructor, DONT_ENUM);

        vm.array_shape = vm.root_shape(Class::Array, object_value(vm.array_prototype), 0);
        vm.string_shape = vm.root_shape(Class::String, object_value(vm.string_prototype), 0);
        // No value fits a string's "elements" (its code units): element
        // stores' fast paths never take one.
        unsafe { (*vm.string_shape.cast_mut()).elems_code = NO_ELEMENTS };
        let ap = vm.array_prototype;
        vm.array_push_fn = vm.define_native(ap, "push", array_push, 1, crate::jit::Ty::I32, false);
        vm.array_pop_fn = vm.define_native(ap, "pop", array_pop, 0, crate::jit::Ty::Any, false);
        let fp = vm.function_prototype;
        let call = vm.define_native(fp, "call", function_call, 1, crate::jit::Ty::Any, false);
        vm.function_call = unsafe { (*call).code };
        vm.function_call_fn = call;
        // The constructors, with `prototype` and `constructor` links.
        let g = vm.global;
        for (name, v) in [("NaN", f64::NAN), ("Infinity", f64::INFINITY)] {
            let a = vm.intern(name);
            vm.add_property_with(
                g,
                a,
                Value::from_f64(v),
                READ_ONLY | DONT_ENUM | DONT_DELETE,
            );
        }
        vm.number_prototype = vm.new_object(op);
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
            0,
            crate::jit::Ty::Any,
            false,
        );
        vm.define_native(ap, "indexOf", array_index_of, 1, crate::jit::Ty::I32, false);
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
        for (name, f, n) in [
            ("log", math_log as NativeFn, 1),
            ("exp", math_exp, 1),
            ("sqrt", math_sqrt, 1),
            ("abs", math_abs, 1),
            ("floor", math_floor, 1),
            ("ceil", math_ceil, 1),
            ("round", math_round, 1),
            ("sin", math_sin, 1),
            ("cos", math_cos, 1),
            ("pow", math_pow, 2),
            ("min", math_min, 2),
            ("max", math_max, 2),
            ("random", math_random, 0),
        ] {
            vm.define_native(math, name, f, n, crate::jit::Ty::Any, false);
        }
        vm.date_prototype = vm.new_object(op);
        let dp = vm.date_prototype;
        vm.define_native(dp, "getTime", date_get_time, 0, crate::jit::Ty::Any, false);
        vm.define_native(dp, "valueOf", date_get_time, 0, crate::jit::Ty::Any, false);
        for (name, f, proto) in [
            ("Date", date_constructor as NativeFn, vm.date_prototype),
            ("Array", array_constructor as NativeFn, vm.array_prototype),
            (
                "Object",
                object_constructor as NativeFn,
                vm.object_prototype,
            ),
        ] {
            let c = vm.define_native(g, name, f, 0, crate::jit::Ty::Any, true);
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
            if name == "Date" {
                vm.define_native(c, "now", date_now, 0, crate::jit::Ty::Any, false);
            }
            if name == "Object" {
                let f = object_define_property as NativeFn;
                vm.define_native(c, "defineProperty", f, 3, crate::jit::Ty::Any, false);
            }
        }
        crate::builtins::install(&mut vm);
        vm
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

    fn add_shape(&mut self, s: Shape) -> *const Shape {
        // (A Rust structure: its prototype is logged if young.)
        self.heap.note(s.proto);
        let mut b = Box::new(s);
        let p: *const Shape = &*b;
        if b.family.is_null() {
            b.family = p;
        }
        self.shapes.push(b);
        p
    }

    /// The shared root shape for `class`, `proto` and `capacity`. Marks
    /// `proto` as a prototype.
    pub(crate) fn root_shape(&mut self, class: Class, proto: Value, capacity: u32) -> *const Shape {
        let key = (class, proto.to_bits(), capacity);
        if let Some(&s) = self.roots.get(&key) {
            return s;
        }
        if proto.is_cell() {
            self.make_prototype(as_object(proto));
        }
        let s = self.add_shape(Shape {
            class,
            proto,
            capacity: Cell::new(capacity),
            keys: Vec::new(),
            attrs: Vec::new(),
            fields: Vec::new(),
            elems: FieldType::None,
            // (No value fits a typed array's or buffer's bytes, so stores'
            // fast paths never take them.)
            elems_code: if matches!(class, Class::Array | Class::TypedArray | Class::ArrayBuffer) {
                NO_ELEMENTS
            } else {
                0
            },
            family: std::ptr::null(),
            push: Cell::new(std::ptr::null_mut()),
            unique: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
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
            keys: Vec::new(),
            attrs: Vec::new(),
            fields: Vec::new(),
            elems: FieldType::None,
            elems_code: 0,
            family: std::ptr::null(),
            push: Cell::new(std::ptr::null_mut()),
            unique: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
        })
    }

    /// `shape` plus property `name` with attributes `attrs`. Shared shapes
    /// share transitions; unique shapes get a fresh unique successor.
    /// `v` and `x`, allocated at its final size (a clone and a push
    /// reallocates).
    fn grown<T: Copy>(v: &[T], x: T) -> Vec<T> {
        let mut w = Vec::with_capacity(v.len() + 1);
        w.extend_from_slice(v);
        w.push(x);
        w
    }

    pub(crate) fn transition_with(
        &mut self,
        shape: *const Shape,
        name: Atom,
        attrs: u8,
    ) -> *const Shape {
        let s = unsafe { &*shape };
        if !s.unique
            && let Some(&(_, _, t)) = s
                .transitions
                .borrow()
                .iter()
                .find(|&&(n, a, _)| n == name && a == attrs)
        {
            return t;
        }
        let keys = Self::grown(&s.keys, name);
        let slot_attrs = Self::grown(&s.attrs, attrs);
        let cell = Box::new(FieldCell::new());
        let fields = Self::grown(&s.fields, &*cell as *const FieldCell);
        self.field_cells.push(cell);
        let t = self.add_shape(Shape {
            class: s.class,
            proto: s.proto,
            capacity: Cell::new(s.capacity.get()),
            keys,
            attrs: slot_attrs,
            fields,
            elems: s.elems,
            elems_code: s.elems_code,
            family: std::ptr::null(),
            push: Cell::new(std::ptr::null_mut()),
            unique: s.unique,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
        });
        if !s.unique {
            self.shape_left_stable(s);
            s.transitions.borrow_mut().push((name, attrs, t));
        }
        t
    }

    /// Flag `o` as a prototype and give it a unique shape, so that no
    /// inline cache can treat a property added to it as an ordinary
    /// transition (which would skip the epoch bump).
    fn make_prototype(&mut self, o: *mut Object) {
        unsafe {
            if (*o).flags & PROTOTYPE != 0 {
                return;
            }
            (*o).flags |= PROTOTYPE;
            let s = &*(*o).shape;
            // The object leaves `s` (for a copy of its own).
            self.shape_left_stable(s);
            let u = Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                keys: s.keys.clone(),
                attrs: s.attrs.clone(),
                fields: s.fields.clone(),
                elems: s.elems,
                elems_code: s.elems_code,
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: true,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
            relied: Cell::new(false),
            };
            (*o).shape = self.add_shape(u);
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

    /// Add the property that shape `new`, a transition from `o`'s, adds,
    /// with `value`: its slot.
    pub(crate) fn add_property_shaped(&mut self, o: *mut Object, new: *const Shape, value: Value) -> u32 {
        unsafe {
            let ns = &*new;
            let slot = ns.keys.len() as u32 - 1;
            if slot >= ns.capacity.get() {
                let i = slot - ns.capacity.get();
                if i >= (*o).props_cap {
                    let cap = ((*o).props_cap * 2).max(4);
                    let p = self.heap.alloc_values(cap as usize);
                    if !(*o).props.is_null() {
                        std::ptr::copy_nonoverlapping((*o).props, p, (*o).props_cap as usize);
                    }
                    (*o).props = p;
                    crate::heap::barrier_young(o.cast(), &raw const (*o).props);
                    (*o).props_cap = cap;
                }
            }
            (*o).shape = new;
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
    fn shape_left_stable(&mut self, s: &Shape) {
        if s.relied.replace(false) {
            self.field_epoch = self.field_epoch.wrapping_add(1);
            self.bump_code_epoch();
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
            let cell = &*shape.fields[slot as usize];
            let at = Object::slot(o, shape.location(slot));
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
            let cell = &*shape.fields[slot as usize];
            let old = cell.get();
            let new = old.join(FieldType::of(value));
            if new != old {
                cell.set(new);
                if cell.take_relied() {
                    self.field_generalized(cell, old);
                }
            }
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
    /// after its calls); no code relies on an empty field's.
    #[cold]
    #[inline(never)]
    fn field_generalized(&mut self, cell: &FieldCell, old: FieldType) {
        let deps = self.field_deps.remove(&(cell as *const FieldCell as usize));
        if old != FieldType::None {
            for code in deps.into_iter().flatten() {
                let c = unsafe { &*(code as *const Code) };
                c.field_deps.set(c.field_deps.get().wrapping_add(1));
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
    /// transitions; unique ones get a fresh unique successor.
    pub(crate) fn elements_transition(
        &mut self,
        shape: *const Shape,
        elems: FieldType,
    ) -> *const Shape {
        let s = unsafe { &*shape };
        if !s.unique
            && let Some(&(_, t)) = s
                .elem_transitions
                .borrow()
                .iter()
                .find(|(e, _)| *e == elems)
        {
            return t;
        }
        let t = self.add_shape(Shape {
            class: s.class,
            proto: s.proto,
            capacity: Cell::new(s.capacity.get()),
            keys: s.keys.clone(),
            attrs: s.attrs.clone(),
            fields: s.fields.clone(),
            elems,
            elems_code: elems.encode(),
            family: s.family,
            push: Cell::new(std::ptr::null_mut()),
            unique: s.unique,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
        });
        if !s.unique {
            self.shape_left_stable(s);
            s.elem_transitions.borrow_mut().push((elems, t));
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
                        let c = s.push_cache();
                        c.to.rotate_right(1);
                        c.to[0] = (crate::object::push_key(value), t as u64);
                    } else if elems == FieldType::Any {
                        s.push_cache().any = t as u64;
                    }
                }
                (*o).shape = t;
            }
            *(*o).elems.add(i) = value;
            crate::heap::barrier((*o).elems.cast::<u8>().sub(8), (*o).elems.add(i), value);
        }
    }

    /// The object holding `name` on `o`'s prototype chain (`o` included),
    /// and its slot there.
    pub(crate) fn lookup(&self, o: *mut Object, name: Atom) -> Option<(*mut Object, u32)> {
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
        unsafe {
            if name == self.a_length
                && matches!((*o).class, Class::Array | Class::String | Class::TypedArray)
            {
                return Ok(Value::from_i32((*o).elems_len as i32));
            }
            Ok(match self.lookup(o, name) {
                Some((h, slot)) if self.is_accessor(h, slot) => {
                    let pair = *Object::slot(h, (*(*h).shape).location(slot));
                    return self.call_getter(pair, obj);
                }
                Some((h, slot)) => *Object::slot(h, (*(*h).shape).location(slot)),
                None => Value::UNDEFINED,
            })
        }
    }

    fn is_accessor(&self, h: *mut Object, slot: u32) -> bool {
        unsafe { (&(*(*h).shape).attrs)[slot as usize] & ACCESSOR != 0 }
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
            let old = s.lookup(name).filter(|&k| s.attrs[k as usize] & ACCESSOR != 0);
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

    fn get_primitive(&self, obj: Value, name: Atom) -> Result<Value> {
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
            return Ok(match self.lookup(o, name) {
                Some((h, slot)) => unsafe { *Object::slot(h, (*(*h).shape).location(slot)) },
                None => Value::UNDEFINED,
            });
        }
        Ok(Value::UNDEFINED)
    }

    /// `obj[name] = value`.
    pub fn set(&mut self, obj: Value, name: Atom, value: Value) -> Result<()> {
        if !obj.is_cell() {
            return self.set_primitive(obj, name);
        }
        let o = as_object(obj);
        unsafe {
            if (*o).class == Class::Array && name == self.a_length {
                return self.set_array_length(o, value);
            }
            // A string is a primitive: stores to it do nothing.
            if (*o).class == Class::String || self.read_only(o, name) {
                return Ok(());
            }
            if let Some(pair) = self.accessor(o, name) {
                return self.call_setter(pair, obj, value);
            }
            match (*(*o).shape).lookup(name) {
                Some(slot) => self.store_field(o, slot, value),
                // No setters on the chain yet.
                None => {
                    self.add_property(o, name, value);
                }
            }
        }
        Ok(())
    }

    fn set_primitive(&self, obj: Value, name: Atom) -> Result<()> {
        if obj.is_undefined() || obj.is_null() {
            return Err(Error::type_error(format!(
                "Cannot set properties of {obj:?} (setting '{}')",
                self.atom_name(name)
            )));
        }
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
                return Ok(Value::from_i32((*o).elems_len as i32));
            }
            // Absent: cached as a load from `missing_holder`'s `undefined`
            // slot, valid (like a prototype's property) while no prototype
            // changes shape.
            let Some((h, slot)) = self.lookup(o, name) else {
                install(
                    ic,
                    Ic {
                        shape: (*o).shape,
                        holder: self.missing_holder,
                        new_shape: std::ptr::null(),
                        loc: crate::object::INLINE_BASE as i32,
                        epoch: self.proto_epoch,
                    },
                );
                return Ok(Value::UNDEFINED);
            };
            let loc = (*(*h).shape).location(slot);
            // A getter's: called, never cached.
            if self.is_accessor(h, slot) {
                return self.call_getter(*Object::slot(h, loc), obj);
            }
            self.heap.note(object_value(h));
            install(
                ic,
                Ic {
                    shape: (*o).shape,
                    holder: if h == o { std::ptr::null_mut() } else { h },
                    new_shape: std::ptr::null(),
                    loc,
                    epoch: self.proto_epoch,
                },
            );
            Ok(*Object::slot(h, loc))
        }
    }

    /// `SetNamed` miss: store `obj.name = value` and cache how, if possible.
    pub fn set_named_miss(&mut self, code: &Code, ic: u16, obj: Value, value: Value) -> Result<()> {
        code.ic_misses.set(code.ic_misses.get().wrapping_add(1));
        let (ic, name) = code.ic(ic);
        if !obj.is_cell() {
            return self.set_primitive(obj, name);
        }
        let o = as_object(obj);
        unsafe {
            let old = (*o).shape;
            if (*o).class == Class::Array && name == self.a_length {
                return self.set_array_length(o, value);
            }
            // Sloppy mode: a store to a read-only property, own or
            // inherited, does nothing (and isn't cached, so it stays slow).
            if (*o).class == Class::String || self.read_only(o, name) {
                return Ok(());
            }
            // A setter's: called, never cached.
            if let Some(pair) = self.accessor(o, name) {
                return self.call_setter(pair, obj, value);
            }
            if let Some(slot) = (*old).lookup(name) {
                self.store_field(o, slot, value);
                // Stores to a prototype stay on this path, which bumps the
                // epoch (see `store_field`), as do those to a constant field
                // of a function's.
                if (*o).flags & PROTOTYPE != 0 || !self.may_cache_store(o, slot) {
                    return Ok(());
                }
                install(
                    ic,
                    Ic {
                        shape: old,
                        holder: (*old).field_type(slot).encode() as *mut Object,
                        loc: (*old).location(slot),
                        ..Ic::EMPTY
                    },
                );
                return Ok(());
            }
            let slot = self.add_property(o, name, value);
            let new = (*o).shape;
            if !(*old).unique && slot < (*new).capacity.get() {
                install(
                    ic,
                    Ic {
                        shape: old,
                        holder: (*new).field_type(slot).encode() as *mut Object,
                        new_shape: new,
                        loc: (*new).location(slot),
                        ..Ic::EMPTY
                    },
                );
            }
        }
        Ok(())
    }

    /// Whether a store of `name` to `o` is blocked by a read-only property
    /// of that name: `o`'s own, or else the nearest on its prototype chain.
    pub(crate) fn read_only(&self, o: *mut Object, name: Atom) -> bool {
        match self.lookup(o, name) {
            Some((h, slot)) => unsafe { (&(*(*h).shape).attrs)[slot as usize] & READ_ONLY != 0 },
            None => false,
        }
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
            if s.attrs[slot as usize] != attrs {
                self.shape_left_stable(s);
                let mut slot_attrs = s.attrs.clone();
                slot_attrs[slot as usize] = attrs;
                let u = Shape {
                    class: s.class,
                    proto: s.proto,
                    capacity: Cell::new(s.capacity.get()),
                    keys: s.keys.clone(),
                    attrs: slot_attrs,
                    fields: s.fields.clone(),
                    elems: s.elems,
                    elems_code: s.elems_code,
                    family: std::ptr::null(),
                    push: Cell::new(std::ptr::null_mut()),
                    unique: true,
                    transitions: RefCell::default(),
                    elem_transitions: RefCell::default(),
            relied: Cell::new(false),
                };
                (*o).shape = self.add_shape(u);
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

    /// `LdaGlobal` miss: a ReferenceError if the global doesn't exist.
    pub fn get_global_miss(&mut self, code: &Code, ic: u16) -> Result<Value> {
        let (_, name) = code.ic(ic);
        let g = self.global;
        if self.lookup(g, name).is_none() {
            // A script's `var` not assigned yet (`toi` makes the global on
            // its first store, keeping its field's type): `undefined`.
            if self.declared.contains(&name) {
                return Ok(Value::UNDEFINED);
            }
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
        if self.lookup(self.global, name).is_none() {
            return Ok(Value::UNDEFINED);
        }
        unsafe { global_way(code, ic) };
        self.get_named_miss(code, ic, object_value(self.global))
    }

    /// `typeof v`, a string constant.
    pub fn type_of(&mut self, v: Value) -> Value {
        let name = if v.is_number() {
            "number"
        } else if v.is_undefined() {
            "undefined"
        } else if v.is_bool() {
            "boolean"
        } else if v.is_cell() {
            match unsafe { (*as_object(v)).class } {
                Class::String => "string",
                Class::Function | Class::Native => "function",
                _ => "object",
            }
        } else {
            "object"
        };
        let units: Vec<u16> = name.encode_utf16().collect();
        self.string_constant(&units)
    }

    /// `StaGlobal` miss (sloppy mode: creates the global if missing). Only
    /// stores to existing globals are cached.
    pub fn set_global_miss(&mut self, code: &Code, ic: u16, value: Value) -> Result<()> {
        let (ic, name) = code.ic(ic);
        let g = self.global;
        if self.read_only(g, name) {
            return Ok(());
        }
        unsafe {
            let shape = (*g).shape;
            match (*shape).lookup(name) {
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
                None => {
                    self.add_property(g, name, value);
                }
            }
        }
        Ok(())
    }

    /// `v instanceof f`: whether `f.prototype` is on `v`'s prototype chain
    /// (OrdinaryHasInstance; there are no bound functions or
    /// `Symbol.hasInstance`).
    pub fn instance_of(&mut self, v: Value, f: Value) -> Result<bool> {
        let callable = f.is_cell()
            && matches!(unsafe { (*as_object(f)).class }, Class::Function | Class::Native);
        if !callable {
            return Err(Error::type_error("Right-hand side of 'instanceof' is not callable"));
        }
        // Strings are primitives.
        if !v.is_cell() || string_units(v).is_some() {
            return Ok(false);
        }
        let p = self.get(f, self.a_prototype)?;
        if !p.is_cell() {
            return Err(Error::type_error(
                "Function has non-object prototype in instanceof check",
            ));
        }
        let mut o = as_object(v);
        loop {
            let proto = unsafe { (*(*o).shape).proto };
            if !proto.is_cell() {
                return Ok(false);
            }
            if proto == p {
                return Ok(true);
            }
            o = as_object(proto);
        }
    }

    /// Whether `o` has element `i` of its own: within the length, and not
    /// a hole (strings and typed arrays have none; buffers no elements).
    /// (A sparse object's far keys are named properties: [`Vm::sparse_get`].)
    pub(crate) fn has_element(o: *mut Object, i: u32) -> bool {
        unsafe {
            match (*o).class {
                Class::ArrayBuffer => false,
                Class::String | Class::TypedArray => i < (*o).elems_len,
                _ => i < (*o).elems_len && *(*o).elems.add(i as usize) != Value::EMPTY,
            }
        }
    }

    /// A sparse object's own key `i`, stored as a named property (see
    /// `object::SPARSE`), if it has it.
    fn sparse_get(&mut self, o: *mut Object, i: u32) -> Result<Option<Value>> {
        let a = self.intern(&i.to_string());
        unsafe {
            if (*(*o).shape).lookup(a).is_none() {
                return Ok(None);
            }
        }
        self.get(object_value(o), a).map(Some)
    }

    /// Whether `o` has key `i` of its own: an element, or a sparse one.
    pub(crate) fn has_own_index(&mut self, o: *mut Object, i: u32) -> bool {
        Self::has_element(o, i)
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
        match self.property_key(key) {
            Key::Name(a) => Ok(self.lookup(o, a).is_some()
                || (a == self.a_length && unsafe { (*o).class } == Class::Array)),
            Key::Index(i) => loop {
                unsafe {
                    if (*o).class != Class::String && self.has_own_index(o, i) {
                        return Ok(true);
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
        match self.property_key(key) {
            Key::Index(i) => unsafe {
                match (*o).class {
                    Class::String | Class::TypedArray => return Ok(i >= (*o).elems_len),
                    Class::ArrayBuffer => return Ok(true),
                    _ => {}
                }
                if i < (*o).elems_len {
                    *(*o).elems.add(i as usize) = Value::EMPTY;
                } else if (*o).flags & crate::object::SPARSE != 0 {
                    let a = self.intern(&i.to_string());
                    return Ok(self.delete_named(o, a));
                }
                Ok(true)
            },
            Key::Name(a) => Ok(self.delete_named(o, a)),
        }
    }

    /// Remove property `name` of `o`, unless it can't be deleted. The
    /// object gets a shape of its own (as a prototype's), with the other
    /// properties' slots moved down. Compiled code that folded `o`'s fields
    /// or relied on its shape is dropped: every field of its old shape is
    /// made mutable (they no longer describe `o`'s), and its shape left
    /// stable.
    pub(crate) fn delete_named(&mut self, o: *mut Object, name: Atom) -> bool {
        unsafe {
            let s = &*(*o).shape;
            let Some(slot) = s.lookup(name) else {
                return !(name == self.a_length
                    && matches!((*o).class, Class::Array | Class::String));
            };
            if s.attrs[slot as usize] & DONT_DELETE != 0 {
                return false;
            }
            let n = s.keys.len() as u32;
            let kept: Vec<u32> = (0..n).filter(|&k| k != slot).collect();
            let values: Vec<Value> = kept
                .iter()
                .map(|&k| *Object::slot(o, s.location(k)))
                .collect();
            self.shape_left_stable(s);
            for &cell in &s.fields {
                self.field_made_mutable(&*cell);
            }
            let mut fields = Vec::with_capacity(kept.len());
            for &k in &kept {
                let cell = Box::new(FieldCell::new());
                cell.set((*s.fields[k as usize]).get());
                fields.push(&*cell as *const FieldCell);
                self.field_cells.push(cell);
            }
            let u = Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                keys: kept.iter().map(|&k| s.keys[k as usize]).collect(),
                attrs: kept.iter().map(|&k| s.attrs[k as usize]).collect(),
                fields,
                elems: s.elems,
                elems_code: s.elems_code,
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: true,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
                relied: Cell::new(false),
            };
            let new = self.add_shape(u);
            (*o).shape = new;
            for (k, v) in values.into_iter().enumerate() {
                let at = Object::slot(o, (*new).location(k as u32));
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

    // ---- Elements ----

    /// What `o[key]` accesses: an element, for an array index (a number,
    /// or its canonical string, below 2^32 - 1), else the property named
    /// by the key's string.
    fn property_key(&mut self, key: Value) -> Key {
        if key.is_int32() && key.as_i32_unchecked() >= 0 {
            return Key::Index(key.as_i32_unchecked() as u32);
        }
        if let Some(d) = key.as_number()
            && d >= 0.0
            && d < u32::MAX as f64
            && d.fract() == 0.0
            && !(d == 0.0 && d.is_sign_negative())
        {
            return Key::Index(d as u32);
        }
        let units = crate::interp::to_string_units(key);
        let name = String::from_utf16_lossy(&units);
        if let Ok(i) = name.parse::<u32>()
            && i != u32::MAX
            && i.to_string() == name
        {
            return Key::Index(i);
        }
        Key::Name(self.intern(&name))
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
            && matches!(
                unsafe { (*as_object(obj)).class },
                Class::String | Class::TypedArray | Class::ArrayBuffer
            )
        {
            let (ic, _) = code.ic(ic);
            unsafe { note_bytes(ic, as_object(obj)) };
        } else if obj.is_cell() {
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
            return Ok(Value::UNDEFINED);
        }
        let i = match self.property_key(key) {
            Key::Index(i) => i,
            Key::Name(a) => return self.get(obj, a),
        };
        let mut o = as_object(obj);
        // A typed array's elements are all there is at an index (no holes,
        // nothing inherited); a buffer has none.
        unsafe {
            match (*o).class {
                Class::TypedArray => {
                    return Ok(if i < (*o).elems_len {
                        let kind = crate::object::TypedKind::from_u8((*o).kind);
                        kind.load((*o).elems.cast(), i as usize)
                    } else {
                        Value::UNDEFINED
                    });
                }
                Class::ArrayBuffer => return Ok(Value::UNDEFINED),
                _ => {}
            }
        }
        // A string's index is its code unit there, as a string.
        if let Some(u) = string_units(obj) {
            if (i as usize) < u.len() {
                let c = [u[i as usize]];
                return Ok(self.string_constant(&c));
            }
            o = self.string_prototype;
        }
        loop {
            unsafe {
                if i < (*o).elems_len && (*o).class != Class::String {
                    let v = *(*o).elems.add(i as usize);
                    if v != Value::EMPTY {
                        return Ok(v);
                    }
                }
                if (*o).flags & crate::object::SPARSE != 0
                    && let Some(v) = self.sparse_get(o, i)?
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
    pub fn set_keyed(&mut self, obj: Value, key: Value, value: Value) -> Result<()> {
        if !obj.is_cell() {
            if obj.is_undefined() || obj.is_null() {
                return Err(Error::type_error(format!(
                    "Cannot set properties of {obj:?}"
                )));
            }
            return Ok(());
        }
        let i = match self.property_key(key) {
            Key::Index(i) => i,
            Key::Name(a) => return self.set(obj, a, value),
        };
        let o = as_object(obj);
        unsafe {
            // A string's characters can't be changed; a typed array stores
            // within its length only (a buffer has no elements).
            match (*o).class {
                Class::String | Class::ArrayBuffer => return Ok(()),
                Class::TypedArray => {
                    if i < (*o).elems_len {
                        let kind = crate::object::TypedKind::from_u8((*o).kind);
                        let d = crate::interp::to_number(value);
                        kind.store((*o).elems.cast(), i as usize, d);
                    }
                    return Ok(());
                }
                _ => {}
            }
            // Far past the elements of an object that isn't an array (a
            // map keyed by numbers): a named property, not as many holes.
            if (*o).class != Class::Array && i as usize > (*o).elems_len as usize * 2 + SPARSE_GAP {
                (*o).flags |= crate::object::SPARSE;
                let a = self.intern(&i.to_string());
                return self.set(obj, a, value);
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

    /// `a.length = value`: truncated (the elements past it removed) or
    /// grown with holes.
    fn set_array_length(&mut self, o: *mut Object, value: Value) -> Result<()> {
        let d = crate::interp::to_number(value);
        if !(d >= 0.0 && d <= u32::MAX as f64 && d.fract() == 0.0) {
            return Err(Error::range_error("Invalid array length"));
        }
        let n = d as usize;
        unsafe {
            let len = (*o).elems_len as usize;
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

    /// Make room for `n` elements.
    fn reserve_elems(&mut self, o: *mut Object, n: usize) {
        unsafe {
            if n <= (*o).elems_cap as usize {
                return;
            }
            let cap = n.max((*o).elems_cap as usize * 2).max(4);
            let cap = u32::try_from(cap).expect("array too large");
            let e = self.heap.alloc_values(cap as usize);
            if !(*o).elems.is_null() {
                std::ptr::copy_nonoverlapping((*o).elems, e, (*o).elems_len as usize);
            }
            (*o).elems = e;
            crate::heap::barrier_young(o.cast(), &raw const (*o).elems);
            (*o).elems_cap = cap;
        }
    }

    /// `CreateObject n`: a plain object with room for `n` properties in
    /// itself, its root shape cached in inline-cache slot `ic`.
    pub fn create_object(&mut self, code: &Code, ic: u16, n: u32) -> Value {
        let (ic, _) = code.ic(ic);
        unsafe {
            if (*ic).shape.is_null() {
                let op = object_value(self.object_prototype);
                (*ic).shape = self.root_shape(Class::Plain, op, n);
            }
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
        if v.is_cell() {
            let mut seen: Vec<Atom> = Vec::new();
            let mut o = as_object(v);
            unsafe {
                // Elements first, in order (any object's: `{1: x}` has one).
                {
                    let n = (*o).elems_len as usize;
                    for i in 0..n {
                        let hole = !Self::has_element(o, i as u32);
                        if !hole {
                            names.push(i.to_string().encode_utf16().collect());
                        }
                    }
                }
                loop {
                    let s = &*(*o).shape;
                    for (k, &name) in s.keys.iter().enumerate() {
                        if seen.contains(&name) {
                            continue;
                        }
                        seen.push(name);
                        if s.attrs[k] & DONT_ENUM == 0 {
                            names.push(self.atom_name(name).encode_utf16().collect());
                        }
                    }
                    if !s.proto.is_cell() {
                        break;
                    }
                    o = as_object(s.proto);
                }
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
            let data = o.cast::<u8>().add(crate::object::INLINE_BASE).cast::<u16>();
            fill(data);
            (*o).elems = data.cast();
            (*o).elems_len = u32::try_from(len).expect("string too long");
            object_value(o)
        }
    }

    /// A string cell with `words` words of units after its header, which
    /// `elems_cap` records (see `heap::cell_size`).
    fn alloc_string(&mut self, words: usize) -> *mut Object {
        let o = self
            .heap
            .alloc(crate::heap::object_bytes(words))
            .cast::<Object>();
        unsafe {
            o.cast::<[u64; crate::object::INLINE_BASE / 8]>()
                .write(crate::object::object_header(&*self.string_shape));
            (*o).elems_cap = u32::try_from(words).expect("string too long");
        }
        o
    }

    /// A new string of `len` code units, which `fill` writes.
    pub fn new_string_with(&mut self, len: usize, fill: impl FnOnce(&mut [u16])) -> Value {
        unsafe {
            let o = self.alloc_string(len.div_ceil(4));
            let data = o.cast::<u8>().add(crate::object::INLINE_BASE).cast::<u16>();
            fill(std::slice::from_raw_parts_mut(data, len));
            (*o).elems = data.cast();
            (*o).elems_len = u32::try_from(len).expect("string too long");
            object_value(o)
        }
    }

    /// The string of the one code unit `c` (shared below 256).
    pub(crate) fn single_char(&mut self, c: u16) -> Value {
        if c >= 256 {
            return self.new_string(&[c]);
        }
        if self.single_chars.is_empty() {
            self.single_chars = vec![Value::EMPTY; 256];
        }
        if self.single_chars[c as usize] == Value::EMPTY {
            self.single_chars[c as usize] = self.new_string(&[c]);
        }
        self.single_chars[c as usize]
    }

    /// A cons string of strings `a` and `b` (V8's): no units of its own
    /// (`elems` null) until something reads them (`string_units` flattens
    /// it), `a` and `b` in `props` and `code`, its length the sum. So
    /// building a long string piece by piece costs memory linear in its
    /// length, not quadratic (pdfjs: 138 GB).
    pub(crate) fn new_cons(&mut self, a: Value, b: Value) -> Value {
        let len = string_len(a).unwrap() + string_len(b).unwrap();
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
            let f = self.heap.alloc_object(&*self.function_shape);
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
            let shape = &*self.function_shape;
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

    /// Give new closure `f` its `prototype` object; `f` as a value.
    fn closure_prototype(&mut self, f: *mut Object) -> Value {
        let p = self.heap.alloc_object(unsafe { &*self.proto_object_shape });
        self.store_field(p, 0, object_value(f));
        self.store_field(f, 0, object_value(p));
        object_value(f)
    }

    /// A closure's upvalues: after its in-object slots (a function's shapes
    /// keep the capacity they were created with).
    pub(crate) fn upvalues(f: *const Object) -> *mut *mut Upvalue {
        unsafe {
            let cap = (*(*f).shape).capacity.get() as usize;
            f.cast::<u8>()
                .add(crate::object::INLINE_BASE + 8 * cap)
                .cast_mut()
                .cast()
        }
    }

    /// The open upvalue of register `at`, made if there is none.
    fn open_upvalue(&mut self, at: *mut Value) -> *mut Upvalue {
        let k = self
            .open_upvalues
            .partition_point(|&u| unsafe { (*u).at } < at);
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

    /// Close the open upvalues of registers at or past `level` (a frame
    /// ending): their values move into them.
    pub(crate) fn close_upvalues(&mut self, level: *mut Value) {
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
    fn native_function(
        &mut self,
        name: &str,
        f: NativeFn,
        nparams: u32,
        ret: crate::jit::Ty,
        constructs: bool,
    ) -> *mut Object {
        let code = self.add_code(Code {
            name: name.to_string(),
            bytecode: Vec::new(),
            nparams,
            nregs: 1 + nparams,
            ics: Box::new([]),
            ic_names: Box::new([]),
            closures: Vec::new(),
            constants: Box::new([]),
            handlers: Box::new([]),
            upvalues: Box::new([]),
            interpreted: false,
            field_deps: Cell::new(0),
            ic_misses: Cell::new(0),
            native: Some(f),
            constructs,
            ctor_capacity: Cell::new(0),
            ctor_shape: Cell::new(std::ptr::null()),
            ctor_settled: Cell::new(true),
            ctor_result: Cell::new(std::ptr::null()),
            jit: RefCell::new(None),
            calls_left: Cell::new(i32::MAX),
            calls_interpret: Cell::new(false),
            loop_budget: Cell::new(i64::MAX),
            ret_hint: Cell::new(ret),
        });
        let fp = object_value(self.function_prototype);
        let fo = self.new_object_with(Class::Native, fp, 0);
        unsafe { (*fo).code = code };
        fo
    }

    /// Add properties `names` (none of which `o` has) with `values` and
    /// attributes `attrs`, as `add_property_with` would one by one, but
    /// through one new shape (no transition leads to it; startup made one
    /// shape per builtin, and copied its keys each time).
    pub(crate) fn add_properties(&mut self, o: *mut Object, names: &[Atom], values: &[Value], attrs: u8) {
        unsafe {
            let s = &*(*o).shape;
            let n = s.keys.len() + names.len();
            let mut keys = Vec::with_capacity(n);
            keys.extend_from_slice(&s.keys);
            keys.extend_from_slice(names);
            let mut slot_attrs = Vec::with_capacity(n);
            slot_attrs.extend_from_slice(&s.attrs);
            slot_attrs.resize(n, attrs);
            let mut fields = Vec::with_capacity(n);
            fields.extend_from_slice(&s.fields);
            for _ in names {
                let cell = Box::new(FieldCell::new());
                fields.push(&*cell as *const FieldCell);
                self.field_cells.push(cell);
            }
            self.shape_left_stable(s);
            let new = self.add_shape(Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                keys,
                attrs: slot_attrs,
                fields,
                elems: s.elems,
                elems_code: s.elems_code,
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: s.unique,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
                relied: Cell::new(false),
            });
            // Out-of-line room for the slots past the object's capacity.
            let cap = (*new).capacity.get() as usize;
            if n > cap && n - cap > (*o).props_cap as usize {
                let want = (n - cap).max((*o).props_cap as usize * 2).max(4);
                let p = self.heap.alloc_values(want);
                if !(*o).props.is_null() {
                    std::ptr::copy_nonoverlapping((*o).props, p, (*o).props_cap as usize);
                }
                (*o).props = p;
                crate::heap::barrier_young(o.cast(), &raw const (*o).props);
                (*o).props_cap = want as u32;
            }
            (*o).shape = new;
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
        object_value(self.heap.alloc_object(unsafe { &*s }))
    }

    /// The shape `new f` gives `this` now: the root for `f.prototype` and
    /// `f`'s in-object capacity.
    pub(crate) fn ctor_root(&mut self, f: *mut Object) -> *const Shape {
        unsafe {
            debug_assert_eq!((*f).class, Class::Function);
            let code = &*(*f).code;
            // A bytecode function's slot 0 is always `prototype`.
            let p = *Object::inline(f, 0);
            let proto = if p.is_cell() {
                p
            } else {
                object_value(self.object_prototype)
            };
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

    /// After `new` returned `this`: grow the in-object capacity for the next
    /// objects if this one outgrew it.
    pub(crate) fn construct_done(code: &Code, this: *mut Object) {
        if !code.ctor_settled.replace(true) {
            settle(code);
        }
        let shape = unsafe { (*this).shape };
        let n = unsafe { (*shape).keys.len() } as u32;
        if n > code.ctor_capacity.get() {
            code.ctor_capacity.set(n.min(MAX_INLINE_CAPACITY));
        }
        let seen = code.ctor_result.get();
        if seen.is_null() {
            code.ctor_result.set(shape);
        } else if seen != shape {
            code.ctor_result.set(MANY_SHAPES);
        }
    }

    /// Call `f` when it isn't a bytecode function (those the interpreter
    /// enters itself).
    #[inline]
    pub fn call_other(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Value> {
        if f.is_cell() {
            let o = as_object(f);
            unsafe {
                if (*o).class == Class::Native {
                    let native = (*(*o).code).native.unwrap();
                    return native(self, this, args);
                }
            }
        }
        Err(Error::type_error(format!("{f:?} is not a function")))
    }

    /// `new f(…args)` when `f` isn't a bytecode function: a native that
    /// constructs (`Array`, `Object`) makes its object.
    pub fn construct_other(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        if f.is_cell() {
            let o = as_object(f);
            unsafe {
                if (*o).class == Class::Native && (*(*o).code).constructs {
                    let native = (*(*o).code).native.unwrap();
                    return native(self, Value::UNDEFINED, args);
                }
            }
        }
        Err(Error::type_error(format!("{f:?} is not a constructor")))
    }

    // ---- Loading and running ----

    fn add_code(&mut self, c: Code) -> *const Code {
        let b = Box::new(c);
        let p: *const Code = &*b;
        self.codes.push(b);
        p
    }

    /// Load `module`'s functions; returns its top-level code.
    pub fn load(&mut self, module: &Module) -> *const Code {
        for n in &module.declared {
            let a = self.intern(n);
            self.declared.insert(a);
        }
        let recursive = recursive_functions(module);
        let codes: Vec<*const Code> = module
            .functions
            .iter()
            .zip(recursive)
            .map(|(f, recursive)| {
                let ics: Vec<UnsafeCell<[Ic; IC_WAYS]>> = f
                    .ics
                    .iter()
                    .map(|_| UnsafeCell::new([Ic::EMPTY; IC_WAYS]))
                    .collect();
                let ic_names: Vec<Atom> = f.ics.iter().map(|n| self.intern(n)).collect();
                let constants: Box<[Value]> = f
                    .constants
                    .iter()
                    .map(|c| match c {
                        crate::asm::Constant::Number(d) => Value::number(*d),
                        crate::asm::Constant::String(t) => {
                            self.string_constant(&t.encode_utf16().collect::<Vec<_>>())
                        }
                    })
                    .collect();
                self.add_code(Code {
                    name: f.name.clone(),
                    bytecode: {
                        let mut b = f.code.clone();
                        if std::env::var_os("TOI_NO_QUICKEN").is_none() {
                            crate::bytecode::quicken(&mut b);
                        }
                        b
                    },
                    nparams: f.nparams,
                    nregs: f.nregs,
                    ics: ics.into_boxed_slice(),
                    ic_names: ic_names.into_boxed_slice(),
                    closures: Vec::new(),
                    constants,
                    handlers: f.handlers.clone().into_boxed_slice(),
                    upvalues: f.upvalues.clone().into_boxed_slice(),
                    interpreted: interpreted(f),
                    field_deps: Cell::new(0),
                    ic_misses: Cell::new(0),
                    native: None,
                    constructs: false,
                    ctor_capacity: Cell::new(MAX_INLINE_CAPACITY),
                    ctor_shape: Cell::new(std::ptr::null()),
                    ctor_settled: Cell::new(false),
                    ctor_result: Cell::new(std::ptr::null()),
                    jit: RefCell::new(None),
                    ret_hint: Cell::new(crate::jit::Ty::Any),
                    calls_interpret: Cell::new(false),
                    calls_left: Cell::new(match (has_loop(f), recursive || large(f)) {
                        _ if interpreted(f) => i32::MAX,
                        (true, _) => self.tier_calls,
                        (false, true) => self.tier_calls.saturating_mul(LOOP_FREE_CALLS),
                        (false, false) => i32::MAX,
                    }),
                    loop_budget: Cell::new(if !interpreted(f) {
                        self.frame_budget
                    } else {
                        i64::MAX
                    }),
                })
            })
            .collect();
        for (f, &c) in module.functions.iter().zip(&codes) {
            let closures = f.closures.iter().map(|&i| codes[i]).collect();
            unsafe { (*c.cast_mut()).closures = closures };
            self.max_frame = self.max_frame.max(f.nregs as usize + crate::interp::HEADER);
        }
        codes[module.script]
    }

    /// Load and run `module`; returns its completion value.
    pub fn run(&mut self, module: &Module) -> Result<Value> {
        crate::interp::CURRENT_VM.with(|v| v.set(self));
        let code = self.load(module);
        let here = 0u8;
        self.native_limit = (&raw const here as usize).saturating_sub(crate::interp::NATIVE_STACK);
        // Roots on the native stack are below here (see `gc`).
        let outer = self.heap.gc.stack_base == 0;
        if outer {
            self.heap.gc.stack_base = &raw const here as usize & !7;
        }
        let r = unsafe { crate::interp::run_script(self, code) };
        if outer {
            self.heap.gc.stack_base = 0;
            self.pause_marker();
        }
        r
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
            self.regexp_prototype,
            self.boolean_prototype,
            self.array_buffer_prototype,
            self.array_push_fn,
            self.array_pop_fn,
        ]
        .into_iter()
        .chain(self.typed_prototypes.iter().copied())
        .chain(self.error_prototypes.iter().copied())
        .filter(|o| !o.is_null())
        .map(object_value)
        .collect();
        v.push(self.bind_helper);
        v.extend_from_slice(&self.single_chars);
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

    /// Whether functions are compiled warm first, then hot, or hot from
    /// the start (the default; `TOI_WARM` set: warm first). See
    /// `jit::Tier`.
    pub fn set_warm_tier(&mut self, on: bool) {
        self.warm_tier = on;
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
        let code = self.codes.iter().find(|c| c.name == function)?;
        let jit = code.jit.borrow();
        let names = jit
            .as_ref()?
            .last_code()?
            .stencils
            .iter()
            .flatten()
            .cloned()
            .collect();
        Some(names)
    }

    /// Words of in-object storage in `o` (for tests).
    pub fn inline_capacity(&self, o: Value) -> u32 {
        unsafe { (*(*as_object(o)).shape).capacity.get() }
    }
}

/// Whether calls to a function should enter its compiled code once it is
/// hot: if it has a loop (whose back-edge budget restarts with each frame,
/// so a short loop in a function called often never tiers up by itself),
/// or at least [`ENTRY_COMPILE_SIZE`] instructions ([`large`]), or if it
/// looks recursive (see [`recursive_functions`]): its compiled code can
/// then call itself, or the functions it recurses through, directly.
/// Entering compiled code costs a round trip through the driver, which a
/// small loop-free function doesn't win back; when compiled code calls it,
/// it is inlined instead. A loop-free one is entered after
/// [`LOOP_FREE_CALLS`] times as many calls: compiled sooner, its callers'
/// feedback is still growing and it gets compiled again (91: f and g
/// three and two times; DeltaBlue's methods).
fn has_loop(f: &crate::asm::FunctionDef) -> bool {
    ops(f).any(|op| op.is_loop())
}

/// See `Code::interpreted`.
fn interpreted(f: &crate::asm::FunctionDef) -> bool {
    !f.handlers.is_empty()
        || f.captures
        || ops(f).any(|op| op == crate::bytecode::Op::LdaCallee)
}

/// See [`has_loop`].
fn large(f: &crate::asm::FunctionDef) -> bool {
    ops(f).count() >= ENTRY_COMPILE_SIZE
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

/// See [`has_loop`]. Measured (perf.sh --ab, against entering loop-free
/// functions after `TIER_CALLS` too): 91 -10%, 90 -5%, 87 -5%; 10 does the
/// same. A higher threshold for functions with loops too cost up to +70%
/// (61, 66, 80, 82: loops in functions called a few hundred times).
const LOOP_FREE_CALLS: i32 = 5;

/// See [`large`].
const ENTRY_COMPILE_SIZE: usize = 64;

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
                .filter_map(|(_, n)| by_name.get(n.as_str()).copied())
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

/// `Array.prototype.pop`.
fn array_pop(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if !this.is_cell() || unsafe { (*as_object(this)).class } != Class::Array {
        return Err(Error::unsupported("Array.prototype.pop on a non-array"));
    }
    let o = as_object(this);
    unsafe {
        let len = (*o).elems_len as usize;
        if len == 0 {
            return Ok(Value::UNDEFINED);
        }
        let v = *(*o).elems.add(len - 1);
        *(*o).elems.add(len - 1) = Value::EMPTY;
        (*o).elems_len = (len - 1) as u32;
        Ok(if v == Value::EMPTY {
            Value::UNDEFINED
        } else {
            v
        })
    }
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
    let r = (x + 0.5).floor();
    // (-0.5 rounds to -0; x + 0.5 is inexact past 2^52, where x is
    // integral already.)
    Ok(Value::number(if x.abs() >= 4503599627370496.0 {
        x
    } else if r == 0.0 && x < 0.0 {
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
fn math_min(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let mut r = f64::INFINITY;
    for i in 0..a.len() {
        let x = num_arg(a, i);
        if x.is_nan() {
            return Ok(Value::from_f64(f64::NAN));
        }
        if x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative()) {
            r = x;
        }
    }
    Ok(Value::number(r))
}
fn math_max(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let mut r = f64::NEG_INFINITY;
    for i in 0..a.len() {
        let x = num_arg(a, i);
        if x.is_nan() {
            return Ok(Value::from_f64(f64::NAN));
        }
        if x > r || (x == 0.0 && r == 0.0 && r.is_sign_negative()) {
            r = x;
        }
    }
    Ok(Value::number(r))
}
/// `Math.random`: xorshift64*, seeded from the clock.
fn math_random(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    thread_local! {
        static STATE: Cell<u64> = Cell::new(
            (now_ms() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
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
    this.as_number()
        .ok_or_else(|| Error::type_error("Number.prototype method called on a non-number"))
}

/// `Number.prototype.toFixed(digits)`: rounded to `digits` (0 to 100)
/// decimals, halves away from zero; `ToString` past 1e21.
fn number_to_fixed(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let x = this_number(this)?;
    let f = a.first().map_or(0.0, |&v| crate::interp::to_number(v));
    if !(0.0..=100.0).contains(&f) {
        return Err(Error::range_error(
            "toFixed() digits argument must be between 0 and 100",
        ));
    }
    let f = f as usize;
    let s = if !x.is_finite() || x.abs() >= 1e21 {
        number_to_string(x)
    } else {
        // Ties (exact in binary) go away from zero, as the spec's "larger
        // n"; Rust's formatting would make them even.
        let scaled = x.abs() * 10f64.powi(f as i32);
        let n = if scaled.fract() == 0.5 {
            scaled.ceil()
        } else {
            scaled.round()
        };
        let digits = format!("{:0>width$}", format!("{n:.0}"), width = f + 1);
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

/// `Number.prototype.toPrecision(p)`: `p` (1 to 100) significant digits,
/// in exponent form when the exponent is below -6 or at least `p`.
fn number_to_precision(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let x = this_number(this)?;
    let s = match a.first() {
        None => number_to_string(x),
        Some(&v) if v.is_undefined() => number_to_string(x),
        Some(&v) => {
            let p = crate::interp::to_number(v);
            if !(1.0..=100.0).contains(&p) {
                return Err(Error::range_error(
                    "toPrecision() argument must be between 1 and 100",
                ));
            }
            let p = p as usize;
            if !x.is_finite() {
                number_to_string(x)
            } else if x == 0.0 {
                if p == 1 {
                    "0".into()
                } else {
                    format!("0.{}", "0".repeat(p - 1))
                }
            } else {
                let e = format!("{:.*e}", p - 1, x.abs());
                let (m, exp) = e.split_once('e').unwrap();
                let exp: i32 = exp.parse().unwrap();
                let sign = if x < 0.0 { "-" } else { "" };
                if exp < -6 || exp >= p as i32 {
                    let es = if exp >= 0 {
                        format!("+{exp}")
                    } else {
                        exp.to_string()
                    };
                    format!("{sign}{m}e{es}")
                } else {
                    let decimals = (p as i32 - 1 - exp).max(0) as usize;
                    format!("{sign}{:.*}", decimals, x.abs())
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

/// `Array.prototype.indexOf(x, from)`: the first index of an element
/// strictly equal to `x`, or -1.
fn array_index_of(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !this.is_cell() || unsafe { (*as_object(this)).class } != Class::Array {
        return Err(Error::unsupported("Array.prototype.indexOf on a non-array"));
    }
    let o = as_object(this);
    let x = a.first().copied().unwrap_or(Value::UNDEFINED);
    let len = unsafe { (*o).elems_len } as i64;
    let from = a.get(1).map_or(0.0, |&v| crate::interp::to_number(v));
    let from = if from.is_nan() {
        0
    } else if from < 0.0 {
        (len + from as i64).max(0)
    } else {
        from as i64
    };
    for i in from..len {
        let e = unsafe { *(*o).elems.add(i as usize) };
        if e != Value::EMPTY && crate::interp::strict_equals(e, x) {
            return Ok(Value::from_i32(i as i32));
        }
    }
    Ok(Value::from_i32(-1))
}

/// Milliseconds since the epoch, now.
fn now_ms() -> f64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    t.map_or(0.0, |d| d.as_millis() as f64)
}

/// `new Date()` (now) or `new Date(ms)`. (Called without `new`, it should
/// give a string; it gives the object.)
fn date_constructor(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value> {
    let t = match args.first() {
        Some(v) if v.is_number() => v.as_number().unwrap().trunc(),
        _ => now_ms(),
    };
    let dp = object_value(vm.date_prototype);
    let o = vm.new_object_with(Class::Date, dp, 0);
    unsafe { (*o).code = t.to_bits() as *const Code };
    Ok(object_value(o))
}

/// `Date.now()`.
fn date_now(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::number(now_ms()))
}

/// The time value of `v` if it is a `Date`.
pub(crate) fn date_value(v: Value) -> Option<f64> {
    let o = v.is_cell().then(|| as_object(v))?;
    unsafe { ((*o).class == Class::Date).then(|| f64::from_bits((*o).code as u64)) }
}

/// `Date.prototype.getTime()` and `valueOf()`.
fn date_get_time(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    match date_value(this) {
        Some(t) => Ok(Value::number(t)),
        None => Err(Error::type_error("this is not a Date object")),
    }
}

fn object_constructor(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value> {
    match args.first() {
        Some(&v) if v.is_cell() => Ok(v),
        _ => {
            let op = object_value(vm.object_prototype);
            Ok(object_value(vm.new_object(op)))
        }
    }
}

/// `Object.defineProperty(o, key, descriptor)` for data properties: the
/// descriptor's `value`, and its `writable`, `enumerable` and
/// `configurable`, false unless given (for an existing property, as they
/// were). Accessors (`get`, `set`) and index keys aren't supported.
fn object_define_property(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value> {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
    let (target, key, desc) = (arg(0), arg(1), arg(2));
    if !target.is_cell() {
        return Err(Error::type_error(
            "Object.defineProperty called on non-object",
        ));
    }
    if !desc.is_cell() {
        return Err(Error::type_error("Property description must be an object"));
    }
    let units = crate::interp::to_string_units(key);
    let name = String::from_utf16_lossy(&units);
    if name.parse::<u32>().is_ok_and(|i| i.to_string() == name) {
        return Err(Error::unsupported(
            "Object.defineProperty with an index key",
        ));
    }
    let name = vm.intern(&name);
    let o = as_object(target);
    let field = |vm: &mut Vm, f: &str| -> Option<Value> {
        let a = vm.intern(f);
        vm.lookup(as_object(desc), a)
            .map(|(h, slot)| unsafe { *Object::slot(h, (*(*h).shape).location(slot)) })
    };
    let (get, set) = (field(vm, "get"), field(vm, "set"));
    let old = unsafe {
        let s = &*(*o).shape;
        s.lookup(name).map(|k| s.attrs[k as usize])
    };
    let mut attrs = old.unwrap_or(READ_ONLY | DONT_ENUM | DONT_DELETE);
    for (f, bit) in [
        ("writable", READ_ONLY),
        ("enumerable", DONT_ENUM),
        ("configurable", DONT_DELETE),
    ] {
        if let Some(v) = field(vm, f) {
            if crate::interp::to_boolean(v) {
                attrs &= !bit;
            } else {
                attrs |= bit;
            }
        }
    }
    if get.is_some() || set.is_some() {
        // (`writable` doesn't apply to an accessor.)
        vm.define_accessor(o, name, get, set, attrs & !READ_ONLY & !ACCESSOR);
        return Ok(target);
    }
    let value = field(vm, "value");
    vm.define_property(o, name, value, attrs & !ACCESSOR);
    Ok(target)
}

/// `Function.prototype.call` on a native: a call through it to a bytecode
/// function is unwrapped where it is made (`interp::unwrap_call`).
fn function_call(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    let (that, rest) = match args {
        [] => (Value::UNDEFINED, args),
        [t, rest @ ..] => (*t, rest),
    };
    if this.is_cell() && unsafe { (*as_object(this)).class } == Class::Native {
        return vm.call_other(this, that, rest);
    }
    Err(Error::type_error(format!(
        "{this:?} is not a native function (call it directly)"
    )))
}

pub(crate) fn array_push(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    if !this.is_cell() || unsafe { (*as_object(this)).class } != Class::Array {
        // An array-like object: `this[length++] = v` for each.
        if !this.is_cell() {
            return Err(Error::type_error("Array.prototype.push called on a non-object"));
        }
        let a_length = vm.a_length;
        let n = crate::interp::to_number(vm.get(this, a_length)?);
        let mut n = if n.is_nan() || n < 0.0 { 0.0 } else { n.trunc() };
        for &v in args {
            vm.set_keyed(this, Value::number(n), v)?;
            n += 1.0;
        }
        vm.set(this, a_length, Value::number(n))?;
        return Ok(Value::number(n));
    }
    let o = as_object(this);
    unsafe {
        let len = (*o).elems_len as usize;
        vm.reserve_elems(o, len + args.len());
        for (i, &v) in args.iter().enumerate() {
            vm.store_element(o, len + i, v);
        }
        (*o).elems_len = (len + args.len()) as u32;
        Ok(Value::from_i32((*o).elems_len as i32))
    }
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

/// A string's length without flattening it (see [`Vm::new_cons`]).
pub(crate) fn string_len(v: Value) -> Option<usize> {
    if !v.is_cell() {
        return None;
    }
    let o = as_object(v);
    unsafe { ((*o).class == Class::String).then(|| (*o).elems_len as usize) }
}

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
