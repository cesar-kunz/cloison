//! `Intl.Collator`. A collator's internal slots: its ICU4X collator (an
//! index into `vm.intl`'s), its bound `compare` (undefined until read),
//! its locale (a string), and the rest of its resolved options packed in
//! an int ([`Flags`]). Its [[Collation]] is "default" (the data has no
//! other collation types).

use super::icu::{self, CaseFirst, CollatorKey, Sensitivity};
use super::{Root, bool_option, canonicalize_locale_list, coerce_options, create, data_error, resolve_locale, root, slot, string_array, string_option, supported_locales};
use crate::builtins::is_class;
use crate::object::{Class, Object, as_object, object_value};
use crate::temporal::arg;
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, string_units};

const HANDLE: usize = 0;
const BOUND_COMPARE: usize = 1;
const LOCALE: usize = 2;
const FLAGS: usize = 3;

const SENSITIVITIES: [&str; 4] = ["base", "accent", "case", "variant"];
const CASE_FIRSTS: [&str; 3] = ["false", "lower", "upper"];

/// A collator's resolved options but its locale.
#[derive(Clone, Copy)]
struct Flags {
    search: bool,
    sensitivity: Sensitivity,
    ignore_punctuation: bool,
    numeric: bool,
    case_first: CaseFirst,
}

impl Flags {
    fn pack(self) -> i32 {
        self.search as i32 | (self.sensitivity as i32) << 1 | (self.ignore_punctuation as i32) << 3 | (self.numeric as i32) << 4 | (self.case_first as i32) << 5
    }

    fn unpack(f: i32) -> Flags {
        Flags {
            search: f & 1 != 0,
            sensitivity: [Sensitivity::Base, Sensitivity::Accent, Sensitivity::Case, Sensitivity::Variant][(f >> 1 & 3) as usize],
            ignore_punctuation: f >> 3 & 1 != 0,
            numeric: f >> 4 & 1 != 0,
            case_first: [CaseFirst::False, CaseFirst::Lower, CaseFirst::Upper][(f >> 5 & 3) as usize],
        }
    }
}

pub(super) fn install(vm: &mut Vm, intl: *mut Object) {
    let op = object_value(vm.object_prototype);
    let proto = vm.new_object(op);
    let c = vm.define_native(intl, "Collator", collator_ctor, 0, crate::jit::Ty::Any, true);
    crate::builtins::link_constructor(vm, c, proto);
    vm.define_natives(c, &[("supportedLocalesOf", supported_locales_of as NativeFn, 1)]);
    crate::builtins::getter(vm, proto, "compare", compare_getter);
    vm.define_natives(proto, &[("resolvedOptions", resolved_options as NativeFn, 0)]);
    crate::symbol::to_string_tag(vm, proto, "Intl.Collator");
    vm.intl_roots[Root::CollatorPrototype as usize] = proto;
    vm.intl_roots[Root::Compare as usize] = vm.native_function("", bound_compare, 2, crate::jit::Ty::Any, false);
}

/// `Intl.Collator(locales, options)`, with `new` or without.
fn collator_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let construct = vm.is_construct();
    let f = vm.callee;
    let default = object_value(root(vm, Root::CollatorPrototype));
    let proto = if construct { vm.take_new_target(f, default)? } else { default };
    // InitializeCollator.
    let requested = canonicalize_locale_list(vm, arg(a, 0))?;
    let options = coerce_options(vm, arg(a, 1))?;
    let search = string_option(vm, options, "usage", &["sort", "search"])?.as_deref() == Some("search");
    string_option(vm, options, "localeMatcher", &["lookup", "best fit"])?;
    let collation = string_option(vm, options, "collation", &[])?;
    if let Some(c) = &collation
        && !is_type(c)
    {
        return Err(Error::range_error(format!("Invalid collation: {c}")));
    }
    let numeric = bool_option(vm, options, "numeric")?.map(|b| b.to_string());
    let case_first = string_option(vm, options, "caseFirst", &CASE_FIRSTS)?;
    let r = resolve_locale(
        vm,
        &requested,
        &[
            ("co", &[None], collation),
            ("kf", &[Some("false"), Some("lower"), Some("upper")], case_first),
            ("kn", &[Some("false"), Some("true")], numeric),
        ],
    )?;
    let sensitivity = match string_option(vm, options, "sensitivity", &SENSITIVITIES)? {
        Some(s) => [Sensitivity::Base, Sensitivity::Accent, Sensitivity::Case, Sensitivity::Variant][SENSITIVITIES.iter().position(|&x| x == s).unwrap()],
        None => Sensitivity::Variant,
    };
    let ignore_punctuation = bool_option(vm, options, "ignorePunctuation")?.unwrap_or(false);
    let flags = Flags {
        search,
        sensitivity,
        ignore_punctuation,
        numeric: r.values[2].as_deref() == Some("true"),
        case_first: [CaseFirst::False, CaseFirst::Lower, CaseFirst::Upper][CASE_FIRSTS.iter().position(|&x| Some(x) == r.values[1].as_deref()).unwrap_or(0)],
    };
    let key = CollatorKey { locale: r.data_locale, sensitivity, ignore_punctuation, numeric: flags.numeric, case_first: flags.case_first };
    let handle = match vm.intl.collator_ids.get(&key) {
        Some(&h) => h,
        None => {
            let c = icu::Collator::new(&key).map_err(data_error)?;
            let h = vm.intl.collators.len() as u32;
            vm.intl.collators.push(c);
            vm.intl.collator_ids.insert(key, h);
            h
        }
    };
    let locale = crate::temporal::string(vm, &r.locale);
    Ok(create(vm, proto, &[Value::from_i32(handle as i32), Value::UNDEFINED, locale, Value::from_i32(flags.pack())]))
}

/// Whether `s` is a `type` of UTS #35's grammar: alphanumeric subtags of
/// 3 to 8, joined by `-`.
fn is_type(s: &str) -> bool {
    s.split('-').all(|t| (3..=8).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn this_collator(this: Value, what: &str) -> Result<*mut Object> {
    if is_class(this, Class::Intl) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error(format!("Method Intl.Collator.prototype.{what} called on incompatible receiver {}", crate::vm::display(this))))
    }
}

/// `get Intl.Collator.prototype.compare`: the collator's bound compare
/// function, made on first read.
fn compare_getter(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_collator(this, "compare")?;
    let f = slot(o, BOUND_COMPARE);
    if !f.is_undefined() {
        return Ok(f);
    }
    let template = root(vm, Root::Compare);
    let f = crate::promise::native_closure(vm, template, &[slot(o, HANDLE)]);
    crate::promise::set_slot(o, BOUND_COMPARE, f);
    Ok(f)
}

/// A collator's bound compare function (a closure of its handle):
/// CompareStrings of its arguments as strings.
fn bound_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let h = slot(vm.callee, 0).as_i32_unchecked() as usize;
    let x = crate::intrinsics::to_string(vm, arg(a, 0))?;
    let y = crate::intrinsics::to_string(vm, arg(a, 1))?;
    let ord = vm.intl.collators[h].compare(string_units(x).unwrap_or(&[]), string_units(y).unwrap_or(&[]));
    Ok(Value::from_i32(ord as i32))
}

/// `Intl.Collator.prototype.resolvedOptions()`.
fn resolved_options(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_collator(this, "resolvedOptions")?;
    let f = Flags::unpack(slot(o, FLAGS).as_i32_unchecked());
    let op = object_value(vm.object_prototype);
    let r = vm.new_object(op);
    let usage = if f.search { "search" } else { "sort" };
    let strings = [
        ("locale", None),
        ("usage", Some(usage)),
        ("sensitivity", Some(SENSITIVITIES[f.sensitivity as usize])),
        ("ignorePunctuation", None),
        ("collation", Some("default")),
        ("numeric", None),
        ("caseFirst", Some(CASE_FIRSTS[f.case_first as usize])),
    ];
    for (name, s) in strings {
        let v = match (name, s) {
            ("locale", _) => slot(o, LOCALE),
            ("ignorePunctuation", _) => Value::from_bool(f.ignore_punctuation),
            ("numeric", _) => Value::from_bool(f.numeric),
            (_, Some(s)) => crate::temporal::string(vm, s),
            _ => unreachable!(),
        };
        let a = vm.intern(name);
        vm.add_property(r, a, v);
    }
    Ok(object_value(r))
}

/// `Intl.Collator.supportedLocalesOf(locales, options)`.
fn supported_locales_of(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let requested = canonicalize_locale_list(vm, arg(a, 0))?;
    let options = coerce_options(vm, arg(a, 1))?;
    string_option(vm, options, "localeMatcher", &["lookup", "best fit"])?;
    let found = supported_locales(vm, &requested)?;
    string_array(vm, &found)
}
