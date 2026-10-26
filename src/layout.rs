//! The memory compiled code shares with the runtime: cells, the heap's
//! side tables, inline caches, call sites, compiled regular expressions.
//!
//! The stencils (`stencils/stencils.rs`) are compiled on their own by
//! `build.rs`, without the runtime's crate, so what they read and write of
//! the runtime's memory is defined here, in a file both include (as
//! `value.rs`), and nowhere else. A type compiled code reads all of, or
//! that holds nothing but numbers and pointers, is defined here and used
//! by the runtime as is. A runtime type compiled code reads part of keeps
//! its own definition, with a mirror here whose fields [`same_layout!`]
//! checks at compile time against it, beside the runtime's definition.
//!
//! Stencils read cells through raw pointers: every accessor here is
//! `unsafe`, its contract that the pointer is to a live cell (or table)
//! of the kind it names.

use crate::value::Value;

/// Check at compile time that runtime type `$t` has `$m`'s fields at the
/// same offsets, `$m` a mirror of it or of its first fields. Invoked where
/// `$t`'s fields are visible.
macro_rules! same_layout {
    ($t:ty, $m:ty: $($f:ident),+ $(,)?) => {
        const _: () = {
            $(assert!(core::mem::offset_of!($t, $f) == core::mem::offset_of!($m, $f));)+
            assert!(size_of::<$m>() <= size_of::<$t>());
        };
    };
}
#[allow(unused_imports)]
pub(crate) use same_layout;

// ---- Cells ----

/// What an object is: its header's byte 8 (`Header::class`).
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
    /// element type (`object::TypedKind`), and `code` holds its buffer.
    TypedArray,
    /// A `RegExp`: `code` holds its compiled `regexp::Regex`.
    RegExp,
    /// A symbol (a primitive: see `symbol`): its key atom in `elems_len`,
    /// its description in `code`'s bits.
    Symbol,
    /// An array's or a string's iterator: `code`'s bits are its state, a
    /// hidden array (see `symbol::new_iterator`).
    ArrayIterator,
    /// A generator object: `code`'s bits are its state, a hidden array
    /// (see `generator`).
    Generator,
    /// A wrapper object (`new Number(1)`, `new String("s")`, `new
    /// Boolean(true)`): `code`'s bits are the primitive (a value, which the
    /// collector traces: a string is a cell).
    Wrapper,
    /// A `Map`: `code`'s bits are a hidden array holding a header (its hash
    /// index, …) and its entries, `k0, v0, k1, v1, …` (a deleted entry's
    /// slots are holes). See `collections`.
    Map,
    /// A `Set`: as [`Class::Map`], the entries `k0, k1, …`.
    Set,
    /// A `WeakMap`: as [`Class::Map`], its keys held weakly (see
    /// `collections`).
    WeakMap,
    /// A `WeakSet`: as [`Class::Set`].
    WeakSet,
    /// A `DataView`: laid out as a [`Class::TypedArray`] (`elems` points
    /// to its first byte in its buffer, `elems_len` is its byte length,
    /// `code` holds its buffer), with no element type.
    DataView,
    /// A `Map` or `Set` iterator: `code`'s bits are the entries array it
    /// walks, `elems_len` its next position, `elems_cap` what it yields
    /// (see `collections::IterMode`).
    CollectionIterator,
    /// A `Promise`: `code`'s bits are a hidden array holding its state,
    /// result and reactions. See `promise`.
    Promise,
    /// A proxy whose target isn't callable (one whose target is, is a
    /// native function): see `proxy`.
    Proxy,
    /// A `WeakRef`: as a [`Class::WeakSet`] of its target alone (cleared
    /// once the target dies).
    WeakRef,
    /// A `FinalizationRegistry`: as a [`Class::WeakMap`] of its
    /// unregister tokens' registration counts, its cleanup callback in
    /// slot 0 (see `collections`).
    FinalizationRegistry,
    /// A BigInt (a primitive: see `bigint`): its sign in `elems_cap`, its
    /// magnitude's 32-bit limbs in `elems` (a byte buffer), their count in
    /// `elems_len`.
    BigInt,
    /// An iterator helper, or `Iterator.from`'s wrapper: `code`'s bits
    /// are its state, a hidden array (see `iterator`).
    IteratorHelper,
    /// A `DisposableStack` or `AsyncDisposableStack`: `code`'s bits are
    /// its state, a hidden array (see `dispose`).
    DisposableStack,
    /// A `SharedArrayBuffer`: as [`Class::ArrayBuffer`], its bytes in a
    /// block outside the heap, which agents share (see `typed::Block`).
    SharedArrayBuffer,
    /// A Temporal object: its internal slots in bytes `elems` points to
    /// (see `temporal`).
    Temporal,
    /// An `Intl` object (an `Intl.Collator`): its internal slots in hidden
    /// in-object slots, beyond its shape's capacity, as a native closure's
    /// data (see `promise::native_closure`, and `intl`).
    Intl,
}

/// [`Class`] numbers as a header's byte holds them, for patterns.
pub mod class {
    use super::Class;
    pub const PLAIN: u8 = Class::Plain as u8;
    pub const ARRAY: u8 = Class::Array as u8;
    pub const FUNCTION: u8 = Class::Function as u8;
    pub const NATIVE: u8 = Class::Native as u8;
    pub const STRING: u8 = Class::String as u8;
    pub const DATE: u8 = Class::Date as u8;
    pub const TYPED_ARRAY: u8 = Class::TypedArray as u8;
    pub const SYMBOL: u8 = Class::Symbol as u8;
    pub const BIGINT: u8 = Class::BigInt as u8;
}

/// Flag of [`Header::flags`]: the object is reachable from outside the
/// internal heap (`object::EXTERNAL`, with the other flags).
pub const EXTERNAL: u8 = 1;
/// Flag: the object has integer keys too far past its elements to store
/// there: named properties (their decimal strings), which keyed accesses
/// check after the elements (`object::SPARSE`; see `Vm::set_keyed`).
pub const SPARSE: u8 = 4;
/// Flag: its elements can't be deleted (`Object.seal`).
pub const SEALED: u8 = 16;
/// Flag: its elements can't be written either (`Object.freeze`).
pub const FROZEN: u8 = 32;
/// Flag: an array's `length` is read-only (`Object.freeze`, or
/// `Object.defineProperty` with `writable: false`).
pub const LENGTH_READ_ONLY: u8 = 64;
/// Flag: an array's `length` isn't `elems_len`, which counts its dense
/// prefix only: it and the elements past the prefix are in the far store
/// (`far::FAR`). The bit is another flag in functions, errors and buffers.
pub const FAR: u8 = 128;

/// An object's header: `object::Object`'s fields, as compiled code sees
/// them (the shape and the code as addresses). Its in-object slots follow
/// ([`INLINE_BASE`]).
#[repr(C)]
pub struct Header {
    pub shape: u64,
    /// A [`Class`], as a byte: compiled code compares it with [`class`]'s
    /// numbers (as a `Class`, every load would assert it is one).
    pub class: u8,
    pub flags: u8,
    /// A typed array's element type; for any other object the low byte of
    /// its in-object capacity.
    pub kind: u8,
    pub cap_hi: u8,
    /// The out-of-line slots' capacity.
    pub props_cap: u32,
    pub props: *mut Value,
    /// Elements, a string's units, a typed array's first element.
    pub elems: *mut u8,
    pub elems_len: u32,
    pub elems_cap: u32,
    pub code: u64,
}

/// Byte offset of an object's first in-object slot.
pub const INLINE_BASE: usize = size_of::<Header>();
/// An object's header in words: a cell for one has this many and its
/// in-object slots.
pub const HEADER_WORDS: usize = INLINE_BASE / 8;

/// The header of the cell at `p`.
#[inline(always)]
pub fn header<T>(p: *const T) -> *mut Header {
    p as *mut Header
}

/// The header of cell `v`.
#[inline(always)]
pub fn cell(v: Value) -> *mut Header {
    v.to_bits() as *mut Header
}

/// The cell payload pointer `p` points into (an object's `props` or
/// `elems`, an upvalue): its header word comes first.
///
/// # Safety
/// `p` must point at a cell's payload.
#[inline(always)]
pub unsafe fn payload_cell<T>(p: *const T) -> *const u8 {
    unsafe { (p as *const u8).sub(8) }
}

/// The header's second word, from `class` through `props_cap`: an object
/// of class `class` with flags `flags` and `cap` in-object slots (below
/// 65536), nothing out of line.
#[inline(always)]
pub const fn header_word(class: Class, flags: u8, cap: u64) -> u64 {
    class as u64 | (flags as u64) << 8 | cap << 16
}

/// Write the header word of a new object at `h` ([`header_word`]).
///
/// # Safety
/// `h` must be a new object's header.
#[inline(always)]
pub unsafe fn set_header_word(h: *mut Header, word: u64) {
    unsafe { *(&raw mut (*h).class).cast::<u64>() = word }
}

/// The bits of [`Header`]'s third 32-bit word that hold `class` and `kind`
/// (`flags` masked off): a typed array of kind `k` has `TypedArray as u32
/// | k << 16` there.
pub const CLASS_KIND: u32 = 0x00ff_00ff;

/// The 32 bits of the header at `h` from `class`, masked by [`CLASS_KIND`].
///
/// # Safety
/// `h` must be an object's header.
#[inline(always)]
pub unsafe fn class_kind(h: *const Header) -> u32 {
    unsafe { *(&raw const (*h).class).cast::<u32>() & CLASS_KIND }
}

// ---- Shapes ----

/// `object::Shape`'s first fields, which compiled code reads.
#[repr(C)]
pub struct ShapeHead {
    /// The elements' type, encoded for store checks (`FIELD_*`, or a
    /// shape's address); [`NO_ELEMENTS`] for an array with none yet.
    pub elems_code: u64,
    /// The shape this one is a variant of by its elements' type alone
    /// (`family_jeq`).
    pub family: u64,
    /// Element transitions pushes make, or null.
    pub push: *const PushCache,
}

/// The shape of the object at `p`.
///
/// # Safety
/// `p` must be an object.
#[inline(always)]
pub unsafe fn shape_of<T>(p: *const T) -> *const ShapeHead {
    unsafe { (*header(p)).shape as *const ShapeHead }
}

/// A field type encoded for a store's check (`object::FieldType::encode`):
/// no check.
pub const FIELD_ANY: u64 = 0;
/// An int32.
pub const FIELD_INT: u64 = 1;
/// A number.
pub const FIELD_NUM: u64 = 2;
/// An elements' type code (`ShapeHead::elems_code`) that no value fits:
/// not a field type (0–2) nor a shape's address.
pub const NO_ELEMENTS: u64 = 3;

/// Entries of [`PushCache::to`]. DeltaBlue's collections start with
/// objects of several constructors.
pub const PUSH_WAYS: usize = 4;

/// `Shape::push`: an array of the shape, with no elements yet, moves to
/// `to[k].1` when a value whose key (`object::push_key`) is `to[k].0` is
/// pushed onto it (null: no entry; the latest first); `any`, if not null,
/// is the shape with elements of any type it moves to.
#[repr(C)]
#[derive(Default)]
pub struct PushCache {
    pub to: [(u64, u64); PUSH_WAYS],
    pub any: u64,
}

// ---- The heap ----

/// Allocation granule: cell addresses and sizes are multiples of it.
pub const GRANULE: usize = 16;
/// Bytes per card.
/// (128 rather than 512: a minor collection scans what dirty cards hold,
/// and a random store dirties a fourth as much. cards.js's longest pause
/// 33 ms -> 25 ms.)
pub const CARD: usize = 1 << 7;
/// Cells larger than this get blocks of their own.
pub const LARGE: usize = 8 << 10;

/// Metadata byte of a cell's first granule: allocated since the last
/// collection.
pub const YOUNG: u8 = 0x80;
/// Metadata bits of an old cell's first granule: the color it was marked
/// with (never 0).
pub const COLOR: u8 = 0x03;

/// Header word of an array of values: `VALUES | capacity << 32`.
pub const VALUES: u64 = 1;

/// Where the side tables are, past their share of the reservation's
/// addresses: a granule's metadata byte at `SIDE + addr / 16`, a card at
/// `SIDE + addr / 128` (see [`meta`], [`card`]). One constant, which arm64
/// builds in one instruction, so the barrier needs no operand. (macOS
/// grants addresses asked for only from 8 TiB up.)
pub const SIDE: usize = 1 << 43;
/// Address space one heap may use (`CLOISON_HEAP_LIMIT`, in MiB, lowers
/// it), aligned to it: the metadata of its first granule is the heap's
/// current color ([`color_byte`]).
pub const SLICE: usize = 32 << 30;

/// The metadata byte of the granule at `a` (in a heap).
#[inline(always)]
pub fn meta(a: usize) -> *mut u8 {
    (SIDE + a / GRANULE) as *mut u8
}

/// The card byte of address `a` (in a heap).
#[inline(always)]
pub fn card(a: usize) -> *mut u8 {
    (SIDE + a / CARD) as *mut u8
}

/// Mark the card of `a`. A minor collection scans the old cells' words on
/// dirty cards, and the whole of an object that starts on one: so for a
/// store to an object's in-object slot, or to its `props` or `elems`, `a`
/// may be the object's own address (what compiled code passes).
#[inline(always)]
pub fn card_mark<T>(a: *const T) {
    unsafe { *card(a as usize) = 1 };
}

/// The byte holding the current color of the heap address `a` is in: the
/// metadata of its slice's first granule (a block of no cells).
#[inline(always)]
pub fn color_byte(a: usize) -> *mut u8 {
    meta(a & !(SLICE - 1))
}

/// `heap::Heap`'s first words, which compiled code bumps
/// (`Heap::bump`): the free space's next byte and its end.
#[repr(C)]
pub struct HeapPtrs {
    pub ptr: *mut u64,
    pub end: *mut u64,
}

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

// ---- Inline caches ----

/// Entries of `Vm::megas` and `Vm::mega_stores` past the first (a power of
/// two).
pub const MEGAS: usize = 2048;

/// An entry of `Vm::megas`: a named load's data property, by the
/// receiver's shape and the name, for compiled code to probe inline (the
/// `get_named_rt` stencils, at sites given up on as megamorphic): in the
/// receiver at `off` bytes (`!off` bytes into its out-of-line slots if
/// negative) if `addr` is 0, else at `addr`, a prototype's slot, while
/// the prototype epoch is still `epoch`. Entry 0 holds, in `addr`, the
/// address of `Vm::proto_epoch` instead.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MegaEntry {
    pub shape: u64,
    pub name: u32,
    pub epoch: u32,
    pub off: i32,
    pub _pad: u32,
    pub addr: u64,
}

impl MegaEntry {
    pub const EMPTY: MegaEntry = MegaEntry {
        shape: 0,
        name: 0,
        epoch: 0,
        off: 0,
        _pad: 0,
        addr: 0,
    };
}

/// An entry of `Vm::mega_stores`: how compiled code stores a named
/// property (the `set_named_rt` stencils, at sites given up on as
/// megamorphic), by the receiver's shape and the name, as the interpreter's
/// store cache does (`interp::op_set_named`): into the slot at `loc` if the
/// value fits field type `ft` (`FIELD_*`, or a shape), moving the object to
/// `new_shape` first if not null, while the prototype epoch is `epoch` and
/// the object has room. Entry 0 holds, in `new_shape`, the address of
/// `Vm::proto_epoch`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MegaStore {
    pub shape: u64,
    pub name: u32,
    pub epoch: u32,
    pub ft: u64,
    pub new_shape: u64,
    pub loc: i32,
    pub _pad: u32,
}

impl MegaStore {
    pub const EMPTY: MegaStore = MegaStore {
        shape: 0,
        name: 0,
        epoch: 0,
        ft: 0,
        new_shape: 0,
        loc: 0,
        _pad: 0,
    };
}

/// The entry of `shape` and `name` in `Vm::megas` and `Vm::mega_stores`.
#[inline(always)]
pub fn mega_index(shape: u64, name: u32) -> usize {
    // (The product's top bits, which every bit of the shape and the name
    // reach: shapes come at strides that left the low bits few values, and
    // TypeScript's syntax tree's stores collided in three of four probes.)
    1 + ((shape ^ (name as u64) << 32).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - MEGAS.trailing_zeros())) as usize
}

/// The other way of entry `k` of `Vm::mega_stores`.
#[inline(always)]
pub fn mega_pair(k: usize) -> usize {
    ((k - 1) ^ 1) + 1
}

/// An entry of an `instanceof` site's cache (two entries; `jit`'s
/// `Prop::InstanceOf`): a receiver of shape `shape` against prototype
/// `proto` gives `result`. A shape fixes its prototype chain.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct InstanceOfEntry {
    pub shape: u64,
    pub proto: u64,
    pub result: u64,
}

// ---- Calls ----

/// The callee's register's offset in bytes below a frame's registers
/// (`interp::FUNC_OFF`): compiled code reads a function's upvalues
/// through it.
pub const FUNC_OFF: usize = 80;

/// The header slot an exit of compiled code writes its code's address to
/// (`interp::OSR_PC`'s offset in bytes below a frame's registers, a slot
/// read only where a tier-up request wrote it): an exit of a direct call
/// is decoded with the version that ran, not the one its site binds now.
pub const EXIT_CODE_OFF: usize = 16;

/// `jit::CallSite`'s first fields, which `call_direct` and
/// `construct_direct` read.
#[repr(C)]
pub struct DirectSite {
    /// The callee's compiled entry; 0: call through the runtime.
    pub entry: u64,
    /// The function compiled: the callee register must hold it.
    pub closure: u64,
    pub f_off: u64,
    /// Where the callee's compiled frame ends: before `stack_end`.
    pub end_off: u64,
    pub stack_end: u64,
    pub native_limit: u64,
    pub this_shape: u64,
    /// For `new`: how to allocate `this` (null if not a direct `new`).
    pub ctor: *mut CtorSite,
    /// Sites for other callees, by the callee's address (bits 4 and up),
    /// or null (`jit::Dispatch`).
    pub table: *const *const DirectSite,
}

/// Slots of a dispatch table (`DirectSite::table`).
pub const DISPATCH: usize = 1024;

/// The slot of callee `key` (a function, or a code) in a dispatch table.
#[inline(always)]
pub fn dispatch_slot(key: u64) -> usize {
    (key >> 4) as usize % DISPATCH
}

/// How `construct_direct` allocates `this` for a direct `new` (see
/// `jit::Lowering::ctor_site`): from the heap's free space, as long as
/// what the constructor's root shape depends on (`Vm::ctor_root`) is
/// unchanged: the constructor's `prototype` slot and its objects'
/// in-object capacity. Otherwise, or when the heap's chunk is full,
/// `cloison_rt_construct` allocates.
#[repr(C)]
#[derive(Clone, PartialEq)]
pub struct CtorSite {
    /// The heap's free space (`Heap::bump`).
    pub heap: *mut HeapPtrs,
    /// The constructor's `prototype` slot, and its value then.
    pub proto_at: *const u64,
    pub proto: u64,
    /// The constructor's `Code::ctor_capacity`, and its value then.
    pub cap_at: *const u32,
    pub cap: u64,
    /// In-object slots of the shape, set to `undefined`.
    pub slots: u64,
    /// Where `this` goes: the callee frame's `r0`, in bytes from the
    /// caller's registers.
    pub this_off: u64,
    /// A shape for which `Vm::construct_done` has nothing to do: the
    /// constructor's objects end with it (see `cloison_rt_construct_done`).
    pub done_shape: u64,
    /// The header of `this`.
    pub header: [u64; INLINE_BASE / 8],
}

// ---- Compiled regular expressions ----

/// What compiled regular expressions (`jit::rx`, the `rx_*` stencils)
/// share with the code that runs them.
pub mod rx {
    /// A run's context.
    #[repr(C)]
    pub struct Ctx {
        /// The backtracking stack's end, less room for two entries.
        pub limit: *mut usize,
        /// Classes past ASCII, by `Class`'s `k`.
        pub classes: *const Class,
        /// Whether case is folded: a unit past ASCII in a class or a
        /// folded character bails.
        pub ic: usize,
    }

    /// A class's ranges, for units past ASCII.
    #[repr(C)]
    pub struct Class {
        pub ranges: *const [u16; 2],
        pub n: usize,
        /// Negated (bit 0), and matching a unit only where it is no
        /// surrogate pair's half (bit 1: `regexp::Insn::Alone`; a surrogate
        /// bails).
        pub neg: usize,
    }

    /// A repetition of one unit (`Run`, `LazyRun`) and its backtracking
    /// piece.
    #[repr(C)]
    #[derive(Default)]
    pub struct Run {
        pub min: usize,
        pub max: usize,
        /// The backtracking piece's address.
        pub back: usize,
        /// The unit: a class's ASCII bits and `k`; a character in `k`
        /// (folded: `c | canon << 16`).
        pub lo: u64,
        pub hi: u64,
        pub k: usize,
    }

    /// A counted repetition's head (`Head`).
    #[repr(C)]
    #[derive(Default)]
    pub struct Head {
        pub c: usize,
        pub min: usize,
        pub max: usize,
        /// The addresses of what follows the loop and of its body.
        pub exit: usize,
        pub body: usize,
    }

    /// A backtracking stack entry restoring register `r`: `TAG | r`.
    pub const TAG: usize = 1 << 63;
    /// A group's start when it is unset.
    pub const NONE: usize = usize::MAX;
    /// What compiled code returns when the pattern doesn't match.
    pub const NOMATCH: usize = usize::MAX;
    /// What it returns to leave the attempt to the interpreter.
    pub const BAIL: usize = usize::MAX - 1;

    /// Compiled code's entry: `(s, end, pos, bt, regs, ctx)`.
    pub type Entry =
        unsafe extern "rust-preserve-none" fn(*const u16, usize, usize, *mut usize, *mut usize, *const Ctx) -> usize;
}
