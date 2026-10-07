//! Objects, shapes and the heap (step 1 of `docs/object-model.md`'s plan).
//!
//! Every object is a heap cell with a fixed header and in-object slots:
//!
//! ```text
//!  0  shape       *const Shape     names → slots, prototype, inline capacity
//!  8  class u8, flags u8, _ u16, props_cap u32
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
//! Every object has the external bit set: the internal heap (step 3) is
//! not there yet. There is no collector either. The heap is a bump arena
//! freed with the VM, and outgrown out-of-line arrays are abandoned in it.
//! Property attributes are not modelled: every property is a writable,
//! enumerable, configurable data property.

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
}

/// The object is reachable from outside the internal heap. Always set for
/// now (see the module docs).
pub const EXTERNAL: u8 = 1;
/// The object is some shape's prototype.
pub const PROTOTYPE: u8 = 2;

#[repr(C)]
pub struct Object {
    pub shape: *const Shape,
    pub class: Class,
    pub flags: u8,
    _pad: u16,
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

impl Object {
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
    /// A number: int32 or double.
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

/// A hidden class.
pub struct Shape {
    pub class: Class,
    /// An object, or null.
    pub proto: Value,
    /// Number of in-object slots.
    pub capacity: u32,
    /// The property in each slot.
    pub keys: Vec<Atom>,
    /// The type of the field in each slot (see [`FieldType`]).
    pub fields: Vec<*const Cell<FieldType>>,
    /// The type of every element stored in objects of this shape, and its
    /// encoding for store checks ([`FieldType::encode`]). Unlike a field's
    /// type it is part of the shape, as V8's elements kinds are part of the
    /// map: an element store that doesn't fit moves the object to a shape
    /// with a more general type (`Vm::store_element`), so a shape guard
    /// also guarantees the elements' type. Holes are not elements.
    pub elems: FieldType,
    pub elems_code: u64,
    /// Owned by a single (prototype) object: transitions from it are not
    /// shared, so inline caches must not cache them.
    pub unique: bool,
    pub(crate) transitions: RefCell<Vec<(Atom, *const Shape)>>,
    /// The shapes this one moves to when its elements' type generalizes.
    pub(crate) elem_transitions: RefCell<Vec<(FieldType, *const Shape)>>,
}

impl Shape {
    pub fn lookup(&self, name: Atom) -> Option<u32> {
        self.keys.iter().position(|&k| k == name).map(|i| i as u32)
    }

    /// The type of the field in slot `i`.
    pub fn field_type(&self, i: u32) -> FieldType {
        unsafe { (*self.fields[i as usize]).get() }
    }

    /// Where slot `i` lives, as an inline cache records it: the byte offset
    /// of an in-object slot, or `!index` into the out-of-line slots.
    pub fn location(&self, i: u32) -> i32 {
        if i < self.capacity {
            (INLINE_BASE + 8 * i as usize) as i32
        } else {
            !((i - self.capacity) as i32)
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

/// A bump allocator over 8-byte words. Memory is zeroed and lives as long
/// as the heap.
///
/// There is no collector, so every object is in memory never touched
/// before, and each new 4 KiB page costs a fault (about 8k cycles, several
/// times the cost of filling it). The first [`SMALL_HEAP`] bytes come in
/// [`CHUNK_WORDS`] chunks, as small programs need little. Past that, chunks
/// are mapped 2 MiB-aligned and marked for transparent huge pages, one
/// fault per 2 MiB where the kernel has them, and each doubles the heap, up
/// to [`MAX_CHUNK`].
pub struct Heap {
    chunks: Vec<Chunk>,
    ptr: *mut u64,
    end: *mut u64,
}

const CHUNK_WORDS: usize = 1 << 16;
const SMALL_HEAP: usize = 8 * CHUNK_WORDS;
const HUGE_PAGE: usize = 2 << 20;
const MAX_CHUNK: usize = 64 << 20;

/// A zeroed, private anonymous mapping.
struct Chunk {
    ptr: *mut u64,
    len: usize,
}

impl Chunk {
    /// `len` bytes (a multiple of the page size), aligned to `align`.
    fn map(len: usize, align: usize) -> Chunk {
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
        let all = len + align - 4096;
        let p = unsafe { libc::mmap(std::ptr::null_mut(), all, prot, flags, -1, 0) };
        assert!(p != libc::MAP_FAILED, "heap mmap failed");
        let start = (p as usize).next_multiple_of(align);
        unsafe {
            // Unmap the slack before and after the aligned range.
            if start > p as usize {
                libc::munmap(p, start - p as usize);
            }
            let tail = p as usize + all - (start + len);
            if tail > 0 {
                libc::munmap((start + len) as *mut libc::c_void, tail);
            }
        }
        Chunk {
            ptr: start as *mut u64,
            len,
        }
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

impl Default for Heap {
    fn default() -> Self {
        Heap {
            chunks: Vec::new(),
            ptr: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
        }
    }
}

impl Heap {
    #[inline]
    pub fn alloc_words(&mut self, n: usize) -> *mut u64 {
        if (self.end as usize - self.ptr as usize) / 8 < n {
            self.refill(n);
        }
        let p = self.ptr;
        self.ptr = unsafe { p.add(n) };
        p
    }

    #[cold]
    fn refill(&mut self, n: usize) {
        let mapped = self.mapped();
        let chunk = if mapped < SMALL_HEAP {
            Chunk::map((8 * n).max(8 * CHUNK_WORDS).next_multiple_of(4096), 4096)
        } else {
            let len = mapped.clamp(HUGE_PAGE, MAX_CHUNK).max(8 * n);
            let c = Chunk::map(len.next_multiple_of(HUGE_PAGE), HUGE_PAGE);
            #[cfg(target_os = "linux")]
            unsafe {
                libc::madvise(c.ptr.cast(), c.len, libc::MADV_HUGEPAGE);
            }
            c
        };
        self.ptr = chunk.ptr;
        self.end = unsafe { chunk.ptr.add(chunk.len / 8) };
        self.chunks.push(chunk);
    }

    /// Bytes mapped for chunks.
    fn mapped(&self) -> usize {
        self.chunks.iter().map(|c| c.len).sum()
    }

    /// Room for `n` values, all `undefined`.
    pub fn alloc_values(&mut self, n: usize) -> *mut Value {
        let p = self.alloc_words(n).cast::<Value>();
        for i in 0..n {
            unsafe { p.add(i).write(Value::UNDEFINED) };
        }
        p
    }

    /// A new object of `shape`, with its in-object slots `undefined`.
    pub fn alloc_object(&mut self, shape: &Shape) -> *mut Object {
        let words = INLINE_BASE / 8 + shape.capacity as usize;
        let o = self.alloc_words(words).cast::<Object>();
        unsafe {
            o.write(Object {
                shape,
                class: shape.class,
                flags: EXTERNAL,
                _pad: 0,
                props_cap: 0,
                props: std::ptr::null_mut(),
                elems: std::ptr::null_mut(),
                elems_len: 0,
                elems_cap: 0,
                code: std::ptr::null(),
            });
            for i in 0..shape.capacity as usize {
                Object::inline(o, i).write(Value::UNDEFINED);
            }
        }
        o
    }

    /// Bytes allocated so far, abandoned space included.
    pub fn allocated(&self) -> usize {
        self.mapped() - (self.end as usize - self.ptr as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations() {
        let s = Shape {
            class: Class::Plain,
            proto: Value::NULL,
            capacity: 2,
            keys: vec![Atom(7), Atom(8), Atom(9)],
            fields: Vec::new(),
            elems: FieldType::None,
            elems_code: 0,
            unique: false,
            transitions: RefCell::default(),
            elem_transitions: RefCell::default(),
        };
        assert_eq!(s.location(0), 48);
        assert_eq!(s.location(1), 56);
        assert_eq!(s.location(2), !0);
        assert_eq!(s.lookup(Atom(9)), Some(2));
        let mut heap = Heap::default();
        let o = heap.alloc_object(&s);
        assert!(object_value(o).is_cell());
        unsafe {
            *Object::slot(o, s.location(1)) = Value::from_i32(5);
            assert_eq!(*Object::inline(o, 1), Value::from_i32(5));
            assert_eq!((*o).flags & EXTERNAL, EXTERNAL);
        }
    }

    /// The first chunk is small; then chunks are huge-page aligned, each
    /// doubling the heap, and an allocation bigger than that gets a chunk
    /// of its size. All of it is zeroed, and `allocated` counts what was
    /// handed out, with space abandoned at chunk ends.
    #[test]
    fn heap_chunks() {
        let mut heap = Heap::default();
        let mut total = 0;
        let mut words = |heap: &mut Heap, n: usize| {
            let p = heap.alloc_words(n);
            let s = unsafe { std::slice::from_raw_parts_mut(p, n) };
            assert!(s.iter().all(|&w| w == 0));
            s.fill(!0);
            total += n;
            p
        };
        for _ in 0..SMALL_HEAP / 8 / 1000 {
            words(&mut heap, 1000);
        }
        assert_eq!(heap.mapped(), SMALL_HEAP);
        let p = words(&mut heap, 1000);
        assert_eq!(p as usize % HUGE_PAGE, 0);
        assert_eq!(heap.chunks[1].len, HUGE_PAGE);
        let big = words(&mut heap, 3 * HUGE_PAGE / 8);
        assert_eq!(big as usize % HUGE_PAGE, 0);
        assert_eq!(heap.chunks[2].len, 3 * HUGE_PAGE);
        for _ in 0..1000 {
            words(&mut heap, 10_000);
        }
        for k in 3..heap.chunks.len() {
            let before: usize = heap.chunks[..k].iter().map(|c| c.len).sum();
            let len = before
                .clamp(HUGE_PAGE, MAX_CHUNK)
                .next_multiple_of(HUGE_PAGE);
            assert_eq!(heap.chunks[k].len, len);
        }
        assert!(heap.allocated() >= 8 * total && heap.allocated() <= heap.mapped());
    }
}
