//! The crate's raw heap reads and writes, behind safe helpers.
//!
//! Inside the crate, an object pointer (`*mut Object`), a shape pointer
//! (`*const Shape`) and a code pointer (`*const Code`) are trusted to
//! point to a live cell, shape or code: the crate alone makes cell values
//! (#71), the native stack's conservative scan keeps the cells Rust code
//! holds alive and in place, a cell keeps its shape, and a function
//! keeps its code. Hundreds of safe functions already take such pointers
//! on that trust (`object::is_error`, `far::is_far`, …). These helpers
//! state it once, so a field read through one needs no `unsafe` block at
//! each use: the dereferences are here, and only here.
//!
//! What stays out of here, and `unsafe` where it is: anything with a
//! precondition beyond "it is live" (an index in bounds of the elements'
//! capacity, the shape of a class, a write the collector must see through
//! a barrier), the VM stack and frames, the JIT's memory and calls into
//! it, and the collector's own accesses.
//!
//! The object accessors read and write through the pointer, never through
//! an `&Object`: the marker thread reads the header concurrently, and the
//! code writes objects through other `*mut Object`s, so a shared or
//! mutable reference to one would assert what isn't so. Shapes and code
//! change only through `Cell`s, so a shared reference to them is sound.

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

    /// The word past the header (see `Object::code`): only for the
    /// classes with it.
    #[inline(always)]
    fn code(self) -> *const Code {
        unsafe { (*self.object_ptr()).code }
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

    /// Element `i` of an object whose elements are values (not a string's,
    /// a typed array's or a buffer's bytes), checked against their array's
    /// capacity (`layout::values_cap`): a hole is `Value::EMPTY`.
    #[inline(always)]
    #[track_caller]
    fn elem(self, i: usize) -> Value {
        // (Below `elems_len` first: a caller's own test of it folds this.)
        if i < self.elems_len() as usize {
            return unsafe { *self.elems().add(i) };
        }
        unsafe { *self.elem_at(i) }
    }

    /// Store `v` as element `i` (as [`ObjectPtr::elem`]), as the marker may
    /// read it (`layout::store_word`). No barrier: for a value the
    /// collector already sees (a hole, a number, one of these elements).
    #[inline(always)]
    #[track_caller]
    fn set_elem(self, i: usize, v: Value) {
        unsafe { crate::layout::store_word(self.elem_at(i), v) }
    }

    /// Where element `i` is, `i` checked against the elements' capacity
    /// (`elems_len` is at most that, for the classes whose elements are
    /// values).
    #[inline(always)]
    #[track_caller]
    fn elem_at(self, i: usize) -> *mut Value {
        let e = self.elems();
        debug_assert!(!matches!(
            self.class(),
            Class::String | Class::TypedArray | Class::ArrayBuffer | Class::SharedArrayBuffer | Class::DataView
        ));
        assert!(i < unsafe { crate::layout::values_cap(e) } as usize, "element {i} past the capacity");
        unsafe { e.add(i) }
    }

    /// The value in the slot at `loc` (see `Shape::location`: an in-object
    /// slot's byte offset, or `!i` for out-of-line slot `i`), checked
    /// against the object's in-object slots or out-of-line capacity.
    #[inline(always)]
    #[track_caller]
    fn slot_value(self, loc: i32) -> Value {
        let o = self.object_ptr();
        if loc >= 0 {
            let start = crate::layout::slots_offset(self.class());
            let end = start + 8 * unsafe { Object::inline_cap(o) };
            assert!((start..end).contains(&(loc as usize)) && loc % 8 == 0, "slot {loc} not in the object");
            unsafe { *o.cast::<u8>().add(loc as usize).cast::<Value>() }
        } else {
            let i = !loc as usize;
            assert!(i < unsafe { crate::layout::values_cap(self.props()) } as usize, "slot {loc} not in the object");
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

/// The shape or code a pointer points to, as a reference: the lifetime is
/// the caller's to keep within the one the cell or function keeps it for
/// (a shape a cell no longer has, code retired, may go at the next
/// collection). Code is written in place once, through its pointer, when
/// a lazy stub is compiled (`Vm::load_functions`): no reference to a
/// stub's code may be held across that.
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
