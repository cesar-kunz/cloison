//! The property model past plain data properties, for `Object`'s functions
//! (installed by [`install`]): property descriptors, elements with
//! attributes, extensibility, and the own properties objects have without
//! a slot for them (an array's `length`, a function's `length` and `name`,
//! a string's characters).
//!
//! An element with attributes other than a plain data property's (or an
//! accessor at an index) is kept as a named property under its decimal
//! string, with the object's `SPARSE` flag set, as far keys are
//! (`Vm::set_keyed`); its element slot, if within the elements, is a hole.
//! The object then gets a unique shape no value's store fits
//! ([`Vm::slow_elements`]), so that every element store takes the runtime
//! path, which looks such properties up. Attributes all elements share
//! are the object's flags (`SEALED`, `FROZEN`, from `Object.seal` and
//! `Object.freeze`), as is an array's read-only `length`.

use std::cell::{Cell, RefCell};

use crate::interp::{call_function, strict_equals, to_boolean, to_number};
use crate::object::{
    ACCESSOR, Atom, Class, DONT_DELETE, DONT_ENUM, FN_LENGTH_DELETED, FN_NAME_DELETED, FROZEN,
    LENGTH_READ_ONLY, NOT_EXTENSIBLE, Object, READ_ONLY, SEALED, SPARSE, Shape, as_object,
    object_value,
};
use crate::value::Value;
use crate::vm::{Error, Key, NO_ELEMENTS, NativeFn, Result, Vm, string_units, wrapped};

/// A property descriptor: each field `None` where the descriptor leaves
/// it out. A property's own ([`Vm::own_property`]) has every field of its
/// kind.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Desc {
    pub value: Option<Value>,
    pub get: Option<Value>,
    pub set: Option<Value>,
    pub writable: Option<bool>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl Desc {
    pub(crate) fn data(value: Value, writable: bool, enumerable: bool, configurable: bool) -> Desc {
        Desc {
            value: Some(value),
            writable: Some(writable),
            enumerable: Some(enumerable),
            configurable: Some(configurable),
            ..Desc::default()
        }
    }

    pub(crate) fn accessor(get: Value, set: Value, enumerable: bool, configurable: bool) -> Desc {
        Desc {
            get: Some(get),
            set: Some(set),
            enumerable: Some(enumerable),
            configurable: Some(configurable),
            ..Desc::default()
        }
    }

    pub(crate) fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    pub(crate) fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    /// The attributes of a complete descriptor, as a shape keeps them.
    fn attrs(&self) -> u8 {
        let mut a = 0;
        if self.is_accessor() {
            a |= ACCESSOR;
        } else if self.writable != Some(true) {
            a |= READ_ONLY;
        }
        if self.enumerable != Some(true) {
            a |= DONT_ENUM;
        }
        if self.configurable != Some(true) {
            a |= DONT_DELETE;
        }
        a
    }

    /// A complete descriptor from attributes and a slot's value (an
    /// accessor's pair).
    fn from_attrs(attrs: u8, v: Value) -> Desc {
        let (enumerable, configurable) = (attrs & DONT_ENUM == 0, attrs & DONT_DELETE == 0);
        if attrs & ACCESSOR != 0 {
            let pair = as_object(v);
            unsafe { Desc::accessor(*(*pair).elems, *(*pair).elems.add(1), enumerable, configurable) }
        } else {
            Desc::data(v, attrs & READ_ONLY == 0, enumerable, configurable)
        }
    }
}

/// SameValue: `===` but NaN is itself and the zeros differ.
pub(crate) fn same_value(a: Value, b: Value) -> bool {
    if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
        return if x.is_nan() { y.is_nan() } else { x == y && x.is_sign_negative() == y.is_sign_negative() };
    }
    strict_equals(a, b)
}

/// Whether `v` is callable.
pub(crate) fn is_callable(v: Value) -> bool {
    v.is_cell() && matches!(unsafe { (*as_object(v)).class }, Class::Function | Class::Native)
}

/// Whether `v` is an object (not a primitive: strings are cells too).
pub(crate) fn is_object(v: Value) -> bool {
    v.is_cell() && !matches!(unsafe { (*as_object(v)).class }, Class::String | Class::Symbol)
}

/// The canonical array index `name` is, if any.
fn index_of_name(name: &str) -> Option<u32> {
    let i = name.parse::<u32>().ok()?;
    (i != u32::MAX && i.to_string() == name).then_some(i)
}

impl Vm {
    /// Make `o`'s shape its own (unique), with attributes `attrs` for its
    /// slots if given, and if `no_elements`, one no element store's fast
    /// path fits (see the module docs). Compiled code relying on the old
    /// shape is dropped.
    pub(crate) fn unique_shape(&mut self, o: *mut Object, attrs: Option<Vec<u8>>, no_elements: bool) {
        unsafe {
            let s = &*(*o).shape;
            let same_attrs = attrs.as_ref().is_none_or(|a| *a == s.attrs);
            if s.unique && same_attrs && (!no_elements || s.elems_code == NO_ELEMENTS) {
                return;
            }
            self.shape_left_stable(s);
            let u = Shape {
                class: s.class,
                proto: s.proto,
                capacity: Cell::new(s.capacity.get()),
                keys: s.keys.clone(),
                attrs: attrs.unwrap_or_else(|| s.attrs.clone()),
                fields: s.fields.clone(),
                elems: s.elems,
                elems_code: if no_elements { NO_ELEMENTS } else { s.elems_code },
                family: std::ptr::null(),
                push: Cell::new(std::ptr::null_mut()),
                unique: true,
                transitions: RefCell::default(),
                elem_transitions: RefCell::default(),
                relied: Cell::new(false),
            };
            (*o).shape = self.add_shape(u);
            self.bump_code_epoch();
            if (*o).flags & crate::object::PROTOTYPE != 0 {
                self.proto_epoch = self.proto_epoch.wrapping_add(1);
            }
        }
    }

    /// Every element store to `o` takes the runtime path from now on.
    pub(crate) fn slow_elements(&mut self, o: *mut Object) {
        self.unique_shape(o, None, true);
    }

    /// `o`'s own property `name` kept in a slot, if any.
    fn own_named(&self, o: *mut Object, name: Atom) -> Option<Desc> {
        unsafe {
            let s = &*(*o).shape;
            let slot = s.lookup(name)?;
            Some(Desc::from_attrs(s.attrs[slot as usize], *Object::slot(o, s.location(slot))))
        }
    }

    /// The number of parameters of function `f`, its `length`.
    fn function_length(f: *mut Object) -> Value {
        unsafe { Value::from_i32((*(*f).code).length as i32) }
    }

    /// Function `f`'s `name`: its code's, empty for an anonymous one.
    fn function_name(&mut self, f: *mut Object) -> Value {
        let name = unsafe { &(*(*f).code).name };
        let name = if name.starts_with('<') { "" } else { name.as_str() };
        let u: Vec<u16> = name.encode_utf16().collect();
        self.new_string(&u)
    }

    /// The string a string object (a wrapper) or a string `o` holds.
    fn string_of(o: *mut Object) -> Option<&'static [u16]> {
        let v = object_value(o);
        string_units(v).or_else(|| string_units(wrapped(v)?))
    }

    /// `o`'s own property `key`, fully described, if it has it.
    pub(crate) fn own_property(&mut self, o: *mut Object, key: Key) -> Option<Desc> {
        // A mapped `arguments` element's value is its parameter's.
        if let Key::Index(i) = key
            && let Some(p) = self.mapped_slot(o, i)
        {
            let mut d = self.own_property_inner(o, key)?;
            d.value = Some(unsafe { *p });
            return Some(d);
        }
        self.own_property_inner(o, key)
    }

    /// Whether element `i` of `o` is a writable data property (or absent).
    pub(crate) fn writable_element(&mut self, o: *mut Object, i: u32) -> bool {
        self.own_property_inner(o, Key::Index(i)).is_none_or(|d| d.writable != Some(false))
    }

    fn own_property_inner(&mut self, o: *mut Object, key: Key) -> Option<Desc> {
        unsafe {
            let class = (*o).class;
            let flags = (*o).flags;
            match key {
                Key::Index(i) => {
                    if let Some(u) = Self::string_of(o) {
                        if (i as usize) < u.len() {
                            let c = self.single_char(u[i as usize]);
                            return Some(Desc::data(c, false, true, false));
                        }
                        if class == Class::String {
                            return None;
                        }
                    }
                    match class {
                        Class::TypedArray => {
                            return (i < (*o).elems_len).then(|| {
                                let kind = crate::object::TypedKind::from_u8((*o).kind);
                                Desc::data(kind.load((*o).elems.cast(), i as usize), true, true, true)
                            });
                        }
                        c if c.no_elements() => return None,
                        _ => {}
                    }
                    if Self::has_element(o, i) {
                        let v = *(*o).elems.add(i as usize);
                        return Some(Desc::data(v, flags & FROZEN == 0, true, flags & SEALED == 0));
                    }
                    if flags & SPARSE != 0 {
                        let a = self.intern(&i.to_string());
                        return self.own_named(o, a);
                    }
                    None
                }
                Key::Name(a) => {
                    if let Some(d) = self.own_named(o, a) {
                        return Some(d);
                    }
                    if a == self.a_length {
                        if let Some(u) = Self::string_of(o) {
                            let n = Value::from_i32(u.len() as i32);
                            return Some(Desc::data(n, false, false, false));
                        }
                        return match class {
                            Class::Array => {
                                let n = Value::from_i32((*o).elems_len as i32);
                                Some(Desc::data(n, flags & LENGTH_READ_ONLY == 0, false, false))
                            }
                            Class::TypedArray => {
                                let n = Value::from_i32((*o).elems_len as i32);
                                Some(Desc::data(n, false, false, false))
                            }
                            Class::Function | Class::Native if flags & FN_LENGTH_DELETED == 0 => {
                                let c = flags & (SEALED | FROZEN) == 0;
                                Some(Desc::data(Self::function_length(o), false, false, c))
                            }
                            _ => None,
                        };
                    }
                    if a == self.a_name
                        && matches!(class, Class::Function | Class::Native)
                        && flags & FN_NAME_DELETED == 0
                    {
                        let n = self.function_name(o);
                        return Some(Desc::data(n, false, false, flags & (SEALED | FROZEN) == 0));
                    }
                    None
                }
            }
        }
    }

    /// `o`'s own property keys: indices in order, then names in insertion
    /// order (the ones without a slot first), each with whether it is
    /// enumerable.
    pub(crate) fn own_keys(&mut self, o: *mut Object) -> Vec<(Key, bool)> {
        let mut out = Vec::new();
        unsafe {
            let class = (*o).class;
            let s = &*(*o).shape;
            let mut indices: Vec<u32> = Vec::new();
            if let Some(u) = Self::string_of(o) {
                indices.extend(0..u.len() as u32);
            }
            match class {
                Class::TypedArray => indices.extend(0..(*o).elems_len),
                Class::String => {}
                c if c.no_elements() => {}
                _ => {
                    for i in 0..(*o).elems_len {
                        if Self::has_element(o, i) {
                            indices.push(i);
                        }
                    }
                }
            }
            let sparse = (*o).flags & SPARSE != 0;
            let mut far: Vec<(u32, bool)> = Vec::new();
            if sparse {
                for (k, &name) in s.keys.iter().enumerate() {
                    if let Some(i) = index_of_name(self.atom_name(name)) {
                        far.push((i, s.attrs[k] & DONT_ENUM == 0));
                    }
                }
            }
            indices.extend(far.iter().map(|&(i, _)| i));
            indices.sort_unstable();
            indices.dedup();
            let far_enum = |i: u32| far.iter().find(|&&(j, _)| j == i).map_or(true, |&(_, e)| e);
            out.extend(indices.into_iter().map(|i| (Key::Index(i), far_enum(i))));
            // The properties without slots (see `own_property`).
            let flags = (*o).flags;
            let length = Self::string_of(o).is_some() || matches!(class, Class::Array | Class::TypedArray);
            if length || matches!(class, Class::Function | Class::Native) && flags & FN_LENGTH_DELETED == 0 {
                out.push((Key::Name(self.a_length), false));
            }
            if matches!(class, Class::Function | Class::Native) && flags & FN_NAME_DELETED == 0 {
                out.push((Key::Name(self.a_name), false));
            }
            for (k, &name) in s.keys.iter().enumerate() {
                if sparse && index_of_name(self.atom_name(name)).is_some() {
                    continue;
                }
                // (Symbols' keys apart: see `symbol::own_symbol_keys`.)
                if crate::symbol::is_symbol_atom(self, name) {
                    continue;
                }
                out.push((Key::Name(name), s.attrs[k] & DONT_ENUM == 0));
            }
        }
        out
    }

    /// A key as a string value.
    pub(crate) fn key_string(&mut self, key: Key) -> Value {
        let s = match key {
            Key::Index(i) => i.to_string(),
            Key::Name(a) => self.atom_name(a).to_string(),
        };
        let u: Vec<u16> = s.encode_utf16().collect();
        self.string_constant(&u)
    }

    /// A key as a property-access key value (an index as a number).
    pub(crate) fn key_value(&mut self, key: Key) -> Value {
        if let Key::Name(a) = key
            && let Some(s) = crate::symbol::atom_symbol(self, a)
        {
            return s;
        }
        match key {
            Key::Index(i) if i <= i32::MAX as u32 => Value::from_i32(i as i32),
            Key::Index(i) => Value::number(i as f64),
            Key::Name(_) => self.key_string(key),
        }
    }

    /// Whether `o` may get new properties.
    pub(crate) fn is_extensible(o: *mut Object) -> bool {
        unsafe { (*o).flags & NOT_EXTENSIBLE == 0 }
    }

    /// `Object.preventExtensions(o)`.
    pub(crate) fn prevent_extensions(&mut self, o: *mut Object) {
        unsafe {
            if (*o).flags & NOT_EXTENSIBLE != 0 {
                return;
            }
            (*o).flags |= NOT_EXTENSIBLE;
        }
        self.unique_shape(o, None, true);
    }

    /// `Object.seal(o)` (`freeze`: `Object.freeze(o)`).
    pub(crate) fn seal(&mut self, o: *mut Object, freeze: bool) {
        unsafe {
            let s = &*(*o).shape;
            let attrs: Vec<u8> = s
                .attrs
                .iter()
                .map(|&a| {
                    let mut a = a | DONT_DELETE;
                    if freeze && a & ACCESSOR == 0 {
                        a |= READ_ONLY;
                    }
                    a
                })
                .collect();
            (*o).flags |= NOT_EXTENSIBLE | SEALED;
            if freeze {
                (*o).flags |= FROZEN;
                // (The bit is a function's `FN_LENGTH_DELETED`.)
                if (*o).class == Class::Array {
                    (*o).flags |= LENGTH_READ_ONLY;
                }
            }
            self.unique_shape(o, Some(attrs), true);
        }
    }

    /// Whether `o` is sealed (`frozen`: frozen): not extensible, and no
    /// own property configurable (nor, frozen, writable).
    pub(crate) fn is_sealed(&mut self, o: *mut Object, frozen: bool) -> bool {
        if Self::is_extensible(o) {
            return false;
        }
        for (key, _) in self.own_keys(o) {
            let Some(d) = self.own_property(o, key) else { continue };
            if d.configurable == Some(true) || frozen && d.writable == Some(true) {
                return false;
            }
        }
        true
    }

    /// Define `o`'s own property `key` by descriptor `d`, as
    /// `Object.defineProperty`: whether it could be (ValidateAndApply-
    /// PropertyDescriptor; a `RangeError` for an array's bad `length`).
    pub(crate) fn define_own_property(&mut self, o: *mut Object, key: Key, d: &Desc) -> Result<bool> {
        // A mapped `arguments` element: its parameter set too, and unmapped
        // by an accessor or a non-writable value.
        if let Key::Index(i) = key
            && let Some(p) = self.mapped_slot(o, i)
        {
            let ok = self.define_own_property_inner(o, key, d)?;
            if ok {
                if d.get.is_some() || d.set.is_some() {
                    self.unmap_argument(o, i);
                } else {
                    if let Some(v) = d.value {
                        unsafe { *p = v };
                    }
                    if d.writable == Some(false) {
                        self.unmap_argument(o, i);
                    }
                }
            }
            return Ok(ok);
        }
        self.define_own_property_inner(o, key, d)
    }

    fn define_own_property_inner(&mut self, o: *mut Object, key: Key, d: &Desc) -> Result<bool> {
        let class = unsafe { (*o).class };
        if class == Class::Array && key == Key::Name(self.a_length) {
            return self.define_array_length(o, d);
        }
        let Some(cur) = self.own_property(o, key) else {
            if !Self::is_extensible(o) {
                return Ok(false);
            }
            let full = if d.is_accessor() {
                Desc::accessor(
                    d.get.unwrap_or(Value::UNDEFINED),
                    d.set.unwrap_or(Value::UNDEFINED),
                    d.enumerable.unwrap_or(false),
                    d.configurable.unwrap_or(false),
                )
            } else {
                Desc::data(
                    d.value.unwrap_or(Value::UNDEFINED),
                    d.writable.unwrap_or(false),
                    d.enumerable.unwrap_or(false),
                    d.configurable.unwrap_or(false),
                )
            };
            return self.store_own(o, key, full, None);
        };
        // Validate against the current property.
        if cur.configurable == Some(false) {
            if d.configurable == Some(true) {
                return Ok(false);
            }
            if d.enumerable.is_some() && d.enumerable != cur.enumerable {
                return Ok(false);
            }
            if (d.is_accessor() || d.is_data()) && d.is_accessor() != cur.is_accessor() {
                return Ok(false);
            }
            if cur.is_accessor() {
                if d.get.is_some_and(|g| !same_value(g, cur.get.unwrap()))
                    || d.set.is_some_and(|s| !same_value(s, cur.set.unwrap()))
                {
                    return Ok(false);
                }
            } else if cur.writable == Some(false) {
                if d.writable == Some(true) {
                    return Ok(false);
                }
                if d.value.is_some_and(|v| !same_value(v, cur.value.unwrap())) {
                    return Ok(false);
                }
            }
        }
        // Merge.
        let enumerable = d.enumerable.or(cur.enumerable).unwrap_or(false);
        let configurable = d.configurable.or(cur.configurable).unwrap_or(false);
        let full = if d.is_accessor() || (!d.is_data() && cur.is_accessor()) {
            let (g, s) = if cur.is_accessor() { (cur.get, cur.set) } else { (None, None) };
            Desc::accessor(
                d.get.or(g).unwrap_or(Value::UNDEFINED),
                d.set.or(s).unwrap_or(Value::UNDEFINED),
                enumerable,
                configurable,
            )
        } else {
            let (v, w) = if cur.is_data() { (cur.value, cur.writable) } else { (None, None) };
            Desc::data(
                d.value.or(v).unwrap_or(Value::UNDEFINED),
                d.writable.or(w).unwrap_or(false),
                enumerable,
                configurable,
            )
        };
        self.store_own(o, key, full, Some(cur))
    }

    /// Store `o`'s own property `key`, now `full` (a complete descriptor),
    /// `cur` being what it was.
    fn store_own(&mut self, o: *mut Object, key: Key, full: Desc, cur: Option<Desc>) -> Result<bool> {
        unsafe {
            let class = (*o).class;
            match key {
                Key::Index(i) => {
                    if Self::string_of(o).is_some_and(|u| (i as usize) < u.len()) {
                        // Validated above: nothing changes.
                        return Ok(true);
                    }
                    match class {
                        Class::String => return Ok(false),
                        c if c.no_elements() => return Ok(false),
                        Class::TypedArray => {
                            if i >= (*o).elems_len || full.is_accessor() || full.attrs() != 0 {
                                return Ok(false);
                            }
                            let kind = crate::object::TypedKind::from_u8((*o).kind);
                            kind.store((*o).elems.cast(), i as usize, to_number(full.value.unwrap()));
                            return Ok(true);
                        }
                        _ => {}
                    }
                    let plain = full.attrs() == 0 && (*o).flags & (SEALED | FROZEN) == 0;
                    let named = (*o).flags & SPARSE != 0 && {
                        let a = self.intern(&i.to_string());
                        (*(*o).shape).lookup(a).is_some()
                    };
                    let far = class != Class::Array
                        && i as usize > (*o).elems_len as usize * 2 + crate::vm::SPARSE_GAP;
                    if plain && !far {
                        if named {
                            let a = self.intern(&i.to_string());
                            self.delete_named(o, a);
                        }
                        self.set_element(o, i, full.value.unwrap());
                        return Ok(true);
                    }
                    // A named property, its element slot a hole.
                    if class == Class::Array && i >= (*o).elems_len {
                        self.grow_elements(o, i as usize + 1)?;
                    }
                    if i < (*o).elems_len && Self::has_element(o, i) {
                        *(*o).elems.add(i as usize) = Value::EMPTY;
                    }
                    (*o).flags |= SPARSE;
                    if !far {
                        self.slow_elements(o);
                    }
                    let a = self.intern(&i.to_string());
                    self.store_named(o, a, full);
                    Ok(true)
                }
                Key::Name(a) => {
                    if let Some(cur) = cur
                        && (*(*o).shape).lookup(a).is_none()
                    {
                        // A property without a slot (a function's `length`,
                        // …) gets one if it changes; a string's or an
                        // array's `length` can't change here.
                        if !matches!(class, Class::Function | Class::Native) {
                            return Ok(full.is_data()
                                && cur.is_data()
                                && same_value(full.value.unwrap(), cur.value.unwrap())
                                && full.attrs() == cur.attrs());
                        }
                    }
                    self.store_named(o, a, full);
                    Ok(true)
                }
            }
        }
    }

    /// Store `o`'s named own property `a`, now `full`, in its slot
    /// (added if new).
    fn store_named(&mut self, o: *mut Object, a: Atom, full: Desc) {
        let attrs = full.attrs();
        if full.is_accessor() {
            self.define_accessor(o, a, full.get, full.set, attrs & !ACCESSOR);
        } else {
            self.define_property(o, a, full.value, attrs);
        }
    }

    /// `o[i] = v` as a plain data element (`o` not a string, typed array
    /// or buffer), the elements grown to hold it.
    fn set_element(&mut self, o: *mut Object, i: u32, v: Value) {
        unsafe {
            if i >= (*o).elems_len {
                self.reserve_elems(o, i as usize + 1);
                for j in (*o).elems_len..i {
                    *(*o).elems.add(j as usize) = Value::EMPTY;
                }
                (*o).elems_len = i + 1;
            }
            self.store_element(o, i as usize, v);
        }
    }

    /// Grow `o`'s elements to length `n` with holes (a `RangeError` past
    /// what the elements can hold).
    fn grow_elements(&mut self, o: *mut Object, n: usize) -> Result<()> {
        if n > crate::vm::MAX_ELEMENTS {
            return Err(Error::range_error("array length too large for this engine"));
        }
        unsafe {
            let len = (*o).elems_len as usize;
            self.reserve_elems(o, n);
            for j in len..n {
                *(*o).elems.add(j) = Value::EMPTY;
            }
            (*o).elems_len = n as u32;
        }
        Ok(())
    }

    /// `Object.defineProperty(array, "length", d)` (ArraySetLength).
    pub(crate) fn define_array_length(&mut self, o: *mut Object, d: &Desc) -> Result<bool> {
        unsafe {
            let read_only = (*o).flags & LENGTH_READ_ONLY != 0;
            if d.is_accessor() || d.configurable == Some(true) || d.enumerable == Some(true) {
                return Ok(false);
            }
            let new_len = match d.value {
                None => None,
                Some(v) => {
                    let n = to_number(v);
                    let u = crate::interp::to_int32(n) as u32 as f64;
                    if u != n && !(u == 0.0 && n == 0.0) {
                        return Err(Error::range_error("Invalid array length"));
                    }
                    Some(u as u32)
                }
            };
            let old_len = (*o).elems_len;
            if read_only {
                if d.writable == Some(true) {
                    return Ok(false);
                }
                return Ok(new_len.is_none_or(|n| n == old_len));
            }
            if let Some(n) = new_len {
                if n > old_len {
                    self.grow_elements(o, n as usize)?;
                } else if n < old_len {
                    // Truncate, from the end, stopping at an element
                    // that can't be deleted.
                    let mut stop = n;
                    if (*o).flags & SEALED != 0 {
                        stop = old_len;
                    } else if (*o).flags & SPARSE != 0 {
                        for i in (n..old_len).rev() {
                            let a = self.intern(&i.to_string());
                            let s = &*(*o).shape;
                            if let Some(k) = s.lookup(a) {
                                if s.attrs[k as usize] & DONT_DELETE != 0 {
                                    stop = i + 1;
                                    break;
                                }
                                self.delete_named(o, a);
                            }
                        }
                    }
                    for i in stop..old_len {
                        *(*o).elems.add(i as usize) = Value::EMPTY;
                    }
                    (*o).elems_len = stop;
                    if stop != n {
                        if d.writable == Some(false) {
                            (*o).flags |= LENGTH_READ_ONLY;
                            self.slow_elements(o);
                        }
                        return Ok(false);
                    }
                }
            }
            if d.writable == Some(false) {
                (*o).flags |= LENGTH_READ_ONLY;
                self.slow_elements(o);
            }
            Ok(true)
        }
    }

    /// `delete o[key]`: whether it is gone.
    pub(crate) fn delete_own(&mut self, o: *mut Object, key: Key) -> bool {
        let r = self.delete_own_inner(o, key);
        if r && let Key::Index(i) = key {
            self.unmap_argument(o, i);
        }
        r
    }

    fn delete_own_inner(&mut self, o: *mut Object, key: Key) -> bool {
        unsafe {
            let class = (*o).class;
            match key {
                Key::Index(i) => {
                    if Self::string_of(o).is_some_and(|u| (i as usize) < u.len()) {
                        return false;
                    }
                    match class {
                        Class::String | Class::TypedArray => return i >= (*o).elems_len,
                        c if c.no_elements() => return true,
                        _ => {}
                    }
                    if i < (*o).elems_len && Self::has_element(o, i) {
                        if (*o).flags & SEALED != 0 {
                            return false;
                        }
                        *(*o).elems.add(i as usize) = Value::EMPTY;
                        return true;
                    }
                    if (*o).flags & SPARSE != 0 {
                        let a = self.intern(&i.to_string());
                        return self.delete_named(o, a);
                    }
                    true
                }
                Key::Name(a) => {
                    if (*(*o).shape).lookup(a).is_none() {
                        if o == self.global {
                            return self.delete_global(a);
                        }
                        if matches!(class, Class::Function | Class::Native) {
                            if (a == self.a_length || a == self.a_name) && (*o).flags & (SEALED | FROZEN) != 0 {
                                return false;
                            }
                            if a == self.a_length {
                                (*o).flags |= FN_LENGTH_DELETED;
                            } else if a == self.a_name {
                                (*o).flags |= FN_NAME_DELETED;
                            }
                            return true;
                        }
                        let length = a == self.a_length
                            && (Self::string_of(o).is_some()
                                || matches!(class, Class::Array | Class::TypedArray));
                        return !length;
                    }
                    let deleted = self.delete_named(o, a);
                    if deleted && matches!(class, Class::Function | Class::Native) {
                        if a == self.a_length {
                            (*o).flags |= FN_LENGTH_DELETED;
                        } else if a == self.a_name {
                            (*o).flags |= FN_NAME_DELETED;
                        }
                    }
                    deleted
                }
            }
        }
    }

    /// Change `o`'s prototype to `proto` (an object or null): whether it
    /// could be (not when `o` isn't extensible, nor to a cycle).
    pub(crate) fn set_prototype_of(&mut self, o: *mut Object, proto: Value) -> bool {
        unsafe {
            let s = &*(*o).shape;
            if s.proto == proto {
                return true;
            }
            if !Self::is_extensible(o) {
                return false;
            }
            let mut p = proto;
            while p.is_cell() {
                if as_object(p) == o {
                    return false;
                }
                p = (*(*as_object(p)).shape).proto;
            }
            if proto.is_cell() {
                self.make_prototype(as_object(proto));
            }
            self.shape_left_stable(s);
            let u = Shape {
                class: s.class,
                proto,
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
            self.bump_code_epoch();
            self.proto_epoch = self.proto_epoch.wrapping_add(1);
            true
        }
    }

    /// ToPropertyDescriptor: descriptor object `v` as a [`Desc`].
    pub(crate) fn to_descriptor(&mut self, v: Value) -> Result<Desc> {
        if !is_object(v) {
            return Err(Error::type_error("Property description must be an object"));
        }
        let mut d = Desc::default();
        let has = |vm: &mut Vm, a: Atom| {
            let k = vm.key_string(Key::Name(a));
            vm.has_property(v, k)
        };
        let (a_enumerable, a_configurable) = (self.a_enumerable, self.a_configurable);
        let (a_value, a_writable, a_get, a_set) = (self.a_value, self.a_writable, self.a_get, self.a_set);
        if has(self, a_enumerable)? {
            d.enumerable = Some(to_boolean(self.get(v, a_enumerable)?));
        }
        if has(self, a_configurable)? {
            d.configurable = Some(to_boolean(self.get(v, a_configurable)?));
        }
        if has(self, a_value)? {
            d.value = Some(self.get(v, a_value)?);
        }
        if has(self, a_writable)? {
            d.writable = Some(to_boolean(self.get(v, a_writable)?));
        }
        if has(self, a_get)? {
            let g = self.get(v, a_get)?;
            if !g.is_undefined() && !is_callable(g) {
                return Err(Error::type_error("Getter must be a function"));
            }
            d.get = Some(g);
        }
        if has(self, a_set)? {
            let s = self.get(v, a_set)?;
            if !s.is_undefined() && !is_callable(s) {
                return Err(Error::type_error("Setter must be a function"));
            }
            d.set = Some(s);
        }
        if d.is_accessor() && d.is_data() {
            return Err(Error::type_error(
                "Invalid property descriptor. Cannot both specify accessors and a value or writable attribute",
            ));
        }
        Ok(d)
    }

    /// FromPropertyDescriptor: `d` as an object.
    pub(crate) fn from_descriptor(&mut self, d: &Desc) -> Value {
        let op = object_value(self.object_prototype);
        let o = self.new_object(op);
        let fields: [(Atom, Option<Value>); 6] = [
            (self.a_value, d.value),
            (self.a_writable, d.writable.map(Value::from_bool)),
            (self.a_get, d.get),
            (self.a_set, d.set),
            (self.a_enumerable, d.enumerable.map(Value::from_bool)),
            (self.a_configurable, d.configurable.map(Value::from_bool)),
        ];
        for (a, v) in fields {
            if let Some(v) = v {
                self.add_property(o, a, v);
            }
        }
        object_value(o)
    }

    /// ToObject: a `TypeError` for `undefined` and `null`, a wrapper for
    /// another primitive.
    pub(crate) fn to_object(&mut self, v: Value) -> Result<*mut Object> {
        if is_object(v) {
            return Ok(as_object(v));
        }
        if v.is_undefined() || v.is_null() {
            return Err(Error::type_error(format!(
                "Cannot convert {} to object",
                crate::vm::display(v)
            )));
        }
        Ok(as_object(self.new_wrapper(v)))
    }

    /// ToPropertyKey of `v`.
    pub(crate) fn to_key(&mut self, v: Value) -> Key {
        self.property_key(v)
    }

    /// `o[key]`, generically.
    pub(crate) fn get_key(&mut self, o: Value, key: Key) -> Result<Value> {
        match key {
            Key::Name(a) => self.get(o, a),
            Key::Index(_) => {
                let k = self.key_value(key);
                self.get_keyed(o, k)
            }
        }
    }

    /// Whether `o` or its prototypes have `key`.
    pub(crate) fn has_key(&mut self, o: Value, key: Key) -> Result<bool> {
        let k = self.key_value(key);
        self.has_property(o, k)
    }

    /// `o[key] = v`, generically.
    pub(crate) fn set_key(&mut self, o: Value, key: Key, v: Value) -> Result<()> {
        match key {
            Key::Name(a) => self.set(o, a, v),
            Key::Index(_) => {
                let k = self.key_value(key);
                self.set_keyed(o, k, v)
            }
        }
    }

    /// `o[key] = v` where sloppy mode's silence is an error (a `TypeError`
    /// where the store couldn't happen): the spec's `Set` with `Throw`
    /// true, as the builtins use it.
    pub(crate) fn put_key(&mut self, o: Value, key: Key, v: Value) -> Result<()> {
        if is_object(o) {
            let obj = as_object(o);
            let own = self.own_property(obj, key);
            let blocked = match own {
                Some(d) if d.is_data() => d.writable == Some(false),
                Some(d) => d.set.is_some_and(|s| s.is_undefined()),
                None => {
                    // An inherited read-only or a setter-less accessor
                    // blocks too; else the object must be extensible.
                    let inherited = self.inherited_property(obj, key);
                    match inherited {
                        Some(d) if d.is_data() => d.writable == Some(false),
                        Some(d) => d.set.is_some_and(|s| s.is_undefined()),
                        None => !Self::is_extensible(obj),
                    }
                }
            };
            if blocked {
                let k = self.key_string(key);
                return Err(Error::type_error(format!(
                    "Cannot assign to read only property '{}' of object",
                    crate::vm::display(k)
                )));
            }
        }
        self.set_key(o, key, v)
    }

    /// `key`'s property on `o`'s prototype chain, `o` excluded.
    fn inherited_property(&mut self, o: *mut Object, key: Key) -> Option<Desc> {
        let mut p = unsafe { (*(*o).shape).proto };
        while p.is_cell() {
            let po = as_object(p);
            if let Some(d) = self.own_property(po, key) {
                return Some(d);
            }
            p = unsafe { (*(*po).shape).proto };
        }
        None
    }

    /// `delete o[key]` where failure is a `TypeError` (the builtins').
    pub(crate) fn delete_key(&mut self, o: Value, key: Key) -> Result<()> {
        if is_object(o) && !self.delete_own(as_object(o), key) {
            let k = self.key_string(key);
            return Err(Error::type_error(format!(
                "Cannot delete property '{}' of object",
                crate::vm::display(k)
            )));
        }
        Ok(())
    }

    /// CreateDataPropertyOrThrow: `o[key] = v` as a new plain property.
    pub(crate) fn create_data_property(&mut self, o: Value, key: Key, v: Value) -> Result<()> {
        let obj = as_object(o);
        let d = Desc::data(v, true, true, true);
        if !self.define_own_property(obj, key, &d)? {
            let k = self.key_string(key);
            return Err(Error::type_error(format!(
                "Cannot define property '{}'",
                crate::vm::display(k)
            )));
        }
        Ok(())
    }

    /// `o.length` as a length (ToLength: an integer from 0 to 2^53 - 1).
    pub(crate) fn length_of(&mut self, o: Value) -> Result<f64> {
        let n = self.get(o, self.a_length)?;
        let n = to_number(n);
        Ok(if n.is_nan() || n <= 0.0 {
            0.0
        } else {
            n.trunc().min(9007199254740991.0)
        })
    }
}

/// `Object`'s functions and `Object.prototype`'s methods over descriptors.
pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = vm.object_prototype;
    let a_object = vm.intern("Object");
    let object = as_object(vm.get(object_value(g), a_object).unwrap());
    vm.define_natives(
        object,
        &[
            ("defineProperty", object_define_property as NativeFn, 3),
            ("defineProperties", object_define_properties, 2),
            ("getOwnPropertyDescriptor", object_get_own_property_descriptor, 2),
            ("getOwnPropertyNames", object_get_own_property_names, 1),
            ("create", object_create, 2),
            ("keys", object_keys, 1),
            ("getPrototypeOf", object_get_prototype_of, 1),
            ("setPrototypeOf", object_set_prototype_of, 2),
            ("preventExtensions", object_prevent_extensions, 1),
            ("isExtensible", object_is_extensible, 1),
            ("seal", object_seal, 1),
            ("isSealed", object_is_sealed, 1),
            ("freeze", object_freeze, 1),
            ("isFrozen", object_is_frozen, 1),
            ("is", object_is, 2),
            ("assign", object_assign, 2),
            ("values", object_values, 1),
            ("entries", object_entries, 1),
            ("getOwnPropertyDescriptors", object_get_own_property_descriptors, 1),
        ],
    );
    vm.define_natives(
        op,
        &[
            ("hasOwnProperty", object_has_own_property as NativeFn, 1),
            ("isPrototypeOf", object_is_prototype_of, 1),
            ("propertyIsEnumerable", object_property_is_enumerable, 1),
            ("toLocaleString", object_to_locale_string, 0),
            ("__defineGetter__", object_define_getter, 2),
            ("__defineSetter__", object_define_setter, 2),
            ("__lookupGetter__", object_lookup_getter, 1),
            ("__lookupSetter__", object_lookup_setter, 1),
        ],
    );
    let get = vm.native_function("get __proto__", object_get_proto, 0, crate::jit::Ty::Any, false);
    let set = vm.native_function("set __proto__", object_set_proto, 1, crate::jit::Ty::Any, false);
    let a = vm.intern("__proto__");
    vm.define_accessor(op, a, Some(object_value(get)), Some(object_value(set)), DONT_ENUM);
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// The object argument of an `Object` function that takes only objects.
fn object_arg(v: Value, what: &str) -> Result<*mut Object> {
    if !is_object(v) {
        return Err(Error::type_error(format!("{what} called on non-object")));
    }
    Ok(as_object(v))
}

/// `Object.defineProperty(o, key, descriptor)`.
fn object_define_property(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = object_arg(arg(a, 0), "Object.defineProperty")?;
    let key = vm.to_key(arg(a, 1));
    let d = vm.to_descriptor(arg(a, 2))?;
    if !vm.define_own_property(o, key, &d)? {
        let k = vm.key_string(key);
        return Err(Error::type_error(format!(
            "Cannot redefine property: {}",
            crate::vm::display(k)
        )));
    }
    Ok(arg(a, 0))
}

/// The descriptors of `props`' own enumerable properties, for
/// `Object.defineProperties` and `Object.create`.
fn descriptors(vm: &mut Vm, props: Value) -> Result<Vec<(Key, Desc)>> {
    let po = vm.to_object(props)?;
    let mut out = Vec::new();
    vm.handle_scope(|vm| {
        for (key, _) in vm.own_keys(po) {
            // (Enumerability as of now: a getter may have changed it.)
            let Some(d) = vm.own_property(po, key) else { continue };
            if d.enumerable != Some(true) {
                continue;
            }
            let v = vm.get_key(object_value(po), key)?;
            let d = vm.to_descriptor(v)?;
            for v in [d.value, d.get, d.set].into_iter().flatten() {
                vm.handles.push(v);
            }
            out.push((key, d));
        }
        Ok(())
    })?;
    Ok(out)
}

fn define_all(vm: &mut Vm, o: *mut Object, descs: &[(Key, Desc)]) -> Result<()> {
    for &(key, d) in descs {
        if !vm.define_own_property(o, key, &d)? {
            let k = vm.key_string(key);
            return Err(Error::type_error(format!(
                "Cannot redefine property: {}",
                crate::vm::display(k)
            )));
        }
    }
    Ok(())
}

/// `Object.defineProperties(o, props)`.
fn object_define_properties(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = object_arg(arg(a, 0), "Object.defineProperties")?;
    let descs = descriptors(vm, arg(a, 1))?;
    vm.handle_scope(|vm| {
        for (_, d) in &descs {
            for v in [d.value, d.get, d.set].into_iter().flatten() {
                vm.handles.push(v);
            }
        }
        define_all(vm, o, &descs)
    })?;
    Ok(arg(a, 0))
}

/// `Object.create(proto, props)`.
fn object_create(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let proto = arg(a, 0);
    if !(proto.is_null() || is_object(proto)) {
        return Err(Error::type_error("Object prototype may only be an Object or null"));
    }
    let o = vm.new_object(proto);
    if !arg(a, 1).is_undefined() {
        let descs = vm.handle_scope(|vm| {
            vm.handles.push(object_value(o));
            descriptors(vm, arg(a, 1))
        })?;
        vm.handle_scope(|vm| {
            vm.handles.push(object_value(o));
            for (_, d) in &descs {
                for v in [d.value, d.get, d.set].into_iter().flatten() {
                    vm.handles.push(v);
                }
            }
            define_all(vm, o, &descs)
        })?;
    }
    Ok(object_value(o))
}

/// `Object.getOwnPropertyDescriptor(o, key)`.
fn object_get_own_property_descriptor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = vm.to_object(arg(a, 0))?;
    let key = vm.to_key(arg(a, 1));
    Ok(match vm.own_property(o, key) {
        Some(d) => vm.from_descriptor(&d),
        None => Value::UNDEFINED,
    })
}

/// An array of `o`'s own keys (`all`: including the non-enumerable).
fn keys_array(vm: &mut Vm, o: *mut Object, all: bool) -> Result<Value> {
    let keys = vm.own_keys(o);
    let arr = vm.new_array();
    let mut values = Vec::with_capacity(keys.len());
    vm.handle_scope(|vm| {
        for (key, enumerable) in keys {
            if all || enumerable {
                let s = vm.key_string(key);
                vm.handles.push(s);
                values.push(s);
            }
        }
        crate::vm::array_push(vm, arr, &values)
    })?;
    Ok(arr)
}

/// `Object.getOwnPropertyNames(o)`.
fn object_get_own_property_names(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = vm.to_object(arg(a, 0))?;
    keys_array(vm, o, true)
}

/// `Object.keys(o)`: its own enumerable keys.
fn object_keys(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = vm.to_object(arg(a, 0))?;
    keys_array(vm, o, false)
}

/// `Object.getPrototypeOf(o)`.
fn object_get_prototype_of(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = vm.to_object(arg(a, 0))?;
    Ok(unsafe { (*(*o).shape).proto })
}

/// `Object.setPrototypeOf(o, proto)`.
fn object_set_prototype_of(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (o, proto) = (arg(a, 0), arg(a, 1));
    if o.is_undefined() || o.is_null() {
        return Err(Error::type_error("Object.setPrototypeOf called on null or undefined"));
    }
    if !(proto.is_null() || is_object(proto)) {
        return Err(Error::type_error("Object prototype may only be an Object or null"));
    }
    if is_object(o) && !vm.set_prototype_of(as_object(o), proto) {
        return Err(Error::type_error("Cannot set prototype"));
    }
    Ok(o)
}

fn object_prevent_extensions(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = arg(a, 0);
    if is_object(o) {
        vm.prevent_extensions(as_object(o));
    }
    Ok(o)
}

fn object_is_extensible(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = arg(a, 0);
    Ok(Value::from_bool(is_object(o) && Vm::is_extensible(as_object(o))))
}

fn object_seal(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = arg(a, 0);
    if is_object(o) {
        vm.seal(as_object(o), false);
    }
    Ok(o)
}

fn object_freeze(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = arg(a, 0);
    if is_object(o) {
        let obj = as_object(o);
        if unsafe { (*obj).class == Class::TypedArray && (*obj).elems_len > 0 } {
            return Err(Error::type_error("Cannot freeze array buffer views with elements"));
        }
        vm.seal(obj, true);
    }
    Ok(o)
}

fn object_is_sealed(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = arg(a, 0);
    Ok(Value::from_bool(!is_object(o) || vm.is_sealed(as_object(o), false)))
}

fn object_is_frozen(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = arg(a, 0);
    Ok(Value::from_bool(!is_object(o) || vm.is_sealed(as_object(o), true)))
}

/// `Object.is(a, b)`: SameValue.
fn object_is(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(same_value(arg(a, 0), arg(a, 1))))
}

/// `Object.assign(target, …sources)`.
fn object_assign(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let target = vm.to_object(arg(a, 0))?;
    let t = object_value(target);
    for &src in a.iter().skip(1) {
        if src.is_undefined() || src.is_null() {
            continue;
        }
        let so = vm.to_object(src)?;
        let s = object_value(so);
        // (String keys, then symbols.)
        let mut keys: Vec<Key> = vm.own_keys(so).into_iter().map(|(k, _)| k).collect();
        for sym in crate::symbol::own_symbol_keys(vm, so) {
            keys.push(Key::Name(crate::symbol::symbol_atom(sym)));
        }
        for key in keys {
            let Some(d) = vm.own_property(so, key) else { continue };
            if d.enumerable != Some(true) {
                continue;
            }
            let v = vm.get_key(s, key)?;
            vm.put_key(t, key, v)?;
        }
    }
    Ok(t)
}

/// `Object.values(o)` (`entries`: `Object.entries(o)`): the own
/// enumerable string-keyed properties' values, or `[key, value]` pairs.
fn values_or_entries(vm: &mut Vm, a: &[Value], entries: bool) -> Result<Value> {
    let o = vm.to_object(arg(a, 0))?;
    let ov = object_value(o);
    let out = vm.new_array();
    let mut n = 0;
    for (key, _) in vm.own_keys(o) {
        // (Enumerability as of now.)
        let Some(d) = vm.own_property(o, key) else { continue };
        if d.enumerable != Some(true) {
            continue;
        }
        let v = vm.get_key(ov, key)?;
        let item = if entries {
            let k = vm.key_string(key);
            let pair = vm.new_array();
            crate::vm::array_push(vm, pair, &[k, v])?;
            pair
        } else {
            v
        };
        vm.create_data_property(out, Key::Index(n), item)?;
        n += 1;
    }
    Ok(out)
}

fn object_values(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    values_or_entries(vm, a, false)
}

fn object_entries(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    values_or_entries(vm, a, true)
}

/// `Object.getOwnPropertyDescriptors(o)`.
fn object_get_own_property_descriptors(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = vm.to_object(arg(a, 0))?;
    let op = object_value(vm.object_prototype);
    let out = object_value(vm.new_object(op));
    for (key, _) in vm.own_keys(o) {
        if let Some(d) = vm.own_property(o, key) {
            let dv = vm.from_descriptor(&d);
            vm.create_data_property(out, key, dv)?;
        }
    }
    Ok(out)
}

/// `Object.prototype.hasOwnProperty(key)`.
fn object_has_own_property(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let key = vm.to_key(arg(a, 0));
    let o = vm.to_object(this)?;
    Ok(Value::from_bool(vm.own_property(o, key).is_some()))
}

/// `Object.prototype.isPrototypeOf(v)`.
fn object_is_prototype_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let v = arg(a, 0);
    if !is_object(v) {
        return Ok(Value::FALSE);
    }
    let o = vm.to_object(this)?;
    let mut p = unsafe { (*(*as_object(v)).shape).proto };
    while p.is_cell() {
        if as_object(p) == o {
            return Ok(Value::TRUE);
        }
        p = unsafe { (*(*as_object(p)).shape).proto };
    }
    Ok(Value::FALSE)
}

/// `Object.prototype.propertyIsEnumerable(key)`.
fn object_property_is_enumerable(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let key = vm.to_key(arg(a, 0));
    let o = vm.to_object(this)?;
    Ok(Value::from_bool(vm.own_property(o, key).is_some_and(|d| d.enumerable == Some(true))))
}

/// `Object.prototype.toLocaleString()`: `this.toString()`.
fn object_to_locale_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if this.is_undefined() || this.is_null() {
        return Err(Error::type_error("Object.prototype.toLocaleString called on null or undefined"));
    }
    let f = vm.get(this, vm.a_to_string)?;
    call_function(vm, f, this, &[])
}

/// `Object.prototype.__proto__`'s getter.
fn object_get_proto(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = vm.to_object(this)?;
    Ok(unsafe { (*(*o).shape).proto })
}

/// `Object.prototype.__proto__`'s setter.
fn object_set_proto(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let proto = arg(a, 0);
    if this.is_undefined() || this.is_null() {
        return Err(Error::type_error("Object.prototype.__proto__ called on null or undefined"));
    }
    if !(proto.is_null() || is_object(proto)) || !is_object(this) {
        return Ok(Value::UNDEFINED);
    }
    if !vm.set_prototype_of(as_object(this), proto) {
        return Err(Error::type_error("Cannot set prototype"));
    }
    Ok(Value::UNDEFINED)
}

fn define_accessor_half(vm: &mut Vm, this: Value, a: &[Value], setter: bool) -> Result<Value> {
    let o = vm.to_object(this)?;
    let f = arg(a, 1);
    if !is_callable(f) {
        return Err(Error::type_error("Getter must be a function"));
    }
    let key = vm.to_key(arg(a, 0));
    let d = Desc {
        get: (!setter).then_some(f),
        set: setter.then_some(f),
        enumerable: Some(true),
        configurable: Some(true),
        ..Desc::default()
    };
    if !vm.define_own_property(o, key, &d)? {
        return Err(Error::type_error("Cannot redefine property"));
    }
    Ok(Value::UNDEFINED)
}

fn object_define_getter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    define_accessor_half(vm, this, a, false)
}

fn object_define_setter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    define_accessor_half(vm, this, a, true)
}

fn lookup_accessor_half(vm: &mut Vm, this: Value, a: &[Value], setter: bool) -> Result<Value> {
    let o = vm.to_object(this)?;
    let key = vm.to_key(arg(a, 0));
    let mut p = object_value(o);
    while p.is_cell() {
        let po = as_object(p);
        if let Some(d) = vm.own_property(po, key) {
            return Ok(if d.is_accessor() {
                if setter { d.set.unwrap() } else { d.get.unwrap() }
            } else {
                Value::UNDEFINED
            });
        }
        p = unsafe { (*(*po).shape).proto };
    }
    Ok(Value::UNDEFINED)
}

fn object_lookup_getter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    lookup_accessor_half(vm, this, a, false)
}

fn object_lookup_setter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    lookup_accessor_half(vm, this, a, true)
}
