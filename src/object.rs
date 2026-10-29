//! Objects, shapes and the heap (step 1 of `docs/object-model.md`'s plan).
//!
//! Every object is a heap cell with a fixed header and in-object slots:
//!
//! ```text
//!  0  shape       *const Shape     names → slots, prototype, inline capacity
//!  8  class u8, flags u8, inline_cap u16, elems_len u32
//! 16  props       *mut Value       out-of-line slots beyond the inline ones
//! 24  elems       *mut Value       indexed elements (arrays, mostly)
//! 32  code        *const Code      not in plain objects and arrays
//! 40  aux u32                      in buffers, views, a few more
//! 32, 40 or 48  inline slots …
//! ```
//!
//! The capacities of `props` and `elems` are their cells' (an array of
//! values' header word, `VALUES | cap << 32`). Plain objects and arrays have
//! no `code` (what most programs make most of: 16 bytes less each); every
//! other class has it, and a few an `aux` word too, so where the slots
//! start depends on the class (`layout::slots_offset`).
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

/// What an object is (defined with the layouts compiled code shares).
pub use crate::layout::Class;

impl Class {
    /// Whether an object of this class has no elements at all (a
    /// buffer's or data view's `elems` are bytes, a collection's and an
    /// iterator's none): to JS, no own indexed properties.
    #[inline(always)]
    pub fn no_elements(self) -> bool {
        matches!(
            self,
            Class::ArrayBuffer
                | Class::SharedArrayBuffer
                | Class::Temporal
                | Class::Map
                | Class::Set
                | Class::WeakMap
                | Class::WeakSet
                | Class::DataView
                | Class::CollectionIterator
                | Class::Promise
                | Class::Symbol
                | Class::BigInt
                | Class::ArrayIterator
                | Class::IteratorHelper
                | Class::DisposableStack
                | Class::Generator
                | Class::Proxy
                | Class::WeakRef
        )
    }
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
    /// `BigInt64Array`'s: BigInts (see [`TypedKind::is_big`]).
    I64,
    U64,
    /// `Float16Array`'s: IEEE binary16 (`math::f16_bits`).
    F16,
}

impl TypedKind {
    pub const ALL: [TypedKind; 12] = [
        TypedKind::I8,
        TypedKind::U8,
        TypedKind::U8Clamped,
        TypedKind::I16,
        TypedKind::U16,
        TypedKind::I32,
        TypedKind::U32,
        TypedKind::F32,
        TypedKind::F64,
        TypedKind::I64,
        TypedKind::U64,
        TypedKind::F16,
    ];

    /// Whether its elements are BigInts (which compiled code leaves to
    /// the runtime: their loads allocate, their stores convert with
    /// ToBigInt).
    pub fn is_big(self) -> bool {
        matches!(self, TypedKind::I64 | TypedKind::U64)
    }

    /// Whether compiled code loads and stores its elements itself (not a
    /// BigInt kind's, nor binary16's, which the runtime converts).
    pub fn compiled(self) -> bool {
        !matches!(self, TypedKind::I64 | TypedKind::U64 | TypedKind::F16)
    }

    pub fn from_u8(k: u8) -> TypedKind {
        TypedKind::ALL[k as usize]
    }

    /// Bytes per element.
    pub fn size(self) -> usize {
        match self {
            TypedKind::I8 | TypedKind::U8 | TypedKind::U8Clamped => 1,
            TypedKind::I16 | TypedKind::U16 | TypedKind::F16 => 2,
            TypedKind::I32 | TypedKind::U32 | TypedKind::F32 => 4,
            TypedKind::F64 | TypedKind::I64 | TypedKind::U64 => 8,
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
            TypedKind::I64 => "BigInt64Array",
            TypedKind::U64 => "BigUint64Array",
            TypedKind::F16 => "Float16Array",
        }
    }

    /// Element `i` at `p` (the first element), as a number (a BigInt,
    /// allocated, of a BigInt kind).
    ///
    /// # Safety
    /// `p` must point to at least `i + 1` elements of this kind.
    pub unsafe fn load(self, p: *const u8, i: usize) -> Value {
        use crate::racy::load;
        unsafe {
            let at = p.add(i * self.size());
            match self {
                TypedKind::I8 => Value::from_i32(load::<1>(at) as i8 as i32),
                TypedKind::U8 | TypedKind::U8Clamped => Value::from_i32(load::<1>(at) as i32),
                TypedKind::I16 => Value::from_i32(load::<2>(at) as i16 as i32),
                TypedKind::U16 => Value::from_i32(load::<2>(at) as i32),
                TypedKind::I32 => Value::from_i32(load::<4>(at) as i32),
                TypedKind::U32 => Value::number(load::<4>(at) as u32 as f64),
                TypedKind::F32 => Value::number(f32::from_bits(load::<4>(at) as u32) as f64),
                TypedKind::F64 => Value::number(f64::from_bits(load::<8>(at))),
                TypedKind::F16 => Value::number(crate::math::f16_value(load::<2>(at) as u16)),
                TypedKind::I64 | TypedKind::U64 => crate::bigint::from_bits(load::<8>(at), self == TypedKind::I64),
            }
        }
    }

    /// Store int32 `v` as element `i` at `p`: as [`TypedKind::store`] of
    /// it, without the conversions through `f64`.
    ///
    /// # Safety
    /// As [`TypedKind::load`].
    pub unsafe fn store_i32(self, p: *mut u8, i: usize, v: i32) {
        use crate::racy::store;
        unsafe {
            let at = p.add(i * self.size());
            match self {
                TypedKind::I8 | TypedKind::U8 => store::<1>(at, v as u64),
                TypedKind::U8Clamped => store::<1>(at, v.clamp(0, 255) as u64),
                TypedKind::I16 | TypedKind::U16 => store::<2>(at, v as u64),
                TypedKind::I32 | TypedKind::U32 => store::<4>(at, v as u32 as u64),
                TypedKind::F32 => store::<4>(at, (v as f32).to_bits() as u64),
                TypedKind::F64 => store::<8>(at, (v as f64).to_bits()),
                TypedKind::F16 => store::<2>(at, crate::math::f16_bits(v as f64) as u64),
                // (Not for BigInts: see `store_value`.)
                TypedKind::I64 | TypedKind::U64 => {}
            }
        }
    }

    /// Store number `d` (ToNumber of the value) as element `i` at `p`.
    ///
    /// # Safety
    /// As [`TypedKind::load`].
    pub unsafe fn store(self, p: *mut u8, i: usize, d: f64) {
        use crate::racy::store;
        let int = || crate::interp::to_int32(d) as u32 as u64;
        unsafe {
            let at = p.add(i * self.size());
            match self {
                TypedKind::I8 | TypedKind::U8 => store::<1>(at, int()),
                TypedKind::U8Clamped => {
                    // Round half to even, clamped to 0..=255 (NaN is 0).
                    let c = if d.is_nan() { 0.0 } else { d.clamp(0.0, 255.0) };
                    let r = c.round();
                    let r = if (c - c.floor()) == 0.5 && r % 2.0 != 0.0 { r - 1.0 } else { r };
                    store::<1>(at, r as u8 as u64);
                }
                TypedKind::I16 | TypedKind::U16 => store::<2>(at, int()),
                TypedKind::I32 | TypedKind::U32 => store::<4>(at, int()),
                TypedKind::F32 => store::<4>(at, (d as f32).to_bits() as u64),
                TypedKind::F64 => store::<8>(at, d.to_bits()),
                TypedKind::F16 => store::<2>(at, crate::math::f16_bits(d) as u64),
                TypedKind::I64 | TypedKind::U64 => {}
            }
        }
    }

    /// Store `v`, a number (a BigInt of a BigInt kind: see
    /// `builtins::to_element`), as element `i` at `p`.
    ///
    /// # Safety
    /// As [`TypedKind::load`].
    pub unsafe fn store_value(self, p: *mut u8, i: usize, v: Value) {
        unsafe {
            if self.is_big() {
                crate::racy::store::<8>(p.add(i * 8), crate::bigint::to_bits(v));
            } else {
                self.store(p, i, v.to_number());
            }
        }
    }
}

/// The object is reachable from outside the internal heap. Always set for
/// now (see the module docs). (Compiled code writes it.)
pub use crate::layout::EXTERNAL;
/// The object is some shape's prototype.
pub const PROTOTYPE: u8 = 2;
/// The object has integer keys too far past its elements, its elements
/// are sealed or frozen, an array's `length` is read-only. (Compiled code
/// reads them.)
pub use crate::layout::{FROZEN, LENGTH_READ_ONLY, SEALED, SPARSE};
/// The object takes no new properties (`Object.preventExtensions`).
pub const NOT_EXTENSIBLE: u8 = 8;
/// A function's `length` was deleted (it has no slot: see
/// `Vm::own_property`); `LENGTH_READ_ONLY`'s bit, as arrays aren't functions.
pub const FN_LENGTH_DELETED: u8 = 64;
/// A function's `name` was deleted.
pub const FN_NAME_DELETED: u8 = 128;
/// An `ArrayBuffer` detached (`$262.detachArrayBuffer`): its views throw;
/// `LENGTH_READ_ONLY`'s bit, as buffers aren't arrays.
pub const DETACHED: u8 = 64;
/// A typed array or `DataView` of an immutable buffer (see `typed`):
/// stores into it fail; `LENGTH_READ_ONLY`'s bit, as views aren't arrays.
pub const IMMUTABLE_VIEW: u8 = 64;
/// A plain object made by `JSON.rawJSON` ([[IsRawJSON]]);
/// `LENGTH_READ_ONLY`'s bit, as plain objects aren't arrays.
pub const RAW_JSON: u8 = 64;
/// A `RegExp` made by a subclass's constructor ([[LegacyFeaturesEnabled]]
/// false: see `builtins::note_legacy`); `LENGTH_READ_ONLY`'s bit, as
/// regular expressions aren't arrays.
pub const LEGACY_DISABLED: u8 = 64;
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
    pub elems_len: u32,
    pub props: *mut Value,
    pub elems: *mut Value,
    /// Past the header (`HEADER_BYTES`): only for the classes with it
    /// (`layout::extra_words`), a plain object's or an array's first slot
    /// otherwise.
    pub code: *const Code,
}

/// An object's header in bytes: where a plain object's or an array's
/// in-object slots start (see `layout::slots_offset`).
pub const HEADER_BYTES: usize = crate::layout::HEADER_BYTES;
const _: () = assert!(std::mem::offset_of!(Object, code) == HEADER_BYTES);
crate::layout::same_layout!(Object, crate::layout::Header:
    shape, class, flags, kind, cap_hi, elems_len, props, elems, code);

/// A new object's header, as words: `shape`, external, its in-object
/// capacity the shape's, no elements, nothing out of line. (A class's
/// words after it, `code` and `aux`, are the allocator's to zero.)
pub fn object_header(shape: &Shape) -> [u64; crate::layout::HEADER_WORDS] {
    let cap = shape.capacity.get();
    let cap = u16::try_from(cap).expect("in-object capacity above 65535");
    assert!(cap == 0 || shape.class != Class::TypedArray, "a typed array with in-object slots");
    [shape as *const Shape as u64, crate::layout::header_word(shape.class, EXTERNAL, cap as u64), 0, 0]
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

    /// The capacity of `this`'s out-of-line slots: their cell's (see
    /// `layout::props_cap`).
    ///
    /// # Safety
    /// `this` must be an object, not a string (whose `props` is a cons
    /// string's half).
    #[inline(always)]
    pub unsafe fn props_cap(this: *const Object) -> u32 {
        unsafe { crate::layout::props_cap(crate::layout::header(this)) }
    }

    /// The capacity of `this`'s elements: their cell's (see
    /// `layout::elems_cap`), less the last slot of a far array's, which
    /// holds its far store (see `far`).
    ///
    /// # Safety
    /// `this` must be an object whose `elems` are values (not a string, a
    /// typed array, a buffer or another class whose `elems` are bytes).
    #[inline(always)]
    pub unsafe fn elems_cap(this: *const Object) -> u32 {
        unsafe { crate::layout::elems_cap(crate::layout::header(this)) - crate::far::is_far(this.cast_mut()) as u32 }
    }

    /// The `aux` word of a class with one (`layout::extra_words` 2), after
    /// `code`: a buffer's maximum byte length, a view's length (or
    /// `typed::TRACKING`), a collection iterator's `IterMode` and stride, a
    /// BigInt's sign.
    ///
    /// # Safety
    /// `this` must be an object of such a class.
    #[inline(always)]
    pub unsafe fn aux(this: *mut Object) -> *mut u32 {
        unsafe {
            debug_assert_eq!(crate::layout::extra_words((*this).class), 2);
            this.cast::<u8>().add(HEADER_BYTES + 8).cast()
        }
    }

    /// The words of units after a string's `code` (see `heap::cell_size`):
    /// a flat one's, in `code`'s low half; a cons string's (`elems` null),
    /// whose `code` is its right half, none.
    ///
    /// # Safety
    /// `this` must be a string.
    #[inline(always)]
    pub unsafe fn string_words(this: *const Object) -> usize {
        unsafe { if (*this).elems.is_null() { 0 } else { (*this).code as usize as u32 as usize } }
    }

    /// Set [`Object::string_words`] (and no atom).
    ///
    /// # Safety
    /// `this` must be a new flat string.
    #[inline(always)]
    pub unsafe fn set_string_words(this: *mut Object, words: usize) {
        unsafe { (*this).code = u32::try_from(words).expect("string too long") as usize as *const Code }
    }

    /// A flat string's atom as a property key plus one, once found (0:
    /// none yet; see `Vm::property_key`): `code`'s high half.
    ///
    /// # Safety
    /// `this` must be a flat string (`elems` not null: a cons string's
    /// `code` is its right half).
    #[inline(always)]
    pub unsafe fn string_atom(this: *mut Object) -> *mut u32 {
        unsafe {
            debug_assert!((*this).class == Class::String && !(*this).elems.is_null());
            this.cast::<u8>().add(HEADER_BYTES + 4).cast()
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
        unsafe { this.cast::<u8>().add(crate::layout::slots_offset((*this).class)).cast::<Value>().add(i) }
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
            FieldType::None | FieldType::Any => crate::layout::FIELD_ANY,
            FieldType::Int => crate::layout::FIELD_INT,
            FieldType::Num => crate::layout::FIELD_NUM,
            FieldType::Obj(s) => s as u64,
        }
    }
}

/// Whether `v` fits a field type encoded by [`FieldType::encode`] (not 0).
#[inline(always)]
pub fn field_fits(want: u64, v: Value) -> bool {
    match want {
        crate::layout::FIELD_INT => v.is_int32(),
        crate::layout::FIELD_NUM => v.is_number(),
        s => v.is_cell() && unsafe { (*as_object(v)).shape } as u64 == s,
    }
}

/// A hidden class. `repr(C)`: compiled code reads its first fields
/// (`layout::ShapeHead`, checked against them below).
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
    /// Its slots: the property in each, its attributes and its field's
    /// cell, a prefix of a table its lineage shares (see [`Slots`]).
    pub slots: Slots,
    /// The type of every element stored in objects of this shape. Unlike a field's
    /// type it is part of the shape, as V8's elements kinds are part of the
    /// map: an element store that doesn't fit moves the object to a shape
    /// with a more general type (`Vm::store_element`), so a shape guard
    /// also guarantees the elements' type. Holes are not elements.
    pub elems: FieldType,
    /// Owned by a single (prototype) object: transitions from it are not
    /// shared, so inline caches must not cache them.
    pub unique: bool,
    /// Shared by objects that may not get properties (made not extensible:
    /// `Vm::fix_shape`): a transition from it, for a property added
    /// regardless (`Vm::transition_with`), is not shared either.
    pub fixed: bool,
    /// By the property added and its attributes.
    pub(crate) transitions: RefCell<Vec<(Atom, u8, *const Shape)>>,
    /// The shapes this one moves to when its elements' type generalizes.
    pub(crate) elem_transitions: RefCell<Vec<(FieldType, *const Shape)>>,
    /// Compiled code takes an object that has at least this shape for one
    /// that has exactly it, as the shape is stable ([`Shape::is_stable`]):
    /// a transition from it drops the code (`Vm::shape_left_stable`).
    pub(crate) relied: Cell<bool>,
    /// An object of this shape or of one made from it ([`Shape::parent`])
    /// lost a property or changed one's attributes or its prototype
    /// (`Vm::shape_left`): an object typed as having at least this shape
    /// (`Ty::Ext`, as from a field typed `FieldType::Obj`) may not have
    /// its slots, and the shape is never stable again.
    pub(crate) left: Cell<bool>,
    /// Compiled code takes an object typed as having at least this shape
    /// to have its slots, as no object has left it ([`Shape::left`]): one
    /// leaving drops the code.
    pub(crate) lineage_relied: Cell<bool>,
    /// The shape this one was made from (by a transition, or a copy of its
    /// own for an object), null for a root: objects that leave this one
    /// leave it too.
    pub(crate) parent: *const Shape,
    /// The latest full marking that found it live ([`crate::gc::code_epoch`],
    /// as `Code::mark`): an object of it was traced, it was made or given
    /// to an object since the marking began ([`set_shape`]), a live shape
    /// was made from it. One a full marking leaves older is dead: a
    /// zombie (see `Vm::sweep_shapes`).
    pub(crate) mark: std::sync::atomic::AtomicU32,
    /// The final pause that marked its prototype (and its ancestors'), by
    /// the marking's epoch (see `gc::trace_shape`).
    pub(crate) traced: Cell<u32>,
    /// Live whatever the marking finds: compiled code gives it to objects
    /// (an allocation's or a transition's constant, a `new`'s `this`).
    pub(crate) pinned: Cell<bool>,
}

/// A shape, owned like a box but in a slot of a chunk of shapes (each
/// thread's [`SHAPE_SLAB`]): freed into the chunk, for the next shape
/// made, rather than to the allocator. (Sass's 4.4M shapes that die, each
/// freed to `malloc` on its own, cost its collections' pauses twice as
/// long: cold memory, and `malloc_consolidate`.)
pub struct ShapeBox(std::ptr::NonNull<Shape>);

/// Shapes per chunk of [`SHAPE_SLAB`].
const SLAB_SHAPES: usize = 256;

/// Each thread's chunks of shapes (never returned to the allocator till
/// the thread ends) and their free slots.
#[derive(Default)]
pub struct ShapeSlab {
    chunks: Vec<*mut Shape>,
    free: Vec<*mut Shape>,
    /// The latest chunk's slots not used yet.
    next: usize,
}

thread_local! {
    static SHAPE_SLAB: RefCell<ShapeSlab> = RefCell::default();
}

impl ShapeSlab {
    fn layout() -> std::alloc::Layout {
        std::alloc::Layout::array::<Shape>(SLAB_SHAPES).unwrap()
    }
}

impl Drop for ShapeSlab {
    fn drop(&mut self) {
        for &c in &self.chunks {
            unsafe { std::alloc::dealloc(c.cast(), ShapeSlab::layout()) };
        }
    }
}

impl ShapeBox {
    pub fn new(s: Shape) -> ShapeBox {
        let p = SHAPE_SLAB.with_borrow_mut(|slab| {
            if let Some(p) = slab.free.pop() {
                return p;
            }
            if slab.next == 0 {
                let c = unsafe { std::alloc::alloc(ShapeSlab::layout()) }.cast::<Shape>();
                if c.is_null() {
                    std::alloc::handle_alloc_error(ShapeSlab::layout());
                }
                slab.chunks.push(c);
                slab.next = SLAB_SHAPES;
            }
            slab.next -= 1;
            unsafe { slab.chunks.last().unwrap().add(slab.next) }
        });
        unsafe { p.write(s) };
        ShapeBox(unsafe { std::ptr::NonNull::new_unchecked(p) })
    }
}

impl std::ops::Deref for ShapeBox {
    type Target = Shape;
    fn deref(&self) -> &Shape {
        unsafe { self.0.as_ref() }
    }
}

impl std::ops::DerefMut for ShapeBox {
    fn deref_mut(&mut self) -> &mut Shape {
        unsafe { self.0.as_mut() }
    }
}

impl Drop for ShapeBox {
    fn drop(&mut self) {
        let p = self.0.as_ptr();
        unsafe { std::ptr::drop_in_place(p) };
        // (No VM outlives its thread's slab: none is a thread-local.)
        SHAPE_SLAB.with_borrow_mut(|slab| slab.free.push(p));
    }
}

/// Keep shape `s` live for good: compiled code gives it to objects.
///
/// # Safety
/// `s` must be a live shape.
#[inline]
pub unsafe fn pin_shape(s: *const Shape) {
    unsafe { (*s).pinned.set(true) }
}

/// Shape `s` found live by a full marking under way (if any). The marker
/// reads its mark for every object of it: written only if older.
///
/// # Safety
/// `s` must be a live shape.
#[inline]
pub unsafe fn mark_shape(s: *const Shape) {
    use std::sync::atomic::Ordering::Relaxed;
    let epoch = crate::gc::code_epoch();
    unsafe {
        if (*s).mark.load(Relaxed) < epoch {
            (*s).mark.store(epoch, Relaxed);
        }
    }
}

/// Give object `o` shape `s`, which a full marking under way then finds
/// live: `o` may have been traced with its old shape (the collector's
/// barrier for shapes, which are Rust structures).
///
/// # Safety
/// `o` must be an object and `s` a live shape.
#[inline]
pub unsafe fn set_shape(o: *mut Object, s: *const Shape) {
    unsafe {
        // (Where the slots are depends on the class: `layout::slots_offset`.)
        debug_assert_eq!((*s).class, (*o).class);
        (*o).shape = s;
        mark_shape(s);
    }
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

crate::layout::same_layout!(Shape, crate::layout::ShapeHead: elems_code, family, push);

/// The slots of a lineage of shapes, as V8 shares a descriptor array along
/// a transition chain: each shape has a prefix of it ([`Slots`]), and a
/// transition from the shape that has all of it appends in place, rather
/// than each shape copying its parent's slots (memory quadratic in an
/// object's properties: 114 MiB of PdfJS's, its literals having thousands).
///
/// Append-only and within a capacity fixed when made: what a prefix holds
/// never moves or changes, so its slices stay valid while the table grows
/// past them; a full table is copied, twice as large.
pub struct SlotTable {
    /// The property in each slot.
    keys: *mut Atom,
    /// Each slot's attributes ([`READ_ONLY`], [`DONT_ENUM`], [`DONT_DELETE`],
    /// [`ACCESSOR`]; 0 for an ordinary property).
    attrs: *mut u8,
    /// The type of the field in each slot, and whether it is constant (see
    /// [`FieldCell`]).
    fields: *mut *const FieldCell,
    len: Cell<u32>,
    cap: u32,
    /// Slot by property, over the first `indexed` slots: a long prefix is
    /// searched there (a table's keys are distinct, a lineage's slots).
    index: RefCell<std::collections::HashMap<Atom, u32>>,
    indexed: Cell<u32>,
}

/// Prefixes up to this long are searched linearly (V8's limit too).
const LINEAR_LOOKUP: u32 = 8;

impl SlotTable {
    /// One allocation for a table's arrays (one each took sass three
    /// `malloc`s a shape): its field cells, keys and attributes, the
    /// offsets of the last two.
    fn layout(cap: u32) -> (std::alloc::Layout, usize, usize) {
        use std::alloc::Layout;
        let n = cap as usize;
        let (l, keys) = Layout::array::<*const FieldCell>(n).unwrap().extend(Layout::array::<Atom>(n).unwrap()).unwrap();
        let (l, attrs) = l.extend(Layout::array::<u8>(n).unwrap()).unwrap();
        (l, keys, attrs)
    }

    fn with_capacity(cap: u32) -> SlotTable {
        let (layout, keys, attrs) = SlotTable::layout(cap);
        let p = if layout.size() == 0 {
            std::ptr::NonNull::<*const FieldCell>::dangling().as_ptr().cast::<u8>()
        } else {
            let p = unsafe { std::alloc::alloc(layout) };
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            p
        };
        SlotTable {
            keys: unsafe { p.add(keys) }.cast(),
            attrs: unsafe { p.add(attrs) },
            fields: p.cast(),
            len: Cell::new(0),
            cap,
            index: RefCell::default(),
            indexed: Cell::new(0),
        }
    }

    fn push(&self, key: Atom, attrs: u8, field: *const FieldCell) {
        let n = self.len.get();
        assert!(n < self.cap);
        // (Past every prefix: no slice covers it.)
        unsafe {
            self.keys.add(n as usize).write(key);
            self.attrs.add(n as usize).write(attrs);
            self.fields.add(n as usize).write(field);
        }
        self.len.set(n + 1);
    }

    /// The field cells of all its slots (of the lineage's longest shape).
    pub(crate) fn all_fields(&self) -> &[*const FieldCell] {
        unsafe { std::slice::from_raw_parts(self.fields, self.len.get() as usize) }
    }

    /// The bytes it takes (for `Vm::memory_stats`).
    pub(crate) fn bytes(&self) -> usize {
        size_of::<SlotTable>()
            + self.cap as usize * (size_of::<Atom>() + 1 + size_of::<*const FieldCell>())
            + self.index.borrow().capacity() * (size_of::<(Atom, u32)>() + 1)
    }
}

impl Drop for SlotTable {
    fn drop(&mut self) {
        let (layout, ..) = SlotTable::layout(self.cap);
        if layout.size() != 0 {
            unsafe { std::alloc::dealloc(self.fields.cast(), layout) };
        }
    }
}

/// A shape's slots: the first `len` of a [`SlotTable`].
#[derive(Clone)]
pub struct Slots {
    table: std::rc::Rc<SlotTable>,
    len: u32,
}

impl Slots {
    /// No slots.
    pub fn empty() -> Slots {
        Slots { table: std::rc::Rc::new(SlotTable::with_capacity(0)), len: 0 }
    }

    /// These slots, in a table of their own.
    pub fn new(keys: &[Atom], attrs: &[u8], fields: &[*const FieldCell]) -> Slots {
        assert!(keys.len() == attrs.len() && keys.len() == fields.len());
        let n = keys.len() as u32;
        let table = SlotTable::with_capacity(n);
        for i in 0..keys.len() {
            table.push(keys[i], attrs[i], fields[i]);
        }
        Slots { table: std::rc::Rc::new(table), len: n }
    }

    /// These slots and one more: in place if they are all of their table
    /// and it has room, else in a copy (a branch of the lineage, or a full
    /// table) with room to grow.
    pub fn with(&self, key: Atom, attrs: u8, field: *const FieldCell) -> Slots {
        let t = &self.table;
        if t.len.get() == self.len && self.len < t.cap {
            t.push(key, attrs, field);
            return Slots { table: t.clone(), len: self.len + 1 };
        }
        let table = SlotTable::with_capacity((self.len + 1).max(4).next_power_of_two());
        for i in 0..self.len as usize {
            table.push(self.keys()[i], self.attrs()[i], self.fields()[i]);
        }
        table.push(key, attrs, field);
        Slots { table: std::rc::Rc::new(table), len: self.len + 1 }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn keys(&self) -> &[Atom] {
        unsafe { std::slice::from_raw_parts(self.table.keys, self.len as usize) }
    }

    pub fn attrs(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.table.attrs, self.len as usize) }
    }

    pub fn fields(&self) -> &[*const FieldCell] {
        unsafe { std::slice::from_raw_parts(self.table.fields, self.len as usize) }
    }

    /// The slot of `name`.
    pub fn lookup(&self, name: Atom) -> Option<u32> {
        if self.len <= LINEAR_LOOKUP {
            return self.keys().iter().position(|&k| k == name).map(|i| i as u32);
        }
        let t = &self.table;
        let mut index = t.index.borrow_mut();
        let all = t.len.get();
        if t.indexed.get() < all {
            let keys = unsafe { std::slice::from_raw_parts(t.keys, all as usize) };
            for (i, &k) in keys.iter().enumerate().skip(t.indexed.get() as usize) {
                index.insert(k, i as u32);
            }
            t.indexed.set(all);
        }
        index.get(&name).copied().filter(|&i| i < self.len)
    }

    /// Whether `other`'s slots are the first of these.
    pub fn starts_with(&self, other: &Slots) -> bool {
        other.len <= self.len
            && (std::rc::Rc::ptr_eq(&self.table, &other.table)
                || (self.keys().starts_with(other.keys())
                    && self.attrs().starts_with(other.attrs())
                    && self.fields().starts_with(other.fields())))
    }

    /// The table, to count it once (`Vm::memory_stats`).
    pub(crate) fn table(&self) -> &SlotTable {
        &self.table
    }
}

pub use crate::layout::{PUSH_WAYS, PushCache};

impl Drop for Shape {
    fn drop(&mut self) {
        let p = self.push.get();
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

impl Shape {
    /// `f` of the shape's [`PushCache`], made if it has none. (A borrow
    /// that can't outlive `f`: no two overlap. Compiled code reads the
    /// cache through `push`, never while this runs.)
    pub(crate) fn with_push_cache<T>(&self, f: impl FnOnce(&mut PushCache) -> T) -> T {
        if self.push.get().is_null() {
            self.push.set(Box::into_raw(Box::default()));
        }
        f(unsafe { &mut *self.push.get() })
    }
}

/// A value's key for [`PushCache`]: 1 for an int32, 2 for another
/// number, an object's shape, else 0 (its type is `Any`). Values of one key
/// have one [`FieldType`].
pub fn push_key(v: Value) -> u64 {
    if v.is_int32() {
        crate::layout::FIELD_INT
    } else if v.is_number() {
        crate::layout::FIELD_NUM
    } else if v.is_cell() {
        unsafe { (*as_object(v)).shape as u64 }
    } else {
        crate::layout::FIELD_ANY
    }
}

impl Shape {
    pub fn lookup(&self, name: Atom) -> Option<u32> {
        self.slots.lookup(name)
    }

    /// Whether a full collection found no object could have it (see
    /// `Vm::sweep_shapes`).
    pub fn is_zombie(&self) -> bool {
        self.capacity.get() == u32::MAX
    }

    /// The property in each slot.
    pub fn keys(&self) -> &[Atom] {
        self.slots.keys()
    }

    /// Each slot's attributes.
    pub fn attrs(&self) -> &[u8] {
        self.slots.attrs()
    }

    /// Each slot's field cell.
    pub fn fields(&self) -> &[*const FieldCell] {
        self.slots.fields()
    }

    /// Whether no object of this shape has gained a property or moved to
    /// more general elements yet (V8's stable maps): an object known to have
    /// at least this shape has exactly it. Not for a prototype's (unique)
    /// shape, nor for arrays and strings, whose elements' type changes in
    /// compiled code (`array_push`).
    pub fn is_stable(&self) -> bool {
        !self.unique
            && !self.left.get()
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
        unsafe { &*self.fields()[i as usize] }
    }

    /// Where slot `i` lives, as an inline cache records it: the byte offset
    /// of an in-object slot, or `!index` into the out-of-line slots.
    pub fn location(&self, i: u32) -> i32 {
        if i < self.capacity.get() {
            (crate::layout::slots_offset(self.class) + 8 * i as usize) as i32
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
            slots: Slots::new(&[Atom(7), Atom(8), Atom(9)], &[0; 3], &[std::ptr::null(); 3]),
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
        };
        assert_eq!(s.location(0), 32);
        assert_eq!(s.location(1), 40);
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
