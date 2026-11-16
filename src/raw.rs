//! The crate's raw heap reads and writes, behind safe helpers.
//!
//! Inside the crate, an object pointer (`*mut Object`), a shape pointer
//! (`*const Shape`), a code pointer (`*const Code`) and an inline cache
//! slot (`*mut Ic`, see `vm::note_bytes`) are trusted to point to a live
//! cell, shape, code or cache: the crate alone makes cell values (#71),
//! the native stack's conservative scan keeps the cells Rust code holds
//! alive and in place, a cell keeps its shape, a function its code, and a
//! code its caches (which go when the code is retired, `gc.md`, "Compiled
//! code's cells"). Hundreds of safe functions already take such pointers
//! on that trust (`object::is_error`, `far::is_far`, …). These helpers
//! state it once, so a field read through one needs no `unsafe` block at
//! each use: the dereferences are here.
//!
//! So these are trusted helpers, not a safety boundary the compiler
//! enforces: fewer `unsafe` blocks don't remove the obligations, they
//! name them in one place. A caller still keeps the pointer live, and a
//! [`Live`] reference within the lifetime it picks. What the helpers do
//! check, in release builds, is what a pointer alone doesn't say: an
//! element index against the elements' array (its header word says it
//! holds values, and its capacity), a slot location against the object's
//! slots, the code word against the class, and a shape given to an object
//! against its class and its room ([`crate::object::set_shape`]). Writes
//! are of what needs no barrier: a hole ([`ObjectPtr::clear_elem`]), the
//! flags, a length (`elems_len`, which nothing here trusts).
//!
//! What stays out of here, and `unsafe` where it is: a precondition no
//! cheap check covers (a store the collector must see through a barrier,
//! a shape of the object's class in the interpreter's cached
//! transitions), the VM stack and frames, the JIT's memory and calls into
//! it, and the collector's own accesses.
//!
//! The object accessors read and write through the pointer, never through
//! an `&Object`: the marker thread reads the header concurrently, and the
//! code writes objects through other `*mut Object`s, so a shared or
//! mutable reference to one would assert what isn't so. Shapes and code
//! change through `Cell`s but for two exceptions, written in place
//! through their pointers: a dictionary shape's slots
//! (`Shape::dictionary_slots`), and a lazy stub's code when it is
//! compiled (`Vm::load_functions`). No [`Live`] reference to such a shape
//! or code may be held across those writes.

use crate::object::{Class, Object, Shape};
use crate::value::Value;
use crate::vm::Code;

/// The header fields of the object a pointer points to.
pub(crate) trait ObjectPtr: Copy {
    fn object_ptr(self) -> *mut Object;

    #[inline(always)]
    fn class(self) -> Class {
        unsafe { (*self.object_ptr()).class }
    }

    #[inline(always)]
    fn flags(self) -> u8 {
        unsafe { (*self.object_ptr()).flags }
    }

    /// A typed array's element type, another object's in-object capacity's
    /// low byte (see `Object::kind`).
    #[inline(always)]
    fn kind(self) -> u8 {
        unsafe { (*self.object_ptr()).kind }
    }

    #[inline(always)]
    fn elems_len(self) -> u32 {
        unsafe { (*self.object_ptr()).elems_len }
    }

    #[inline(always)]
    fn shape(self) -> *const Shape {
        unsafe { (*self.object_ptr()).shape }
    }

    /// The word past the header (see `Object::code`). Panics for a class
    /// without it (`layout::extra_words` 0: a plain object's or an array's
    /// first slot is there, past the end of one with none).
    #[inline(always)]
    #[track_caller]
    fn code(self) -> *const Code {
        assert!(crate::layout::extra_words(self.class()) > 0, "no code word in a {:?}", self.class());
        unsafe { (*self.object_ptr()).code }
    }

    /// [`ObjectPtr::code`], or null for a class without the word: to tell
    /// whether two objects run one code, either maybe not a function.
    #[inline(always)]
    fn code_or_null(self) -> *const Code {
        if crate::layout::extra_words(self.class()) > 0 {
            unsafe { (*self.object_ptr()).code }
        } else {
            std::ptr::null()
        }
    }

    /// The out-of-line slots (a cons string's half, see `Object::props`).
    #[inline(always)]
    fn props(self) -> *mut Value {
        unsafe { (*self.object_ptr()).props }
    }

    #[inline(always)]
    fn elems(self) -> *mut Value {
        unsafe { (*self.object_ptr()).elems }
    }

    /// Element `i`, `i` checked as [`ObjectPtr::elem_at`] checks it (a hole
    /// is `Value::EMPTY`).
    #[inline(always)]
    #[track_caller]
    fn elem(self, i: usize) -> Value {
        unsafe { *self.elem_at(i) }
    }

    /// Make element `i` a hole (`Value::EMPTY`), `i` checked as
    /// [`ObjectPtr::elem_at`] checks it, as the marker may read it
    /// (`layout::store_word`). (A hole is no cell: it needs no barrier,
    /// which is why this stores nothing else; other stores go through
    /// `Vm::store_element`.)
    #[inline(always)]
    #[track_caller]
    fn clear_elem(self, i: usize) {
        unsafe { crate::layout::store_word(self.elem_at(i), Value::EMPTY) }
    }

    /// Where element `i` is. Panics unless the elements are an array of
    /// values (its header word says so, see [`values_cap`]; never a
    /// string's, typed array's or data view's, whose `elems` may point
    /// into another cell's bytes) and `i` is below its capacity. (Not
    /// below `elems_len`, which a class may use for something else.)
    #[inline(always)]
    #[track_caller]
    fn elem_at(self, i: usize) -> *mut Value {
        assert!(
            !matches!(self.class(), Class::String | Class::TypedArray | Class::DataView),
            "no values in a {:?}'s elements",
            self.class()
        );
        let e = self.elems();
        assert!(i < values_cap(e).unwrap_or(0), "element {i} past the elements");
        unsafe { e.add(i) }
    }

    /// The value of field `slot` of the object's own shape: `slot` checked
    /// against the shape's slots, which the object has room for (shapes
    /// are given through `object::set_shape`, which checks it, or its
    /// unchecked form where that is known).
    #[inline(always)]
    #[track_caller]
    fn field(self, slot: u32) -> Value {
        let s = self.shape().live();
        assert!((slot as usize) < s.slots.len(), "field {slot} not in the shape");
        let loc = s.location(slot);
        let o = self.object_ptr();
        unsafe {
            if loc >= 0 { *o.cast::<u8>().add(loc as usize).cast::<Value>() } else { *self.props().add(!loc as usize) }
        }
    }

    /// The value in the slot at `loc` (see `Shape::location`: an in-object
    /// slot's byte offset, or `!i` for out-of-line slot `i`), checked
    /// against the object's in-object slots or its out-of-line slots'
    /// array (as [`ObjectPtr::elem_at`] checks elements).
    #[inline(always)]
    #[track_caller]
    fn slot_value(self, loc: i32) -> Value {
        let o = self.object_ptr();
        assert!(self.class() != Class::String, "no slots in a string");
        if loc >= 0 {
            let start = crate::layout::slots_offset(self.class());
            let end = start + 8 * Object::inline_cap(o);
            assert!((start..end).contains(&(loc as usize)) && loc % 8 == 0, "slot {loc} not in the object");
            unsafe { *o.cast::<u8>().add(loc as usize).cast::<Value>() }
        } else {
            let i = !loc as usize;
            assert!(i < values_cap(self.props()).unwrap_or(0), "slot {loc} not in the object");
            unsafe { *self.props().add(i) }
        }
    }

    #[inline(always)]
    fn set_flags(self, flags: u8) {
        unsafe { (*self.object_ptr()).flags = flags }
    }

    #[inline(always)]
    fn set_elems_len(self, n: u32) {
        unsafe { (*self.object_ptr()).elems_len = n }
    }
}

impl ObjectPtr for crate::object::Obj {
    #[inline(always)]
    fn object_ptr(self) -> *mut Object {
        self.ptr()
    }
}

impl ObjectPtr for *mut Object {
    #[inline(always)]
    fn object_ptr(self) -> *mut Object {
        self
    }
}

impl ObjectPtr for *const Object {
    #[inline(always)]
    fn object_ptr(self) -> *mut Object {
        self.cast_mut()
    }
}

/// The capacity of the array of values whose payload `p` is, if it is
/// one: its header word, the word before `p`, is `VALUES` or `WEAK_VALUES`
/// and the capacity (`layout::VALUES`); `None` for null or a payload of
/// something else (bytes, string units). `p` must be null or a payload's
/// start, as an object's `elems` or `props` is but for the classes
/// [`ObjectPtr::elem_at`] refuses.
#[inline(always)]
pub(crate) fn values_cap(p: *const Value) -> Option<usize> {
    if p.is_null() {
        return None;
    }
    let h = unsafe { *(p as *const u64).sub(1) };
    let tag = h & 0xffff_ffff;
    (tag == crate::layout::VALUES || tag == crate::heap::WEAK_VALUES).then_some((h >> 32) as usize)
}

/// The shape or code a pointer points to, as a reference: the lifetime is
/// the caller's to keep within the one the cell or function keeps it for
/// (a shape a cell no longer has, code retired, may go at the next
/// collection), and no reference may be held across the writes in place
/// the module docs list: a dictionary shape's slots
/// (`Shape::dictionary_slots`), a lazy stub's code once compiled
/// (`Vm::load_functions`).
pub(crate) trait Live<T> {
    fn live<'a>(self) -> &'a T;
}

macro_rules! live {
    ($($p:ty => $t:ty),*) => {$(
        impl Live<$t> for $p {
            #[inline(always)]
            fn live<'a>(self) -> &'a $t {
                debug_assert!(!self.is_null());
                unsafe { &*self }
            }
        }
    )*};
}
live!(*const Shape => Shape, *mut Shape => Shape, *const Code => Code, *mut Code => Code);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{as_object, set_shape};
    use crate::vm::Vm;

    /// `src`'s completion value, with its VM (kept alive with it).
    fn value(src: &str) -> (Vm, *mut Object) {
        let mut vm = Vm::new();
        let v = vm.run(&crate::js::compile(src).unwrap()).unwrap();
        (vm, as_object(v))
    }

    #[test]
    fn elements_read_within_the_capacity() {
        let (_vm, a) = value("[1, 2, 3]");
        assert_eq!(a.elem(2), Value::from_i32(3));
        a.clear_elem(2);
        assert_eq!(a.elem(2), Value::EMPTY);
    }

    #[test]
    #[should_panic(expected = "past the elements")]
    fn an_element_past_the_capacity_panics() {
        let (_vm, a) = value("[1, 2, 3]");
        a.elem(1 << 20);
    }

    #[test]
    #[should_panic(expected = "past the elements")]
    fn a_hole_past_the_capacity_panics() {
        let (_vm, a) = value("[1, 2, 3]");
        a.clear_elem(1 << 20);
    }

    #[test]
    #[should_panic(expected = "no values in a String's elements")]
    fn a_strings_units_are_no_elements() {
        let (_vm, s) = value("'abc'.repeat(10)");
        s.elem(0);
    }

    #[test]
    #[should_panic(expected = "past the elements")]
    fn elements_of_bytes_are_no_values() {
        // (A BigInt's elements are its magnitude's bytes.)
        let (_vm, b) = value("2n ** 200n");
        b.elem(0);
    }

    #[test]
    fn slots_read_within_the_object() {
        let (_vm, o) = value("({a: 1, b: 2})");
        let s = o.shape().live();
        assert_eq!(
            (o.slot_value(s.location(0)), o.slot_value(s.location(1))),
            (Value::from_i32(1), Value::from_i32(2))
        );
    }

    #[test]
    #[should_panic(expected = "not in the object")]
    fn an_in_object_slot_past_the_object_panics() {
        let (_vm, o) = value("({a: 1})");
        o.slot_value(1 << 20);
    }

    #[test]
    #[should_panic(expected = "not in the object")]
    fn an_out_of_line_slot_past_the_object_panics() {
        let (_vm, o) = value("({a: 1})");
        o.slot_value(!(1 << 20));
    }

    #[test]
    #[should_panic(expected = "no code word in a Plain")]
    fn a_plain_object_has_no_code_word() {
        let (_vm, o) = value("({})");
        o.code();
    }

    #[test]
    fn code_or_null_is_null_without_a_code_word() {
        let (_vm, o) = value("({})");
        assert!(o.code_or_null().is_null());
        let (_vm, f) = value("(function f() {})");
        assert_eq!(f.code_or_null(), f.code());
    }

    #[test]
    #[should_panic(expected = "a shape of another class")]
    fn a_shape_of_another_class_panics() {
        let (vm, o) = value("({})");
        set_shape(o, vm.array_shape);
    }

    #[test]
    #[should_panic(expected = "no room for the shape's slots")]
    fn a_shape_with_more_slots_than_room_panics() {
        let (_vm, big) = value(
            "({a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8, i: 9, j: 10, k: 11, l: 12, m: 13, n: 14, o: 15, p: 16, q: 17, r: 18})",
        );
        let (_vm2, small) = value("Object.create(null)");
        set_shape(small, big.shape());
    }
}
