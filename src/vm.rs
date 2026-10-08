//! The VM: loaded code, intrinsics, and the runtime behind the interpreter's
//! slow paths (inline-cache misses, allocation, calls to natives).
//!
//! The interpreter reaches it through each frame's header (see
//! `src/interp.rs`), which holds the frame's [`Code`] and the `Vm`.

use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::fmt;

use crate::asm::Module;
use crate::object::{
    Atom, Class, DONT_DELETE, DONT_ENUM, FieldType, Heap, Object, PROTOTYPE, READ_ONLY, Shape,
    as_object, field_fits, object_value,
};
use crate::value::Value;

/// A JS exception, or a feature not implemented yet. There is no `try`
/// yet, so every error ends the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn type_error(m: impl fmt::Display) -> Error {
        Error(format!("TypeError: {m}"))
    }
    pub fn reference_error(m: impl fmt::Display) -> Error {
        Error(format!("ReferenceError: {m}"))
    }
    pub fn range_error(m: impl fmt::Display) -> Error {
        Error(format!("RangeError: {m}"))
    }
    pub fn unsupported(m: impl fmt::Display) -> Error {
        Error(format!("unsupported: {m}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

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
/// Stack size in values (frame headers included).
const STACK_VALUES: usize = 1 << 18;

// Shapes and code are boxed so their addresses stay put as the vectors grow.
#[allow(clippy::vec_box)]
pub struct Vm {
    pub heap: Heap,
    shapes: Vec<Box<Shape>>,
    field_cells: Vec<Box<Cell<FieldType>>>,
    roots: HashMap<(Class, u64, u32), *const Shape>,
    atoms: Vec<String>,
    atom_ids: HashMap<String, Atom>,
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
    function_shape: *const Shape,
    /// The root shape of arrays (cached: `root_shape` hashes).
    pub(crate) array_shape: *const Shape,
    proto_object_shape: *const Shape,
    /// `Function.prototype.call`'s code: a call through it is a call of its
    /// receiver (see `interp::unwrap_call`).
    pub(crate) function_call: *const Code,
    /// `Function.prototype.call` itself.
    pub(crate) function_call_fn: *mut Object,
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
            heap: Heap::default(),
            shapes: Vec::new(),
            field_cells: Vec::new(),
            roots: HashMap::new(),
            atoms: Vec::new(),
            atom_ids: HashMap::new(),
            codes: Vec::new(),
            jit_caches: Default::default(),
            global: std::ptr::null_mut(),
            object_prototype: std::ptr::null_mut(),
            function_prototype: std::ptr::null_mut(),
            array_prototype: std::ptr::null_mut(),
            string_prototype: std::ptr::null_mut(),
            missing_holder: std::ptr::null_mut(),
            string_shape: std::ptr::null(),
            strings: HashMap::new(),
            proto_epoch: 0,
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
            function_shape: std::ptr::null(),
            array_shape: std::ptr::null(),
            proto_object_shape: std::ptr::null(),
            function_call: std::ptr::null(),
            function_call_fn: std::ptr::null_mut(),
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

        vm.object_prototype = vm.new_object_with(Class::Plain, Value::NULL, 4);
        let op = object_value(vm.object_prototype);
        vm.function_prototype = vm.new_object(op);
        vm.array_prototype = vm.new_object(op);
        vm.string_prototype = vm.new_object(op);
        vm.missing_holder = vm.new_object_with(Class::Plain, Value::NULL, 1);
        // Room for the builtins and the script's globals in the object
        // itself: out-of-line globals cost compiled code a load each.
        vm.global = vm.new_object_with(Class::Plain, op, MAX_INLINE_CAPACITY);

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
        for (name, f, proto) in [
            ("Array", array_constructor as NativeFn, vm.array_prototype),
            (
                "Object",
                object_constructor as NativeFn,
                vm.object_prototype,
            ),
        ] {
            let c = vm.define_native(g, name, f, 0, crate::jit::Ty::Any, true);
            let (a_p, a_c) = (vm.a_prototype, vm.a_constructor);
            vm.add_property_with(
                c,
                a_p,
                object_value(proto),
                READ_ONLY | DONT_ENUM | DONT_DELETE,
            );
            // Not enumerable, as the builtins' properties.
            vm.add_property_with(proto, a_c, object_value(c), DONT_ENUM);
            if name == "Object" {
                let f = object_define_property as NativeFn;
                vm.define_native(c, "defineProperty", f, 3, crate::jit::Ty::Any, false);
            }
        }
        vm
    }

    // ---- Names ----

    pub fn intern(&mut self, s: &str) -> Atom {
        if let Some(&a) = self.atom_ids.get(s) {
            return a;
        }
        let a = Atom(self.atoms.len() as u32);
        self.atoms.push(s.to_string());
        self.atom_ids.insert(s.to_string(), a);
        a
    }

    pub fn atom_name(&self, a: Atom) -> &str {
        &self.atoms[a.0 as usize]
    }

    // ---- Shapes ----

    fn add_shape(&mut self, s: Shape) -> *const Shape {
        let b = Box::new(s);
        let p: *const Shape = &*b;
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
            elems_code: if class == Class::Array {
                NO_ELEMENTS
            } else {
                0
            },
            unique: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
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
            unique: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
        })
    }

    /// `shape` plus property `name` with attributes `attrs`. Shared shapes
    /// share transitions; unique shapes get a fresh unique successor.
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
        let mut keys = s.keys.clone();
        keys.push(name);
        let mut slot_attrs = s.attrs.clone();
        slot_attrs.push(attrs);
        let cell = Box::new(Cell::new(FieldType::None));
        let mut fields = s.fields.clone();
        fields.push(&*cell);
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
            unique: s.unique,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
        });
        if !s.unique {
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
            let u = Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                keys: s.keys.clone(),
                attrs: s.attrs.clone(),
                fields: s.fields.clone(),
                elems: s.elems,
                elems_code: s.elems_code,
                unique: true,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
            };
            (*o).shape = self.add_shape(u);
        }
    }

    // ---- Objects ----

    fn new_object_with(&mut self, class: Class, proto: Value, capacity: u32) -> *mut Object {
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
        unsafe {
            let new = self.transition_with((*o).shape, name, attrs);
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
                    (*o).props_cap = cap;
                }
            }
            (*o).shape = new;
            self.store_field(o, slot, value);
            if (*o).flags & PROTOTYPE != 0 {
                self.proto_epoch = self.proto_epoch.wrapping_add(1);
                self.code_epoch = self.code_epoch.wrapping_add(1);
            }
            slot
        }
    }

    /// Write `value` to slot `slot` of `o`, generalizing the field's type
    /// first if `value` doesn't fit it. Every store to a field that doesn't
    /// go through an inline cache whose check passed must use this.
    pub(crate) fn store_field(&mut self, o: *mut Object, slot: u32, value: Value) {
        unsafe {
            let shape = &*(*o).shape;
            let cell = &*shape.fields[slot as usize];
            let old = cell.get();
            let new = old.join(FieldType::of(value));
            if new != old {
                cell.set(new);
                // Nothing can have relied on an empty field.
                if old != FieldType::None {
                    self.field_epoch = self.field_epoch.wrapping_add(1);
                    self.code_epoch = self.code_epoch.wrapping_add(1);
                }
            }
            let at = Object::slot(o, shape.location(slot));
            // A prototype's slots hold what compiled code assumes they do
            // while the prototype epoch holds (see `jit`'s `Case::value`);
            // every store to one comes here (inline caches don't cache
            // them).
            if (*o).flags & PROTOTYPE != 0 && *at != value {
                self.proto_epoch = self.proto_epoch.wrapping_add(1);
                self.code_epoch = self.code_epoch.wrapping_add(1);
            }
            *at = value;
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
            unique: s.unique,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
        });
        if !s.unique {
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
                (*o).shape = self.elements_transition((*o).shape, elems);
            }
            *(*o).elems.add(i) = value;
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
            if name == self.a_length && matches!((*o).class, Class::Array | Class::String) {
                return Ok(Value::from_i32((*o).elems_len as i32));
            }
            Ok(match self.lookup(o, name) {
                Some((h, slot)) => *Object::slot(h, (*(*h).shape).location(slot)),
                None => Value::UNDEFINED,
            })
        }
    }

    fn get_primitive(&self, obj: Value, name: Atom) -> Result<Value> {
        if obj.is_undefined() || obj.is_null() {
            return Err(Error::type_error(format!(
                "Cannot read properties of {obj:?} (reading '{}')",
                self.atom_name(name)
            )));
        }
        // No Number or Boolean prototypes yet.
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
                return Err(Error::unsupported("setting an array's length"));
            }
            // A string is a primitive: stores to it do nothing.
            if (*o).class == Class::String || self.read_only(o, name) {
                return Ok(());
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
        let (ic, name) = code.ic(ic);
        if !obj.is_cell() {
            return self.get_primitive(obj, name);
        }
        let o = as_object(obj);
        unsafe {
            if name == self.a_length && matches!((*o).class, Class::Array | Class::String) {
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
        let (ic, name) = code.ic(ic);
        if !obj.is_cell() {
            return self.set_primitive(obj, name);
        }
        let o = as_object(obj);
        unsafe {
            let old = (*o).shape;
            if (*o).class == Class::Array && name == self.a_length {
                return Err(Error::unsupported("setting an array's length"));
            }
            // Sloppy mode: a store to a read-only property, own or
            // inherited, does nothing (and isn't cached, so it stays slow).
            if (*o).class == Class::String || self.read_only(o, name) {
                return Ok(());
            }
            if let Some(slot) = (*old).lookup(name) {
                self.store_field(o, slot, value);
                // Stores to a prototype stay on this path, which bumps the
                // epoch (see `store_field`).
                if (*o).flags & PROTOTYPE != 0 {
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
                    unique: true,
                    transitions: RefCell::default(),
                    elem_transitions: RefCell::default(),
                };
                (*o).shape = self.add_shape(u);
                self.code_epoch = self.code_epoch.wrapping_add(1);
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
            return Err(Error::reference_error(format!(
                "{} is not defined",
                self.atom_name(name)
            )));
        }
        self.get_named_miss(code, ic, object_value(g))
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
        // A string's elements are code units, not values: never cached.
        if obj.is_cell() && unsafe { (*as_object(obj)).class } != Class::String {
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
            // A string's characters can't be changed.
            if (*o).class == Class::String {
                return Ok(());
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
                if matches!((*o).class, Class::Array | Class::String) {
                    let n = (*o).elems_len as usize;
                    for i in 0..n {
                        let hole = (*o).class == Class::Array && *(*o).elems.add(i) == Value::EMPTY;
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

    /// A new string of `len` code units, which `fill` writes.
    pub fn new_string_with(&mut self, len: usize, fill: impl FnOnce(&mut [u16])) -> Value {
        let header = crate::object::INLINE_BASE / 8;
        let words = header + len.div_ceil(4);
        unsafe {
            let o = self.heap.alloc_words(words).cast::<Object>();
            o.cast::<[u64; crate::object::INLINE_BASE / 8]>()
                .write(crate::object::object_header(&*self.string_shape));
            let data = o.cast::<u8>().add(crate::object::INLINE_BASE).cast::<u16>();
            fill(std::slice::from_raw_parts_mut(data, len));
            (*o).elems = data.cast();
            (*o).elems_len = u32::try_from(len).expect("string too long");
            object_value(o)
        }
    }

    /// The string constant with `units`, shared by equal ones.
    pub fn string_constant(&mut self, units: &[u16]) -> Value {
        if let Some(&v) = self.strings.get(units) {
            return v;
        }
        let v = self.new_string(units);
        self.strings.insert(units.to_vec(), v);
        v
    }

    // ---- Functions ----

    /// A new closure of `code`'s nested function `i`, with a fresh
    /// `prototype` object.
    pub fn create_closure(&mut self, code: &Code, i: u16) -> Value {
        let callee = code.closures[i as usize];
        unsafe {
            let f = self.heap.alloc_object(&*self.function_shape);
            (*f).code = callee;
            let p = self.heap.alloc_object(&*self.proto_object_shape);
            self.store_field(p, 0, object_value(f));
            self.store_field(f, 0, object_value(p));
            object_value(f)
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
        let code = self.add_code(Code {
            name: name.to_string(),
            bytecode: Vec::new(),
            nparams,
            nregs: 1 + nparams,
            ics: Box::new([]),
            ic_names: Box::new([]),
            closures: Vec::new(),
            constants: Box::new([]),
            native: Some(f),
            constructs,
            ctor_capacity: Cell::new(0),
            ctor_shape: Cell::new(std::ptr::null()),
            ctor_settled: Cell::new(true),
            ctor_result: Cell::new(std::ptr::null()),
            jit: RefCell::new(None),
            calls_left: Cell::new(i32::MAX),
            loop_budget: Cell::new(i64::MAX),
            ret_hint: Cell::new(ret),
        });
        let fp = object_value(self.function_prototype);
        let fo = self.new_object_with(Class::Native, fp, 0);
        unsafe { (*fo).code = code };
        let a = self.intern(name);
        // Builtins' methods aren't enumerable.
        self.add_property_with(obj, a, object_value(fo), DONT_ENUM);
        fo
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
                    native: None,
                    constructs: false,
                    ctor_capacity: Cell::new(MAX_INLINE_CAPACITY),
                    ctor_shape: Cell::new(std::ptr::null()),
                    ctor_settled: Cell::new(false),
                    ctor_result: Cell::new(std::ptr::null()),
                    jit: RefCell::new(None),
                    ret_hint: Cell::new(crate::jit::Ty::Any),
                    calls_left: Cell::new(if recursive || worth_entering_compiled(f) {
                        self.tier_calls
                    } else {
                        i32::MAX
                    }),
                    loop_budget: Cell::new(self.frame_budget),
                })
            })
            .collect();
        for (f, &c) in module.functions.iter().zip(&codes) {
            let closures = f.closures.iter().map(|&i| codes[i]).collect();
            unsafe { (*c.cast_mut()).closures = closures };
        }
        codes[module.script]
    }

    /// Load and run `module`; returns its completion value.
    pub fn run(&mut self, module: &Module) -> Result<Value> {
        crate::interp::CURRENT_VM.with(|v| v.set(self));
        let code = self.load(module);
        let here = 0u8;
        self.native_limit = (&raw const here as usize).saturating_sub(crate::interp::NATIVE_STACK);
        unsafe { crate::interp::run_script(self, code) }
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

    /// How many of `function`'s calls go straight into their callees'
    /// compiled code (for tests).
    pub fn jit_bound_direct_calls(&self, function: &str) -> usize {
        let Some(code) = self.codes.iter().find(|c| c.name == function) else {
            return 0;
        };
        let jit = code.jit.borrow();
        jit.as_ref().map_or(0, |f| f.bound_direct_calls())
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
/// or at least [`ENTRY_COMPILE_SIZE`] instructions, or if it looks
/// recursive (see [`recursive_functions`]): its compiled code can then call
/// itself, or the functions it recurses through, directly. Entering
/// compiled code costs a round trip through the driver, which a small
/// loop-free function doesn't win back; when compiled code calls it, it is
/// inlined instead.
fn worth_entering_compiled(f: &crate::asm::FunctionDef) -> bool {
    use crate::bytecode::Op;
    let code = &f.code;
    let (mut pc, mut n) = (0, 0);
    while pc < code.len() {
        let op = Op::from_u8(code[pc]).expect("valid bytecode");
        if op.is_loop() {
            return true;
        }
        n += 1;
        pc += op.size();
    }
    n >= ENTRY_COMPILE_SIZE
}

/// See [`worth_entering_compiled`].
const ENTRY_COMPILE_SIZE: usize = 64;

/// Which of `module`'s functions look recursive: they reach themselves
/// through the names their inline caches look up (`LdaGlobal fib` in
/// `fib`; `isOdd` naming `isEven`, which names `isOdd`).
fn recursive_functions(module: &Module) -> Vec<bool> {
    let fs = &module.functions;
    let by_name: HashMap<&str, usize> = fs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();
    let edges: Vec<Vec<usize>> = fs
        .iter()
        .map(|f| {
            f.ics
                .iter()
                .filter_map(|n| by_name.get(n.as_str()).copied())
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
    if field(vm, "get").is_some() || field(vm, "set").is_some() {
        return Err(Error::unsupported("accessor properties (get/set)"));
    }
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
    let value = field(vm, "value");
    vm.define_property(o, name, value, attrs);
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
        return Err(Error::unsupported("Array.prototype.push on a non-array"));
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
        ((*o).class == Class::String)
            .then(|| std::slice::from_raw_parts((*o).elems.cast::<u16>(), (*o).elems_len as usize))
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
    // `{:e}` gives the shortest round-trip digits: `d1.d2…dke<x>`.
    let e = format!("{d:e}");
    let (mantissa, exp) = e.split_once('e').unwrap();
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
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
