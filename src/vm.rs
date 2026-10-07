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
    Atom, Class, FieldType, Heap, Object, PROTOTYPE, Shape, as_object, field_fits, object_value,
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

/// One inline-cache slot. A load hits when the receiver's shape is `shape`:
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
const _: fn(u64, Value) -> bool = field_fits;

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
    pub ics: Box<[UnsafeCell<Ic>]>,
    /// The property name of each inline-cache slot.
    pub ic_names: Box<[Atom]>,
    /// The nested functions, by `CreateClosure` operand.
    pub closures: Vec<*const Code>,
    pub native: Option<NativeFn>,
    /// For `new`: the in-object capacity to give new objects (raised to the
    /// most properties a constructed object has had on return, a simple
    /// form of slack tracking), and the root shape last used.
    pub ctor_capacity: Cell<u32>,
    pub ctor_shape: Cell<*const Shape>,
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
    /// What calls to the function are predicted to give, by its latest
    /// compilation, or the one in progress: a direct call to it from a
    /// function compiled meanwhile (mutual recursion) has no version of it
    /// to predict from (see `jit::JitCode::compile`).
    pub(crate) ret_hint: Cell<crate::jit::Ty>,
}

impl Code {
    /// The inline-cache slot `i`, and the name it looks up.
    pub fn ic(&self, i: u16) -> (*mut Ic, Atom) {
        (self.ics[i as usize].get(), self.ic_names[i as usize])
    }
}

const INITIAL_CTOR_CAPACITY: u32 = 4;
/// `Code::ctor_result` when constructed objects have had several shapes.
pub(crate) const MANY_SHAPES: *const Shape = std::ptr::dangling::<Shape>();
/// Loop back edges before a frame tiers up, and calls before a function's
/// calls enter compiled code (as `jit::Tiering::default`).
pub(crate) const TIER_BACK_EDGES: i64 = 1000;
pub(crate) const TIER_CALLS: i32 = 10;
const MAX_INLINE_CAPACITY: u32 = 64;
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
    array_shape: *const Shape,
    proto_object_shape: *const Shape,
    a_prototype: Atom,
    a_constructor: Atom,
    a_length: Atom,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    pub fn new() -> Vm {
        let mut stack = vec![Value::UNDEFINED; STACK_VALUES].into_boxed_slice();
        let stack_end = unsafe { stack.as_mut_ptr().add(stack.len()) };
        let mut vm = Vm {
            heap: Heap::default(),
            shapes: Vec::new(),
            field_cells: Vec::new(),
            roots: HashMap::new(),
            atoms: Vec::new(),
            atom_ids: HashMap::new(),
            codes: Vec::new(),
            global: std::ptr::null_mut(),
            object_prototype: std::ptr::null_mut(),
            function_prototype: std::ptr::null_mut(),
            array_prototype: std::ptr::null_mut(),
            proto_epoch: 0,
            field_epoch: 0,
            code_epoch: 0,
            error: None,
            stack,
            stack_end,
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
        vm.global = vm.new_object(op);

        let fp = object_value(vm.function_prototype);
        let root = vm.root_shape(Class::Function, fp, 2);
        vm.function_shape = vm.transition(root, vm.a_prototype);
        let root = vm.root_shape(Class::Plain, op, INITIAL_CTOR_CAPACITY);
        vm.proto_object_shape = vm.transition(root, vm.a_constructor);

        vm.array_shape = vm.root_shape(Class::Array, object_value(vm.array_prototype), 0);
        let ap = vm.array_prototype;
        vm.define_native(ap, "push", array_push, 1, crate::jit::Ty::I32);
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
            capacity,
            keys: Vec::new(),
            fields: Vec::new(),
            elems: FieldType::None,
            elems_code: 0,
            unique: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
        });
        self.roots.insert(key, s);
        s
    }

    /// `shape` plus property `name`. Shared shapes share transitions;
    /// unique shapes get a fresh unique successor.
    pub(crate) fn transition(&mut self, shape: *const Shape, name: Atom) -> *const Shape {
        let s = unsafe { &*shape };
        if !s.unique
            && let Some(&(_, t)) = s.transitions.borrow().iter().find(|(n, _)| *n == name)
        {
            return t;
        }
        let mut keys = s.keys.clone();
        keys.push(name);
        let cell = Box::new(Cell::new(FieldType::None));
        let mut fields = s.fields.clone();
        fields.push(&*cell);
        self.field_cells.push(cell);
        let t = self.add_shape(Shape {
            class: s.class,
            proto: s.proto,
            capacity: s.capacity,
            keys,
            fields,
            elems: s.elems,
            elems_code: s.elems_code,
            unique: s.unique,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
        });
        if !s.unique {
            s.transitions.borrow_mut().push((name, t));
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
                capacity: s.capacity,
                keys: s.keys.clone(),
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
        unsafe {
            let new = self.transition((*o).shape, name);
            let ns = &*new;
            let slot = ns.keys.len() as u32 - 1;
            if slot >= ns.capacity {
                let i = slot - ns.capacity;
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
            *Object::slot(o, shape.location(slot)) = value;
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
            capacity: s.capacity,
            keys: s.keys.clone(),
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
            if (*o).class == Class::Array && name == self.a_length {
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
            match (*(*o).shape).lookup(name) {
                Some(slot) => self.store_field(o, slot, value),
                // No setters or read-only properties on the chain yet.
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
            if (*o).class == Class::Array && name == self.a_length {
                return Ok(Value::from_i32((*o).elems_len as i32));
            }
            let Some((h, slot)) = self.lookup(o, name) else {
                return Ok(Value::UNDEFINED);
            };
            let loc = (*(*h).shape).location(slot);
            *ic = Ic {
                shape: (*o).shape,
                holder: if h == o { std::ptr::null_mut() } else { h },
                new_shape: std::ptr::null(),
                loc,
                epoch: self.proto_epoch,
            };
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
            if let Some(slot) = (*old).lookup(name) {
                self.store_field(o, slot, value);
                *ic = Ic {
                    shape: old,
                    holder: (*old).field_type(slot).encode() as *mut Object,
                    loc: (*old).location(slot),
                    ..Ic::EMPTY
                };
                return Ok(());
            }
            let slot = self.add_property(o, name, value);
            let new = (*o).shape;
            if !(*old).unique && slot < (*new).capacity {
                *ic = Ic {
                    shape: old,
                    holder: (*new).field_type(slot).encode() as *mut Object,
                    new_shape: new,
                    loc: (*new).location(slot),
                    ..Ic::EMPTY
                };
            }
        }
        Ok(())
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

    fn index(key: Value) -> Result<u32> {
        if key.is_int32() && key.as_i32_unchecked() >= 0 {
            return Ok(key.as_i32_unchecked() as u32);
        }
        if let Some(d) = key.as_number()
            && d >= 0.0
            && d < u32::MAX as f64
            && d.fract() == 0.0
        {
            return Ok(d as u32);
        }
        Err(Error::unsupported(format!("property key {key:?}")))
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
        if obj.is_cell() {
            let (ic, _) = code.ic(ic);
            unsafe {
                *ic = Ic {
                    shape: (*as_object(obj)).shape,
                    ..Ic::EMPTY
                };
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
        let i = Self::index(key)?;
        let mut o = as_object(obj);
        loop {
            unsafe {
                if i < (*o).elems_len {
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
        let i = Self::index(key)?;
        let o = as_object(obj);
        unsafe {
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
    ) {
        let code = self.add_code(Code {
            name: name.to_string(),
            bytecode: Vec::new(),
            nparams,
            nregs: 1 + nparams,
            ics: Box::new([]),
            ic_names: Box::new([]),
            closures: Vec::new(),
            native: Some(f),
            ctor_capacity: Cell::new(0),
            ctor_shape: Cell::new(std::ptr::null()),
            ctor_result: Cell::new(std::ptr::null()),
            jit: RefCell::new(None),
            calls_left: Cell::new(i32::MAX),
            ret_hint: Cell::new(ret),
        });
        let fp = object_value(self.function_prototype);
        let fo = self.new_object_with(Class::Native, fp, 0);
        unsafe { (*fo).code = code };
        let a = self.intern(name);
        self.add_property(obj, a, object_value(fo));
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
            if s.is_null() || (*s).proto != proto || (*s).capacity != cap {
                s = self.root_shape(Class::Plain, proto, cap);
                code.ctor_shape.set(s);
            }
            s
        }
    }

    /// After `new` returned `this`: grow the in-object capacity for the next
    /// objects if this one outgrew it.
    pub(crate) fn construct_done(code: &Code, this: *mut Object) {
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

    /// `new f(…)` when `f` isn't a bytecode function.
    pub fn construct_other(&mut self, f: Value) -> Result<Value> {
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
                let ics: Vec<UnsafeCell<Ic>> =
                    f.ics.iter().map(|_| UnsafeCell::new(Ic::EMPTY)).collect();
                let ic_names: Vec<Atom> = f.ics.iter().map(|n| self.intern(n)).collect();
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
                    native: None,
                    ctor_capacity: Cell::new(INITIAL_CTOR_CAPACITY),
                    ctor_shape: Cell::new(std::ptr::null()),
                    ctor_result: Cell::new(std::ptr::null()),
                    jit: RefCell::new(None),
                    ret_hint: Cell::new(crate::jit::Ty::Any),
                    calls_left: Cell::new(if recursive || worth_entering_compiled(f) {
                        self.tier_calls
                    } else {
                        i32::MAX
                    }),
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
        let code = self.load(module);
        let here = 0u8;
        self.native_limit = (&raw const here as usize).saturating_sub(crate::interp::NATIVE_STACK);
        unsafe { crate::interp::run_script(self, code) }
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
        unsafe { (*(*as_object(o)).shape).capacity }
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
        if matches!(
            op,
            Op::JumpLoop | Op::JumpIfLessLoop | Op::JumpIfLessSmiLoop
        ) {
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

fn array_push(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
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
        return if d.is_nan() {
            "NaN".into()
        } else if d.is_infinite() {
            if d > 0.0 { "Infinity" } else { "-Infinity" }.into()
        } else if d == 0.0 {
            "0".into()
        } else {
            format!("{d}")
        };
    }
    if v.is_undefined() {
        "undefined".into()
    } else if v.is_null() {
        "null".into()
    } else if v.is_bool() {
        (v == Value::TRUE).to_string()
    } else if v.is_cell() {
        "[object Object]".into()
    } else {
        format!("{v:?}")
    }
}
