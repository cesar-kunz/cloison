//! ECMA-402, the `Intl` object (the `intl` feature): `Intl.Collator`
//! alone so far, what TypeScript's compiler uses. ICU4X does the work
//! behind [`icu`]; this module is the JS side: locale lists, options,
//! locale resolution, the objects. See docs/intl.md.
//!
//! An `Intl` object is of class `Intl`, its internal slots hidden
//! in-object slots ([`slot`]); `vm.intl` holds what Rust needs (the
//! collators, interned by what they are made of).

mod collator;
mod icu;

use std::collections::HashMap;

use crate::object::{DONT_ENUM, Object, object_value};
use crate::props::is_object;
use crate::temporal::to_rust_string;
use crate::value::Value;
use crate::vm::{Error, Result, Vm, string_units};

/// The locale `Intl` takes when none asked for is available
/// (DefaultLocale), one the data must have.
const DEFAULT_LOCALE: &str = "en-US";

/// The VM's `Intl` state: the collators made, the locales asked about.
#[derive(Default)]
pub(crate) struct State {
    collators: Vec<icu::Collator>,
    collator_ids: HashMap<icu::CollatorKey, u32>,
    available: HashMap<String, bool>,
}

/// [`Vm::intl_roots`]' entries (each realm's).
#[derive(Clone, Copy)]
enum Root {
    CollatorPrototype,
    /// The template of collators' bound `compare` functions.
    Compare,
}

fn root(vm: &Vm, r: Root) -> *mut Object {
    vm.intl_roots[r as usize]
}

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let intl = vm.new_object(op);
    let a = vm.intern("Intl");
    vm.add_property_with(g, a, object_value(intl), DONT_ENUM);
    crate::symbol::to_string_tag(vm, intl, "Intl");
    vm.intl_roots = vec![std::ptr::null_mut(); 2];
    collator::install(vm, intl);
}

fn data_error(e: String) -> Error {
    Error(format!("Error: {e}"), None)
}

// ---- Objects ----

/// A new `Intl` object of prototype `proto` with internal slots `slots`.
fn create(vm: &mut Vm, proto: Value, slots: &[Value]) -> Value {
    let full = vm.root_shape(crate::object::Class::Intl, proto, slots.len() as u32);
    let bare = vm.root_shape(crate::object::Class::Intl, proto, 0);
    let o = vm.heap.alloc_object(unsafe { &*full });
    unsafe { crate::object::set_shape(o, bare) };
    for (i, &v) in slots.iter().enumerate() {
        crate::promise::set_slot(o, i, v);
    }
    object_value(o)
}

/// Internal slot `i` of `Intl` object `o`.
fn slot(o: *mut Object, i: usize) -> Value {
    crate::promise::slot(o, i)
}

// ---- Locales ----

/// CanonicalizeLocaleList.
fn canonicalize_locale_list(vm: &mut Vm, locales: Value) -> Result<Vec<String>> {
    if locales.is_undefined() {
        return Ok(Vec::new());
    }
    let mut seen = Vec::new();
    if string_units(locales).is_some() {
        seen.push(canonical_tag(&to_rust_string(vm, locales)?)?);
        return Ok(seen);
    }
    let o = object_value(vm.to_object(locales)?);
    let len = vm.length_of(o)?;
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let mut k = 0.0;
    while k < len {
        let key = Value::number(k);
        if vm.has_property(o, key)? {
            let v = vm.get_keyed(o, key)?;
            if string_units(v).is_none() && !is_object(v) {
                return Err(Error::type_error("a locale must be a string or an object"));
            }
            let tag = canonical_tag(&to_rust_string(vm, v)?)?;
            if !seen.contains(&tag) {
                seen.push(tag);
            }
        }
        k += 1.0;
    }
    Ok(seen)
}

/// IsStructurallyValidLanguageTag, then CanonicalizeUnicodeLocaleId.
fn canonical_tag(tag: &str) -> Result<String> {
    let subtags: Vec<&str> = tag.split('-').collect();
    // (What ICU4X's parser lets through: a variant or a singleton twice.)
    let mut variants = Vec::new();
    let mut singletons = Vec::new();
    for (i, s) in subtags.iter().enumerate() {
        let s = s.to_ascii_lowercase();
        if s.len() == 1 {
            if singletons.contains(&s) {
                return Err(invalid_tag(tag));
            }
            if s == "x" {
                break;
            }
            singletons.push(s);
        } else if i > 0 && singletons.is_empty() && (s.len() >= 5 || s.len() == 4 && s.as_bytes()[0].is_ascii_digit()) {
            if variants.contains(&s) {
                return Err(invalid_tag(tag));
            }
            variants.push(s);
        }
    }
    icu::canonicalize(tag).ok_or_else(|| invalid_tag(tag))
}

fn invalid_tag(tag: &str) -> Error {
    Error::range_error(format!("Incorrect locale information provided: {tag}"))
}

/// A canonical tag without its Unicode extension (`-u-…`), and the
/// extension's subtags (after `u`), if any.
fn split_extension(tag: &str) -> (String, Option<Vec<String>>) {
    let subtags: Vec<&str> = tag.split('-').collect();
    let Some(u) = subtags.iter().position(|&s| s == "u") else {
        return (tag.into(), None);
    };
    if subtags[..u].contains(&"x") {
        return (tag.into(), None);
    }
    let end = (u + 1..subtags.len()).find(|&i| subtags[i].len() == 1).unwrap_or(subtags.len());
    let mut rest = subtags[..u].to_vec();
    rest.extend_from_slice(&subtags[end..]);
    (rest.join("-"), Some(subtags[u + 1..end].iter().map(|s| s.to_string()).collect()))
}

/// An extension's value for `key` ("" for a key without one), if it has
/// the key.
fn keyword(ext: &[String], key: &str) -> Option<String> {
    let at = ext.iter().position(|s| s == key)?;
    let values: Vec<&str> = ext[at + 1..].iter().take_while(|s| s.len() > 2).map(String::as_str).collect();
    Some(values.join("-"))
}

/// BestAvailableLocale: `locale` (no extensions) or the longest of its
/// prefixes the data has.
fn best_available(vm: &mut Vm, locale: &str) -> Result<Option<String>> {
    let mut candidate = locale.to_string();
    loop {
        if candidate != "und" && is_available(vm, &candidate)? {
            return Ok(Some(candidate));
        }
        let Some(mut pos) = candidate.rfind('-') else {
            return Ok(None);
        };
        if pos >= 2 && candidate.as_bytes()[pos - 2] == b'-' {
            pos -= 2;
        }
        candidate.truncate(pos);
    }
}

fn is_available(vm: &mut Vm, locale: &str) -> Result<bool> {
    if let Some(&a) = vm.intl.available.get(locale) {
        return Ok(a);
    }
    let a = icu::available(locale).map_err(data_error)?;
    vm.intl.available.insert(locale.into(), a);
    Ok(a)
}

/// LookupSupportedLocales (best fit is lookup).
fn supported_locales(vm: &mut Vm, requested: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for locale in requested {
        if best_available(vm, &split_extension(locale).0)?.is_some() {
            out.push(locale.clone());
        }
    }
    Ok(out)
}

/// What ResolveLocale resolves: the locale (with the extension keywords
/// it took), the locale the data is for, each relevant key's value.
struct Resolved {
    locale: String,
    data_locale: String,
    values: Vec<Option<String>>,
}

/// ResolveLocale (best fit is lookup): `keys` are the relevant extension
/// keys, each with its values the data supports (the first the default;
/// `None` for null) and what the options asked for it.
fn resolve_locale(vm: &mut Vm, requested: &[String], keys: &[(&str, &[Option<&str>], Option<String>)]) -> Result<Resolved> {
    let mut found = None;
    for locale in requested {
        let (bare, ext) = split_extension(locale);
        if let Some(a) = best_available(vm, &bare)? {
            found = Some((a, ext));
            break;
        }
    }
    let (data_locale, ext) = found.unwrap_or_else(|| (DEFAULT_LOCALE.into(), None));
    let mut values = Vec::new();
    let mut supported = Vec::new();
    for &(key, data, ref option) in keys {
        let mut value = data[0].map(String::from);
        let mut keyword_used = None;
        if let Some(requested) = ext.as_deref().and_then(|e| keyword(e, key)) {
            if !requested.is_empty() {
                if data.contains(&Some(requested.as_str())) {
                    value = Some(requested.clone());
                    keyword_used = Some(format!("{key}-{requested}"));
                }
            } else if data.contains(&Some("true")) {
                value = Some("true".into());
                keyword_used = Some(key.to_string());
            }
        }
        if let Some(o) = option {
            let o = if o.is_empty() { "true".to_string() } else { o.to_ascii_lowercase() };
            if data.contains(&Some(o.as_str())) && value.as_deref() != Some(&o) {
                value = Some(o);
                keyword_used = None;
            }
        }
        supported.extend(keyword_used);
        values.push(value);
    }
    let locale = if supported.is_empty() {
        data_locale.clone()
    } else {
        canonical_tag(&format!("{data_locale}-u-{}", supported.join("-")))?
    };
    Ok(Resolved { locale, data_locale, values })
}

// ---- Options ----

/// CoerceOptionsToObject.
fn coerce_options(vm: &mut Vm, v: Value) -> Result<Value> {
    if v.is_undefined() {
        return Ok(object_value(vm.new_object(Value::NULL)));
    }
    Ok(object_value(vm.to_object(v)?))
}

/// GetOption of a string: undefined, or one of `allowed` (any, if empty).
fn string_option(vm: &mut Vm, options: Value, name: &str, allowed: &[&str]) -> Result<Option<String>> {
    let a = vm.intern(name);
    let v = vm.get(options, a)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = to_rust_string(vm, v)?;
    if !allowed.is_empty() && !allowed.contains(&s.as_str()) {
        return Err(Error::range_error(format!("Value {s} out of range for Intl options property {name}")));
    }
    Ok(Some(s))
}

/// GetOption of a boolean.
fn bool_option(vm: &mut Vm, options: Value, name: &str) -> Result<Option<bool>> {
    let a = vm.intern(name);
    let v = vm.get(options, a)?;
    Ok((!v.is_undefined()).then(|| crate::interp::to_boolean(v)))
}

/// An array of `strings`.
fn string_array(vm: &mut Vm, strings: &[String]) -> Result<Value> {
    let arr = vm.new_array();
    for s in strings {
        let v = crate::temporal::string(vm, s);
        crate::vm::array_push(vm, arr, &[v])?;
    }
    Ok(arr)
}
