//! Objects, shapes and the heap (step 1 of `docs/object-model.md`'s plan).
//!
//! Every object is a heap cell with a fixed header and in-object slots:
//!
//! ```text
//!  0  shape       *const Shape     names → slots, prototype, inline capacity
//!  8  class u8, flags u8, inline_cap u16, props_cap u32
//! 16  props       *mut Value       out-of-line slots beyond the inline ones
//! 24  elems       *mut Value       indexed elements (arrays, mostly)
//! 32  elems_len u32, elems_cap u32
//! 40  code        *const Code      functions only
//! 48  inline slots …
//! ```
//!
//! The shape decides the layout: its first `capacity` slots are in-object,
//! the rest are out of line. So an inline cache that has checked the shape
//! knows where a property lives, and caches it as one `i32`
//! ([`Shape::location`]): a byte offset from the object if non-negative,
//! otherwise `!index` into `props`.
//!
//! Shapes form transition trees from roots keyed by class, prototype and
//! inline capacity, so objects built the same way share shapes. An object
//! that becomes a prototype gets a *unique* shape, never shared and never
//! cached as a transition source. A property added to it bumps
//! [`crate::vm::Vm::proto_epoch`], which invalidates every cached
//! prototype-chain load at once. (V8 and JSC invalidate per prototype with
//! validity cells; a global epoch is the simple version.)
//!
//! `inline_cap` is the number of in-object slots the object was allocated
//! with (a shape's capacity may shrink later: see `Vm::construct_done`),
//! so the collector knows the object's size without its shape (see
//! `heap::cell_size`). A typed array has none, and its element type in the
//! low byte instead (`kind`).
//!
//! Every object has the external bit set: the internal heap of
//! `docs/object-model.md` is not there. The heap is `crate::heap`.

use std::cell::{Cell, RefCell};

use crate::value::Value;
use crate::vm::Code;

/// An interned property name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Atom(pub u32);

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Class {
    Plain,
    Array,
    /// A bytecode function: `code` is set, and its slot 0 is `prototype`.
    Function,
    /// A function implemented in Rust: `code` is set, with `native`.
    Native,
    /// A string: its UTF-16 code units follow the header; `elems` points
    /// to them and `elems_len` is the length (as arrays', so `length` reads
    /// the same field).
    String,
    /// A `Date`: its time value (milliseconds since the epoch, an `f64`) is
    /// kept in `code`'s bits (see `vm::date_value`).
    Date,
    /// An `ArrayBuffer`: `elems` points to its bytes, `elems_len` is their
    /// count.
    ArrayBuffer,
    /// A typed array (`Uint8Array`, …): `elems` points to its first
    /// element in its buffer's bytes, `elems_len` is its length, `kind` its
    /// element type ([`TypedKind`]), and `code` holds its buffer.
    TypedArray,
    /// A `RegExp`: `code` holds its compiled `regexp::Regex`.
    RegExp,
    /// A wrapper object (`new Number(1)`, `new String("s")`, `new
    /// Boolean(true)`): `code`'s bits are the primitive (a value, which the
    /// collector traces: a string is a cell).
    Wrapper,
}

/// A typed array's element type, as `Object::kind` holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum TypedKind {
    I8,
    U8,
    U8Clamped,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl TypedKind {
    pub const ALL: [TypedKind; 9] = [
        TypedKind::I8,
        TypedKind::U8,
        TypedKind::U8Clamped,
        TypedKind::I16,
        TypedKind::U16,
        TypedKind::I32,
        TypedKind::U32,
        TypedKind::F32,
        TypedKind::F64,
    ];

    pub fn from_u8(k: u8) -> TypedKind {
        TypedKind::ALL[k as usize]
    }

    /// Bytes per element.
    pub fn size(self) -> usize {
        match self {
            TypedKind::I8 | TypedKind::U8 | TypedKind::U8Clamped => 1,
            TypedKind::I16 | TypedKind::U16 => 2,
            TypedKind::I32 | TypedKind::U32 | TypedKind::F32 => 4,
            TypedKind::F64 => 8,
        }
    }

    /// The constructor's name.
    pub fn name(self) -> &'static str {
        match self {
            TypedKind::I8 => "Int8Array",
            TypedKind::U8 => "Uint8Array",
            TypedKind::U8Clamped => "Uint8ClampedArray",
            TypedKind::I16 => "Int16Array",
            TypedKind::U16 => "Uint16Array",
            TypedKind::I32 => "Int32Array",
            TypedKind::U32 => "Uint32Array",
            TypedKind::F32 => "Float32Array",
            TypedKind::F64 => "Float64Array",
        }
    }

    /// Element `i` at `p` (the first element), as a number.
    ///
    /// # Safety
    /// `p` must point to at least `i + 1` elements of this kind.
    pub unsafe fn load(self, p: *const u8, i: usize) -> Value {
        unsafe {
            match self {
                TypedKind::I8 => Value::from_i32(*p.cast::<i8>().add(i) as i32),
                TypedKind::U8 | TypedKind::U8Clamped => Value::from_i32(*p.add(i) as i32),
                TypedKind::I16 => Value::from_i32(p.cast::<i16>().add(i).read_unaligned() as i32),
                TypedKind::U16 => Value::from_i32(p.cast::<u16>().add(i).read_unaligned() as i32),
                TypedKind::I32 => Value::from_i32(p.cast::<i32>().add(i).read_unaligned()),
                TypedKind::U32 => Value::number(p.cast::<u32>().add(i).read_unaligned() as f64),
                TypedKind::F32 => Value::number(p.cast::<f32>().add(i).read_unaligned() as f64),
                TypedKind::F64 => Value::number(p.cast::<f64>().add(i).read_unaligned()),
            }
        }
    }

    /// Store int32 `v` as element `i` at `p`: as [`TypedKind::store`] of
    /// it, without the conversions through `f64`.
    ///
    /// # Safety
    /// As [`TypedKind::load`].
    pub unsafe fn store_i32(self, p: *mut u8, i: usize, v: i32) {
        unsafe {
            match self {
                TypedKind::I8 | TypedKind::U8 => *p.add(i) = v as u8,
                TypedKind::U8Clamped => *p.add(i) = v.clamp(0, 255) as u8,
                TypedKind::I16 | TypedKind::U16 => p.cast::<u16>().add(i).write_unaligned(v as u16),
                TypedKind::I32 | TypedKind::U32 => p.cast::<u32>().add(i).write_unaligned(v as u32),
                TypedKind::F32 => p.cast::<f32>().add(i).write_unaligned(v as f32),
                TypedKind::F64 => p.cast::<f64>().add(i).write_unaligned(v as f64),
            }
        }
    }

    /// Store number `d` (ToNumber of the value) as element `i` at `p`.
    ///
    /// # Safety
    /// As [`TypedKind::load`].
    pub unsafe fn store(self, p: *mut u8, i: usize, d: f64) {
        let int = || crate::interp::to_int32(d);
        unsafe {
            match self {
                TypedKind::I8 | TypedKind::U8 => *p.add(i) = int() as u8,
                TypedKind::U8Clamped => {
                    // Round half to even, clamped to 0..=255 (NaN is 0).
                    let c = if d.is_nan() { 0.0 } else { d.clamp(0.0, 255.0) };
                    let r = c.round();
                    let r = if (c - c.floor()) == 0.5 && r % 2.0 != 0.0 { r - 1.0 } else { r };
                    *p.add(i) = r as u8;
                }
                TypedKind::I16 | TypedKind::U16 => p.cast::<u16>().add(i).write_unaligned(int() as u16),
                TypedKind::I32 | TypedKind::U32 => p.cast::<u32>().add(i).write_unaligned(int() as u32),
                TypedKind::F32 => p.cast::<f32>().add(i).write_unaligned(d as f32),
                TypedKind::F64 => p.cast::<f64>().add(i).write_unaligned(d),
            }
        }
    }
}

/// The object is reachable from outside the internal heap. Always set for
/// now (see the module docs).
pub const EXTERNAL: u8 = 1;
/// The object is some shape's prototype.
pub const PROTOTYPE: u8 = 2;
/// The object has integer keys too far past its elements to store there:
/// named properties (their decimal strings), which keyed accesses check
/// after the elements (see `Vm::set_keyed`).
pub const SPARSE: u8 = 4;
/// The object takes no new properties (`Object.preventExtensions`).
pub const NOT_EXTENSIBLE: u8 = 8;
/// Its elements can't be deleted (`Object.seal`).
pub const SEALED: u8 = 16;
/// Its elements can't be written either (`Object.freeze`).
pub const FROZEN: u8 = 32;
/// An array's `length` is read-only (`Object.freeze`, or
/// `Object.defineProperty` with `writable: false`).
pub const LENGTH_READ_ONLY: u8 = 64;
/// A function's `length` was deleted (it has no slot: see
/// `Vm::own_property`); `LENGTH_READ_ONLY`'s bit, as arrays aren't functions.
pub const FN_LENGTH_DELETED: u8 = 64;
/// A function's `name` was deleted.
pub const FN_NAME_DELETED: u8 = 128;
/// A plain object made by an `Error` constructor (`[object Error]`);
/// `FN_NAME_DELETED`'s bit, as errors aren't functions.
pub const ERROR: u8 = 128;

#[repr(C)]
pub struct Object {
    pub shape: *const Shape,
    pub class: Class,
    pub flags: u8,
    /// A typed array's element type (see [`TypedKind`]); for any other
    /// object the low byte of its in-object capacity (see the module docs).
    pub kind: u8,
    cap_hi: u8,
    pub props_cap: u32,
    pub props: *mut Value,
    pub elems: *mut Value,
    pub elems_len: u32,
    pub elems_cap: u32,
    pub code: *const Code,
}

/// Byte offset of the first in-object slot.
pub const INLINE_BASE: usize = size_of::<Object>();
const _: () = assert!(INLINE_BASE == 48);

/// A new object's header, as words: `shape`, external, its in-object
/// capacity the shape's, nothing out of line. (`Object` has no padding, so
/// every byte is defined.)
pub fn object_header(shape: &Shape) -> [u64; INLINE_BASE / 8] {
    let mut h = [0u64; INLINE_BASE / 8];
    let cap = shape.capacity.get();
    let cap = u16::try_from(cap).expect("in-object capacity above 65535");
    assert!(cap == 0 || shape.class != Class::TypedArray, "a typed array with in-object slots");
    unsafe {
        h.as_mut_ptr().cast::<Object>().write(Object {
            shape,
            class: shape.class,
            flags: EXTERNAL,
            kind: cap as u8,
            cap_hi: (cap >> 8) as u8,
            props_cap: 0,
            props: std::ptr::null_mut(),
            elems: std::ptr::null_mut(),
            elems_len: 0,
            elems_cap: 0,
            code: std::ptr::null(),
        });
    }
    h
}

impl Object {
    /// The number of in-object slots `this` was allocated with.
    ///
    /// # Safety
    /// `this` must be an object.
    #[inline(always)]
    pub unsafe fn inline_cap(this: *const Object) -> usize {
        unsafe {
            if (*this).class == Class::TypedArray {
                0
            } else {
                (*this).kind as usize | ((*this).cap_hi as usize) << 8
            }
        }
    }

    /// The slot at `loc` (see [`Shape::location`]).
    ///
    /// # Safety
    /// `this` must be a live object and `loc` a location in its shape.
    #[inline(always)]
    pub unsafe fn slot(this: *mut Object, loc: i32) -> *mut Value {
        unsafe {
            if loc >= 0 {
                this.cast::<u8>().add(loc as usize).cast()
            } else {
                (*this).props.add(!loc as usize)
            }
        }
    }

    /// In-object slot `i`.
    ///
    /// # Safety
    /// `this` must be a live object with more than `i` inline slots.
    #[inline(always)]
    pub unsafe fn inline(this: *mut Object, i: usize) -> *mut Value {
        unsafe { this.cast::<u8>().add(INLINE_BASE).cast::<Value>().add(i) }
    }
}

/// A field's type ([`FieldType`]), and whether it is constant: on the
/// global object and on functions (constructors' statics, such as
/// DeltaBlue's `Direction.FORWARD`), compiled code takes a constant field's
/// value as a constant without checking it (`jit`'s `fold_constant_loads`).
///
/// A field stays constant while every store to it, on any object with the
/// field, either filled it (it held `undefined`: a new property, or a
/// declared global) or stored what it held; `Vm::store_field` makes it
/// mutable at any other store, and bumps `Vm::field_epoch` if compiled code
/// took its value (`folded`). So every store to a constant field must go
/// through `Vm::store_field`: inline caches cache stores to the global
/// object's and functions' constant fields only once they are mutable, and
/// caching any other store makes the field mutable first.
pub struct FieldCell {
    ty: Cell<FieldType>,
    mutable: Cell<bool>,
    folded: Cell<bool>,
    /// Compiled code took the field's type (`Shape::field_type_relied`):
    /// the functions whose code did are in `Vm::field_deps`, keyed by the
    /// cell's address; generalizing it makes them recompile, and no other.
    relied: Cell<bool>,
    /// It went from `Int` to `Num` and may still hold int32s in objects'
    /// own memory, until `Vm::int_fields_to_doubles` (see `FieldType::Num`).
    ints: Cell<bool>,
}

thread_local! {
    /// The function being compiled (a `vm::Code`'s address; 0: none), which
    /// the field types its code reads are recorded for (see `FieldCell`).
    pub static COMPILING: Cell<usize> = const { Cell::new(0) };
}

impl FieldCell {
    pub fn new() -> FieldCell {
        FieldCell {
            ty: Cell::new(FieldType::None),
            mutable: Cell::new(false),
            folded: Cell::new(false),
            relied: Cell::new(false),
            ints: Cell::new(false),
        }
    }

    /// Whether objects may still hold int32s in the field (a `Num` one).
    pub fn holds_ints(&self) -> bool {
        self.ints.get()
    }

    pub(crate) fn set_ints(&self, ints: bool) {
        self.ints.set(ints)
    }

    pub fn get(&self) -> FieldType {
        self.ty.get()
    }

    pub fn set(&self, ty: FieldType) {
        self.ty.set(ty)
    }

    pub fn is_constant(&self) -> bool {
        !self.mutable.get()
    }

    /// Compiled code takes the field's value as a constant.
    pub fn fold(&self) {
        self.folded.set(true)
    }

    /// Whether compiled code took the field's type, forgotten.
    pub fn take_relied(&self) -> bool {
        self.relied.replace(false)
    }

    /// Make the field mutable; whether compiled code had folded it.
    pub fn make_mutable(&self) -> bool {
        self.mutable.set(true);
        self.folded.replace(false)
    }
}

impl Default for FieldCell {
    fn default() -> Self {
        Self::new()
    }
}

/// What every value ever stored in a field has been, for each field of a
/// shape. The JIT types values loaded from the field with it.
///
/// A field's type lives in a cell made by the transition that added the
/// field and shared by every shape descending from it, so it covers every
/// object that has the field at that slot. It starts as the type of the
/// first value stored and only generalizes: `Vm::store_field` joins the
/// stored value's type in before writing, and bumps
/// `Vm::field_epoch` when that changes it, which drops compiled code that
/// assumed the old type.
///
/// `Obj(s)` means the values were objects of shape `s` when stored. They
/// may have gained properties since, so a load gives an object of *at
/// least* shape `s` (`jit::Ty::Ext`): transitions only append slots, so
/// `s`'s own properties are where `s` has them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldType {
    /// Nothing stored yet.
    None,
    Int,
    /// A number, stored as a double, never an int32, so that compiled code
    /// reads one in an object's own memory without a check (V8's double
    /// fields, boxed the NaN way): every store converts int32s. A field that
    /// was `Int` keeps its int32s until the JIT compiles such a load from
    /// it (`FieldCell::holds_ints`); then those in objects' own memory
    /// become doubles (`Vm::int_fields_to_doubles`). Out of line they may
    /// stay, so loads from there check.
    Num,
    Obj(*const Shape),
    Any,
}

impl FieldType {
    /// The type of a value, as a field type.
    pub fn of(v: Value) -> FieldType {
        if v.is_int32() {
            FieldType::Int
        } else if v.is_number() {
            FieldType::Num
        } else if v.is_cell() {
            FieldType::Obj(unsafe { (*as_object(v)).shape })
        } else {
            FieldType::Any
        }
    }

    pub fn join(self, other: FieldType) -> FieldType {
        use FieldType::*;
        match (self, other) {
            (a, b) if a == b => a,
            (None, t) | (t, None) => t,
            (Int, Num) | (Num, Int) => Num,
            _ => Any,
        }
    }

    /// The type as a store's inline cache keeps it, to check stored values
    /// against on its fast path (see [`field_fits`]): 0 for no check (`Any`,
    /// or nothing yet), 1 for an int32, 2 for a number, else the shape.
    pub fn encode(self) -> u64 {
        match self {
            FieldType::None | FieldType::Any => 0,
            FieldType::Int => 1,
            FieldType::Num => 2,
            FieldType::Obj(s) => s as u64,
        }
    }
}

/// Whether `v` fits a field type encoded by [`FieldType::encode`] (not 0).
#[inline(always)]
pub fn field_fits(want: u64, v: Value) -> bool {
    match want {
        1 => v.is_int32(),
        2 => v.is_number(),
        s => v.is_cell() && unsafe { (*as_object(v)).shape } as u64 == s,
    }
}

/// A hidden class. `repr(C)` with `elems_code` first: stencils read it
/// (`array_push`).
#[repr(C)]
pub struct Shape {
    /// [`Shape::elems`] encoded for store checks ([`FieldType::encode`]);
    /// an array that has no elements' type yet has `NO_ELEMENTS`, which no
    /// value fits, so its first store moves it to a typed shape.
    pub elems_code: u64,
    /// The shape this one is a variant of by its elements' type alone (see
    /// `Vm::elements_transition`), itself if none: shapes of one family
    /// differ in nothing else, so they find the same property in the same
    /// prototype. Compiled code reads it (`family_jeq`).
    pub family: *const Shape,
    /// Element transitions the `array_push` stencils make themselves
    /// (`Vm::store_element` records them), out of line as few shapes have
    /// any: null until one is recorded.
    pub push: Cell<*mut PushCache>,
    pub class: Class,
    /// An object, or null.
    pub proto: Value,
    /// Number of in-object slots. Shrinks once, in place, for the shapes
    /// of a constructor's first objects (see `Vm::construct_done`).
    pub capacity: Cell<u32>,
    /// The property in each slot.
    pub keys: Vec<Atom>,
    /// Each slot's attributes ([`READ_ONLY`], [`DONT_ENUM`], [`DONT_DELETE`];
    /// 0 for an ordinary property).
    pub attrs: Vec<u8>,
    /// The type of the field in each slot, and whether it is constant (see
    /// [`FieldCell`]).
    pub fields: Vec<*const FieldCell>,
    /// The type of every element stored in objects of this shape. Unlike a field's
    /// type it is part of the shape, as V8's elements kinds are part of the
    /// map: an element store that doesn't fit moves the object to a shape
    /// with a more general type (`Vm::store_element`), so a shape guard
    /// also guarantees the elements' type. Holes are not elements.
    pub elems: FieldType,
    /// Owned by a single (prototype) object: transitions from it are not
    /// shared, so inline caches must not cache them.
    pub unique: bool,
    /// By the property added and its attributes.
    pub(crate) transitions: RefCell<Vec<(Atom, u8, *const Shape)>>,
    /// The shapes this one moves to when its elements' type generalizes.
    pub(crate) elem_transitions: RefCell<Vec<(FieldType, *const Shape)>>,
    /// Compiled code takes an object that has at least this shape for one
    /// that has exactly it, as the shape is stable ([`Shape::is_stable`]):
    /// a transition from it drops the code (`Vm::shape_left_stable`).
    pub(crate) relied: Cell<bool>,
}

/// Property attributes: not writable, not enumerable, not configurable
/// (`Object.defineProperty`'s defaults; ordinary properties have none).
pub const READ_ONLY: u8 = 1;
pub const DONT_ENUM: u8 = 2;
pub const DONT_DELETE: u8 = 4;
/// An accessor property: its slot holds an array of the getter and the
/// setter (`undefined` where missing). Inline caches never cache one; the
/// runtime calls them (see `Vm::get`, `Vm::set`).
pub const ACCESSOR: u8 = 8;

const _: () = assert!(std::mem::offset_of!(Shape, elems_code) == 0);
const _: () = assert!(std::mem::offset_of!(Shape, family) == 8);
const _: () = assert!(std::mem::offset_of!(Shape, push) == 16);

/// Entries of [`PushCache::to`]. DeltaBlue's collections start with
/// objects of several constructors.
pub const PUSH_WAYS: usize = 4;

/// [`Shape::push`]: an array of the shape, with no elements yet, moves to
/// `to[k].1` when a value whose key ([`push_key`]) is `to[k].0` is pushed
/// onto it (null: no entry; the latest first); `any`, if not null, is the
/// shape with elements of any type it moves to. The stencils read it.
#[repr(C)]
#[derive(Default)]
pub struct PushCache {
    pub to: [(u64, u64); PUSH_WAYS],
    pub any: u64,
}

impl Drop for Shape {
    fn drop(&mut self) {
        let p = self.push.get();
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

impl Shape {
    /// The shape's [`PushCache`], made if it has none.
    pub(crate) fn push_cache(&self) -> &mut PushCache {
        if self.push.get().is_null() {
            self.push.set(Box::into_raw(Box::default()));
        }
        unsafe { &mut *self.push.get() }
    }
}

/// A value's key for [`PushCache`]: 1 for an int32, 2 for another
/// number, an object's shape, else 0 (its type is `Any`). Values of one key
/// have one [`FieldType`].
pub fn push_key(v: Value) -> u64 {
    if v.is_int32() {
        1
    } else if v.is_number() {
        2
    } else if v.is_cell() {
        unsafe { (*as_object(v)).shape as u64 }
    } else {
        0
    }
}

impl Shape {
    pub fn lookup(&self, name: Atom) -> Option<u32> {
        self.keys.iter().position(|&k| k == name).map(|i| i as u32)
    }

    /// Whether no object of this shape has gained a property or moved to
    /// more general elements yet (V8's stable maps): an object known to have
    /// at least this shape has exactly it. Not for a prototype's (unique)
    /// shape, nor for arrays and strings, whose elements' type changes in
    /// compiled code (`array_push`).
    pub fn is_stable(&self) -> bool {
        !self.unique
            && !matches!(self.class, Class::Array | Class::String)
            && self.transitions.borrow().is_empty()
            && self.elem_transitions.borrow().is_empty()
    }

    /// The type of the field in slot `i`.
    pub fn field_type(&self, i: u32) -> FieldType {
        self.field_cell(i).get()
    }

    /// The type of the field in slot `i`, for the code of the function
    /// being compiled, which relies on it (see `FieldCell::deps`).
    pub fn field_type_relied(&self, i: u32) -> FieldType {
        let cell = self.field_cell(i);
        let code = COMPILING.with(Cell::get);
        if code != 0 {
            cell.relied.set(true);
            let vm = unsafe { &mut *crate::interp::current_vm() };
            let deps = vm.field_deps.entry(cell as *const FieldCell as usize).or_default();
            if deps.last() != Some(&code) && !deps.contains(&code) {
                deps.push(code);
                vm.relied_cells.entry(code).or_default().push(cell as *const FieldCell as usize);
            }
        }
        cell.get()
    }

    /// The cell of the field in slot `i`.
    pub fn field_cell(&self, i: u32) -> &FieldCell {
        unsafe { &*self.fields[i as usize] }
    }

    /// Where slot `i` lives, as an inline cache records it: the byte offset
    /// of an in-object slot, or `!index` into the out-of-line slots.
    pub fn location(&self, i: u32) -> i32 {
        if i < self.capacity.get() {
            (INLINE_BASE + 8 * i as usize) as i32
        } else {
            !((i - self.capacity.get()) as i32)
        }
    }
}

/// A cell (heap pointer) value as an object. All cells are objects so far.
#[inline(always)]
pub fn as_object(v: Value) -> *mut Object {
    v.to_bits() as *mut Object
}

#[inline(always)]
pub fn object_value(o: *mut Object) -> Value {
    Value::from_bits(o as u64)
}

pub(crate) use crate::heap::PAGE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations() {
        let s = Shape {
            class: Class::Plain,
            proto: Value::NULL,
            capacity: Cell::new(2),
            keys: vec![Atom(7), Atom(8), Atom(9)],
            attrs: vec![0; 3],
            fields: Vec::new(),
            elems: FieldType::None,
            elems_code: 0,
            family: std::ptr::null(),
            push: Cell::new(std::ptr::null_mut()),
            unique: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
            relied: Cell::new(false),
        };
        assert_eq!(s.location(0), 48);
        assert_eq!(s.location(1), 56);
        assert_eq!(s.location(2), !0);
        assert_eq!(s.lookup(Atom(9)), Some(2));
        let mut heap = crate::heap::Heap::new();
        let o = heap.alloc_object(&s);
        assert!(object_value(o).is_cell());
        unsafe {
            *Object::slot(o, s.location(1)) = Value::from_i32(5);
            assert_eq!(*Object::inline(o, 1), Value::from_i32(5));
            assert_eq!((*o).flags & EXTERNAL, EXTERNAL);
        }
    }
}
