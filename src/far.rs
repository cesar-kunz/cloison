//! The far elements of sparse arrays (`docs/sparse-arrays.md`).
//!
//! An array with flag [`FAR`] keeps a dense prefix in its elements as any
//! array does (`elems[0..elems_len)`, holes `EMPTY`), but its `length` is
//! not `elems_len`: it, and the elements past the prefix, are in the *far
//! store*, a hidden array (`Class::Array`, which JS never sees) whose value
//! is in the array's `code` bits, traced by the collector as a
//! collection's entries are. The store is a hash table, open addressing
//! with linear probing:
//!
//! ```text
//!  0  length     number, up to 2^32 - 1
//!  1  count      int32, live entries
//!  2  used       int32, live entries and deleted ones
//!  3  mutations  int32, bumped by each new key (see [`next_key`])
//!  4  mask       int32, buckets - 1
//!  5… key, value a key is an index as a number (up to 2^32 - 2); a free
//!                bucket's is `EMPTY`, a deleted one's `DELETED`
//! ```
//!
//! Far elements are plain data elements: writable, enumerable and
//! configurable but as the array's flags say (`FROZEN`, `SEALED`), as its
//! dense ones. An element with attributes of its own or an accessor is a
//! named property of the shape (`SPARSE`), as in any array.
//!
//! Whether an array goes far, and back, is decided by [`Density`], which
//! the runtime alone consults: compiled code exits on `FAR` where it would
//! read the length (`get_length_*`, `array_pop_`), and a far array's shape
//! takes no store's fast path (`slow_elements`), so it never appends.

use std::sync::OnceLock;

use crate::object::{Class, FROZEN, LENGTH_READ_ONLY, NOT_EXTENSIBLE, Object, SEALED, as_object};
use crate::value::Value;
use crate::vm::{Code, MAX_ELEMENTS, Vm};

pub use crate::layout::FAR;

const LENGTH: usize = 0;
const COUNT: usize = 1;
const USED: usize = 2;
const MUTATIONS: usize = 3;
const MASK: usize = 4;
const BASE: usize = 5;
/// A deleted entry's key (no index is negative).
const DELETED: Value = Value::from_i32(-1);
/// The fewest buckets a store has.
const MIN_BUCKETS: usize = 8;

/// When an array is dense, and when far (see the module docs and
/// `docs/sparse-arrays.md`, "Transitions"). Read once per VM: from
/// `CLOISON_ARRAY_DENSE_MIN`, `CLOISON_ARRAY_DENSE_BACK`,
/// `CLOISON_ARRAY_WASTE` and `CLOISON_ARRAY_PREALLOC` at run time, else as
/// they were set when building,
/// else the defaults here; `--array-density` sets them for a run (see
/// [`set_density`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Density {
    /// An array stays or goes dense only while its elements are at least
    /// this share of its length.
    pub min: f64,
    /// A far array goes dense again once its elements are this share of
    /// its length (above `min`, so that one near the line doesn't flip).
    pub back: f64,
    /// An array whose holes take at most this many bytes stays dense, as
    /// sparse as it is.
    pub waste: usize,
    /// `waste` for a length set with no element (`new Array(n)`,
    /// `a.length = n`): mostly filled next, from 0.
    pub prealloc: usize,
}

impl Density {
    /// From the sweep over `bench/sparse` (`bench/sparse/README.md`): on
    /// the Pareto front of time and memory, within 1.23 and 1.12 times the
    /// best setting per workload (geometric means).
    const DEFAULT: Density = Density { min: 1.0 / 16.0, back: 1.0 / 8.0, waste: 8 << 10, prealloc: 16 << 20 };

    /// The settings from the environment, as the module docs say.
    pub fn from_env() -> Density {
        if let Some(&d) = OVERRIDE.get() {
            return d;
        }
        let var = |name: &str, built: Option<&'static str>| std::env::var(name).ok().or(built.map(str::to_string));
        let d = Density::DEFAULT;
        let min = var("CLOISON_ARRAY_DENSE_MIN", option_env!("CLOISON_ARRAY_DENSE_MIN"))
            .and_then(|s| parse_fraction(&s))
            .unwrap_or(d.min);
        let back = var("CLOISON_ARRAY_DENSE_BACK", option_env!("CLOISON_ARRAY_DENSE_BACK"))
            .and_then(|s| parse_fraction(&s))
            .unwrap_or(d.back.max(min));
        let waste = var("CLOISON_ARRAY_WASTE", option_env!("CLOISON_ARRAY_WASTE"))
            .and_then(|s| parse_bytes(&s))
            .unwrap_or(d.waste);
        let prealloc = var("CLOISON_ARRAY_PREALLOC", option_env!("CLOISON_ARRAY_PREALLOC"))
            .and_then(|s| parse_bytes(&s))
            .unwrap_or(d.prealloc);
        Density { min, back: back.max(min), waste, prealloc }
    }

    /// `D[,D'[,W[,P]]]`, as `--array-density` takes them: fractions (`1/8`
    /// or `0.125`) and bytes (`64k`, `1m`, `inf`).
    pub fn parse(s: &str) -> Option<Density> {
        let mut it = s.split(',');
        let min = parse_fraction(it.next()?)?;
        let back = it.next().map_or(Some(Density::DEFAULT.back.max(min)), parse_fraction)?;
        let waste = it.next().map_or(Some(Density::DEFAULT.waste), parse_bytes)?;
        let prealloc = it.next().map_or(Some(Density::DEFAULT.prealloc), parse_bytes)?;
        Some(Density { min, back: back.max(min), waste, prealloc })
    }

    /// Whether an array going to length `n`, with at most `present`
    /// elements, should be far: by a store if `store`, else by a length
    /// set alone (`prealloc` for `waste`).
    pub fn goes_far(&self, present: usize, n: usize, store: bool) -> bool {
        let waste = if store { self.waste } else { self.prealloc.max(self.waste) };
        n > MAX_ELEMENTS || (n - present.min(n)).saturating_mul(8) > waste && (present as f64) < self.min * n as f64
    }

    /// Whether a far array of length `n` with `present` elements should be
    /// dense again.
    pub fn goes_dense(&self, present: usize, n: usize) -> bool {
        n <= MAX_ELEMENTS && (present as f64 >= self.back * n as f64 || (n - present.min(n)).saturating_mul(8) <= self.waste)
    }
}

static OVERRIDE: OnceLock<Density> = OnceLock::new();

/// The settings every VM made from now on takes (`--array-density`).
pub fn set_density(d: Density) {
    let _ = OVERRIDE.set(d);
}

fn parse_fraction(s: &str) -> Option<f64> {
    let s = s.trim();
    let f = match s.split_once('/') {
        Some((a, b)) => a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?,
        None => s.parse::<f64>().ok()?,
    };
    (0.0..=1.0).contains(&f).then_some(f)
}

fn parse_bytes(s: &str) -> Option<usize> {
    let s = s.trim().to_ascii_lowercase();
    if s == "inf" || s == "max" {
        return Some(usize::MAX);
    }
    let (num, mul) = match s.as_bytes().last()? {
        b'k' => (&s[..s.len() - 1], 1 << 10),
        b'm' => (&s[..s.len() - 1], 1 << 20),
        b'g' => (&s[..s.len() - 1], 1 << 30),
        _ => (&s[..], 1),
    };
    num.parse::<usize>().ok()?.checked_mul(mul)
}

/// Whether dense array `o`, going to length `n` with `adding` elements
/// past its elements, should go far. Its elements' count bounds the
/// present ones; where that bound keeps it dense but its elements are
/// reallocated, large enough for their holes to pass `Density::waste`, the
/// present ones are counted, which costs what the copy does (else
/// `a[i * 16] = v` would leave 15 holes in 16 with each store under the
/// bound, and `a[1024]`, `a[3072]`, `a[7168]`, … would double the elements
/// each time).
pub(crate) fn goes_far(vm: &Vm, o: *mut Object, n: usize, adding: usize) -> bool {
    let (len, cap) = unsafe { ((*o).elems_len as usize, (*o).elems_cap as usize) };
    let d = &vm.density;
    let store = adding > 0;
    if d.goes_far(len + adding, n, store) {
        return true;
    }
    if n <= cap || n.saturating_mul(8) <= if store { d.waste } else { d.prealloc.max(d.waste) } {
        return false;
    }
    let present = (0..len).filter(|&k| unsafe { *(*o).elems.add(k) } != Value::EMPTY).count();
    d.goes_far(present + adding, n, store)
}

/// Whether `o` is a far array (the bit is another flag in other classes).
#[inline]
pub(crate) fn is_far(o: *mut Object) -> bool {
    unsafe { (*o).flags & FAR != 0 && (*o).class == Class::Array }
}

/// Array `o`'s `length`: its far store's, else its elements' count.
#[inline]
pub(crate) fn array_length(o: *mut Object) -> u32 {
    if is_far(o) { length(o) } else { unsafe { (*o).elems_len } }
}

fn store(o: *mut Object) -> *mut Object {
    unsafe { as_object(Value::from_bits((*o).code as u64)) }
}

fn slot(e: *mut Object, i: usize) -> Value {
    unsafe { *(*e).elems.add(i) }
}

fn put(e: *mut Object, i: usize, v: Value) {
    unsafe {
        let at = (*e).elems.add(i);
        *at = v;
        crate::heap::barrier(e.cast(), at, v);
    }
}

fn int(e: *mut Object, i: usize) -> usize {
    slot(e, i).as_i32_unchecked() as u32 as usize
}

fn set_int(e: *mut Object, i: usize, n: usize) {
    put(e, i, Value::from_i32(n as i32));
}

/// Far array `o`'s length.
pub(crate) fn length(o: *mut Object) -> u32 {
    slot(store(o), LENGTH).as_number().unwrap_or(0.0) as u32
}

/// Set far array `o`'s length to `n` (no key is at or past it).
pub(crate) fn set_length(o: *mut Object, n: u32) {
    put(store(o), LENGTH, Value::number(n as f64));
}

/// The number of far array `o`'s far elements.
pub(crate) fn count(o: *mut Object) -> usize {
    int(store(o), COUNT)
}

/// Far array `o`'s far keys in order, as [`next_key`] and [`prev_key`]
/// search them: sorted once, and again once a key has been added (a store
/// rehashed into is another; a key deleted since is still there, which
/// the caller's HasProperty finds absent).
#[derive(Default)]
pub(crate) struct SortedKeys {
    store: usize,
    mutations: usize,
    keys: Vec<u32>,
}

fn sorted(vm: &mut Vm, o: *mut Object) -> &[u32] {
    let e = store(o);
    let (at, m) = (e as usize, int(e, MUTATIONS));
    let c = &mut vm.far_keys;
    if c.store != at || c.mutations != m {
        c.keys = keys(o);
        c.keys.sort_unstable();
        c.store = at;
        c.mutations = m;
    }
    &vm.far_keys.keys
}

/// Far array `o`'s first far key at `k` or past it.
pub(crate) fn next_key(vm: &mut Vm, o: *mut Object, k: u32) -> Option<u32> {
    let keys = sorted(vm, o);
    keys.get(keys.partition_point(|&x| x < k)).copied()
}

/// Far array `o`'s last far key at `k` or before it.
pub(crate) fn prev_key(vm: &mut Vm, o: *mut Object, k: u32) -> Option<u32> {
    let keys = sorted(vm, o);
    keys.partition_point(|&x| x <= k).checked_sub(1).map(|j| keys[j])
}

fn key_value(i: u32) -> Value {
    Value::number(i as f64)
}

fn hash(i: u32) -> usize {
    // (murmur3's finalizer: strided indices spread over the buckets.)
    let mut h = i;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h as usize
}

/// The bucket of key `i` in store `e`, if it has it.
fn find(e: *mut Object, i: u32) -> Option<usize> {
    let mask = int(e, MASK);
    let k = key_value(i);
    let mut b = hash(i) & mask;
    loop {
        let at = BASE + 2 * b;
        let key = slot(e, at);
        if key == Value::EMPTY {
            return None;
        }
        if key == k {
            return Some(at);
        }
        b = (b + 1) & mask;
    }
}

/// Far array `o`'s far element `i`, if it has it.
pub(crate) fn get(o: *mut Object, i: u32) -> Option<Value> {
    let e = store(o);
    find(e, i).map(|at| slot(e, at + 1))
}

/// Whether far array `o` has far element `i`.
pub(crate) fn has(o: *mut Object, i: u32) -> bool {
    find(store(o), i).is_some()
}

/// Set far element `i` of far array `o` (`i` past its dense prefix and
/// within its length), adding it if new.
pub(crate) fn insert(vm: &mut Vm, o: *mut Object, i: u32, v: Value) {
    let mut e = store(o);
    if let Some(at) = find(e, i) {
        put(e, at + 1, v);
        return;
    }
    let buckets = int(e, MASK) + 1;
    if (int(e, USED) + 1) * 4 > buckets * 3 {
        // (Grown if more than half live, else rehashed as large: deleted
        // entries dropped. Small ones grow 4 times over: fewer tables left
        // to collect while an array fills.)
        let live = int(e, COUNT) + 1;
        let grow = if buckets < 1024 { 4 } else { 2 };
        let n = if live * 2 > buckets { buckets * grow } else { buckets };
        rehash(vm, o, n);
        e = store(o);
    }
    let mask = int(e, MASK);
    let mut b = hash(i) & mask;
    loop {
        let at = BASE + 2 * b;
        let key = slot(e, at);
        if key == Value::EMPTY || key == DELETED {
            if key == Value::EMPTY {
                set_int(e, USED, int(e, USED) + 1);
            }
            put(e, at, key_value(i));
            put(e, at + 1, v);
            break;
        }
        b = (b + 1) & mask;
    }
    set_int(e, COUNT, int(e, COUNT) + 1);
    set_int(e, MUTATIONS, (int(e, MUTATIONS) + 1) & i32::MAX as usize);
}

/// Remove far element `i` of far array `o`: whether it had it.
pub(crate) fn remove(o: *mut Object, i: u32) -> bool {
    let e = store(o);
    let Some(at) = find(e, i) else {
        return false;
    };
    // (A deleted key, not a free one: probes for keys after it go on.)
    put(e, at, DELETED);
    put(e, at + 1, Value::EMPTY);
    set_int(e, COUNT, int(e, COUNT) - 1);
    true
}

/// Far array `o`'s far keys, in no order.
pub(crate) fn keys(o: *mut Object) -> Vec<u32> {
    let e = store(o);
    let buckets = int(e, MASK) + 1;
    let mut out = Vec::with_capacity(int(e, COUNT));
    for b in 0..buckets {
        let key = slot(e, BASE + 2 * b);
        if key != Value::EMPTY && key != DELETED {
            out.push(key.as_number().unwrap() as u32);
        }
    }
    out
}

/// Far array `o`'s far entries, in no order.
fn entries(o: *mut Object) -> Vec<(u32, Value)> {
    let e = store(o);
    let buckets = int(e, MASK) + 1;
    let mut out = Vec::with_capacity(int(e, COUNT));
    for b in 0..buckets {
        let at = BASE + 2 * b;
        let key = slot(e, at);
        if key != Value::EMPTY && key != DELETED {
            out.push((key.as_number().unwrap() as u32, slot(e, at + 1)));
        }
    }
    out
}

/// A new store of `buckets` buckets for an array of length `len`.
fn new_store(vm: &mut Vm, buckets: usize, len: u32) -> *mut Object {
    let e = as_object(vm.new_array());
    let n = BASE + 2 * buckets;
    vm.reserve_elems(e, n);
    unsafe {
        for k in 0..n {
            *(*e).elems.add(k) = Value::EMPTY;
        }
        (*e).elems_len = n as u32;
    }
    put(e, LENGTH, Value::number(len as f64));
    set_int(e, COUNT, 0);
    set_int(e, USED, 0);
    set_int(e, MUTATIONS, 0);
    set_int(e, MASK, buckets - 1);
    e
}

fn hold(o: *mut Object, e: *mut Object) {
    let v = crate::object::object_value(e);
    unsafe {
        (*o).code = v.to_bits() as *const Code;
        crate::heap::barrier(o.cast(), &raw const (*o).code, v);
    }
}

/// Move far array `o`'s entries to a store of `buckets` buckets.
fn rehash(vm: &mut Vm, o: *mut Object, buckets: usize) {
    let old = store(o);
    let len = length(o);
    let mutations = int(old, MUTATIONS);
    let all = entries(o);
    let e = new_store(vm, buckets.max(MIN_BUCKETS), len);
    set_int(e, MUTATIONS, mutations);
    let mask = int(e, MASK);
    for &(i, v) in &all {
        let mut b = hash(i) & mask;
        while slot(e, BASE + 2 * b) != Value::EMPTY {
            b = (b + 1) & mask;
        }
        put(e, BASE + 2 * b, key_value(i));
        put(e, BASE + 2 * b + 1, v);
    }
    set_int(e, COUNT, all.len());
    set_int(e, USED, all.len());
    hold(o, e);
}

/// Make array `o` far with length `len` (at least its elements' count):
/// its elements up to the first hole stay its dense prefix, the ones after
/// move to the store (and the elements shrink, if that frees half).
pub(crate) fn make_far(vm: &mut Vm, o: *mut Object, len: u32) {
    debug_assert!(unsafe { (*o).class == Class::Array } && !is_far(o));
    let n = unsafe { (*o).elems_len } as usize;
    let first_hole = (0..n).find(|&k| unsafe { *(*o).elems.add(k) } == Value::EMPTY).unwrap_or(n);
    let rest: Vec<(u32, Value)> = (first_hole..n)
        .filter_map(|k| {
            let v = unsafe { *(*o).elems.add(k) };
            (v != Value::EMPTY).then_some((k as u32, v))
        })
        .collect();
    let buckets = (rest.len() * 2).next_power_of_two().max(MIN_BUCKETS);
    let e = new_store(vm, buckets, len.max(n as u32));
    hold(o, e);
    vm.far_elements(o);
    unsafe {
        (*o).flags |= FAR;
        for k in first_hole..n {
            *(*o).elems.add(k) = Value::EMPTY;
        }
        (*o).elems_len = first_hole as u32;
        if first_hole * 2 < (*o).elems_cap as usize {
            vm.shrink_elems(o);
        }
    }
    for (k, v) in rest {
        insert(vm, o, k, v);
    }
}

/// Far array `o` given element `i` (past its dense prefix: its first
/// hole at the end or beyond) as a plain data element; the length grows
/// to hold it. An element next to the prefix extends it, taking the far
/// keys after it along.
pub(crate) fn set(vm: &mut Vm, o: *mut Object, i: u32, v: Value) {
    unsafe {
        let n = (*o).elems_len;
        if i == n && (i as usize) < MAX_ELEMENTS {
            remove(o, i);
            vm.reserve_elems(o, i as usize + 1);
            vm.store_element(o, i as usize, v);
            (*o).elems_len = i + 1;
            pull(vm, o);
        } else {
            insert(vm, o, i, v);
        }
        if i >= length(o) {
            set_length(o, i + 1);
        }
    }
    maybe_dense(vm, o);
}

/// Move the far keys that follow far array `o`'s dense prefix into it.
fn pull(vm: &mut Vm, o: *mut Object) {
    unsafe {
        if count(o) == 0 {
            return;
        }
        loop {
            let n = (*o).elems_len;
            if n as usize >= MAX_ELEMENTS {
                return;
            }
            let Some(v) = get(o, n) else { return };
            remove(o, n);
            vm.reserve_elems(o, n as usize + 1);
            vm.store_element(o, n as usize, v);
            (*o).elems_len = n + 1;
        }
    }
}

/// Far array `o` cut to length `n` (its elements at `n` and past it gone:
/// the caller checked they may be).
pub(crate) fn truncate(o: *mut Object, n: u32) {
    unsafe {
        let len = (*o).elems_len;
        if n < len {
            for k in n..len {
                *(*o).elems.add(k as usize) = Value::EMPTY;
            }
            (*o).elems_len = n;
        }
    }
    for k in keys(o) {
        if k >= n {
            remove(o, k);
        }
    }
    set_length(o, n);
}

/// Far array `o` made dense again if [`Density`] says so: its far keys
/// moved to its elements, and its shape one whose elements take stores'
/// fast paths again.
pub(crate) fn maybe_dense(vm: &mut Vm, o: *mut Object) {
    let len = length(o) as usize;
    let present = unsafe { (*o).elems_len } as usize + count(o);
    if !vm.density.goes_dense(present, len) {
        return;
    }
    // (One that isn't extensible has the slow shape it would have anyway.)
    let flags = unsafe { (*o).flags };
    let all = entries(o);
    unsafe {
        let n = (*o).elems_len as usize;
        vm.reserve_elems(o, len);
        for k in n..len {
            *(*o).elems.add(k) = Value::EMPTY;
        }
        (*o).elems_len = len as u32;
        for (i, v) in all {
            vm.store_element(o, i as usize, v);
        }
        (*o).code = std::ptr::null();
        (*o).flags &= !FAR;
    }
    if flags & (NOT_EXTENSIBLE | SEALED | FROZEN | LENGTH_READ_ONLY) == 0 {
        vm.dense_elements(o);
    }
}
