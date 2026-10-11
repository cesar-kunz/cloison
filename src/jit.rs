//! Copy-and-patch baseline JIT (see `docs/future-work.md`, milestones 1–3).
//!
//! Each reachable bytecode instruction becomes a copy of a stencil
//! (`stencils/stencils.rs`) with its holes patched. Stencils are chosen per
//! instruction from inferred types and register homes:
//!
//! 1. [`infer`]: forward dataflow from the entry instruction over the
//!    lattice `I32 < F64 < Any`, for `acc` and each register, seeded with the
//!    exact types of the live values at entry.
//! 2. [`assign_homes`] (milestone 3): each VM register lives, for the whole
//!    region, in an FP slot as an unboxed double (if it's only ever int32 or
//!    double, and at least once double; it's then *pinned* as double and
//!    inference runs again), in a GPR slot as an unboxed int32 (if it's only
//!    ever int32), in a GPR slot boxed, or in memory. Registers not live at
//!    entry start as `Bot`, and may be pinned only if definitely assigned
//!    before any spill ([`pinnable`]). Slots go
//!    to the most used registers: [`GPR_SLOTS`] is 6 on x86-64 and 12 on
//!    arm64, [`FP_SLOTS`] 7 on both.
//! 3. [`select`] and [`stencil_for`]: a specialized stencil without type
//!    checks when operand types are known (`add_ii_g0`, `add_ff_f1`,
//!    `jlt_smi_i`, …), the generic one otherwise, for the operand's home.
//!    int32 arithmetic side-exits on overflow (see [`JitFunction`]). A double
//!    accumulator lives unboxed in `facc`.
//! 4. Where a join widens a type, the edge gets conversions: int32 → double,
//!    or `facc` → boxed `acc`. They go inline on fall-through edges and in a
//!    cold stub on jumps.
//! 5. [`Fusion`]: common sequences on slot registers (`ldar r; add_smi k;
//!    star r`, the loop tail `ldar i; inc; star i; jlt_smi n`, …) become a
//!    single fused stencil that bypasses `acc`, guided by accumulator
//!    liveness ([`acc_live_out`]).
//!
//! Layout of the generated code:
//!
//! ```text
//! [ hot: entry (load cached registers, convert); per instruction:        ]
//! [      fall-through conversions, then (at its label) e.g. spills       ]
//! [      before `return`, then the instruction's stencil                 ]
//! [ cold: edge stubs, exit stubs (spill, then exit), `__slow` helpers    ]
//! [ runtime-call wrappers: one per `toi_rt_*` referenced, saving the     ]
//! [      slot registers this region uses (for `toi_rt_enter`, the call   ]
//! [      into compiled code itself, inline)                              ]
//! ```
//!
//! On arm64 most stencils come from family templates: `build.rs` stores one
//! template per family of slot variants plus the positions of its register
//! fields, and [`Instance::copy_to`] writes the slot registers in while
//! copying (see `compact_arm64` in `build.rs`).
//!
//! A stencil whose last instruction jumps to `toi_continue` falls through
//! into the next piece with that jump trimmed. Calls to runtime functions
//! go to wrappers in JIT memory ([`arch::runtime_wrapper`]), so `rel32` /
//! `CALL26` relocations only ever reach inside JIT memory. The wrappers save
//! only the slot registers the region uses, instead of every stencil
//! saving all of them around a C call. Operand holes are absolute
//! relocations (x86-64: immediates and displacements, some folded into the
//! instruction through inline asm; arm64: `movz`/`movk`), so JIT memory can
//! be anywhere.

use crate::bytecode::Op;
use crate::interp::{self, Frame, Run};
use crate::object::{Atom, FieldType, Object, PAGE, Shape};
use crate::value::{NumberTag, Value};
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

/// The JIT's maps and sets, keyed by small integers and pointers, use
/// FxHash (as rustc does): `std`'s SipHash dominated entering compiled code
/// at a function call.
pub(crate) type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;
pub(crate) type HashSet<K> = std::collections::HashSet<K, BuildHasherDefault<FxHasher>>;

#[derive(Default, Clone, Copy)]
pub(crate) struct FxHasher(u64);

impl FxHasher {
    #[inline]
    fn add(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(b));
        }
    }
    fn write_u8(&mut self, x: u8) {
        self.add(x as u64);
    }
    fn write_u16(&mut self, x: u16) {
        self.add(x as u64);
    }
    fn write_u32(&mut self, x: u32) {
        self.add(x as u64);
    }
    fn write_u64(&mut self, x: u64) {
        self.add(x);
    }
    fn write_usize(&mut self, x: usize) {
        self.add(x as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// The largest slot offset the property stencils take: on arm64 the
/// access's `imm12`, scaled by 8 (`ldst_imm!` in `stencils/stencils.rs`).
const MAX_SLOT_OFF: i32 = if cfg!(target_arch = "aarch64") { 0x7FF8 } else { 0xFFFF };

/// What a stencil relocation refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sym {
    HoleImm,
    /// A 64-bit immediate (`mod_smi_i`'s magic multiplier, a shape).
    HoleImm64,
    /// A second one (an address, a transition's new shape).
    HoleImm64b,
    HoleReg,
    Continue,
    JumpTarget,
    Exit,
    /// The stencil's own `__slow` helper.
    Slow,
    /// A runtime function, `toi_rt_*`: an index into `RT_NAMES`.
    Rt(u8),
}

/// A relocation in a stencil, from `build.rs`: offset of the patched field,
/// ELF relocation type, symbol, addend.
pub(crate) struct R(u16, u16, Sym, i32);

pub(crate) struct Stencil {
    /// The stencil's name, or for an arm64 family template its key, with
    /// `{}` for each slot number (`addr_ii_u{}_u{}`).
    name: &'static str,
    /// Whether `<name>__slow` exists.
    slow: bool,
    /// Its `STENCILS` index + 1 if that is the helper of every instance
    /// (with the instance's registers), else 0.
    slow_at: u16,
    code: &'static [u8],
    relocs: &'static [R],
    /// For an arm64 family template, its register fields.
    template: Option<&'static Template>,
}

/// What makes a stencil a family template (arm64; see `compact_arm64` in
/// `build.rs`).
pub(crate) struct Template {
    /// (instruction offset, bit position, parameter): the JIT writes the
    /// parameter's register into the 5-bit field.
    fields: &'static [(u16, u8, u8)],
    /// The slot numbers of the family members this template covers (a
    /// family may need several templates).
    covers: &'static [[u8; 2]],
}

include!(concat!(env!("OUT_DIR"), "/stencils.rs"));

/// The concrete stencil named `name`, in `BY_NAME` (a hash table
/// `build.rs` emits, so there is none to build at startup).
fn concrete(name: &str) -> Option<&'static Stencil> {
    let mask = BY_NAME.len() - 1;
    let mut h = name_hash(name.as_bytes()) as usize & mask;
    loop {
        let i = BY_NAME[h] as usize;
        if i == 0 {
            return None;
        }
        let s = &STENCILS[i - 1];
        if s.name == name {
            return Some(s);
        }
        h = (h + 1) & mask;
    }
}

/// FNV-1a, 64-bit: `fnv1a` in `build.rs`, which hashes `BY_NAME`'s keys.
fn name_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// Family templates (arm64) by (family key, slot numbers), built at the
/// first lookup of a stencil that isn't concrete.
fn families() -> &'static HashMap<(&'static str, [u8; 2]), &'static Stencil> {
    static FAMILIES: std::sync::OnceLock<HashMap<(&'static str, [u8; 2]), &'static Stencil>> =
        std::sync::OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut m = HashMap::default();
        for s in STENCILS {
            if let Some(t) = s.template {
                for &slots in t.covers {
                    m.insert((s.name, slots), s);
                }
            }
        }
        m
    })
}

/// Split a stencil name into its family key and slot parameters; see
/// `slot_params` in `build.rs`, which this must match.
fn slot_params(name: &str) -> (String, Vec<(char, u8)>) {
    let mut key = String::with_capacity(name.len());
    let mut params = Vec::new();
    for (i, tok) in name.split('_').enumerate() {
        if i > 0 {
            key.push('_');
        }
        let mut cs = tok.chars();
        match (cs.next(), cs.as_str().parse::<u8>()) {
            (Some(c @ ('g' | 'u' | 'f')), Ok(k)) if i > 0 && !cs.as_str().is_empty() => {
                key.push(c);
                key.push_str("{}");
                params.push((c, k));
            }
            _ => key.push_str(tok),
        }
    }
    (key, params)
}

/// A stencil to copy, with the registers to patch into a family template's
/// fields.
#[derive(Clone, Copy)]
struct Instance {
    /// The stencil's index in `STENCILS` (small: pieces hold instances).
    at: u16,
    regs: [u8; 2],
}

impl Instance {
    fn of(stencil: &'static Stencil, regs: [u8; 2]) -> Instance {
        let at = (stencil as *const Stencil as usize - STENCILS.as_ptr() as usize)
            / size_of::<Stencil>();
        Instance {
            at: at as u16,
            regs,
        }
    }

    fn stencil(self) -> &'static Stencil {
        &STENCILS[self.at as usize]
    }

    /// The stencil named `name`: a concrete stencil, or an instance of the
    /// family template it belongs to (arm64).
    fn find(name: &str) -> Option<Instance> {
        if let Some(stencil) = concrete(name) {
            return Some(Instance::of(stencil, [0; 2]));
        }
        let (key, params) = slot_params(name);
        let (mut slots, mut regs) = ([0; 2], [0; 2]);
        for (i, &p) in params.iter().enumerate().take(2) {
            slots[i] = p.1;
            regs[i] = arch::slot_reg(p);
        }
        let &stencil = families().get(&(key.as_str(), slots))?;
        Some(Instance::of(stencil, regs))
    }

    fn get(name: &str) -> Instance {
        Instance::find(name).unwrap_or_else(|| panic!("no stencil `{name}`"))
    }

    /// Copy the stencil's first `len` bytes to `buf` and patch in the
    /// registers of a family template.
    fn copy_to(&self, buf: &mut [u8], len: usize) {
        buf[..len].copy_from_slice(&self.stencil().code[..len]);
        let Some(template) = self.stencil().template else {
            return;
        };
        for &(at, shift, p) in template.fields {
            let at = at as usize;
            let w = u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
            let w = (w & !(31 << shift)) | (self.regs[p as usize] as u32) << shift;
            buf[at..at + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
}

fn has_stencil(name: &str) -> bool {
    Instance::find(name).is_some()
}

// ---- Runtime functions called from stencil slow paths (C ABI) ----

/// Stencils call it as `(acc, rhs)`. `Add r` computes `r + acc`, so the
/// operands go to the generic runtime in that order. `AddSmi` (`acc + imm`)
/// shares it, which is fine while `+` is only numeric; strings will need a
/// helper per order.
extern "C" fn toi_rt_add(acc: Value, r: Value) -> Value {
    match (r.as_number(), acc.as_number()) {
        (Some(x), Some(y)) => Value::from_f64_arith(x + y),
        _ => {
            // (A string's concatenation allocates.)
            let vm = crate::interp::current_vm();
            if !vm.is_null() {
                unsafe { (*vm).safepoint(std::ptr::null_mut()) };
            }
            crate::interp::generic_add(r, acc)
        }
    }
}

extern "C" fn toi_rt_inc(a: Value) -> Value {
    match a.as_number() {
        Some(x) => Value::from_f64_arith(x + 1.0),
        None => crate::interp::generic_inc(a),
    }
}

extern "C" fn toi_rt_less_than(a: Value, b: Value) -> bool {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => x < y,
        _ => crate::interp::generic_less_than(a, b),
    }
}

/// `for … in`'s keys of `v` (`for_in_keys`; `vm` is the VM's address).
extern "C" fn toi_rt_for_in_keys(vm: Value, v: Value) -> u64 {
    unsafe {
        let vm = &mut *(vm.to_bits() as *mut crate::vm::Vm);
        vm.safepoint(std::ptr::null_mut());
        vm.for_in_keys(v).to_bits()
    }
}

/// A new object of shape `shape` (`create_object`, when the heap's chunk
/// is full).
extern "C" fn toi_rt_create_object(shape: Value, _: Value) -> u64 {
    unsafe {
        let vm = crate::interp::current_vm();
        (*vm).safepoint(std::ptr::null_mut());
        let o = (*vm).heap.alloc_object(&*(shape.to_bits() as *const Shape));
        crate::object::object_value(o).to_bits()
    }
}

/// A new, empty array (`create_array`; `vm` is the VM's address).
extern "C" fn toi_rt_create_array(vm: Value, _: Value) -> u64 {
    unsafe {
        let vm = &mut *(vm.to_bits() as *mut crate::vm::Vm);
        vm.safepoint(std::ptr::null_mut());
        vm.new_array().to_bits()
    }
}

/// How `a` compares with `b`: 0 less, 1 equal, 2 greater, 3 unordered.
extern "C" fn toi_rt_compare(a: Value, b: Value) -> u64 {
    let (a, b) = (
        crate::interp::to_primitive(a),
        crate::interp::to_primitive(b),
    );
    if let (Some(p), Some(q)) = (crate::vm::string_units(a), crate::vm::string_units(b)) {
        return match p.cmp(q) {
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Equal => 1,
            std::cmp::Ordering::Greater => 2,
        };
    }
    let (x, y) = (crate::interp::to_number(a), crate::interp::to_number(b));
    match x.partial_cmp(&y) {
        Some(std::cmp::Ordering::Less) => 0,
        Some(std::cmp::Ordering::Equal) => 1,
        Some(std::cmp::Ordering::Greater) => 2,
        None => 3,
    }
}

/// Stencils call these as `(acc, r)` for `r - acc` (and `DivSmi`'s as
/// `(k, acc)` for `acc / k`).
extern "C" fn toi_rt_sub(acc: Value, r: Value) -> Value {
    crate::interp::generic_arith(Op::Sub, r, acc)
}

extern "C" fn toi_rt_mul(acc: Value, r: Value) -> Value {
    crate::interp::generic_arith(Op::Mul, r, acc)
}

extern "C" fn toi_rt_div(acc: Value, r: Value) -> Value {
    crate::interp::generic_arith(Op::Div, r, acc)
}

extern "C" fn toi_rt_strict_eq(a: Value, b: Value) -> bool {
    crate::interp::strict_equals(a, b)
}

/// `a % b` (`ModSmi`'s slow path; `b` is the immediate).
extern "C" fn toi_rt_mod(a: Value, b: Value) -> Value {
    Value::number(crate::interp::to_number(a) % crate::interp::to_number(b))
}

/// ToInt32(ToNumber(a)), in the low 32 bits.
extern "C" fn toi_rt_to_int32(a: Value, _: Value) -> u64 {
    crate::interp::to_int32(crate::interp::to_number(a)) as u32 as u64
}

/// `a == b`.
extern "C" fn toi_rt_loose_eq(a: Value, b: Value) -> bool {
    crate::interp::loose_equals(a, b).unwrap_or_else(|e| panic!("JIT code: {e}"))
}

/// `a[b]` for any `a` (a string, a typed array, …: what `get_keyed`
/// stencils don't read themselves), or 0 where it would throw or call a
/// function (the interpreter does).
extern "C" fn toi_rt_get_keyed(a: Value, b: Value) -> u64 {
    let vm = unsafe { &mut *crate::interp::current_vm() };
    vm.get_keyed(a, b).map_or(0, |v| v.to_bits())
}

/// `args[0][args[1]] = args[2]` (what `set_keyed` stencils don't store
/// themselves: a typed array's element, one past an array's length, …);
/// 0 where it would throw or make a named property (the interpreter
/// does it). It may change the elements' type, so the receiver's shape:
/// `set_keyed_rt` forgets exact shapes.
extern "C" fn toi_rt_set_keyed(args: Value, _: Value) -> u64 {
    use crate::object::Class;
    let vm = unsafe { &mut *crate::interp::current_vm() };
    let a = unsafe { std::slice::from_raw_parts(args.to_bits() as *const Value, 3) };
    let (o, key, v) = (a[0], a[1], a[2]);
    if !o.is_cell() || !key.is_int32() || key.as_i32_unchecked() < 0 {
        return 0;
    }
    unsafe {
        let p = crate::object::as_object(o);
        match (*p).class {
            Class::TypedArray => {}
            Class::Plain | Class::Array => {
                if key.as_i32_unchecked() as usize > (*p).elems_len as usize * 2 + 1024 {
                    return 0;
                }
            }
            _ => return 0,
        }
    }
    u64::from(vm.set_keyed(o, key, v).is_ok())
}

/// `a instanceof b`, or 0 where it would throw (the interpreter does).
extern "C" fn toi_rt_instance_of(a: Value, b: Value) -> u64 {
    let vm = unsafe { &mut *crate::interp::current_vm() };
    vm.instance_of(a, b).map_or(0, |r| Value::from_bool(r).to_bits())
}

/// `a in b`, or 0 where it would throw (the interpreter does).
extern "C" fn toi_rt_in(a: Value, b: Value) -> u64 {
    let vm = unsafe { &mut *crate::interp::current_vm() };
    vm.has_property(b, a).map_or(0, |r| Value::from_bool(r).to_bits())
}

extern "C" fn toi_rt_to_boolean(a: Value, _: Value) -> bool {
    crate::interp::to_boolean(a)
}

fn runtime_function(name: &str) -> usize {
    match name {
        "toi_rt_mod" => toi_rt_mod as *const () as usize,
        "toi_rt_instance_of" => toi_rt_instance_of as *const () as usize,
        "toi_rt_get_keyed" => toi_rt_get_keyed as *const () as usize,
        "toi_rt_set_keyed" => toi_rt_set_keyed as *const () as usize,
        "toi_rt_in" => toi_rt_in as *const () as usize,
        "toi_rt_call" => toi_rt_call as *const () as usize,
        "toi_rt_enter" => toi_rt_enter as *const () as usize,
        "toi_rt_construct" => toi_rt_construct as *const () as usize,
        "toi_rt_construct_done" => toi_rt_construct_done as *const () as usize,
        "toi_rt_array_push" => toi_rt_array_push as *const () as usize,
        "toi_rt_materialize" => toi_rt_materialize as *const () as usize,
        "toi_rt_to_int32" => toi_rt_to_int32 as *const () as usize,
        "toi_rt_loose_eq" => toi_rt_loose_eq as *const () as usize,
        "toi_rt_to_boolean" => toi_rt_to_boolean as *const () as usize,
        "toi_rt_add" => toi_rt_add as *const () as usize,
        "toi_rt_inc" => toi_rt_inc as *const () as usize,
        "toi_rt_less_than" => toi_rt_less_than as *const () as usize,
        "toi_rt_compare" => toi_rt_compare as *const () as usize,
        "toi_rt_create_array" => toi_rt_create_array as *const () as usize,
        "toi_rt_create_object" => toi_rt_create_object as *const () as usize,
        "toi_rt_for_in_keys" => toi_rt_for_in_keys as *const () as usize,
        "toi_rt_sub" => toi_rt_sub as *const () as usize,
        "toi_rt_mul" => toi_rt_mul as *const () as usize,
        "toi_rt_div" => toi_rt_div as *const () as usize,
        "toi_rt_strict_eq" => toi_rt_strict_eq as *const () as usize,
        _ => panic!("unknown runtime function `{name}`"),
    }
}

// ---- Architecture specifics ----

#[cfg(target_arch = "x86_64")]
mod arch {
    /// Length of the trailing `jmp rel32` dropped on fall-through.
    pub const TRAILING_JUMP: usize = 5;
    pub const CODE_ALIGN: usize = 16;

    /// Bytes of a counting prefix (see `count_prefix` in the parent).
    pub const COUNT_PREFIX: usize = 13;

    /// `movabs rax, counter; inc qword ptr [rax]`: rax and the flags carry
    /// nothing between stencils (r11 may: a keyed store's key, `key_*`).
    pub fn count_prefix(buf: &mut [u8], counter: *mut u64) {
        buf[..2].copy_from_slice(&[0x48, 0xB8]);
        buf[2..10].copy_from_slice(&(counter as u64).to_le_bytes());
        buf[10..13].copy_from_slice(&[0x48, 0xFF, 0x00]);
    }

    /// Whether the stencil ends with `jmp toi_continue`.
    pub fn ends_with_continue(code: &[u8], relocs: &[super::R]) -> bool {
        let n = code.len();
        n >= 5
            && code[n - 5] == 0xE9
            && relocs
                .iter()
                .any(|r| r.2 == super::Sym::Continue && r.0 as usize == n - 4)
    }

    /// x86-64 stencils have no family templates (register fields aren't
    /// patchable in place: encodings vary in length).
    pub fn slot_reg(_: (char, u8)) -> u8 {
        unreachable!("no register-patched stencils on x86-64")
    }

    /// The register number of GPR slot `k` (rdi rsi rdx rcx r8 r9), if the
    /// C ABI doesn't preserve it, so a runtime-call wrapper must. On x86-64
    /// all GPR slots are caller-saved.
    pub fn caller_saved_gpr_slot(k: u8) -> Option<u8> {
        Some(slot_gpr(k))
    }

    /// The register number of GPR slot `k`: rdi rsi rdx rcx r8 r9.
    pub fn slot_gpr(k: u8) -> u8 {
        [7, 6, 2, 1, 8, 9][k as usize]
    }

    /// Link a jump of [`direct_call`]'s code, once placed: the rel32 field
    /// ending at `at` jumps to `to`.
    pub fn link_raw(buf: &mut [u8], at: usize, to: usize) {
        let d = (to as i64 - at as i64) as i32;
        buf[at - 4..at].copy_from_slice(&d.to_le_bytes());
    }

    /// A runtime-call wrapper (see `rt_call!` in `stencils/stencils.rs`):
    /// called with the arguments in rax and r11, it saves GPRs `gprs` and
    /// xmm registers `fprs`, calls the C function `target`, restores them
    /// and returns the result in rax. Position-independent.
    ///
    /// Entered with rsp ≡ 8 (mod 16), as from any call at an aligned site.
    pub fn runtime_wrapper(target: u64, gprs: &[u8], fprs: &[u8]) -> Vec<u8> {
        let (mut c, restore) = saves(gprs, fprs);
        c.extend([0x48, 0x89, 0xC7]); // mov rdi, rax
        c.extend([0x4C, 0x89, 0xDE]); // mov rsi, r11
        call_abs(&mut c, target);
        c.extend(restore);
        c
    }

    /// The wrapper for `toi_rt_enter` (see [`super::toi_rt_enter`]), which
    /// does its work inline, for compiled code calling compiled code: with
    /// `regs` in rax and the site in r11, and the slot registers `gprs` and
    /// `fprs` saved as for any runtime call, it checks the native stack
    /// (else calls `toi_rt_call`), sets up the callee's frame register,
    /// flags and `acc`, and calls its entry. The callee shares the caller's
    /// `ctx` (the exit cell, `NO_EXIT` while code runs) and tag. The
    /// callee's code uses every register; the caller relies on r13, rbx
    /// and rbp (which LLVM may keep values in across the call), so those
    /// are saved; `acc` (r12) is declared clobbered by `call_direct`. If the
    /// callee exited, its record is in the cell: `toi_direct_exit` finishes
    /// the call, and the cell is reset.
    pub fn enter_wrapper(gprs: &[u8], fprs: &[u8], k: &super::EnterConsts) -> Vec<u8> {
        let disp8 = |d: usize| u8::try_from(d).ok().filter(|&d| d < 0x80).unwrap();
        let (mut c, restore) = saves(gprs, fprs);
        c.extend([0x4D, 0x8B, 0x53, disp8(k.limit)]); // mov r10, [r11 + limit]
        c.extend([0x49, 0x3B, 0x22]); // cmp rsp, [r10]
        c.extend([0x0F, 0x82, 0, 0, 0, 0]); // jb slow
        let to_slow = c.len();
        c.extend([0x53, 0x55, 0x41, 0x53, 0x41, 0x55]); // push rbx, rbp, r11, r13
        c.extend([0x4D, 0x8B, 0x53, disp8(k.frame)]); // mov r10, [r11 + frame]
        c.extend([0x4E, 0x8D, 0x2C, 0xD0]); // lea r13, [rax + r10 * 8]
        // mov qword ptr [r13 - flags], 0
        c.extend([
            0x49,
            0xC7,
            0x45,
            (disp8(k.flags) as i8).wrapping_neg() as u8,
            0,
            0,
            0,
            0,
        ]);
        c.extend([0x41, 0xBC]); // mov r12d, undefined
        c.extend(u32::try_from(k.undefined).unwrap().to_le_bytes());
        c.extend([0x41, 0xFF, 0x13]); // call [r11]
        c.extend([0x41, 0x5D, 0x41, 0x5B, 0x5D, 0x5B]); // pop r13, r11, rbp, rbx
        c.extend([0x41, 0x83, 0x3F, 0xFF]); // cmp dword ptr [r15], NO_EXIT
        c.extend([0x0F, 0x85, 0, 0, 0, 0]); // jne exit
        let to_exit = c.len();
        let done = c.len();
        c.extend(restore);
        let rel = |c: &mut Vec<u8>, from: usize, to: usize| {
            let d = (to as i32 - from as i32).to_le_bytes();
            c[from - 4..from].copy_from_slice(&d);
        };
        let jmp_done = |c: &mut Vec<u8>| {
            c.extend([0xE9, 0, 0, 0, 0]);
            let at = c.len();
            rel(c, at, done);
        };
        // exit: toi_direct_exit(regs, site, record, acc).
        let at = c.len();
        rel(&mut c, to_exit, at);
        c.extend([0x4C, 0x89, 0xEF]); // mov rdi, r13
        c.extend([0x4C, 0x89, 0xDE]); // mov rsi, r11
        c.extend([0x41, 0x8B, 0x17]); // mov edx, [r15]
        c.extend([0x41, 0xC7, 0x07, 0xFF, 0xFF, 0xFF, 0xFF]); // mov dword ptr [r15], NO_EXIT
        c.extend([0x48, 0x89, 0xC1]); // mov rcx, rax
        call_abs(&mut c, k.exit);
        jmp_done(&mut c);
        // slow: toi_rt_call(regs, site).
        let at = c.len();
        rel(&mut c, to_slow, at);
        c.extend([0x48, 0x89, 0xC7]); // mov rdi, rax
        c.extend([0x4C, 0x89, 0xDE]); // mov rsi, r11
        call_abs(&mut c, k.call);
        jmp_done(&mut c);
        c
    }

    /// A direct call without a wrapper (see `call_done` in the stencils):
    /// a piece of its own between stencils, where only the stencil state
    /// registers carry values, so it saves just the slot registers live
    /// after the call (GPRs `gprs`, xmm registers `fprs`) on the native
    /// stack. Then, if call site `site` (a `CallSite`) has an entry, the
    /// callee register (at byte `f_off` of the frame, if `Some`) holds its
    /// closure, and the VM and native stacks have room: enter the callee's
    /// frame (`frame` bytes on) and call the entry. If the callee exited,
    /// `toi_direct_exit` finishes the call; if the checks failed,
    /// `toi_rt_call` makes it: both in the cold part, which jumps back. The
    /// result goes to `acc` (r12).
    ///
    /// Entered as stencils are, with rsp ≡ 8 (mod 16).
    pub fn direct_call(
        gprs: &[u8],
        fprs: &[u8],
        site: u64,
        f_off: Option<u32>,
        frame: u32,
        k: &super::DirectConsts,
    ) -> super::RawCall {
        let disp8 = |d: usize| u8::try_from(d).ok().filter(|&d| d < 0x80).unwrap();
        let mut c = Vec::new();
        for &g in gprs {
            if g >= 8 {
                c.push(0x41);
            }
            c.push(0x50 + (g & 7)); // push g
        }
        // At the call rsp ≡ 0 (mod 16): the saves take 8 (mod 16).
        let mut area = 8 * fprs.len();
        if (8 * gprs.len() + area) % 16 != 8 {
            area += 8;
        }
        if area > 0 {
            c.extend([0x48, 0x83, 0xEC, area as u8]); // sub rsp, area
        }
        for (i, &x) in fprs.iter().enumerate() {
            c.extend([0xF2, 0x0F, 0x11, 0x44 | (x << 3), 0x24, (8 * i) as u8]); // movsd [rsp+8i], x
        }
        c.extend([0x49, 0xBB]); // movabs r11, site
        c.extend(site.to_le_bytes());
        // Jumps to the cold part: (field end, slow?).
        let mut to_cold: Vec<(usize, bool)> = Vec::new();
        let jcc = |c: &mut Vec<u8>, cc: u8, slow: bool, to: &mut Vec<(usize, bool)>| {
            c.extend([0x0F, cc, 0, 0, 0, 0]);
            to.push((c.len(), slow));
        };
        c.extend([0x49, 0x8B, 0x03]); // mov rax, [r11]: the entry
        c.extend([0x48, 0x85, 0xC0]); // test rax, rax
        jcc(&mut c, 0x84, true, &mut to_cold); // jz slow
        if let Some(off) = f_off {
            c.extend([0x4D, 0x8B, 0x95]); // mov r10, [r13 + off]
            c.extend(off.to_le_bytes());
            c.extend([0x4D, 0x3B, 0x53, disp8(k.closure)]); // cmp r10, [r11 + closure]
            jcc(&mut c, 0x85, true, &mut to_cold); // jne slow
        }
        c.extend([0x4D, 0x8B, 0x53, disp8(k.end_off)]); // mov r10, [r11 + end_off]
        c.extend([0x4D, 0x01, 0xEA]); // add r10, r13
        c.extend([0x4D, 0x3B, 0x53, disp8(k.stack_end)]); // cmp r10, [r11 + stack_end]
        jcc(&mut c, 0x87, true, &mut to_cold); // ja slow
        c.extend([0x4D, 0x8B, 0x53, disp8(k.limit)]); // mov r10, [r11 + limit]
        c.extend([0x49, 0x3B, 0x22]); // cmp rsp, [r10]
        jcc(&mut c, 0x82, true, &mut to_cold); // jb slow
        c.extend([0x4D, 0x8D, 0xAD]); // lea r13, [r13 + frame]
        c.extend(frame.to_le_bytes());
        // mov qword ptr [r13 - flags], 0
        c.extend([0x49, 0xC7, 0x45, (disp8(k.flags) as i8).wrapping_neg() as u8, 0, 0, 0, 0]);
        c.extend([0x41, 0xBC]); // mov r12d, undefined
        c.extend(u32::try_from(k.undefined).unwrap().to_le_bytes());
        c.extend([0xFF, 0xD0]); // call rax
        c.extend([0x4D, 0x8D, 0xAD]); // lea r13, [r13 - frame]
        c.extend(frame.wrapping_neg().to_le_bytes());
        c.extend([0x41, 0x83, 0x3F, 0xFF]); // cmp dword ptr [r15], NO_EXIT
        jcc(&mut c, 0x85, false, &mut to_cold); // jne exit
        let done = c.len();
        c.extend([0x49, 0x89, 0xC4]); // mov r12, rax
        for (i, &x) in fprs.iter().enumerate() {
            c.extend([0xF2, 0x0F, 0x10, 0x44 | (x << 3), 0x24, (8 * i) as u8]); // movsd x, [rsp+8i]
        }
        if area > 0 {
            c.extend([0x48, 0x83, 0xC4, area as u8]); // add rsp, area
        }
        for &g in gprs.iter().rev() {
            if g >= 8 {
                c.push(0x41);
            }
            c.push(0x58 + (g & 7)); // pop g
        }
        // The cold part. exit: toi_direct_exit(regs, site, record, acc).
        let mut d = Vec::new();
        let mut to_done = Vec::new();
        let exit = d.len();
        d.extend([0x4C, 0x89, 0xEF]); // mov rdi, r13
        d.extend([0x48, 0xBE]); // movabs rsi, site
        d.extend(site.to_le_bytes());
        d.extend([0x41, 0x8B, 0x17]); // mov edx, [r15]
        d.extend([0x41, 0xC7, 0x07, 0xFF, 0xFF, 0xFF, 0xFF]); // mov dword ptr [r15], NO_EXIT
        d.extend([0x48, 0x89, 0xC1]); // mov rcx, rax
        call_abs(&mut d, k.exit);
        d.extend([0xE9, 0, 0, 0, 0]); // jmp done
        to_done.push(d.len());
        // slow: toi_rt_call(regs, site).
        let slow = d.len();
        d.extend([0x4C, 0x89, 0xEF]); // mov rdi, r13
        d.extend([0x4C, 0x89, 0xDE]); // mov rsi, r11
        call_abs(&mut d, k.call);
        d.extend([0xE9, 0, 0, 0, 0]); // jmp done
        to_done.push(d.len());
        super::RawCall {
            hot: c,
            cold: d,
            hot_to_cold: to_cold.into_iter().map(|(f, s)| (f, if s { slow } else { exit })).collect(),
            cold_to_hot: to_done.into_iter().map(|f| (f, done)).collect(),
        }
    }

    /// `movabs rax, target; call rax`.
    fn call_abs(c: &mut Vec<u8>, target: u64) {
        c.extend([0x48, 0xB8]);
        c.extend(target.to_le_bytes());
        c.extend([0xFF, 0xD0]);
    }

    /// A wrapper's saves of GPRs `gprs` and xmm registers `fprs`, leaving
    /// rsp aligned for a call, and the restores and `ret` after it.
    fn saves(gprs: &[u8], fprs: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut c = Vec::new();
        let mut r = Vec::new();
        for &g in gprs {
            if g >= 8 {
                c.push(0x41);
            }
            c.push(0x50 + (g & 7)); // push g
        }
        // Spill area for xmm registers, padded so the call site is aligned.
        let mut frame = 8 * fprs.len();
        if !(8 + 8 * gprs.len() + frame).is_multiple_of(16) {
            frame += 8;
        }
        if frame > 0 {
            c.extend([0x48, 0x83, 0xEC, frame as u8]); // sub rsp, frame
        }
        let movsd = |c: &mut Vec<u8>, op: u8, x: u8, i: usize| {
            // movsd [rsp + 8i], xmm / movsd xmm, [rsp + 8i]
            c.extend([0xF2, 0x0F, op, 0x44 | (x << 3), 0x24, (8 * i) as u8]);
        };
        for (i, &x) in fprs.iter().enumerate() {
            movsd(&mut c, 0x11, x, i);
            movsd(&mut r, 0x10, x, i);
        }
        if frame > 0 {
            r.extend([0x48, 0x83, 0xC4, frame as u8]); // add rsp, frame
        }
        for &g in gprs.iter().rev() {
            if g >= 8 {
                r.push(0x41);
            }
            r.push(0x58 + (g & 7)); // pop g
        }
        r.push(0xC3); // ret
        (c, r)
    }

    /// Apply relocation `r_type` at `buf[0..]` (the field), located at
    /// address `p`, for symbol value `s` and addend `a`.
    pub fn patch(buf: &mut [u8], r_type: u32, s: u64, a: i64, p: u64) {
        let v = s.wrapping_add(a as u64);
        match r_type {
            // R_X86_64_64
            1 => buf[..8].copy_from_slice(&v.to_le_bytes()),
            // R_X86_64_PC32, R_X86_64_PLT32
            2 | 4 => {
                let d = v.wrapping_sub(p) as i64;
                let d = i32::try_from(d).expect("rel32 out of range");
                buf[..4].copy_from_slice(&d.to_le_bytes());
            }
            // R_X86_64_32, R_X86_64_32S: operand holes. Stencils only use the
            // low 32 bits, so the same bits work for both extensions.
            10 | 11 => {
                let v = u32::try_from(v).expect("32-bit hole out of range");
                buf[..4].copy_from_slice(&v.to_le_bytes());
            }
            _ => panic!("unsupported x86-64 relocation type {r_type}"),
        }
    }

    pub fn flush_icache(_mem: *const u8, _len: usize) {
        // x86 instruction caches are coherent with data writes.
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    const R_AARCH64_ABS64: u32 = 257;
    const R_AARCH64_MOVW_UABS_G0_NC: u32 = 264;
    const R_AARCH64_MOVW_UABS_G1_NC: u32 = 266;
    const R_AARCH64_MOVW_UABS_G2_NC: u32 = 268;
    const R_AARCH64_MOVW_UABS_G3: u32 = 269;
    const R_AARCH64_ADR_PREL_PG_HI21: u32 = 275;
    const R_AARCH64_ADD_ABS_LO12_NC: u32 = 277;
    const R_AARCH64_LDST8_ABS_LO12_NC: u32 = 278;
    const R_AARCH64_TSTBR14: u32 = 279;
    const R_AARCH64_CONDBR19: u32 = 280;
    const R_AARCH64_JUMP26: u32 = 282;
    const R_AARCH64_CALL26: u32 = 283;
    const R_AARCH64_LDST16_ABS_LO12_NC: u32 = 284;
    const R_AARCH64_LDST32_ABS_LO12_NC: u32 = 285;
    const R_AARCH64_LDST64_ABS_LO12_NC: u32 = 286;
    const R_AARCH64_LDST128_ABS_LO12_NC: u32 = 299;

    pub const TRAILING_JUMP: usize = 4;
    pub const CODE_ALIGN: usize = 16;

    /// No counting prefix on arm64 (see `count_prefix` in the parent).
    pub const COUNT_PREFIX: usize = 0;

    pub fn count_prefix(_: &mut [u8], _: *mut u64) {}

    pub fn ends_with_continue(code: &[u8], relocs: &[super::R]) -> bool {
        let n = code.len();
        n >= 4
            && relocs.iter().any(|r| {
                r.2 == super::Sym::Continue
                    && r.0 as usize == n - 4
                    && r.1 as u32 == R_AARCH64_JUMP26
            })
    }

    /// The register of a slot parameter, as `slot_params` names them: GPR
    /// slots 0–4 are x24–x28, 5–11 are x0–x6; FP slot k is d(k+1). Must
    /// match `arm64_slot_reg` in `build.rs`.
    pub fn slot_reg((class, k): (char, u8)) -> u8 {
        match class {
            'f' => k + 1,
            _ if k < 5 => 24 + k,
            _ => k - 5,
        }
    }

    /// The register number of GPR slot `k`, if the C ABI doesn't preserve
    /// it, so a runtime-call wrapper must. Slots 0–4 are x24–x28,
    /// callee-saved; slots 5–11 are x0–x6.
    pub fn caller_saved_gpr_slot(k: u8) -> Option<u8> {
        (k >= 5).then(|| k - 5)
    }

    /// The register number of GPR slot `k`: x24–x28, then x0–x6.
    pub fn slot_gpr(k: u8) -> u8 {
        slot_reg(('g', k))
    }

    /// Link a branch of [`direct_call`]'s code, once placed: the `b`,
    /// `b.cond` or `cbz` at `at` branches to `to`.
    pub fn link_raw(buf: &mut [u8], at: usize, to: usize) {
        let insn = read(&buf[at..]);
        let d = (to as i64 - at as i64) / 4;
        let patched = if insn & 0xFC00_0000 == 0x1400_0000 {
            assert!((-(1 << 25)..1 << 25).contains(&d), "branch out of range");
            insn | (d as u32 & 0x03FF_FFFF)
        } else {
            assert!((-(1 << 18)..1 << 18).contains(&d), "branch out of range");
            insn | (d as u32 & 0x7_FFFF) << 5
        };
        write(&mut buf[at..], patched);
    }

    /// `movz`/`movk` loading the 64-bit `v` into `x{rd}`.
    fn mov64(c: &mut Vec<u32>, rd: u32, v: u64) {
        c.push(0xD280_0000 | ((v & 0xFFFF) as u32) << 5 | rd); // movz
        for hw in 1..4 {
            let part = ((v >> (16 * hw)) & 0xFFFF) as u32;
            c.push(0xF280_0000 | hw << 21 | part << 5 | rd); // movk
        }
    }

    /// `ldr x{rt}, [x{rn}, #off]` (unsigned, scaled offset).
    fn ldr(rt: u32, rn: u32, off: usize) -> u32 {
        assert!(off.is_multiple_of(8) && off < 8 * 4096);
        0xF940_0000 | ((off / 8) as u32) << 10 | rn << 5 | rt
    }

    /// A direct call without a wrapper: see the x86-64 version. Between
    /// stencils only the stencil state registers carry values (x20 `acc`,
    /// x21 `regs`, x22 the tag, x23 `ctx`, the slot registers); the
    /// callee's code may use every other register and preserves only x29
    /// and x30 (`preserve_none`), so the piece saves the link register
    /// (which `blr` overwrites) and the slot registers live after the call,
    /// GPRs `gprs` and FP registers `fprs` (d1–d7), on the native stack.
    /// The callee shares the tag and `ctx`, returns its result in x0 and
    /// leaves x21 as it found it. The site is in x9; the rare paths are in
    /// the cold part, which branches back. Branches to link are recorded at
    /// their instruction (see [`link_raw`]).
    pub fn direct_call(
        gprs: &[u8],
        fprs: &[u8],
        site: u64,
        f_off: Option<u32>,
        frame: u32,
        k: &super::DirectConsts,
    ) -> super::RawCall {
        let mut c: Vec<u32> = Vec::new();
        c.push(0xA9BF_7BFD); // stp x29, x30, [sp, #-16]!
        let n = gprs.len() + fprs.len();
        let area = (8 * n).next_multiple_of(16) as u32;
        if area > 0 {
            c.push(0xD100_03FF | area << 10); // sub sp, sp, #area
        }
        // str / ldr x or d register at [sp, #8i]
        let slot = |op: u32, r: u8, i: usize| op | (i as u32) << 10 | (31 << 5) | r as u32;
        let saves: Vec<(u32, u32, u8)> = (gprs.iter().map(|&r| (0xF900_0000, 0xF940_0000, r)))
            .chain(fprs.iter().map(|&r| (0xFD00_0000, 0xFD40_0000, r)))
            .collect();
        for (i, &(st, _, r)) in saves.iter().enumerate() {
            c.push(slot(st, r, i));
        }
        mov64(&mut c, 9, site);
        // Branches to the cold part: (instruction, slow?).
        let mut to_cold: Vec<(usize, bool)> = Vec::new();
        c.push(ldr(10, 9, 0)); // ldr x10, [x9]: the entry
        to_cold.push((c.len(), true));
        c.push(0xB400_000A); // cbz x10, slow
        if let Some(off) = f_off {
            c.push(ldr(16, 21, off as usize)); // ldr x16, [x21, #off]
            c.push(ldr(17, 9, k.closure)); // ldr x17, [x9, #closure]
            c.push(0xEB11_021F); // cmp x16, x17
            to_cold.push((c.len(), true));
            c.push(0x5400_0001); // b.ne slow
        }
        c.push(ldr(16, 9, k.end_off)); // ldr x16, [x9, #end_off]
        c.push(0x8B15_0210); // add x16, x16, x21
        c.push(ldr(17, 9, k.stack_end)); // ldr x17, [x9, #stack_end]
        c.push(0xEB11_021F); // cmp x16, x17
        to_cold.push((c.len(), true));
        c.push(0x5400_0008); // b.hi slow
        c.push(ldr(16, 9, k.limit)); // ldr x16, [x9, #limit]
        c.push(0xF940_0210); // ldr x16, [x16]
        c.push(0x9100_03F1); // mov x17, sp
        c.push(0xEB10_023F); // cmp x17, x16
        to_cold.push((c.len(), true));
        c.push(0x5400_0003); // b.lo slow
        assert!(frame < 4096);
        c.push(0x9100_02B5 | frame << 10); // add x21, x21, #frame
        assert!(k.flags < 256);
        let imm9 = (k.flags as u32).wrapping_neg() & 0x1FF;
        c.push(0xF800_02BF | imm9 << 12); // stur xzr, [x21, #-flags]
        assert!(k.undefined < 1 << 16);
        c.push(0xD280_0014 | (k.undefined as u32) << 5); // mov x20, #undefined
        c.push(0xD63F_0140); // blr x10
        c.push(0xD100_02B5 | frame << 10); // sub x21, x21, #frame
        c.push(0xB940_02F0); // ldr w16, [x23]
        c.push(0x3100_061F); // cmn w16, #1
        to_cold.push((c.len(), false));
        c.push(0x5400_0001); // b.ne exit
        let done = c.len();
        c.push(0xAA00_03F4); // mov x20, x0
        for (i, &(_, ld, r)) in saves.iter().enumerate() {
            c.push(slot(ld, r, i));
        }
        if area > 0 {
            c.push(0x9100_03FF | area << 10); // add sp, sp, #area
        }
        c.push(0xA8C1_7BFD); // ldp x29, x30, [sp], #16
        // The cold part. exit: toi_direct_exit(regs, site, record, acc).
        let mut d: Vec<u32> = Vec::new();
        let mut to_done = Vec::new();
        let exit = d.len();
        d.push(0xAA00_03E3); // mov x3, x0
        d.push(0xAA15_03E0); // mov x0, x21
        mov64(&mut d, 1, site);
        d.push(0xB940_02E2); // ldr w2, [x23]
        d.push(0x1280_0010); // mov w16, #-1
        d.push(0xB900_02F0); // str w16, [x23]
        mov64(&mut d, 16, k.exit);
        d.push(0xD63F_0200); // blr x16
        to_done.push(d.len());
        d.push(0x1400_0000); // b done
        // slow: toi_rt_call(regs, site).
        let slow = d.len();
        d.push(0xAA15_03E0); // mov x0, x21
        d.push(0xAA09_03E1); // mov x1, x9
        mov64(&mut d, 16, k.call);
        d.push(0xD63F_0200); // blr x16
        to_done.push(d.len());
        d.push(0x1400_0000); // b done
        let bytes = |c: &[u32]| c.iter().flat_map(|i| i.to_le_bytes()).collect::<Vec<u8>>();
        super::RawCall {
            hot: bytes(&c),
            cold: bytes(&d),
            hot_to_cold: (to_cold.into_iter())
                .map(|(at, s)| (4 * at, 4 * if s { slow } else { exit }))
                .collect(),
            cold_to_hot: to_done.into_iter().map(|at| (4 * at, 4 * done)).collect(),
        }
    }

    /// A runtime-call wrapper (see `rt_call!` in `stencils/stencils.rs`):
    /// called with `bl` and the arguments in x8 and x9, it saves GPRs `gprs`
    /// and FP registers `fprs` (d0–d7), calls the C function `target`,
    /// restores them and returns the result in x8. Position-independent; the
    /// target address is a literal after the code.
    pub fn runtime_wrapper(target: u64, gprs: &[u8], fprs: &[u8]) -> Vec<u8> {
        let (mut c, restore) = saves(gprs, fprs);
        c.push(0xAA08_03E0); // mov x0, x8
        c.push(0xAA09_03E1); // mov x1, x9
        let call = c.len();
        c.push(0); // ldr x16, literal (patched below)
        c.push(0xD63F_0200); // blr x16
        c.push(0xAA00_03E8); // mov x8, x0
        c.extend(restore);
        with_literals(c, &[(call, target)])
    }

    /// The wrapper for `toi_rt_enter`, doing its work inline: see the
    /// x86-64 version. Besides the slot registers saved as for any runtime
    /// call, it saves the C ABI's callee-saved registers the caller may
    /// rely on and the callee's code may use (`preserve_none` preserves
    /// only x29 and x30): x19, x21 (`regs`), x24–x28 and d8–d15; `acc`
    /// (x20) is declared clobbered by `call_direct`.
    pub fn enter_wrapper(gprs: &[u8], fprs: &[u8], k: &super::EnterConsts) -> Vec<u8> {
        let ldr_x16_x9 = |off: usize| {
            assert!(off.is_multiple_of(8) && off < 8 * 4096);
            0xF940_0130 | ((off / 8) as u32) << 10
        };
        let (mut c, restore) = saves(gprs, fprs);
        c.push(ldr_x16_x9(k.limit)); // ldr x16, [x9, #limit]
        c.push(0xF940_0210); // ldr x16, [x16]
        c.push(0x9100_03F1); // mov x17, sp
        c.push(0xEB10_023F); // cmp x17, x16
        let to_slow = c.len();
        c.push(0x5400_0003); // b.lo slow
        c.push(0xD102_03FF); // sub sp, sp, #128
        let pairs = [
            0xA900_57F3u32, // stp x19, x21, [sp]
            0xA901_67F8,    // stp x24, x25, [sp, #16]
            0xA902_6FFA,    // stp x26, x27, [sp, #32]
            0xA903_27FC,    // stp x28, x9, [sp, #48]
            0x6D04_27E8,    // stp d8, d9, [sp, #64]
            0x6D05_2FEA,    // stp d10, d11, [sp, #80]
            0x6D06_37EC,    // stp d12, d13, [sp, #96]
            0x6D07_3FEE,    // stp d14, d15, [sp, #112]
        ];
        c.extend(pairs);
        c.push(ldr_x16_x9(k.frame)); // ldr x16, [x9, #frame]
        c.push(0x8B10_0D15); // add x21, x8, x16, lsl #3
        assert!(k.flags < 256);
        let imm9 = (k.flags as u32).wrapping_neg() & 0x1FF;
        c.push(0xF800_02BF | imm9 << 12); // stur xzr, [x21, #-flags]
        assert!(k.undefined < 1 << 16);
        c.push(0xD280_0014 | (k.undefined as u32) << 5); // mov x20, #undefined
        c.push(0xF940_0130); // ldr x16, [x9]
        c.push(0xD63F_0200); // blr x16
        c.extend(pairs.map(|p| p | 1 << 22)); // ldp, the same
        c.push(0x9102_03FF); // add sp, sp, #128
        c.push(0xAA00_03E8); // mov x8, x0
        c.push(0xB940_02F0); // ldr w16, [x23]
        c.push(0x3100_061F); // cmn w16, #1
        let to_exit = c.len();
        c.push(0x5400_0001); // b.ne exit
        let done = c.len();
        c.extend(restore);
        let branch = |c: &mut Vec<u32>, from: usize, to: usize, bits: u32, shift: u32| {
            let d = (to as i64 - from as i64) as u32 & ((1 << bits) - 1);
            c[from] |= d << shift;
        };
        // exit: toi_direct_exit(regs, site, record, acc).
        let at = c.len();
        branch(&mut c, to_exit, at, 19, 5);
        c.push(0xAA15_03E0); // mov x0, x21
        c.push(0xAA09_03E1); // mov x1, x9
        c.push(0x2A10_03E2); // mov w2, w16
        c.push(0xAA08_03E3); // mov x3, x8
        c.push(0x1280_0010); // mov w16, #-1
        c.push(0xB900_02F0); // str w16, [x23]
        let exit = c.len();
        c.push(0); // ldr x16, literal
        c.push(0xD63F_0200); // blr x16
        c.push(0xAA00_03E8); // mov x8, x0
        let at = c.len();
        c.push(0x1400_0000); // b done
        branch(&mut c, at, done, 26, 0);
        // slow: toi_rt_call(regs, site).
        let at = c.len();
        branch(&mut c, to_slow, at, 19, 5);
        c.push(0xAA08_03E0); // mov x0, x8
        c.push(0xAA09_03E1); // mov x1, x9
        let call = c.len();
        c.push(0); // ldr x16, literal
        c.push(0xD63F_0200); // blr x16
        c.push(0xAA00_03E8); // mov x8, x0
        let at = c.len();
        c.push(0x1400_0000); // b done
        branch(&mut c, at, done, 26, 0);
        with_literals(c, &[(exit, k.exit), (call, k.call)])
    }

    /// A wrapper's frame and saves of GPRs `gprs` and FP registers `fprs`,
    /// and the restores and `ret` after its call.
    fn saves(gprs: &[u8], fprs: &[u8]) -> (Vec<u32>, Vec<u32>) {
        let mut c: Vec<u32> = Vec::new();
        let mut r: Vec<u32> = Vec::new();
        c.push(0xA9BF_7BFD); // stp x29, x30, [sp, #-16]!
        c.push(0x9100_03FD); // mov x29, sp
        let n = gprs.len() + fprs.len();
        let frame = (8 * n).next_multiple_of(16) as u32;
        if frame > 0 {
            c.push(0xD100_03FF | frame << 10); // sub sp, sp, #frame
        }
        // str / ldr x or d register at [sp, #8i]
        let slot = |op: u32, r: u8, i: usize| op | (i as u32) << 10 | (31 << 5) | r as u32;
        let saves = gprs.iter().map(|&r| (0xF900_0000, 0xF940_0000, r));
        let saves = saves.chain(fprs.iter().map(|&r| (0xFD00_0000, 0xFD40_0000, r)));
        for (i, (st, ld, g)) in saves.enumerate() {
            c.push(slot(st, g, i));
            r.push(slot(ld, g, i));
        }
        if frame > 0 {
            r.push(0x9100_03FF | frame << 10); // add sp, sp, #frame
        }
        r.push(0xA8C1_7BFD); // ldp x29, x30, [sp], #16
        r.push(0xD65F_03C0); // ret
        (c, r)
    }

    /// Code `c` as bytes, followed by 64-bit literals, each loaded into x16
    /// by the `ldr` (literal) placeholder at its index.
    fn with_literals(mut c: Vec<u32>, lits: &[(usize, u64)]) -> Vec<u8> {
        for (k, &(at, _)) in lits.iter().enumerate() {
            let lit = c.len() + 2 * k;
            c[at] = 0x5800_0010 | ((lit - at) as u32) << 5;
        }
        let mut bytes: Vec<u8> = c.iter().flat_map(|i| i.to_le_bytes()).collect();
        for &(_, v) in lits {
            bytes.extend(v.to_le_bytes());
        }
        bytes
    }

    fn read(buf: &[u8]) -> u32 {
        u32::from_le_bytes(buf[..4].try_into().unwrap())
    }

    fn write(buf: &mut [u8], insn: u32) {
        buf[..4].copy_from_slice(&insn.to_le_bytes());
    }

    /// A pc-relative branch displacement in instructions, checked to fit
    /// `bits` signed bits.
    fn branch_disp(v: u64, p: u64, bits: u32) -> u32 {
        let d = v.wrapping_sub(p) as i64;
        assert!(d & 3 == 0, "misaligned branch target");
        let d = d >> 2;
        let limit = 1i64 << (bits - 1);
        assert!((-limit..limit).contains(&d), "branch out of range: {bits}-bit displacement {d} instructions");
        (d as u32) & ((1 << bits) - 1)
    }

    pub fn patch(buf: &mut [u8], r_type: u32, s: u64, a: i64, p: u64) {
        let v = s.wrapping_add(a as u64);
        match r_type {
            R_AARCH64_ABS64 => buf[..8].copy_from_slice(&v.to_le_bytes()),
            // movz / movk: imm16 at [20:5], bits 0–15, 16–31, 32–47 or 48–63
            // of the value.
            R_AARCH64_MOVW_UABS_G0_NC
            | R_AARCH64_MOVW_UABS_G1_NC
            | R_AARCH64_MOVW_UABS_G2_NC
            | R_AARCH64_MOVW_UABS_G3 => {
                let shift = match r_type {
                    R_AARCH64_MOVW_UABS_G0_NC => 0,
                    R_AARCH64_MOVW_UABS_G1_NC => 16,
                    R_AARCH64_MOVW_UABS_G2_NC => 32,
                    _ => 48,
                };
                let imm16 = ((v >> shift) & 0xFFFF) as u32;
                write(buf, (read(buf) & !(0xFFFF << 5)) | imm16 << 5);
            }
            R_AARCH64_JUMP26 | R_AARCH64_CALL26 => {
                write(buf, (read(buf) & !0x03FF_FFFF) | branch_disp(v, p, 26));
            }
            R_AARCH64_CONDBR19 => {
                write(
                    buf,
                    (read(buf) & !(0x7FFFF << 5)) | (branch_disp(v, p, 19) << 5),
                );
            }
            R_AARCH64_TSTBR14 => {
                write(
                    buf,
                    (read(buf) & !(0x3FFF << 5)) | (branch_disp(v, p, 14) << 5),
                );
            }
            R_AARCH64_ADR_PREL_PG_HI21 => {
                let pages = ((v & !0xFFF) as i64 - (p & !0xFFF) as i64) >> 12;
                assert!(
                    (-(1 << 20)..(1 << 20)).contains(&pages),
                    "adrp out of range"
                );
                let (lo, hi) = ((pages as u32) & 3, ((pages as u32) >> 2) & 0x7FFFF);
                write(
                    buf,
                    (read(buf) & !(3 << 29 | 0x7FFFF << 5)) | lo << 29 | hi << 5,
                );
            }
            R_AARCH64_ADD_ABS_LO12_NC => {
                write(
                    buf,
                    (read(buf) & !(0xFFF << 10)) | ((v as u32 & 0xFFF) << 10),
                );
            }
            R_AARCH64_LDST8_ABS_LO12_NC
            | R_AARCH64_LDST16_ABS_LO12_NC
            | R_AARCH64_LDST32_ABS_LO12_NC
            | R_AARCH64_LDST64_ABS_LO12_NC
            | R_AARCH64_LDST128_ABS_LO12_NC => {
                let scale = match r_type {
                    R_AARCH64_LDST8_ABS_LO12_NC => 0,
                    R_AARCH64_LDST16_ABS_LO12_NC => 1,
                    R_AARCH64_LDST32_ABS_LO12_NC => 2,
                    R_AARCH64_LDST64_ABS_LO12_NC => 3,
                    _ => 4,
                };
                // The hole's whole value (an offset, not an address's low 12
                // bits): `imm12`, scaled by the access size.
                assert!(v.is_multiple_of(1 << scale), "misaligned imm12 offset");
                let imm12 = v >> scale;
                assert!(imm12 < 1 << 12, "imm12 offset {v:#x} out of range");
                write(buf, (read(buf) & !(0xFFF << 10)) | (imm12 as u32) << 10);
            }
            _ => panic!("unsupported AArch64 relocation type {r_type}"),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn flush_icache(mem: *const u8, len: usize) {
        // The system's own cache maintenance (libkern/OSCacheControl.h).
        unsafe extern "C" {
            fn sys_icache_invalidate(start: *const core::ffi::c_void, len: usize);
        }
        unsafe { sys_icache_invalidate(mem.cast(), len) }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn flush_icache(mem: *const u8, len: usize) {
        // Clean the data cache to the point of unification, then invalidate
        // the instruction cache, line by line (CTR_EL0 gives line sizes).
        unsafe {
            let ctr: u64;
            core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr);
            let dline = 4usize << ((ctr >> 16) & 0xF);
            let iline = 4usize << (ctr & 0xF);
            let (start, end) = (mem as usize, mem as usize + len);
            let mut a = start & !(dline - 1);
            while a < end {
                core::arch::asm!("dc cvau, {}", in(reg) a);
                a += dline;
            }
            core::arch::asm!("dsb ish");
            let mut a = start & !(iline - 1);
            while a < end {
                core::arch::asm!("ic ivau, {}", in(reg) a);
                a += iline;
            }
            core::arch::asm!("dsb ish", "isb");
        }
    }
}

// ---- Executable memory ----

/// An anonymous RW mapping, made RX by [`ExecMem::seal`].
/// Executable memory: pages of a reserved range, never reused for other
/// code once freed. Code mapped where freed code was ran up to a third
/// slower in some runs (frontend stalls, 13-missing-arguments' recompile
/// landing on its first version's page: 46M or 61M cycles from one run to
/// the next), as if the CPU kept state for the old code.
///
/// Each piece of code has pages of its own, but starts where the one before
/// it ended within a page (`EXEC_RANGE`'s offset), so consecutive pieces
/// use the instruction cache's sets as if they were packed. Starting each
/// at a page boundary put every piece's first lines (the entry, its
/// `toi_rt_enter` wrapper) in the same few sets: on DeltaBlue, any change
/// to code size moved which of them conflicted, up to twice the L1i misses
/// and 3% of the cycles.
struct ExecMem {
    /// Where the code starts, `offset` bytes into the pages.
    ptr: *mut u8,
    len: usize,
    offset: usize,
}

/// Measuring: `TOI_CODE_SHIFT=<bytes>` (0 to 63) starts every piece that
/// much past where it would, to sample how alignment alone moves a
/// benchmark: on DeltaBlue, builds running the same instructions differ by
/// up to 3% in cycles as the hot code's alignment changes.
fn code_shift() -> usize {
    static SHIFT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SHIFT.get_or_init(|| {
        std::env::var("TOI_CODE_SHIFT").ok().and_then(|s| s.parse().ok()).unwrap_or(0) % 64
    })
}

/// The reserved range code is carved from: (next free address, end, the
/// next piece's offset into its first page).
static EXEC_RANGE: std::sync::Mutex<(usize, usize, usize)> =
    std::sync::Mutex::new((0, 0, 0));
const EXEC_RESERVE: usize = 1 << 30;

impl ExecMem {
    fn new(len: usize) -> Self {
        let len = len.max(1);
        let mut range = EXEC_RANGE.lock().unwrap();
        let offset = range.2 + code_shift();
        let pages = (offset + len).next_multiple_of(PAGE);
        if range.1 - range.0 < pages {
            let size = EXEC_RESERVE.max(pages);
            let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
            let base =
                unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_NONE, flags, -1, 0) };
            assert!(base != libc::MAP_FAILED, "mmap failed");
            (range.0, range.1) = (base as usize, base as usize + size);
        }
        let start = range.0 as *mut u8;
        range.0 += pages;
        range.2 = (offset + len).next_multiple_of(64) % 4096;
        let r = unsafe { libc::mprotect(start.cast(), pages, libc::PROT_READ | libc::PROT_WRITE) };
        assert_eq!(r, 0, "mprotect failed");
        ExecMem {
            ptr: unsafe { start.add(offset) },
            len,
            offset,
        }
    }

    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// The pages: their start and length.
    fn pages(&self) -> (*mut u8, usize) {
        let start = unsafe { self.ptr.sub(self.offset) };
        (start, (self.offset + self.len).next_multiple_of(PAGE))
    }

    fn seal(&mut self) {
        arch::flush_icache(self.ptr, self.len);
        let (start, len) = self.pages();
        let r = unsafe { libc::mprotect(start.cast(), len, libc::PROT_READ | libc::PROT_EXEC) };
        assert_eq!(r, 0, "mprotect failed");
    }
}

impl Drop for ExecMem {
    /// The pages go back to the system; their addresses stay reserved.
    fn drop(&mut self) {
        let (start, len) = self.pages();
        unsafe {
            libc::madvise(start.cast(), len, libc::MADV_DONTNEED);
            libc::mprotect(start.cast(), len, libc::PROT_NONE);
        }
    }
}

// ---- Decoding ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Insn {
    /// Bytecode offset, in the code of frame `frame`.
    pc: usize,
    op: Op,
    /// `toi_hole_imm`: the immediate operand (1 for `inc`; a call's
    /// argument count).
    imm: i32,
    /// `toi_hole_reg`: the register operand, if any (a call's callee,
    /// `Mov`'s source).
    reg: u8,
    /// A second register operand: a call's argument window, `Mov`'s
    /// destination.
    reg2: u8,
    /// Instruction index of the jump target, if any.
    target: usize,
    /// Not compiled: the code exits to the interpreter before it (see
    /// [`JitFunction::for_vm`]).
    bail: bool,
    /// The (inlined) frame the instruction belongs to; 0 is the function
    /// compiled. See [`lower`].
    frame: u8,
    /// A call's inline-cache slot.
    ic: u16,
    /// For a guard [`lower`] adds before an inlined call: the function the
    /// callee register must hold (and `op` is `Nop`).
    guard: u64,
    /// The guard is before a self tail call (see `Lowering::tail_call`):
    /// the frame must not be a `new` one, either.
    tail: bool,
    /// The guard is a case of a polymorphic inlined call, not the last:
    /// on a mismatch it jumps to `target`, the next case, instead of
    /// exiting (see `Lowering::inline_poly`).
    next_case: bool,
    /// For a `Nop` [`lower`] adds in a case of a polymorphic inlined call:
    /// the register operand is known to hold an object of this shape there
    /// (see `Lowering::receiver_shapes`); no code.
    assume: u64,
    /// The guard (`guard`) is on the receiver's shape, not the callee: a
    /// case of a polymorphic inlined call whose callee load was dropped
    /// (see `Lowering::inline_poly`); `guard` 1 is the receiver's cell
    /// check before the cases.
    on_shape: bool,
    /// For a `Nop` [`lower`] adds before an inlined constructor (with
    /// `guard` its function): the `CallSite` whose `ctor` allocates `this`
    /// (into `acc`); see `Lowering::inline_construct`.
    alloc: u64,
    /// A call [`lower`] compiles as `Array.prototype.push` (1) or `pop`
    /// (2) of its receiver, after a guard on the callee.
    native: u8,
    /// For a `GetNamed`: only its receiver's shape check, the value unused
    /// (a proven callee's load; see `Lowering::drop_callee_load`). For the
    /// `Star` of that value: no code (analyses still see the register
    /// overwritten, by something not an object they track).
    check_only: bool,
    /// For a `push` or `pop` whose callee's load was dropped: the callee,
    /// which an exit before it stores to the callee's register (the
    /// interpreter makes the call again).
    exit_const: u64,
    /// An `AddSmi` decoded from `SubSmi`: on numbers `x - k` is `x + -k`,
    /// but not on other values (`"12" - 1`), so its generic case subtracts.
    sub: bool,
    /// A check [`hoist_guards`] moved before a loop: the frame and bytecode
    /// offset of the load it checked for.
    hoisted: Option<(u8, u32)>,
    /// A call through `f.call(that, …)` whose `that` may be `undefined` or
    /// `null`, which the callee would see as the global object: not the
    /// caller's own `this` (see [`Lowering::nullable_this`]).
    nullable_this: bool,
}

impl Insn {
    fn falls_through(&self) -> bool {
        !self.bail && !matches!(self.op, Op::Jump | Op::Return)
    }

    fn jumps(&self) -> bool {
        !self.bail
            && matches!(
                self.op,
                Op::Jump
                    | Op::JumpIfTrue
                    | Op::JumpIfFalse
                    | Op::JumpIfToBooleanTrue
                    | Op::JumpIfToBooleanFalse
            )
            || !self.bail && is_compare_jump(self.op)
            || !self.bail && self.next_case
    }

    fn reads_reg(&self) -> bool {
        self.guard != 0
            // (An `assume` refines its register's copies: they must still
            // be its copies there.)
            || self.assume != 0
            || !self.bail
                && matches!(
                    self.op,
                    Op::Ldar
                        | Op::Star
                        | Op::Add
                        | Op::Sub
                        | Op::Mul
                        | Op::Div
                        | Op::Mod
                        | Op::BitAnd
                        | Op::BitOr
                        | Op::BitXor
                        | Op::ShiftLeft
                        | Op::ShiftRight
                        | Op::ShiftRightLogical
                        | Op::JumpIfLess
                        | Op::JumpIfLessEqual
                        | Op::JumpIfGreater
                        | Op::JumpIfGreaterEqual
                        | Op::TestLessThan
                        | Op::TestGreaterThan
                        | Op::TestLessThanOrEqual
                        | Op::TestGreaterThanOrEqual
                        | Op::TestEqual
                        | Op::TestStrictEqual
                        | Op::TestInstanceOf
                        | Op::TestIn
                        | Op::GetNamed
                        | Op::SetNamed
                        | Op::GetKeyed
                        | Op::SetKeyed
                )
    }

    fn is_call(&self) -> bool {
        matches!(self.op, Op::Call | Op::CallUndefined | Op::Construct)
    }

    /// The registers a call reads: the callee and the argument window
    /// (receiver first, for `Call`).
    fn call_reads(&self) -> impl Iterator<Item = usize> {
        let n = self.imm as usize + usize::from(self.op == Op::Call);
        let (f, w) = (self.reg as usize, self.reg2 as usize);
        std::iter::once(f).chain(w..w + n)
    }

    /// What an exit before it records: its frame and bytecode offset
    /// (below 2^22; see [`AFTER_CALL`] and [`BAD_RESULT`]).
    fn exit_record(&self) -> i32 {
        debug_assert!(self.pc < 1 << 21);
        ((self.frame as u32) << 24 | self.pc as u32) as i32
    }

    /// Whether it is a property access, whose stencil exits when the
    /// receiver's shape isn't the one its inline cache held.
    fn is_property(&self) -> bool {
        matches!(
            self.op,
            Op::GetNamed
                | Op::SetNamed
                | Op::LdaGlobal
                | Op::StaGlobal
                | Op::GetKeyed
                | Op::SetKeyed
        )
    }

    /// Whether it reads the register operand's value (`star` only writes it).
    /// The frame whose feedback (inline caches, names) the instruction's
    /// `ic` indexes: its own, or for a load moved out of a loop (in the
    /// frame the loop is in, see `hoist_loads`) the one it was moved from.
    fn ic_frame(&self) -> usize {
        match (self.op, self.hoisted) {
            (Op::GetNamed, Some((f, _))) => f as usize,
            _ => self.frame as usize,
        }
    }

    fn uses_reg_value(&self) -> bool {
        self.reads_reg() && self.op != Op::Star
    }
}

/// The compare-and-jumps (plain forms): `acc OP r` or `acc OP imm`.
fn is_compare_jump(op: Op) -> bool {
    matches!(
        op,
        Op::JumpIfLess
            | Op::JumpIfLessSmi
            | Op::JumpIfLessEqual
            | Op::JumpIfGreater
            | Op::JumpIfGreaterEqual
            | Op::JumpIfLessEqualSmi
            | Op::JumpIfGreaterSmi
            | Op::JumpIfGreaterEqualSmi
            | Op::JumpIfStrictEqualSmi
    )
}

/// Decode `code`. With `vm`, instructions the JIT doesn't compile are
/// marked `bail`; otherwise they're an error.
fn decode(code: &[u8], vm: bool) -> Vec<Insn> {
    let imm_at = |at: usize| i32::from_le_bytes(code[at..at + 4].try_into().unwrap());
    let mut starts = HashMap::default();
    let mut raw = Vec::with_capacity(code.len() / 2);
    let mut pc = 0;
    while pc < code.len() {
        let op = Op::from_u8(code[pc]).unwrap_or_else(|| panic!("bad opcode {} at {pc}", code[pc]));
        starts.insert(pc, raw.len());
        // Loop opcodes only count iterations in the interpreter.
        raw.push((pc, op.plain()));
        pc += op.size();
    }
    assert_eq!(pc, code.len(), "truncated instruction");
    let index_of = |pc: usize, off: i32| {
        let to = pc
            .checked_add_signed(off as isize)
            .expect("jump out of range");
        *starts
            .get(&to)
            .unwrap_or_else(|| panic!("jump into the middle of an instruction at {to}"))
    };
    let mut insns: Vec<Insn> = raw
        .into_iter()
        .map(|(pc, op)| {
            let (mut imm, mut reg, mut target, mut bail) = (0, 0, 0, false);
            let (mut reg2, mut ic) = (0, 0);
            // On numbers `x - k` is `x + -k`, so `SubSmi` shares every
            // typed `AddSmi` stencil and fusion (but for k = INT32_MIN).
            let mut sub = false;
            let op = match op {
                Op::SubSmi if imm_at(pc + 1) != i32::MIN => {
                    imm = -imm_at(pc + 1);
                    sub = true;
                    Op::AddSmi
                }
                op => op,
            };
            match op {
                Op::LdaSmi => imm = imm_at(pc + 1),
                // (A `SubSmi`'s negated immediate is already in `imm`.)
                Op::AddSmi if code[pc] == Op::AddSmi as u8 => imm = imm_at(pc + 1),
                Op::AddSmi => {}
                Op::Inc => imm = 1,
                Op::Ldar | Op::Star | Op::Add => reg = code[pc + 1],
                Op::JumpIfLess
                | Op::JumpIfLessEqual
                | Op::JumpIfGreater
                | Op::JumpIfGreaterEqual => {
                    (reg, target) = (code[pc + 1], index_of(pc, imm_at(pc + 2)))
                }
                Op::JumpIfLessSmi
                | Op::JumpIfLessEqualSmi
                | Op::JumpIfGreaterSmi
                | Op::JumpIfGreaterEqualSmi
                | Op::JumpIfStrictEqualSmi => {
                    (imm, target) = (imm_at(pc + 1), index_of(pc, imm_at(pc + 5)))
                }
                Op::Jump
                | Op::JumpIfTrue
                | Op::JumpIfFalse
                | Op::JumpIfToBooleanTrue
                | Op::JumpIfToBooleanFalse => target = index_of(pc, imm_at(pc + 1)),
                Op::ModSmi
                | Op::BitAndSmi
                | Op::BitOrSmi
                | Op::TestEqualSmi
                | Op::MulSmi
                | Op::DivSmi
                | Op::ShiftLeftSmi
                | Op::ShiftRightSmi
                | Op::ShiftRightLogicalSmi => imm = imm_at(pc + 1),
                Op::TestLessThan
                | Op::TestEqual
                | Op::TestGreaterThan
                | Op::TestLessThanOrEqual
                | Op::TestGreaterThanOrEqual
                | Op::TestStrictEqual
                | Op::TestInstanceOf
                | Op::TestIn
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::Mod
                | Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::ShiftLeft
                | Op::ShiftRight
                | Op::ShiftRightLogical => reg = code[pc + 1],
                Op::TestUndefined
                | Op::TestNull
                | Op::TestNullish
                | Op::ToNumber
                | Op::Negate
                | Op::BitNot => {}
                Op::LdaUpvalue | Op::StaUpvalue if vm => imm = code[pc + 1] as i32,
                Op::Nop
                | Op::Return
                | Op::LdaTrue
                | Op::LdaFalse
                | Op::LdaUndefined
                | Op::LdaNull => {}
                op if op.plain() != op => unreachable!("decoded as plain jumps"),
                // The inline-cache slot goes in `imm`.
                Op::GetNamed | Op::SetNamed if vm => {
                    (reg, imm) = (
                        code[pc + 1],
                        u16::from_le_bytes([code[pc + 2], code[pc + 3]]) as i32,
                    )
                }
                Op::LdaGlobal | Op::StaGlobal if vm => {
                    imm = u16::from_le_bytes([code[pc + 1], code[pc + 2]]) as i32
                }
                Op::Mov if vm => (reg, reg2) = (code[pc + 1], code[pc + 2]),
                Op::CreateArray | Op::ForInKeys if vm => {}
                // The capacity in `imm`, the shape's cache slot in `ic`.
                Op::CreateObject if vm => {
                    imm = code[pc + 1] as i32;
                    ic = u16::from_le_bytes([code[pc + 2], code[pc + 3]]);
                }
                Op::LdaConstant if vm => {
                    imm = u16::from_le_bytes([code[pc + 1], code[pc + 2]]) as i32
                }
                Op::Call | Op::CallUndefined | Op::Construct if vm => {
                    (reg, reg2, imm) = (code[pc + 1], code[pc + 2], code[pc + 3] as i32);
                    ic = u16::from_le_bytes([code[pc + 4], code[pc + 5]]);
                }
                Op::GetKeyed if vm => {
                    (reg, imm) = (
                        code[pc + 1],
                        u16::from_le_bytes([code[pc + 2], code[pc + 3]]) as i32,
                    )
                }
                // The object in `reg`, the key in `reg2`.
                // The inline-cache slot in `imm`, as `GetKeyed`'s.
                Op::SetKeyed if vm => {
                    (reg, reg2, imm) = (
                        code[pc + 1],
                        code[pc + 2],
                        u16::from_le_bytes([code[pc + 3], code[pc + 4]]) as i32,
                    )
                }
                _ if vm => bail = true,
                op => panic!("the JIT does not compile {op:?} yet"),
            }
            Insn {
                pc,
                op,
                imm,
                reg,
                reg2,
                target,
                bail,
                frame: 0,
                ic,
                guard: 0,
                tail: false,
                next_case: false,
                assume: 0,
                on_shape: false,
                alloc: 0,
                native: 0,
                check_only: false,
                exit_const: 0,
                sub,
                hoisted: None,
                nullable_this: false,
            }
        })
        .collect();
    unswap_backward_jumps(&mut insns);
    insns
}

/// The bytecode has no backward conditional jumps (the interpreter counts
/// iterations only at `Jump` and the compare-and-jumps' loop forms), so
/// the front end writes one as its negation over a `Jump`: a loop whose
/// test isn't a compare-and-jump (`i < this.size()`) ends `JumpIfFalse
/// exit; Jump body; exit:`. Compiled code counts nothing: one conditional
/// jump, taken each iteration, and a `Nop`. Measured on DeltaBlue: 135M
/// unconditional jumps a run (`Plan.execute`'s loop, 89M).
fn unswap_backward_jumps(insns: &mut [Insn]) {
    let mut targeted = vec![false; insns.len() + 1];
    for i in insns.iter().filter(|i| i.jumps()) {
        targeted[i.target] = true;
    }
    for k in 0..insns.len().saturating_sub(1) {
        let negated = match insns[k].op {
            Op::JumpIfTrue => Op::JumpIfFalse,
            Op::JumpIfFalse => Op::JumpIfTrue,
            Op::JumpIfToBooleanTrue => Op::JumpIfToBooleanFalse,
            Op::JumpIfToBooleanFalse => Op::JumpIfToBooleanTrue,
            _ => continue,
        };
        let next = insns[k + 1];
        if insns[k].target == k + 2 && next.op == Op::Jump && next.target <= k && !targeted[k + 1] {
            (insns[k].op, insns[k].target) = (negated, next.target);
            insns[k + 1] = Insn { op: Op::Nop, target: 0, ..next };
        }
    }
}

/// The registers live into each instruction `insns` of the function with
/// bytecode `code` (decoded) as the interpreter runs it. Conservative:
/// every register operand is read, but the destinations of `Star` and
/// `Mov`, and it covers instructions the JIT doesn't compile (which
/// [`reg_live_in`] doesn't). Live into the first: what the function may
/// read before writing, when called (see [`Lowering::entry_live`]).
fn bytecode_live(code: &[u8], insns: &[Insn]) -> Vec<RegSet> {
    use crate::bytecode::Operand;
    // Per instruction: registers read, and the register written.
    let effects: Vec<(RegSet, Option<usize>)> = insns
        .iter()
        .map(|insn| {
            let op = Op::from_u8(code[insn.pc]).unwrap();
            let (mut at, mut regs, mut count) = (insn.pc + 1, Vec::new(), 0);
            for &k in op.operands() {
                match k {
                    Operand::Reg => regs.push(code[at] as usize),
                    Operand::Count => count = code[at] as usize,
                    _ => {}
                }
                at += k.width();
            }
            let mut reads = [0; 4];
            let write = match op {
                Op::Star => Some(regs[0]),
                Op::Mov => {
                    rs_add(&mut reads, regs[0]);
                    Some(regs[1])
                }
                Op::Call | Op::CallUndefined | Op::Construct => {
                    let n = count + usize::from(op == Op::Call);
                    for r in std::iter::once(regs[0]).chain(regs[1]..regs[1] + n) {
                        rs_add(&mut reads, r);
                    }
                    None
                }
                _ => {
                    regs.iter().for_each(|&r| rs_add(&mut reads, r));
                    None
                }
            };
            (reads, write)
        })
        .collect();
    let n = insns.len();
    let mut live_in: Vec<RegSet> = vec![[0; 4]; n];
    let mut changed = true;
    while changed {
        changed = false;
        for k in (0..n).rev() {
            let insn = &insns[k];
            let mut live = [0; 4];
            let falls = !matches!(insn.op, Op::Jump | Op::Return | Op::Throw);
            let jumps = is_compare_jump(insn.op)
                || matches!(
                    insn.op,
                    Op::Jump
                        | Op::JumpIfTrue
                        | Op::JumpIfFalse
                        | Op::JumpIfToBooleanTrue
                        | Op::JumpIfToBooleanFalse
                );
            let succs = [falls.then_some(k + 1), jumps.then_some(insn.target)];
            for s in succs.into_iter().flatten().filter(|&s| s < n) {
                rs_or(&mut live, &live_in[s]);
            }
            let (reads, write) = &effects[k];
            if let Some(w) = write {
                live[w / 64] &= !(1 << (w % 64));
            }
            rs_or(&mut live, reads);
            if !rs_eq(&live, &live_in[k]) {
                live_in[k] = live;
                changed = true;
            }
        }
    }
    live_in
}

// ---- Inline-cache feedback (VM frames) ----

/// What a property instruction's inline cache held when compiling, as
/// stencil operands: the receiver's shape, where the property is, and the
/// field's type (see `object::FieldType`): what a load gives, or what a
/// store must check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prop {
    /// Not a property instruction, or nothing cached: bail.
    None,
    /// A keyed load left to the runtime: a site that read a typed array's,
    /// a buffer's or a string's elements (`vm::BYTES_LOC`; see
    /// `get_keyed_rt` in the stencils).
    Runtime,
    /// An upvalue of an inlined frame's function, known: its cell
    /// (`vm::Upvalue`), at address `cell`.
    Upvalue { cell: u64 },
    /// A keyed load whose receivers were typed arrays of element kind
    /// `kind` (`object::TypedKind`; see `tget_*` in the stencils).
    Typed { kind: u8 },
    /// In the receiver, at byte offset `off`.
    Own { shape: u64, off: i32, ft: FieldType },
    /// In the receiver's out-of-line slots, at byte offset `off`.
    Oop { shape: u64, off: i32, ft: FieldType },
    /// In a prototype, at address `addr` (stable while no prototype
    /// changes shape; see [`JitFunction::vm_enter`]).
    Proto {
        shape: u64,
        addr: u64,
        ft: FieldType,
    },
    /// A store adding the property: write `new_shape`, then the in-object
    /// slot at `off`.
    Transition {
        shape: u64,
        new_shape: u64,
        off: i32,
        ft: FieldType,
    },
    /// A load folded to the value it had when compiled, `value`, of a
    /// field of type `ft`, after a check that the word at `at` still is
    /// `expect` (no check if `at` is 0). See [`fold_constant_loads`].
    Const {
        value: u64,
        at: u64,
        expect: u64,
        ft: FieldType,
        /// The object the word is in (or 0).
        obj: u64,
    },
    /// A global, in the global object at `global`.
    Global {
        shape: u64,
        global: u64,
        off: i32,
        oop: bool,
        ft: FieldType,
    },
    /// A keyed load whose receivers had shape `shape`, with elements of
    /// type `elems`.
    Keyed { shape: u64, elems: FieldType },
    /// A `GetNamed` that read an array's or a string's `length`.
    Length,
    /// A named access whose cache holds several receiver shapes: one
    /// guard dispatching to `n` accesses, one per shape: the lowering's
    /// [`Case`]s at `at` (see [`Lowered::polys`]; kept apart, they would
    /// make every `Prop` several times larger).
    Poly { n: u8, at: u32 },
    /// A call, described by `CallSite` number `site` of the lowering:
    /// straight into the callee's compiled code if `direct`, else through
    /// the runtime. The VM's code epoch, at `epoch_addr`, must still be
    /// `epoch` after it (and before, to call directly).
    Call {
        site: u32,
        direct: bool,
        /// The callee register holds the site's closure for sure (a guard
        /// passed, or `Lowering::proven_callee`): `call_direct_nc`, without
        /// that check.
        proven: bool,
        epoch: u32,
        epoch_addr: u64,
    },
}

impl Prop {
    /// A monomorphic property access's stencil operands `(imm, imm64,
    /// imm64b)`: the offset, the receiver's shape, and a prototype's slot
    /// address or the new shape.
    fn operands(self) -> Option<(i32, u64, u64)> {
        match self {
            Prop::Own { shape, off, .. } | Prop::Oop { shape, off, .. } => Some((off, shape, 0)),
            Prop::Proto { shape, addr, .. } => Some((0, shape, addr)),
            Prop::Transition {
                shape,
                new_shape,
                off,
                ..
            } => Some((off, shape, new_shape)),
            _ => None,
        }
    }
}

/// A polymorphic access's cases, in [`Lowered::polys`].
pub(crate) type Cases = [Case; crate::vm::IC_WAYS];

/// The cases of polymorphic access `n`, `at` in `polys`.
fn poly_cases(polys: &[Cases], n: u8, at: u32) -> &[Case] {
    &polys[at as usize][..n as usize]
}

/// One shape of a polymorphic access ([`Prop::Poly`]): a receiver of shape
/// `shape` has the property at `loc`, of type `ft`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Case {
    pub shape: u64,
    pub loc: CaseLoc,
    pub ft: FieldType,
}

/// Where a [`Case`]'s property is, as in the monomorphic [`Prop`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaseLoc {
    Own(i32),
    Oop(i32),
    Proto(u64),
    Transition { new_shape: u64, off: i32 },
}

/// Bit 0 of a [`Case`]'s `shape`: the case stands for every shape of a
/// family (see `Shape::family`), the rest of the word being the family.
const FAMILY: u64 = 1;

impl Case {
    /// Whether the case stands for a family of shapes ([`FAMILY`]).
    fn is_family(&self) -> bool {
        self.shape & FAMILY != 0
    }

    /// Whether a receiver of shape `s` takes this case.
    fn admits(&self, s: u64) -> bool {
        self.shape == s
            || self.is_family()
                && (unsafe { (*(s as *const Shape)).family } as u64 | FAMILY) == self.shape
    }

    /// A shape every receiver taking this case extends (see [`extends`]):
    /// its own, or its family's.
    fn base(&self) -> u64 {
        self.shape & !FAMILY
    }

    /// The guard dispatching to the case: `shape_*`, or `family_*`.
    fn guard(&self, kind: &str) -> String {
        format!(
            "{}_{kind}",
            if self.is_family() { "family" } else { "shape" }
        )
    }

    const NONE: Case = Case {
        shape: 0,
        loc: CaseLoc::Own(0),
        ft: FieldType::None,
    };

    /// The case of a monomorphic property access, if it is one.
    fn of(prop: Prop) -> Option<Case> {
        let (shape, loc, ft) = match prop {
            Prop::Own { shape, off, ft } => (shape, CaseLoc::Own(off), ft),
            Prop::Oop { shape, off, ft } => (shape, CaseLoc::Oop(off), ft),
            Prop::Proto { shape, addr, ft } => (shape, CaseLoc::Proto(addr), ft),
            Prop::Transition {
                shape,
                new_shape,
                off,
                ft,
            } => (shape, CaseLoc::Transition { new_shape, off }, ft),
            _ => return None,
        };
        Some(Case { shape, loc, ft })
    }

    /// The monomorphic access for this case alone.
    fn prop(&self) -> Prop {
        let (shape, ft) = (self.shape, self.ft);
        match self.loc {
            CaseLoc::Own(off) => Prop::Own { shape, off, ft },
            CaseLoc::Oop(off) => Prop::Oop { shape, off, ft },
            CaseLoc::Proto(addr) => Prop::Proto { shape, addr, ft },
            CaseLoc::Transition { new_shape, off } => Prop::Transition {
                shape,
                new_shape,
                off,
                ft,
            },
        }
    }

    /// The access for a receiver known to have the case's shape: its
    /// `_nc` stencil family (load or store) and operands `(imm, imm64b)`.
    fn access(&self, load: bool) -> (&'static str, i32, u64) {
        match (self.loc, load) {
            (CaseLoc::Own(off), true) => ("get_own", off, 0),
            (CaseLoc::Oop(off), true) => ("get_oop", off, 0),
            (CaseLoc::Proto(addr), true) => ("get_proto", 0, addr),
            (CaseLoc::Own(off), false) => ("set_own", off, 0),
            (CaseLoc::Oop(off), false) => ("set_oop", off, 0),
            (CaseLoc::Transition { new_shape, off }, false) => ("set_tr", off, new_shape),
            _ => unreachable!("a load's case is no transition, a store's no prototype's"),
        }
    }
}

/// The VM state the JIT reads to compile a function's property accesses.
#[derive(Clone, Copy)]
pub(crate) struct Feedback {
    pub ics: *const crate::vm::Ic,
    /// The property name of each inline-cache slot.
    pub names: *const Atom,
    pub global: *mut crate::object::Object,
    /// For stencils calling into it (`create_array`).
    pub vm: *mut crate::vm::Vm,
    /// The function's constants (`LdaConstant`).
    pub constants: *const Value,
}

/// A slot location (`Shape::location`) as `(out of line, byte offset)`.
fn split_location(loc: i32) -> (bool, i32) {
    if loc >= 0 {
        (false, loc)
    } else {
        (true, !loc * 8)
    }
}

impl Feedback {
    /// Way 0 of inline-cache slot `k`, the other ways following.
    fn ic(&self, k: usize) -> *const crate::vm::Ic {
        unsafe { self.ics.add(k * crate::vm::IC_WAYS) }
    }

    /// The property name a property instruction accesses.
    fn name(&self, insn: &Insn) -> Atom {
        unsafe { *self.names.add(insn.imm as usize) }
    }

    /// What instruction `insn`'s inline cache says, for the stencil, under
    /// prototype epoch `epoch`. A prototype load cached under another epoch
    /// says nothing: its holder may be shadowed now, and the interpreter
    /// hasn't re-run the access to find out.
    fn prop(&self, insn: &Insn, epoch: u32, polys: &mut Vec<Cases>) -> Prop {
        if insn.bail
            || !matches!(
                insn.op,
                Op::GetNamed
                    | Op::SetNamed
                    | Op::LdaGlobal
                    | Op::StaGlobal
                    | Op::GetKeyed
                    | Op::SetKeyed
            )
        {
            return Prop::None;
        }
        let ic = unsafe { *self.ic(insn.imm as usize) };
        if insn.op == Op::GetNamed && ic.loc == crate::vm::LENGTH_LOC {
            return Prop::Length;
        }
        if matches!(insn.op, Op::GetKeyed | Op::SetKeyed) && ic.loc == crate::vm::BYTES_LOC {
            return match ic.epoch {
                k @ 1..=9 => Prop::Typed { kind: k as u8 - 1 },
                _ if insn.op == Op::SetKeyed => Prop::None,
                _ => Prop::Runtime,
            };
        }
        // (A store's cache records typed arrays only.)
        if insn.op == Op::SetKeyed {
            return Prop::None;
        }
        if ic.shape.is_null() {
            return Prop::None;
        }
        if insn.op == Op::GetKeyed {
            return Prop::Keyed {
                shape: ic.shape as u64,
                elems: unsafe { (*ic.shape).elems },
            };
        }
        if !matches!(insn.op, Op::GetNamed | Op::SetNamed) {
            return self.way(insn, ic, epoch);
        }
        // Named accesses: every way that caches something usable.
        let mut cases = [Case::NONE; crate::vm::IC_WAYS];
        let mut n = 0;
        let mut first = Prop::None;
        for k in 0..crate::vm::IC_WAYS {
            let w = unsafe { *self.ic(insn.imm as usize).add(k) };
            if w.shape.is_null() {
                break;
            }
            let p = self.way(insn, w, epoch);
            let Some(c) = Case::of(p) else { continue };
            // Loads from one prototype slot by shapes of one family (arrays
            // whose elements' types differ, say): one case for the family,
            // which takes the family's shapes not cached yet too.
            let family = |c: &Case| match c.loc {
                CaseLoc::Proto(_) if insn.op == Op::GetNamed => {
                    (unsafe { (*(c.base() as *const Shape)).family }) as u64
                }
                _ => 0,
            };
            if let f @ 1.. = family(&c)
                && let Some(d) = cases[..n]
                    .iter_mut()
                    .find(|d| d.loc == c.loc && d.ft == c.ft && family(d) == f)
            {
                d.shape = f | FAMILY;
                continue;
            }
            if n == 0 {
                first = p;
            }
            cases[n] = c;
            n += 1;
        }
        match n {
            0 => Prop::None,
            1 if !cases[0].is_family() => first,
            _ => {
                polys.push(cases);
                Prop::Poly {
                    n: n as u8,
                    at: polys.len() as u32 - 1,
                }
            }
        }
    }

    /// What one way `ic` of instruction `insn`'s inline cache says (see
    /// [`Feedback::prop`]).
    fn way(&self, insn: &Insn, ic: crate::vm::Ic, epoch: u32) -> Prop {
        let shape = ic.shape as u64;
        let (oop, off) = split_location(ic.loc);
        if off > MAX_SLOT_OFF {
            return Prop::None;
        }
        let name = self.name(insn);
        let field = |s: *const Shape| {
            let s = unsafe { &*s };
            s.lookup(name).map_or(FieldType::Any, |k| s.field_type_relied(k))
        };
        // For loads, `holder` is a prototype; for stores, the field's type.
        let holder = ic.holder;
        match insn.op {
            Op::GetNamed if !holder.is_null() && ic.epoch != epoch => Prop::None,
            Op::GetNamed if !holder.is_null() => Prop::Proto {
                shape,
                addr: unsafe { Object::slot(holder, ic.loc) } as u64,
                ft: field(unsafe { (*holder).shape }),
            },
            Op::SetNamed if !ic.new_shape.is_null() => Prop::Transition {
                shape,
                new_shape: ic.new_shape as u64,
                off,
                ft: field(ic.new_shape),
            },
            Op::GetNamed | Op::SetNamed if oop => Prop::Oop {
                shape,
                off,
                ft: field(ic.shape),
            },
            Op::GetNamed | Op::SetNamed => Prop::Own {
                shape,
                off,
                ft: field(ic.shape),
            },
            // Only own properties of the global object are compiled.
            Op::LdaGlobal if !holder.is_null() => Prop::None,
            Op::StaGlobal if !ic.new_shape.is_null() => Prop::None,
            _ => Prop::Global {
                shape,
                global: self.global as u64,
                off,
                oop,
                ft: field(ic.shape),
            },
        }
    }
}

// ---- Type inference ----

/// The type lattice: `Bot < I32 < F64 < Any`, and `Bot < Obj(s) < Any` for
/// each shape `s` (see [`Ty::join`]). The join of `I32` and `F64` is `F64`:
/// an int32 converts losslessly to a double, so an edge carrying `I32` into
/// an `F64` point gets a conversion stencil.
///
/// `Obj(s)` is an object whose shape is `s` (the address of its `Shape`);
/// `Ext(s)` one whose shape is `s` or one of its descendants, so it has
/// `s`'s own properties where `s` has them (`Obj(s) < Ext(s) < Any`).
/// Loads from a field typed `Obj(s)` give `Ext(s)`: the object may have
/// gained properties since it was stored (see `object::FieldType`).
/// Values get it at entry, from the live values, and from a property
/// access's shape guard, which then needn't be repeated. A transitioning
/// store forgets every `Obj` fact but its receiver's. Between transitions
/// shapes stay exact; even a stale fact would still read and replace
/// correctly, because a transition only appends slots, but a prototype load
/// (which a new own property shadows) and a transition need the exact
/// shape.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Ty {
    Bot,
    I32,
    F64,
    Obj(u64),
    Ext(u64),
    Any,
}

impl Ty {
    /// The exact type of a runtime value. All cells are objects so far.
    pub fn of(v: Value) -> Ty {
        if v.is_int32() {
            Ty::I32
        } else if v.is_double() {
            Ty::F64
        } else if v.is_cell() {
            Ty::Obj(unsafe { *(v.to_bits() as *const u64) })
        } else {
            Ty::Any
        }
    }

    /// The least upper bound.
    pub fn join(self, other: Ty) -> Ty {
        use Ty::*;
        match (self, other) {
            (a, b) if a == b => a,
            (Bot, t) | (t, Bot) => t,
            (I32, F64) | (F64, I32) => F64,
            (Obj(a) | Ext(a), Obj(b) | Ext(b)) => Ty::join_shapes(a, b),
            _ => Any,
        }
    }

    /// The join of two object types, of shapes `a` and `b`: `Ext` of the
    /// one the other extends, if one does (an object of either has its
    /// properties where it has them). Out of line, to keep `join` small.
    #[inline(never)]
    fn join_shapes(a: u64, b: u64) -> Ty {
        let (sa, sb) = (a as *const Shape, b as *const Shape);
        if a == b || unsafe { extends(sa, sb) } {
            Ty::Ext(b)
        } else if unsafe { extends(sb, sa) } {
            Ty::Ext(a)
        } else {
            Ty::Any
        }
    }

    /// The shape an object type knows.
    fn shape(self) -> Option<u64> {
        match self {
            Ty::Obj(s) | Ty::Ext(s) => Some(s),
            _ => None,
        }
    }

    /// The prediction for the result of a call into code whose `Return`s
    /// give `ret`, or `Any` for none. A check after the call establishes
    /// it. For an object of shape `s` or a descendant, the check is for `s`
    /// exactly (descendants can't be told apart cheaply): mostly the shape
    /// hasn't changed, and if it has, the check fails and the call isn't
    /// predicted again.
    fn of_result(ret: Ty) -> Ty {
        match ret {
            Ty::I32 | Ty::F64 => ret,
            Ty::Obj(s) | Ty::Ext(s) => Ty::Obj(s),
            _ => Ty::Any,
        }
    }

    /// The type of values loaded from a field of type `ft`, and the piece
    /// that puts it in its representation (a number goes to `facc`).
    fn of_field(ft: FieldType) -> (Ty, Option<&'static str>) {
        match ft {
            FieldType::Int => (Ty::I32, None),
            FieldType::Num => (Ty::F64, Some("unbox_num_acc")),
            FieldType::Obj(s) => (Ty::Ext(s as u64), None),
            FieldType::None | FieldType::Any => (Ty::Any, None),
        }
    }
}

/// The types of `len` registers, kept for those not `Bot` only (in
/// register order): a few are live at any point, of the dozens a frame
/// with inlined calls has, and states are copied and joined per
/// instruction. Copies share the list until one changes it (most
/// instructions change `acc` only).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Regs {
    len: usize,
    tys: Rc<Vec<(u8, Ty)>>,
}

static BOT: Ty = Ty::Bot;

impl Regs {
    fn filled(len: usize, t: Ty) -> Regs {
        let tys = if t == Ty::Bot {
            Vec::new()
        } else {
            (0..len).map(|r| (r as u8, t)).collect()
        };
        Regs {
            len,
            tys: Rc::new(tys),
        }
    }

    /// From every register's type.
    fn from_dense(tys: impl IntoIterator<Item = Ty>) -> Regs {
        let (mut len, mut out) = (0, Vec::new());
        for t in tys {
            if t != Ty::Bot {
                out.push((len as u8, t));
            }
            len += 1;
        }
        Regs {
            len,
            tys: Rc::new(out),
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// Register `r`'s type, if it is one of them (as a slice's `get`).
    fn get(&self, r: usize) -> Option<&Ty> {
        (r < self.len).then(|| &self[r])
    }

    fn set(&mut self, r: usize, t: Ty) {
        match self.tys.binary_search_by_key(&(r as u8), |e| e.0) {
            Ok(k) if t == Ty::Bot => {
                Rc::make_mut(&mut self.tys).remove(k);
            }
            Ok(k) if self.tys[k].1 != t => Rc::make_mut(&mut self.tys)[k].1 = t,
            Ok(_) => {}
            Err(_) if t == Ty::Bot => {}
            Err(k) => Rc::make_mut(&mut self.tys).insert(k, (r as u8, t)),
        }
    }

    /// The registers not `Bot`, with their types.
    fn iter(&self) -> impl Iterator<Item = (usize, Ty)> + '_ {
        self.tys.iter().map(|&(r, t)| (r as usize, t))
    }

    /// Replace each type not `Bot` by `f` of it (`Bot` drops it); `f`
    /// must be a function of its arguments (it may be called twice).
    fn map(&mut self, mut f: impl FnMut(usize, Ty) -> Ty) {
        let Some(first) = self.tys.iter().position(|&(r, t)| f(r as usize, t) != t) else {
            return;
        };
        let tys = Rc::make_mut(&mut self.tys);
        let mut k = first;
        for j in first..tys.len() {
            let (r, t) = tys[j];
            let t = f(r as usize, t);
            if t != Ty::Bot {
                tys[k] = (r, t);
                k += 1;
            }
        }
        tys.truncate(k);
    }

    /// Join `other` in; whether anything changed.
    fn join(&mut self, other: &Regs) -> bool {
        if Rc::ptr_eq(&self.tys, &other.tys) {
            return false;
        }
        // Whether `other` has a register `self` hasn't, or a wider type.
        let (mut a, mut b) = (self.tys.iter().peekable(), other.tys.iter().peekable());
        let changed = loop {
            match (a.peek(), b.peek()) {
                (Some(&&(ra, ta)), Some(&&(rb, tb))) if ra == rb => {
                    if ta.join(tb) != ta {
                        break true;
                    }
                    a.next();
                    b.next();
                }
                (Some(&&(ra, _)), Some(&&(rb, _))) if ra < rb => {
                    a.next();
                }
                (_, Some(_)) => break true,
                (_, None) => break false,
            }
        };
        if !changed {
            return false;
        }
        let mut out: Vec<(u8, Ty)> = Vec::with_capacity(self.tys.len() + other.tys.len());
        let (mut a, mut b) = (self.tys.iter().peekable(), other.tys.iter().peekable());
        loop {
            match (a.peek(), b.peek()) {
                (Some(&&(ra, ta)), Some(&&(rb, tb))) if ra == rb => {
                    out.push((ra, ta.join(tb)));
                    a.next();
                    b.next();
                }
                (Some(&&e), Some(&&(rb, _))) if e.0 < rb => {
                    out.push(e);
                    a.next();
                }
                (_, Some(&&e)) => {
                    out.push(e);
                    b.next();
                }
                (Some(&&e), None) => {
                    out.push(e);
                    a.next();
                }
                (None, None) => break,
            }
        }
        self.tys = Rc::new(out);
        true
    }
}

impl std::ops::Index<usize> for Regs {
    type Output = Ty;
    fn index(&self, r: usize) -> &Ty {
        match self.tys.binary_search_by_key(&(r as u8), |e| e.0) {
            Ok(k) => &self.tys[k].1,
            Err(_) => &BOT,
        }
    }
}

/// A short list, inline up to `N` elements (then on the heap): inference
/// clones type states, and their copy lists (`TypeState::same`) are short,
/// so as `Vec`s they were most of compiling's allocations.
#[derive(Clone)]
pub struct Small<T: Copy + Default, const N: usize> {
    len: usize,
    inline: [T; N],
    heap: Vec<T>,
}

impl<T: Copy + Default, const N: usize> Small<T, N> {
    fn new() -> Self {
        Small {
            len: 0,
            inline: [T::default(); N],
            heap: Vec::new(),
        }
    }

    fn spilled(&self) -> bool {
        self.len > N
    }

    fn as_slice(&self) -> &[T] {
        if self.spilled() {
            &self.heap
        } else {
            &self.inline[..self.len]
        }
    }

    fn as_mut_slice(&mut self) -> &mut [T] {
        if self.spilled() {
            &mut self.heap
        } else {
            &mut self.inline[..self.len]
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn iter(&self) -> std::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    fn push(&mut self, x: T) {
        if self.len < N {
            self.inline[self.len] = x;
        } else {
            if self.len == N {
                self.heap = self.inline.to_vec();
            }
            self.heap.push(x);
        }
        self.len += 1;
    }

    fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        if self.spilled() {
            self.heap.retain(|x| keep(x));
            self.len = self.heap.len();
            if self.len <= N {
                self.inline[..self.len].copy_from_slice(&self.heap);
                self.heap = Vec::new();
            }
        } else {
            let mut k = 0;
            for j in 0..self.len {
                let x = self.inline[j];
                if keep(&x) {
                    self.inline[k] = x;
                    k += 1;
                }
            }
            self.len = k;
        }
    }
}

impl<T: Copy + Default + Ord, const N: usize> Small<T, N> {
    fn sort_unstable(&mut self) {
        self.as_mut_slice().sort_unstable();
    }

    /// Drop consecutive repeats (as `Vec::dedup`).
    fn dedup(&mut self) {
        let mut last = None;
        self.retain(|&x| last.replace(x) != Some(x));
    }
}

impl<T: Copy + Default, const N: usize> std::ops::Deref for Small<T, N> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy + Default, const N: usize> Default for Small<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default, const N: usize> Extend<T> for Small<T, N> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, it: I) {
        for x in it {
            self.push(x);
        }
    }
}

impl<T: Copy + Default, const N: usize> FromIterator<T> for Small<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(it: I) -> Self {
        let mut s = Self::new();
        s.extend(it);
        s
    }
}

impl<T: Copy + Default + PartialEq, const N: usize> PartialEq for Small<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Copy + Default + Eq, const N: usize> Eq for Small<T, N> {}

impl<T: Copy + Default + std::hash::Hash, const N: usize> std::hash::Hash for Small<T, N> {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.as_slice().hash(h)
    }
}

impl<T: Copy + Default + std::fmt::Debug, const N: usize> std::fmt::Debug for Small<T, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

/// Copy pairs (see [`TypeState::same`]).
pub type Copies = Small<(u8, u8), 12>;
/// Registers of a class.
type Members = Small<u8, 24>;

/// Types of the accumulator and of every register, at an instruction
/// boundary.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeState {
    pub acc: Ty,
    pub regs: Regs,
    /// Copies: registers holding the same value (copied by `Ldar` and
    /// `Star`, neither rewritten since) form a class, named by its lowest
    /// register. Here each other register of a class, with that lowest
    /// (sorted by register); registers alone aren't. What an instruction
    /// proves about a register (a shape guard passed, a transition) holds
    /// for its copies (see [`copies`]).
    pub same: Copies,
    /// The class `acc` holds the value of, if any.
    pub acc_same: Option<u8>,
}

impl TypeState {
    fn any(nregs: usize) -> TypeState {
        TypeState {
            acc: Ty::Any,
            regs: Regs::filled(nregs, Ty::Any),
            same: Copies::new(),
            acc_same: None,
        }
    }

    /// Join `other` into `self`; returns whether `self` changed. Copies
    /// stay copies only where both say so.
    fn join(&mut self, other: &TypeState) -> bool {
        let acc = self.acc.join(other.acc);
        let mut changed = acc != self.acc;
        self.acc = acc;
        changed |= self.regs.join(&other.regs);
        if !self.same.is_empty() {
            let before = self.same.len();
            self.same.retain(|&(r, c)| other.class(r as usize) == c);
            changed |= self.same.len() != before;
        }
        if self.acc_same.is_some() && self.acc_same != other.acc_same {
            self.acc_same = None;
            changed = true;
        }
        changed
    }

    /// The class of register `r` (its lowest copy).
    fn class(&self, r: usize) -> u8 {
        class_in(self.same.as_slice(), r)
    }
}

/// Dead registers (and a dead `acc`) leave their classes.
fn leave_dead(st: &mut TypeState, live: &RegSet, acc_live: bool) {
    let old = std::mem::take(&mut st.same);
    // Per class (its lowest register), its lowest live register.
    let mut low = Copies::new();
    for &(r, c) in old.iter() {
        if !low.iter().any(|e| e.0 == c) {
            let first = std::iter::once(c)
                .chain(old.iter().filter(|e| e.1 == c).map(|e| e.0))
                .find(|&q| rs_has(live, q as usize));
            if let Some(l) = first {
                low.push((c, l));
            }
        }
        if rs_has(live, r as usize)
            && let Some(&(_, l)) = low.iter().find(|e| e.0 == c)
            && l != r
        {
            st.same.push((r, l));
        }
    }
    st.same.sort_unstable();
    let acc = st
        .acc_same
        .and_then(|c| match low.iter().find(|e| e.0 == c) {
            Some(&(_, l)) => Some(l),
            None => rs_has(live, c as usize).then_some(c),
        });
    st.acc_same = if acc_live { acc } else { None };
}

/// The class of register `r` in copies `same` (see [`TypeState::same`]).
fn class_in(same: &[(u8, u8)], r: usize) -> u8 {
    match same.binary_search_by_key(&(r as u8), |e| e.0) {
        Ok(k) => same[k].1,
        Err(_) => r as u8,
    }
}

/// The registers of class `c` in copies `same`, lowest first.
fn members(same: &[(u8, u8)], c: u8) -> Members {
    std::iter::once(c)
        .chain(same.iter().filter(|e| e.1 == c).map(|e| e.0))
        .collect()
}

/// Make the registers `regs` (sorted) one class in `same`, the others
/// keeping theirs.
fn set_class(same: &mut Copies, regs: &[u8]) {
    same.retain(|e| !regs.contains(&e.0));
    if let Some((&low, rest)) = regs.split_first() {
        same.extend(rest.iter().map(|&q| (q, low)));
    }
    same.sort_unstable();
}

/// After `insn` (the out-state `out`, from the in-state `st`): track copies
/// (`Ldar` makes `acc` a copy, `Star` a register; anything else writing
/// `acc` ends its copy), and give the copies of the register an instruction
/// learned something about (its operand, whose type changed and which it
/// doesn't write) that type too.
fn copies(insn: &Insn, st: &TypeState, out: &mut TypeState, fixed_from: usize) {
    let n = st.regs.len();
    let mut same = st.same.clone();
    out.acc_same = st.acc_same;
    if insn.bail {
        out.acc_same = None;
        out.same = same;
        return;
    }
    let r = insn.reg as usize;
    match insn.op {
        Op::Ldar if r < n => out.acc_same = Some(class_in(&same, r)),
        Op::Star if r < n => {
            // `r` leaves its class: the others' lowest may change.
            let old = class_in(&same, r);
            let rest: Members = members(&same, old)
                .iter()
                .copied()
                .filter(|&q| q as usize != r)
                .collect();
            same.retain(|e| e.0 as usize != r && e.1 != old);
            set_class(&mut same, &rest);
            match out.acc_same {
                // `acc` copies class `c`: `r` joins it.
                Some(c) => {
                    let c = if c as usize == r {
                        rest.first().copied().unwrap_or(r as u8)
                    } else {
                        c
                    };
                    let mut joined: Members = members(&same, c)
                        .iter()
                        .copied()
                        .filter(|&q| q as usize != r)
                        .chain(std::iter::once(r as u8))
                        .collect();
                    joined.sort_unstable();
                    joined.dedup();
                    set_class(&mut same, &joined);
                    out.acc_same = Some(joined[0]);
                }
                None => out.acc_same = Some(r as u8),
            }
        }
        _ => {
            // What the instruction learned about its operand holds for the
            // operand's copies.
            if insn.uses_reg_value() && r < n && out.regs[r] != st.regs[r] {
                let t = out.regs[r];
                let c = class_in(&same, r);
                for &q in members(&same, c).iter().filter(|&&q| (q as usize) < fixed_from) {
                    out.regs.set(q as usize, t);
                }
                if out.acc_same == Some(c) && writes_acc(insn).is_none() {
                    out.acc = t;
                }
            }
            if writes_acc(insn).is_some() {
                out.acc_same = None;
            }
        }
    }
    out.same = same;
}

/// `Some(())` if `insn` may write `acc` (other than `Ldar`).
fn writes_acc(insn: &Insn) -> Option<()> {
    let keeps = insn.alloc == 0
        && insn.native == 0
        && (insn.jumps()
            || matches!(
                insn.op,
                Op::Nop | Op::Jump | Op::Star | Op::SetNamed | Op::SetKeyed | Op::StaGlobal
            ));
    (!keeps).then_some(())
}

/// Inputs to stencil selection that are fixed for a compilation.
#[derive(Clone, Copy)]
struct Spec<'a> {
    /// `None`: no specialization; every type is `Any`, every stencil generic.
    widened: Option<&'a HashSet<usize>>,
    /// Registers pinned as doubles (they live in FP slots).
    pinned: &'a [bool],
    /// Whether `acc` is live into each instruction, and which registers
    /// are. [`infer`] gives dead values the type `Bot`, so a value that is
    /// never read (a comparison's boolean after its jump, a temporary out
    /// of scope) doesn't widen a join or need conversions on its way there.
    acc_live: &'a [bool],
    reg_live: &'a [RegSet],
    /// Per instruction, its inline cache's contents (property instructions
    /// in VM frames); empty otherwise.
    props: &'a [Prop],
    /// The cases of polymorphic `props`.
    polys: &'a [Cases],
    /// Per instruction: an int32 add whose result only feeds the bitwise
    /// operation right after it, so it may wrap (`(a + b) | 0`).
    wraps: &'a [bool],
    /// Per instruction: `acc` is never a negative int32 there (see
    /// [`nonneg_acc`]).
    nonneg: &'a [bool],
    /// Per instruction: a `GetNamed` whose value only goes into the
    /// `SetNamed` right after it (`a.x = b.y`): a number stays boxed (see
    /// [`select_property`]).
    moved: &'a [bool],
    /// For VM functions: each frame's feedback (property names; see
    /// [`lower`] for frames).
    feedback: Option<&'a [Feedback]>,
    /// Per call instruction, the type its result is predicted to have
    /// (checked after the call; see [`Ty::of_result`]); `Any` for none.
    rets: &'a [Ty],
    /// An object of at least a stable shape has exactly it (see
    /// `Shape::is_stable`): the code has no store through the runtime,
    /// which could add a transition while the code runs (only calls check
    /// the code epoch after them).
    stable_ok: bool,
    /// Registers from here keep their types: what an instruction learns
    /// about a copy of one isn't given to it (see [`copies`]). The values
    /// of loads moved out of loops (`hoist_loads`): a loop learning their
    /// shape (from a check on a copy) gave the loop's back edge another
    /// type than its entry, and inference went over the loop again (71's
    /// compiles 20-37% slower).
    fixed_from: usize,
}

/// What [`select`] decided for one instruction: the stencil family, the
/// operand-type case (`"ii"`, `"f"`, … or `""` for generic) and the
/// out-state.
struct Sel {
    base: &'static str,
    case: &'static str,
    out: TypeState,
    /// The stencil's operands `(imm, imm64, imm64b)`, if `select` worked
    /// them out (property accesses on a known shape).
    ops: Option<(i32, u64, u64)>,
    /// A piece checking `acc` before the stencil, with its `imm64`: a
    /// store's value against the field's type. It exits before the
    /// instruction.
    check: Option<(&'static str, u64, u64)>,
    /// A piece checking a call's result against its predicted type, with
    /// its `imm64`. It exits after the call, with the result.
    result: Option<(&'static str, u64)>,
    /// A piece after the stencil (`unbox_num_acc`).
    post: Option<&'static str>,
    /// The register whose slot the stencil reads, if not the
    /// instruction's: a copy's, if in memory or a general register (see
    /// [`slot_reg`]).
    reg: Option<u8>,
}

impl Sel {
    fn new(base: &'static str, case: &'static str, out: TypeState) -> Sel {
        Sel {
            base,
            case,
            out,
            ops: None,
            check: None,
            result: None,
            post: None,
            reg: None,
        }
    }
}

/// The register whose slot the stencil for `insn` reads: `sel.reg` if its
/// home is memory or a general register, else the instruction's (`reg2`
/// for `push` and `pop`).
fn slot_reg(insn: &Insn, sel: &Sel, homes: &[Home]) -> u8 {
    let own = if insn.native == 1 || insn.native == 2 {
        insn.reg2
    } else {
        insn.reg
    };
    match sel
        .reg
        .map(|r| (r, homes.get(r as usize).copied().unwrap_or(Home::Mem)))
    {
        Some((r, Home::Mem | Home::Gpr(_))) => r,
        _ => own,
    }
}

/// The type case of a one-operand number operation on `acc`.
fn num_case(acc: Ty) -> &'static str {
    match acc {
        Ty::I32 => "i",
        Ty::F64 => "f",
        _ => "",
    }
}

/// The type case of a two-operand number operation, acc first.
fn pair_case(acc: Ty, reg: Ty) -> &'static str {
    match (acc, reg) {
        (Ty::I32, Ty::I32) => "ii",
        (Ty::I32, Ty::F64) => "if",
        (Ty::F64, Ty::I32) => "fi",
        (Ty::F64, Ty::F64) => "ff",
        _ => "",
    }
}

/// Relations for the generic comparison stencils, between a register or
/// literal operand `x` and `acc` (`a`): masks of the orders (`x` less,
/// equal, greater than `a`, unordered: bits 0–3) they hold for (`holds` in
/// `stencils/stencils.rs`).
const REL_X_LT_A: i32 = 0b0001;
const REL_A_LT_X: i32 = 0b0100;
const REL_X_LE_A: i32 = 0b0011;
const REL_A_LE_X: i32 = 0b0110;

/// The relation code of a test `r OP acc`.
fn rel_code(op: Op) -> i32 {
    match op {
        Op::TestLessThan => REL_X_LT_A,
        // `r > acc` is `acc < r`, `r >= acc` is `acc <= r`.
        Op::TestGreaterThan => REL_A_LT_X,
        Op::TestLessThanOrEqual => REL_X_LE_A,
        Op::TestGreaterThanOrEqual => REL_A_LE_X,
        op => unreachable!("{op:?} is not relational"),
    }
}

/// Whether a value of type `t` is an object other than a string: two such
/// are equal (`==` or `===`) only if they are the same object.
fn identity_ty(t: Ty) -> bool {
    t.shape()
        .is_some_and(|s| unsafe { (*(s as *const Shape)).class } != crate::object::Class::String)
}

/// Whether a value of type `t` is never null or undefined.
fn never_nullish(t: Ty) -> bool {
    matches!(t, Ty::I32 | Ty::F64 | Ty::Obj(_) | Ty::Ext(_))
}

/// Choose the operation's type case from its in-state. Instructions in
/// `widened` overflowed int32 before, so they produce a double instead of
/// exiting. A store to a pinned register makes it `F64` (an int32 is
/// converted).
fn select(insn: &Insn, i: usize, st: &TypeState, spec: &Spec) -> Sel {
    let mut sel = select_types(insn, i, st, spec);
    // A load only checking its receiver (`emit_poly` handles `poly`), and
    // the store of its value, which nothing reads.
    // A load into `acc` that nothing reads (an inlined callee's `acc =
    // undefined` at its start, say) emits nothing.
    let dead = !insn.bail
        && matches!(
            insn.op,
            Op::Ldar
                | Op::LdaSmi
                | Op::LdaUndefined
                | Op::LdaNull
                | Op::LdaTrue
                | Op::LdaFalse
                | Op::LdaConstant
        )
        && spec.acc_live.get(i + 1) == Some(&false);
    if dead || insn.check_only && matches!(insn.op, Op::Star | Op::Ldar) {
        sel.base = "nop";
        sel.case = "";
    } else if insn.check_only && sel.base.starts_with("get_") {
        if sel.case == "nc" {
            sel.base = "nop";
        } else if let Some(c) = spec.props.get(i).and_then(|&p| Case::of(p)) {
            sel.base = "check_shape";
            sel.ops = Some((0, c.shape, 0));
        } else {
            unreachable!("a load proven to hold the callee has a case");
        }
        sel.case = "";
        sel.post = None;
    }
    copies(insn, st, &mut sel.out, spec.fixed_from);
    sel
}

/// [`select`], but for the copies.
fn select_types(insn: &Insn, i: usize, st: &TypeState, spec: &Spec) -> Sel {
    use Ty::{Any, F64, I32};
    // A check moved before a loop (see `hoist_guards`).
    if insn.op == Op::Nop
        && let Some(&Prop::Const { at, expect, .. }) = spec.props.get(i)
    {
        return Sel {
            ops: Some((0, at, expect)),
            ..Sel::new("guard_word", "", st.clone())
        };
    }
    let Some(widened) = spec.widened else {
        let any = TypeState::any(st.regs.len());
        let no = Spec {
            widened: Some(&HashSet::default()),
            ..*spec
        };
        let mut sel = select_types(insn, i, &any, &no);
        // Generic code knows no shapes.
        if sel.case == "nc" {
            return Sel::new("bail", "", any);
        }
        let (base, case) = (sel.base, sel.case);
        if sel.ops.is_some() {
            sel.out = any;
            return sel;
        }
        // Generic code keeps `acc` boxed, so `lda_smi`, not `lda_smi_i`.
        let case = if base == "lda_smi" { "" } else { case };
        return Sel::new(base, case, any);
    };
    let mut out = st.clone();
    let reg = st.regs.get(insn.reg as usize).copied().unwrap_or(Any);
    // An int32 op: exits on overflow unless it overflowed before.
    let int_op = |exits: &'static str, widened_case: &'static str| {
        if widened.contains(&i) {
            (widened_case, F64)
        } else {
            (exits, I32)
        }
    };
    let wraps = spec.wraps.get(i).copied().unwrap_or(false);
    let (base, case) = match insn.op {
        _ if insn.bail => ("bail", ""),
        // A number is never the function an inlined call expects.
        _ if insn.guard != 0 && matches!(reg, I32 | F64) => ("bail", ""),
        _ if insn.guard != 0 && insn.tail => {
            return Sel {
                ops: Some((interp::FLAGS as i32, insn.guard, 0)),
                ..Sel::new("check_tail_callee", "", out)
            };
        }
        // Its copies too (the dispatch guards a copy made for the call:
        // the inlined code reads the original).
        _ if insn.assume != 0 => {
            let r = insn.reg as usize;
            if r < st.regs.len() {
                for q in members(&st.same, class_in(&st.same, r)).iter() {
                    if (*q as usize) < spec.fixed_from {
                        out.regs.set(*q as usize, Ty::Obj(insn.assume));
                    }
                }
            }
            out.regs.set(r, Ty::Obj(insn.assume));
            return Sel::new("nop", "", out);
        }
        // An allocation scalar replacement removed: its check only.
        _ if insn.native == 3 => {
            let c = unsafe { (*(insn.alloc as *const CallSite)).ctor.as_deref().unwrap() };
            out.acc = Any;
            return Sel {
                ops: Some((0, c.proto_at as u64, c.proto)),
                ..Sel::new("ctor_check", "", out)
            };
        }
        // The receiver in `reg2` (read where its copies' lowest is), known
        // to be an array if its type has an array's shape; `push`'s
        // argument in `acc`.
        _ if insn.native == 1 || insn.native == 2 => {
            let a = insn.reg2 as usize;
            let array = st
                .regs
                .get(a)
                .and_then(|t| t.shape())
                .is_some_and(|s| unsafe {
                    (*(s as *const Shape)).class == crate::object::Class::Array
                });
            let case = match (insn.native, st.acc == I32, array) {
                (1, true, true) => "i_nc",
                (1, true, false) => "i",
                (_, _, true) => "nc",
                _ => "",
            };
            let (base, acc) = if insn.native == 1 {
                ("array_push", I32)
            } else {
                ("array_pop", Any)
            };
            out.acc = acc;
            return Sel {
                reg: (a < st.regs.len()).then(|| st.class(a)),
                ..Sel::new(base, case, out)
            };
        }
        // An inlined `new`: `this` allocated as an object literal is, with
        // the shape the constructor's root has (whose capacity doesn't
        // change once settled), after a check that the constructor's
        // `prototype` didn't change.
        _ if insn.alloc != 0 => {
            let site = unsafe { &*(insn.alloc as *const CallSite) };
            let c = site.ctor.as_deref().unwrap();
            debug_assert_eq!(
                &c.header[1..],
                &[1 << 8 | c.slots << 16, 0, 0, 0, 0],
                "the header `create_object` writes"
            );
            out.acc = Ty::Obj(site.this_shape);
            return Sel {
                ops: Some((c.slots as i32, site.this_shape, c.heap as u64)),
                check: Some(("guard_word", c.proto_at as u64, c.proto)),
                ..Sel::new("create_object", "", out)
            };
        }
        _ if insn.on_shape && insn.guard == 1 => return Sel::new("guard_cell", "", out),
        _ if insn.on_shape => {
            let family = insn.guard & FAMILY != 0;
            return Sel {
                ops: Some((0, insn.guard & !FAMILY, 0)),
                ..Sel::new(
                    match (family, insn.next_case) {
                        (false, true) => "shape_jne",
                        (false, false) => "shape_guard",
                        (true, true) => "family_jne",
                        (true, false) => "family_guard",
                    },
                    "",
                    out,
                )
            };
        }
        _ if insn.guard != 0 && insn.next_case => {
            return Sel {
                ops: Some((0, insn.guard, 0)),
                ..Sel::new("callee_jne", "", out)
            };
        }
        _ if insn.guard != 0 => {
            return Sel {
                ops: Some((0, insn.guard, 0)),
                ..Sel::new("check_callee", "", out)
            };
        }
        // The callee may add properties to any object: exact shapes become
        // lower bounds (see `Ty::Ext`).
        Op::Call | Op::CallUndefined | Op::Construct => {
            out.regs.map(|_, t| match t {
                Ty::Obj(x) => Ty::Ext(x),
                t => t,
            });
            let base = match spec.props.get(i) {
                Some(Prop::Call { direct: true, .. }) if insn.op == Op::Construct => {
                    "construct_direct"
                }
                Some(Prop::Call { direct: true, .. }) => "call_direct",
                _ => "call",
            };
            let (acc, result, post) = match spec.rets.get(i).copied().unwrap_or(Any) {
                I32 => (I32, Some(("check_acc_int", 0)), None),
                F64 => (F64, Some(("check_acc_num", 0)), Some("unbox_num_acc")),
                Ty::Obj(s) => (Ty::Obj(s), Some(("check_acc_shape", s)), None),
                _ => (Any, None, None),
            };
            out.acc = acc;
            let proven = matches!(spec.props.get(i), Some(Prop::Call { proven: true, .. }));
            let case = if base == "call_direct" && proven {
                "nc"
            } else {
                ""
            };
            return Sel {
                result,
                post,
                ..Sel::new(base, case, out)
            };
        }
        // ToInt32(a + b) is the 32-bit wrapping add: no exit, no widening.
        Op::Add if wraps && st.acc == I32 && reg == I32 => {
            out.acc = I32;
            ("add", "wrap_ii")
        }
        Op::AddSmi | Op::Inc if wraps && st.acc == I32 => {
            out.acc = I32;
            ("add_smi", "wrap_i")
        }
        Op::Sub if wraps && st.acc == I32 && reg == I32 => {
            out.acc = I32;
            ("sub", "wrap_ii")
        }
        op if op.plain() != op => unreachable!("decoded as plain jumps"),
        Op::Sub | Op::Mul => {
            let (case, ty) = match (st.acc, reg) {
                (I32, I32) => int_op("ii", "iif"),
                (I32, F64) => ("if", F64),
                (F64, I32) => ("fi", F64),
                (F64, F64) => ("ff", F64),
                _ => ("", Any),
            };
            out.acc = ty;
            (if insn.op == Op::Sub { "sub" } else { "mul" }, case)
        }
        // `r & acc`, …: an int32 (`>>>` a uint32, as `Sub`'s overflow).
        Op::ShiftRightLogical if (st.acc, reg) == (I32, I32) => {
            let (case, ty) = int_op("ii", "iif");
            out.acc = ty;
            ("shr", case)
        }
        Op::BitAnd | Op::BitOr | Op::BitXor | Op::ShiftLeft | Op::ShiftRight | Op::ShiftRightLogical => {
            let base = match insn.op {
                Op::BitAnd => "bit_and",
                Op::BitOr => "bit_or",
                Op::BitXor => "bit_xor",
                Op::ShiftLeft => "shl",
                Op::ShiftRight => "sar",
                _ => "shr",
            };
            if (st.acc, reg) == (I32, I32) {
                out.acc = I32;
                (base, "ii")
            } else {
                out.acc = Any;
                (base, "")
            }
        }
        // `r % acc` on int32s: an int32, but for a zero or negative
        // divisor or a -0 (`mod_ii` exits; once it has, `mod_iif`).
        Op::Mod => {
            let (case, ty) = match (st.acc, reg) {
                (I32, I32) => int_op("ii", "iif"),
                _ => ("", Any),
            };
            out.acc = ty;
            ("mod", case)
        }
        // A quotient is a double, even an exact one.
        Op::Div => {
            let case = pair_case(st.acc, reg);
            out.acc = if case.is_empty() { Any } else { F64 };
            ("div", case)
        }
        // By the sign of k: a positive one never makes -0, a negative one
        // does from 0, and 0 does from a negative (`mul_smi_*_i`).
        Op::MulSmi => {
            let (case, ty) = match st.acc {
                I32 if widened.contains(&i) => ("iif", F64),
                I32 if insn.imm > 0 => ("pos_i", I32),
                I32 if insn.imm < 0 => ("neg_i", I32),
                I32 => ("zero_i", I32),
                F64 => ("f", F64),
                _ => ("", Any),
            };
            out.acc = ty;
            ("mul_smi", case)
        }
        Op::DivSmi => {
            let case = num_case(st.acc);
            out.acc = if case.is_empty() { Any } else { F64 };
            ("div_smi", case)
        }
        // On an int32 the count is part of the stencil (`stencil_for`);
        // `<< 0` and `>> 0` leave it as it is, `>>> 0` exits when it is
        // negative (the result is above 2^31 - 1).
        Op::ShiftLeftSmi | Op::ShiftRightSmi | Op::ShiftRightLogicalSmi => {
            let base = match insn.op {
                Op::ShiftLeftSmi => "shl_smi",
                Op::ShiftRightSmi => "sar_smi",
                _ => "shr_smi",
            };
            let shr0 = insn.op == Op::ShiftRightLogicalSmi && insn.imm & 31 == 0;
            match st.acc {
                I32 if insn.imm & 31 == 0 && !shr0 => {
                    out.acc = I32;
                    ("nop", "")
                }
                I32 if shr0 && widened.contains(&i) => {
                    out.acc = Any;
                    (base, "")
                }
                I32 => {
                    out.acc = I32;
                    (base, "i")
                }
                F64 if !shr0 => {
                    out.acc = I32;
                    (base, "f")
                }
                // Not known to be a number: checked to be an int32 (and
                // compiled generic once that check has exited).
                Any if !shr0 && !widened.contains(&i) => {
                    out.acc = I32;
                    return Sel {
                        check: Some(("check_acc_int", 0, 0)),
                        ..Sel::new(base, "i", out)
                    };
                }
                _ => {
                    out.acc = if shr0 { Any } else { I32 };
                    (base, "")
                }
            }
        }
        // `acc OP k`: on an int32, `acc <= k` is `acc < k + 1` and `acc > k`
        // is `acc >= k + 1`. Generic: `rel_int`'s code with `x` = k.
        Op::JumpIfLessEqualSmi | Op::JumpIfGreaterSmi | Op::JumpIfGreaterEqualSmi => {
            let k = insn.imm;
            let code = match insn.op {
                Op::JumpIfLessEqualSmi => REL_A_LE_X,
                Op::JumpIfGreaterSmi => REL_X_LT_A,
                _ => REL_X_LE_A,
            };
            let generic = Sel {
                ops: Some((k, code as u64, 0)),
                ..Sel::new("jrel_smi", "", out.clone())
            };
            return match (st.acc, insn.op) {
                (I32, Op::JumpIfLessEqualSmi) if k < i32::MAX => Sel {
                    ops: Some((k + 1, 0, 0)),
                    ..Sel::new("jlt_smi", "i", out)
                },
                (I32, Op::JumpIfGreaterSmi) if k < i32::MAX => Sel {
                    ops: Some((k + 1, 0, 0)),
                    ..Sel::new("jge_smi", "i", out)
                },
                (I32, Op::JumpIfGreaterEqualSmi) => Sel::new("jge_smi", "i", out),
                (F64, Op::JumpIfLessEqualSmi) => Sel::new("jle_smi", "f", out),
                (F64, Op::JumpIfGreaterSmi) => Sel::new("jgt_smi", "f", out),
                (F64, Op::JumpIfGreaterEqualSmi) => Sel::new("jge_smi", "f", out),
                _ => generic,
            };
        }
        // A `switch` case: `acc === k`, `acc` unchanged.
        Op::JumpIfStrictEqualSmi => ("jseq_smi", num_case(st.acc)),
        // `acc OP r` on int32s as an existing `<` stencil (`tlt_*` test
        // `r < acc`); generic `jrel`.
        Op::JumpIfLessEqual | Op::JumpIfGreater | Op::JumpIfGreaterEqual => {
            if (st.acc, reg) == (I32, I32) {
                let base = match insn.op {
                    Op::JumpIfLessEqual => "tlt_jf",
                    Op::JumpIfGreater => "tlt_jt",
                    _ => "jge",
                };
                (base, "ii")
            } else {
                let code = match insn.op {
                    Op::JumpIfLessEqual => REL_A_LE_X,
                    Op::JumpIfGreater => REL_X_LT_A,
                    _ => REL_X_LE_A,
                };
                return Sel {
                    ops: Some((code, 0, 0)),
                    ..Sel::new("jrel", "", out)
                };
            }
        }
        Op::TestGreaterThan | Op::TestLessThanOrEqual | Op::TestGreaterThanOrEqual => {
            out.acc = Any;
            if (st.acc, reg) == (I32, I32) {
                let base = match insn.op {
                    Op::TestGreaterThan => "test_gt",
                    Op::TestLessThanOrEqual => "test_le",
                    _ => "test_ge",
                };
                (base, "ii")
            } else {
                return Sel {
                    ops: Some((rel_code(insn.op), 0, 0)),
                    ..Sel::new("test_rel", "", out)
                };
            }
        }
        Op::TestStrictEqual | Op::TestEqual if identity_ty(st.acc) && identity_ty(reg) => {
            out.acc = Any;
            ("test_id", "")
        }
        Op::TestStrictEqual => {
            out.acc = Any;
            if (st.acc, reg) == (I32, I32) {
                ("test_eq", "ii")
            } else {
                ("test_seq", "")
            }
        }
        // A new, empty array, through the runtime (the VM is the 64-bit
        // immediate); typed `Ext` of the empty array's shape, which says
        // only that it is a cell.
        Op::CreateArray => {
            let Some(vm) = spec.feedback.map(|f| f[insn.frame as usize].vm) else {
                return Sel::new("bail", "", out);
            };
            out.acc = Ty::Ext(unsafe { (*vm).array_shape } as u64);
            return Sel {
                ops: Some((0, vm as u64, 0)),
                ..Sel::new("create_array", "", out)
            };
        }
        // `for … in`'s keys, through the runtime (the VM is the 64-bit
        // immediate): an array.
        Op::ForInKeys => {
            let Some(vm) = spec.feedback.map(|f| f[insn.frame as usize].vm) else {
                return Sel::new("bail", "", out);
            };
            out.acc = Ty::Ext(unsafe { (*vm).array_shape } as u64);
            return Sel {
                ops: Some((0, vm as u64, 0)),
                ..Sel::new("for_in_keys", "", out)
            };
        }
        // A new plain object of the shape the inline cache holds (none
        // yet: the interpreter hasn't run it), allocated in the stencil:
        // the slot count, the shape and the heap's bump pointer are its
        // operands.
        Op::CreateObject => {
            let Some(fb) = spec.feedback.map(|f| &f[insn.frame as usize]) else {
                return Sel::new("bail", "", out);
            };
            let shape = unsafe { (*fb.ic(insn.ic as usize)).shape };
            if shape.is_null() {
                return Sel::new("bail", "", out);
            }
            debug_assert_eq!(
                &crate::object::object_header(unsafe { &*shape })[1..],
                &[1 << 8 | (insn.imm as u64) << 16, 0, 0, 0, 0],
                "the header `create_object` writes"
            );
            out.acc = Ty::Obj(shape as u64);
            let heap = unsafe { (*fb.vm).heap.bump() };
            return Sel {
                ops: Some((insn.imm, shape as u64, heap as u64)),
                ..Sel::new("create_object", "", out)
            };
        }
        // A constant the lowering gives (see `Lowering::global_this`).
        Op::LdaConstant if let Some(&Prop::Const { value, .. }) = spec.props.get(i) => {
            out.acc = Any;
            return Sel {
                ops: Some((0, value, 0)),
                ..Sel::new("lda_constant", "", out)
            };
        }
        // The constant is the 64-bit immediate: a double goes to `facc`.
        Op::LdaConstant => {
            let Some(fb) = spec.feedback.map(|f| &f[insn.frame as usize]) else {
                return Sel::new("bail", "", out);
            };
            let v = unsafe { *fb.constants.add(insn.imm as usize) };
            out.acc = Ty::of(v);
            let (case, bits) = match v.as_number() {
                Some(d) if out.acc == F64 => ("f", d.to_bits()),
                _ => ("", v.to_bits()),
            };
            return Sel {
                ops: Some((0, bits, 0)),
                ..Sel::new("lda_constant", case, out)
            };
        }
        Op::TestUndefined | Op::TestNull | Op::TestNullish => {
            out.acc = Any;
            if never_nullish(st.acc) {
                ("lda_false", "")
            } else {
                let base = match insn.op {
                    Op::TestUndefined => "test_undefined",
                    Op::TestNull => "test_null",
                    _ => "test_nullish",
                };
                (base, "")
            }
        }
        Op::Nop => ("nop", ""),
        Op::Negate => {
            let (case, ty) = match st.acc {
                I32 => int_op("i", "iif"),
                F64 => ("f", F64),
                _ => ("", Any),
            };
            out.acc = ty;
            ("neg", case)
        }
        Op::BitNot => {
            out.acc = if st.acc == I32 { I32 } else { Any };
            ("bit_not", if st.acc == I32 { "i" } else { "" })
        }
        // `lda_upvalue` finds the cell in the frame's function, at byte
        // `imm`: after the in-object slots of the function shape, which
        // every function object with upvalues has (`Vm::upvalues`); `_c`
        // has the cell (`Prop::Upvalue`).
        Op::LdaUpvalue | Op::StaUpvalue => {
            if insn.op == Op::LdaUpvalue {
                out.acc = Any;
            }
            let load = insn.op == Op::LdaUpvalue;
            return match spec.props.get(i).copied() {
                Some(Prop::Upvalue { cell }) => Sel {
                    ops: Some((0, cell, 0)),
                    ..Sel::new(if load { "lda_upvalue_c" } else { "sta_upvalue_c" }, "", out)
                },
                _ => Sel {
                    ops: Some((
                        (crate::object::INLINE_BASE as u32
                            + 8 * unsafe { (*(*interp::current_vm()).function_shape).capacity.get() }
                            + 8 * insn.imm as u32) as i32,
                        0,
                        0,
                    )),
                    ..Sel::new(if load { "lda_upvalue" } else { "sta_upvalue" }, "", out)
                },
            };
        }
        // `x++`'s old value: a number is itself; anything else exits, for
        // the interpreter to convert.
        Op::ToNumber if matches!(st.acc, I32 | F64) => ("nop", ""),
        Op::ToNumber if widened.contains(&i) => ("bail", ""),
        Op::ToNumber => ("check_acc_num", ""),
        Op::Jump => ("jump", ""),
        Op::Return => ("return", ""),
        Op::LdaSmi => {
            out.acc = I32;
            ("lda_smi", "i")
        }
        Op::Ldar => {
            out.acc = reg;
            ("ldar", if reg == F64 { "f" } else { "" })
        }
        Op::Star => {
            let r = insn.reg as usize;
            out.regs.set(
                r,
                if spec.pinned[r] && st.acc <= F64 {
                    F64
                } else {
                    st.acc
                },
            );
            let case = match st.acc {
                F64 => "f",
                I32 => "i",
                _ => "",
            };
            ("star", case)
        }
        Op::Add => {
            let (case, ty) = match (st.acc, reg) {
                (I32, I32) => int_op("ii", "iif"),
                (I32, F64) => ("if", F64),
                (F64, I32) => ("fi", F64),
                (F64, F64) => ("ff", F64),
                _ => ("", Any),
            };
            out.acc = ty;
            ("add", case)
        }
        // Untyped, a `SubSmi` subtracts (`sub_smi`, with k).
        Op::AddSmi if insn.sub && !matches!(st.acc, I32 | F64) => {
            out.acc = Any;
            return Sel {
                ops: Some((-insn.imm, 0, 0)),
                ..Sel::new("sub_smi", "", out)
            };
        }
        Op::AddSmi | Op::Inc => {
            let base = if insn.op == Op::Inc { "inc" } else { "add_smi" };
            let (case, ty) = match st.acc {
                I32 => int_op("i", "iif"),
                F64 => ("f", F64),
                _ => ("", Any),
            };
            out.acc = ty;
            (base, case)
        }
        Op::JumpIfLess => {
            let case = match (st.acc, reg) {
                (I32, I32) => "ii",
                (I32, F64) => "if",
                (F64, I32) => "fi",
                (F64, F64) => "ff",
                _ => "",
            };
            ("jlt", case)
        }
        Op::JumpIfLessSmi => {
            let case = match st.acc {
                I32 => "i",
                F64 => "f",
                _ => "",
            };
            ("jlt_smi", case)
        }
        Op::LdaTrue | Op::LdaFalse | Op::LdaUndefined | Op::LdaNull => {
            out.acc = Any;
            let base = match insn.op {
                Op::LdaTrue => "lda_true",
                Op::LdaFalse => "lda_false",
                Op::LdaUndefined => "lda_undefined",
                _ => "lda_null",
            };
            (base, "")
        }
        // A power-of-two modulus needs no division (`mod_smi_pow2`, whose
        // immediate is the mask; see `stencil_for`).
        Op::ModSmi => {
            let k = insn.imm;
            let base = if k > 0 && k & (k - 1) == 0 {
                "mod_smi_pow2"
            } else {
                "mod_smi"
            };
            let (case, ty) = match st.acc {
                // Never -0: the `_u` forms (see `stencil_for`).
                I32 if k > 0 && spec.nonneg.get(i).copied().unwrap_or(false) => ("u", I32),
                I32 if k > 0 => int_op("i", "iif"),
                _ => ("", Any),
            };
            out.acc = ty;
            (if case.is_empty() { "mod_smi" } else { base }, case)
        }
        // ToInt32, then the operation: always an int32.
        Op::BitAndSmi | Op::BitOrSmi => {
            out.acc = I32;
            let identity = if insn.op == Op::BitAndSmi { -1 } else { 0 };
            let base = if insn.op == Op::BitAndSmi {
                "bit_and_smi"
            } else {
                "bit_or_smi"
            };
            // `x | 0` and `x & -1` leave an int32 as it is.
            if st.acc == I32 && insn.imm == identity {
                ("nop", "")
            } else {
                (base, num_case(st.acc))
            }
        }
        Op::TestEqualSmi => {
            out.acc = Any;
            ("test_eq_smi", num_case(st.acc))
        }
        Op::TestInstanceOf => {
            out.acc = Any;
            ("test_instanceof", "")
        }
        Op::TestIn => {
            out.acc = Any;
            ("test_in", "")
        }
        Op::TestLessThan | Op::TestEqual => {
            out.acc = Any;
            let base = if insn.op == Op::TestLessThan {
                "test_lt"
            } else {
                "test_eq"
            };
            (base, pair_case(st.acc, reg))
        }
        // Booleans are `Any`; a number never equals `true` or `false`, but it
        // takes no special case: `stencil_for` boxes it for the generic test.
        Op::JumpIfTrue => ("jtrue", ""),
        Op::JumpIfFalse => ("jfalse", ""),
        Op::JumpIfToBooleanTrue => ("jtotrue", num_case(st.acc)),
        Op::JumpIfToBooleanFalse => ("jtofalse", num_case(st.acc)),
        // Property accesses. A receiver known to be a number has no
        // properties the cache could describe, one no definition reaches
        // may share an int32's slot: leave them to the interpreter.
        Op::GetNamed | Op::SetNamed | Op::GetKeyed | Op::SetKeyed
            if matches!(reg, I32 | F64 | Ty::Bot) =>
        {
            ("bail", "")
        }
        // A store whose site gave up on the stencils: through the runtime,
        // which may change the receiver's elements' type (and so its
        // shape: exact shapes become lower bounds, as after a call).
        Op::SetKeyed if spec.props.get(i) == Some(&Prop::Runtime) => {
            out.regs.map(|_, t| match t {
                Ty::Obj(x) => Ty::Ext(x),
                t => t,
            });
            return Sel {
                ops: Some((8 * insn.reg2 as i32, 0, 0)),
                ..Sel::new("set_keyed_rt", if st.acc == I32 { "i" } else { "" }, out)
            };
        }
        // A number into a typed array of the kind the cache saw: stored as
        // the kind stores it (`tset_*`), the key in a scratch register (see
        // `stencil_for`); checked to be one unless the receiver's shape
        // says so (`_chk`, against `typed_class_kind`).
        Op::SetKeyed
            if let Some(Prop::Typed { kind }) = spec.props.get(i).copied()
                && matches!(st.acc, I32 | F64)
                && typed_store(Some(kind), st.acc == I32).is_some() =>
        {
            let case = typed_store(Some(kind), st.acc == I32).unwrap();
            let known = reg.shape().and_then(typed_kind) == Some(kind);
            return Sel {
                ops: Some((typed_class_kind(kind), 0, 0)),
                ..Sel::new("tset", if known { case } else { typed_checked(case) }, out)
            };
        }
        // `o[k] = acc` (see `set_keyed` in the stencils): no checks of a
        // receiver known to be an array (`nc`); an int32 (`i`) or an object
        // (`o`) checked against the elements' type as such; none of the
        // value where the array's exact shape says its elements take it
        // (`nc_ia`, `nc_a`), or just that it's a number (`nc_n`).
        Op::SetKeyed => {
            let array = reg.shape().is_some_and(|s| unsafe {
                (*(s as *const Shape)).class == crate::object::Class::Array
            });
            let elems = match reg {
                Ty::Obj(s) => Some(unsafe { (*(s as *const Shape)).elems }),
                _ => None,
            };
            let object = matches!(st.acc, Ty::Obj(_) | Ty::Ext(_));
            let case = match (array, st.acc, elems) {
                (true, I32, Some(FieldType::Int | FieldType::Num | FieldType::Any)) => "nc_ia",
                (true, I32, _) => "nc_i",
                (true, _, _) if object => "nc_o",
                (true, _, Some(FieldType::Any)) => "nc_a",
                (true, _, Some(FieldType::Num)) => "nc_n",
                (true, _, _) => "nc",
                (false, I32, _) => "i",
                (false, _, _) if object => "o",
                _ => "",
            };
            return Sel {
                ops: Some((8 * insn.reg2 as i32, 0, 0)),
                ..Sel::new("set_keyed", case, out)
            };
        }
        Op::GetNamed | Op::SetNamed | Op::LdaGlobal | Op::StaGlobal => {
            return select_property(insn, i, st, spec);
        }
        // An exact receiver shape, known or guarded against the cached one,
        // types the element; any known shape skips the cell check.
        // A string's elements are code units: the interpreter reads them.
        Op::GetKeyed
            if reg.shape().is_some_and(|s| unsafe {
                (*(s as *const Shape)).class == crate::object::Class::String
            }) =>
        {
            ("bail", "")
        }
        Op::GetKeyed => {
            let ix = st.acc == I32;
            let pick = |plain: &'static str, i: &'static str| if ix { i } else { plain };
            let prop = spec.props.get(i).copied().unwrap_or(Prop::None);
            // A typed array's or buffer's elements are bytes: the generic
            // stencil, through the runtime.
            let bytes = reg.shape().is_some_and(|s| unsafe {
                matches!(
                    (*(s as *const Shape)).class,
                    crate::object::Class::TypedArray | crate::object::Class::ArrayBuffer
                )
            });
            // A typed array's element, of the kind the cache saw: checked to
            // be one unless the receiver's shape says so.
            if let Prop::Typed { kind } = prop
                && ix
            {
                use crate::object::TypedKind as K;
                let known = reg.shape().and_then(typed_kind) == Some(kind);
                let (w, ty) = match K::from_u8(kind) {
                    K::I8 => ("i8", I32),
                    K::U8 | K::U8Clamped => ("u8", I32),
                    K::I16 => ("i16", I32),
                    K::U16 => ("u16", I32),
                    K::I32 => ("i32", I32),
                    K::U32 => ("u32f", F64),
                    K::F32 => ("f32", F64),
                    K::F64 => ("f64", F64),
                };
                const CHECKED: [&str; 9] =
                    ["i8_chk", "u8_chk", "u8_chk", "i16_chk", "u16_chk", "i32_chk", "u32f_chk", "f32_chk", "f64_chk"];
                out.acc = ty;
                return Sel {
                    ops: Some((typed_class_kind(kind), 0, 0)),
                    ..Sel::new("tget", if known { w } else { CHECKED[kind as usize] }, out)
                };
            }
            // (A site that gave up on the stencils, but with an int32 key
            // into a receiver of known shape: `get_keyed_i_nc`, as before.)
            let known = ix && matches!(reg, Ty::Obj(_) | Ty::Ext(_));
            if bytes || matches!(prop, Prop::Typed { .. }) || (prop == Prop::Runtime && !known) {
                out.acc = Any;
                return Sel::new("get_keyed_rt", "", out);
            }
            let (case, elems) = match (reg, prop) {
                (Ty::Obj(s), _) => (pick("nc", "i_nc"), unsafe { (*(s as *const Shape)).elems }),
                (_, Prop::Keyed { shape, elems })
                    if !matches!(elems, FieldType::None | FieldType::Any) =>
                {
                    out.regs.set(insn.reg as usize, Ty::Obj(shape));
                    (pick("sh", "i_sh"), elems)
                }
                (Ty::Ext(_), _) => (pick("nc", "i_nc"), FieldType::Any),
                _ => (pick("", "i"), FieldType::Any),
            };
            let (t, post) = Ty::of_field(elems);
            out.acc = t;
            return Sel {
                post,
                ..Sel::new("get_keyed", case, out)
            };
        }
        op => unreachable!("rejected by decode: {op:?}"),
    };
    Sel::new(base, case, out)
}

/// While alive: the function whose code is being made, which the field
/// types read are recorded for (`object::COMPILING`); the one before after.
struct Compiling(usize);

impl Compiling {
    fn of(code: *const crate::vm::Code) -> Compiling {
        Compiling(crate::object::COMPILING.with(|c| c.replace(code as usize)))
    }
}

impl Drop for Compiling {
    fn drop(&mut self) {
        crate::object::COMPILING.with(|c| c.set(self.0));
    }
}

/// Compiled code takes the value in prototype slot `addr` as fixed: a
/// store of another drops it (`Vm::relied_slots`).
fn rely_on_slot(addr: u64) {
    unsafe { (*crate::interp::current_vm()).relied_slots.insert(addr as usize) };
}

/// The element kind (`object::TypedKind`) of typed arrays of shape `s`,
/// if they are: each kind has its own prototype.
/// What `tget_*_chk` / `tset_*_chk` compare an object's bytes 8 to 11
/// with, its flags (byte 9) masked off: class `TypedArray` and `kind`.
fn typed_class_kind(kind: u8) -> i32 {
    const _: () = assert!(crate::object::Class::TypedArray as u8 == 7);
    7 | (kind as i32) << 16
}

fn typed_kind(s: u64) -> Option<u8> {
    unsafe {
        let s = &*(s as *const Shape);
        if s.class != crate::object::Class::TypedArray {
            return None;
        }
        let vm = &*crate::interp::current_vm();
        (vm.typed_prototypes.iter())
            .position(|&p| crate::object::object_value(p) == s.proto)
            .map(|k| k as u8)
    }
}

/// The `tset_*` stencil's case storing an int32 (`int`) or a double into a
/// typed array of kind `kind`, if there is one.
fn typed_store(kind: Option<u8>, int: bool) -> Option<&'static str> {
    use crate::object::TypedKind as K;
    Some(match (K::from_u8(kind?), int) {
        (K::I8 | K::U8, true) => "b8_i",
        (K::I8 | K::U8, false) => "b8_f",
        (K::U8Clamped, true) => "c8_i",
        (K::U8Clamped, false) => return None,
        (K::I16 | K::U16, true) => "b16_i",
        (K::I16 | K::U16, false) => "b16_f",
        (K::I32 | K::U32, true) => "b32_i",
        (K::I32 | K::U32, false) => "b32_f",
        (K::F32, true) => "f32_i",
        (K::F32, false) => "f32_f",
        (K::F64, true) => "f64_i",
        (K::F64, false) => "f64_f",
    })
}

/// The checking variant (`_chk`) of a `tset_*` case.
fn typed_checked(case: &str) -> &'static str {
    match case {
        "b8_i" => "b8_i_chk",
        "b8_f" => "b8_f_chk",
        "c8_i" => "c8_i_chk",
        "b16_i" => "b16_i_chk",
        "b16_f" => "b16_f_chk",
        "b32_i" => "b32_i_chk",
        "b32_f" => "b32_f_chk",
        "f32_i" => "f32_i_chk",
        "f32_f" => "f32_f_chk",
        "f64_i" => "f64_i_chk",
        _ => "f64_f_chk",
    }
}

/// Whether shape `s` extends shape `b`: same class, prototype and
/// capacity, and `b`'s properties first, where `b` has them, as a shape
/// reached from `b` by adding properties or generalizing its elements has.
/// An object of shape `s` passes for one of `b` (`Ty::Ext(b)`).
unsafe fn extends(s: *const Shape, b: *const Shape) -> bool {
    let (s, b) = unsafe { (&*s, &*b) };
    s.class == b.class
        && s.proto == b.proto
        && s.capacity.get() == b.capacity.get()
        && s.keys.starts_with(&b.keys)
        && s.attrs.starts_with(&b.attrs)
        && s.fields.starts_with(&b.fields)
}

/// `select` for `GetNamed`, `SetNamed`, `LdaGlobal` and `StaGlobal`.
///
/// - A receiver whose shape is known (`Obj` or `Ext`) and has the property
///   needs no guard and no cache: the shape says where it is.
/// - Otherwise the inline cache does, with a guard unless the receiver's
///   exact shape is the cached one; a guard that passed makes it known.
///   A receiver known to have another exact shape would fail it: bail.
/// - A load is typed by the field's type. A store checks the value against
///   it, unless the value's type proves it fits (or proves it doesn't:
///   bail, and let the interpreter generalize the field).
/// - A transitioning store downgrades every `Obj` fact to `Ext`, as another
///   register may hold the same object.
fn select_property(insn: &Insn, i: usize, st: &TypeState, spec: &Spec) -> Sel {
    use Ty::{Any, F64, I32};
    let mut out = st.clone();
    let bail = |out: TypeState| Sel::new("bail", "", out);
    let reg = st.regs.get(insn.reg as usize).copied().unwrap_or(Any);
    let prop = spec.props.get(i).copied().unwrap_or(Prop::None);
    let load = matches!(insn.op, Op::GetNamed | Op::LdaGlobal);
    if let Prop::Const {
        value,
        at,
        expect,
        ft,
        ..
    } = prop
    {
        // A typed array keeps its kind: its shape, or one extending it,
        // says which (no `_chk` on its elements).
        out.acc = match Ty::of(Value::from_bits(value)) {
            Ty::Obj(s) if typed_kind(s).is_some() => Ty::Ext(s),
            _ => Ty::of_field(ft).0,
        };
        return Sel {
            ops: Some((0, value, 0)),
            check: (at != 0).then_some(("guard_word", at, expect)),
            ..Sel::new("lda_constant", "", out)
        };
    }
    // The check a store of `acc` into a field of type `ft` needs; `Err` if
    // `acc` can't fit.
    let check = |ft: FieldType| -> Result<Option<(&'static str, u64, u64)>, ()> {
        match (ft, st.acc) {
            (FieldType::None | FieldType::Any, _) => Ok(None),
            (FieldType::Int, I32) | (FieldType::Num, I32 | F64) => Ok(None),
            (FieldType::Int, F64) => Err(()),
            (FieldType::Int, _) => Ok(Some(("check_acc_int", 0, 0))),
            (FieldType::Num, _) => Ok(Some(("check_acc_num", 0, 0))),
            (FieldType::Obj(s), acc) if acc == Ty::Obj(s as u64) => Ok(None),
            (FieldType::Obj(_), I32 | F64) => Err(()),
            (FieldType::Obj(s), _) => Ok(Some(("check_acc_shape", s as u64, 0))),
        }
    };
    // Finish a selection: a load's result type, or a store's check.
    // A number moved from a field into another stays boxed: no unboxing
    // into `facc` and boxing back (DeltaBlue's `EqualityConstraint`
    // `execute`, `this.output().value = this.input().value`); the store
    // checks it is a number instead.
    let moved = spec.moved.get(i).copied().unwrap_or(false);
    let finish = |mut sel: Sel, ft: FieldType| -> Sel {
        if load && moved && ft == FieldType::Num {
            sel.out.acc = Ty::Any;
            sel
        } else if load {
            let (t, post) = Ty::of_field(ft);
            sel.out.acc = t;
            sel.post = post;
            sel
        } else {
            match check(ft) {
                Ok(c) => {
                    sel.check = c;
                    sel
                }
                Err(()) => bail(sel.out),
            }
        }
    };

    if matches!(insn.op, Op::GetNamed | Op::SetNamed) {
        if matches!(reg, I32 | F64) {
            return bail(out);
        }
        // An array's or string's length: no check if the receiver's shape
        // is one's.
        if prop == Prop::Length {
            let array = reg.shape().is_some_and(|s| {
                matches!(
                    unsafe { (*(s as *const Shape)).class },
                    crate::object::Class::Array
                        | crate::object::Class::String
                        | crate::object::Class::TypedArray
                )
            });
            out.acc = I32;
            return Sel::new("get_length", if array { "nc" } else { "" }, out);
        }
        // A known shape with the property. Not for a load from a
        // prototype's slot as cached (all its cases, if several): lowering
        // may take its value to be the one there (`proven_callee`, the
        // function an inlined call needs no guard for), which an own
        // property found here would shadow. The cache's path checks the
        // shape, or bails.
        let from_proto = match prop {
            Prop::Proto { .. } => true,
            Prop::Poly { n, at } => poly_cases(spec.polys, n, at)
                .iter()
                .all(|c| matches!(c.loc, CaseLoc::Proto(_))),
            _ => false,
        };
        if let (Some(s), Some(fbs)) = (reg.shape(), spec.feedback)
            && !(load && from_proto)
        {
            let fb = &fbs[insn.ic_frame()];
            let shape = unsafe { &*(s as *const Shape) };
            // (Not an accessor's slot: the runtime calls its functions.)
            if let Some(slot) = shape
                .lookup(fb.name(insn))
                .filter(|&k| shape.attrs[k as usize] & crate::object::ACCESSOR == 0)
            {
                // A store to a read-only property does nothing, one to a
                // prototype's (a unique shape's) must bump the epoch, and one
                // to a function's constant field may have to make it mutable
                // (see `Vm::store_field`): left to the interpreter. (The
                // global object isn't a register's.) Other objects' fields
                // stores make mutable, as caching them does.
                let cell = shape.field_cell(slot);
                let function = shape.class == crate::object::Class::Function;
                if !load
                    && (shape.attrs[slot as usize] & crate::object::READ_ONLY != 0
                        || shape.unique
                        || (function && cell.is_constant()))
                {
                    return bail(out);
                }
                if !load && cell.is_constant() {
                    unsafe { (*crate::interp::current_vm()).field_made_mutable(cell) };
                }
                let (oop, off) = split_location(shape.location(slot));
                if off <= MAX_SLOT_OFF {
                    let base = match (load, oop) {
                        (true, false) => "get_own",
                        (true, true) => "get_oop",
                        (false, false) => "set_own",
                        (false, true) => "set_oop",
                    };
                    let sel = Sel {
                        ops: Some((off, 0, 0)),
                        ..Sel::new(base, "nc", out)
                    };
                    return finish(sel, shape.field_type_relied(slot));
                }
            }
        }
        // Several shapes: one of them if the receiver's is known, else a
        // dispatch on the shape. A load's type is their fields' join; a
        // store's check, the same for all, goes before it.
        let collapsed = matches!((prop, reg), (Prop::Poly { .. }, Ty::Obj(_)));
        let prop = match (prop, reg) {
            (Prop::Poly { n, at }, Ty::Obj(s)) => poly_cases(spec.polys, n, at)
                .iter()
                .find(|c| c.admits(s))
                .map_or(Prop::None, |c| Case { shape: s, ..*c }.prop()),
            (Prop::Poly { n, at }, _) => {
                let cases = poly_cases(spec.polys, n, at);
                let ft = cases.iter().fold(FieldType::None, |a, c| a.join(c.ft));
                // Past the dispatch, the receiver has one of the shapes: if
                // they all extend one of them, it is known to extend that.
                if !matches!(reg, Ty::Ext(_))
                    && let Some(base) = cases.iter().map(|c| c.base()).find(|&b| {
                        cases.iter().all(|c| unsafe {
                            extends(c.base() as *const Shape, b as *const Shape)
                        })
                    })
                {
                    out.regs.set(insn.reg as usize, Ty::Ext(base));
                }
                if !load {
                    if cases.iter().any(|c| check(c.ft) != check(cases[0].ft)) {
                        return bail(out);
                    }
                    if cases
                        .iter()
                        .any(|c| matches!(c.loc, CaseLoc::Transition { .. }))
                    {
                        let ext = |t: Ty| match t {
                            Ty::Obj(x) => Ty::Ext(x),
                            t => t,
                        };
                        out.regs.map(|_, t| ext(t));
                        out.acc = ext(out.acc);
                    }
                    return finish(Sel::new("poly", "", out), cases[0].ft);
                }
                return finish(Sel::new("poly", "", out), ft);
            }
            (p, _) => p,
        };
        let shape = match prop {
            Prop::Own { shape, .. }
            | Prop::Oop { shape, .. }
            | Prop::Proto { shape, .. }
            | Prop::Transition { shape, .. } => shape,
            _ => return bail(out),
        };
        let stable = || {
            let s = unsafe { &*(shape as *const Shape) };
            spec.stable_ok && s.is_stable() && {
                s.relied.set(true);
                true
            }
        };
        let exact = reg == Ty::Obj(shape) || (reg == Ty::Ext(shape) && stable());
        if matches!(reg, Ty::Obj(_)) && !exact {
            return bail(out);
        }
        let r = insn.reg as usize;
        let case = if exact { "nc" } else { "" };
        out.regs.set(r, Ty::Obj(shape));
        let mut sel = match prop {
            Prop::Own { ft, .. } => finish(
                Sel::new(if load { "get_own" } else { "set_own" }, case, out),
                ft,
            ),
            Prop::Oop { ft, .. } => finish(
                Sel::new(if load { "get_oop" } else { "set_oop" }, case, out),
                ft,
            ),
            Prop::Proto { ft, .. } if load => finish(Sel::new("get_proto", case, out), ft),
            Prop::Transition { new_shape, ft, .. } if !load => {
                let ext = |t: Ty| match t {
                    Ty::Obj(x) => Ty::Ext(x),
                    t => t,
                };
                out.regs.map(|_, t| ext(t));
                out.acc = ext(out.acc);
                out.regs.set(r, Ty::Obj(new_shape));
                finish(Sel::new("set_tr", case, out), ft)
            }
            _ => bail(out),
        };
        // The instruction's cache holds the other cases too: the operands
        // are this one's.
        if collapsed && sel.base != "bail" {
            sel.ops = prop.operands();
        }
        sel
    } else {
        match prop {
            Prop::Global { oop, ft, .. } => {
                let base = match (load, oop) {
                    (true, false) => "lda_global",
                    (true, true) => "lda_global_oop",
                    (false, false) => "sta_global",
                    (false, true) => "sta_global_oop",
                };
                finish(Sel::new(base, "", out), ft)
            }
            _ => bail(out),
        }
    }
}

/// What the function's own `Return`s give (the join of `acc`'s types
/// there), from the in-states `ins`: `Bot` if none is reachable. With
/// `known`, `Return`s of `Any` are left out.
fn returned(insns: &[Insn], ins: &[Option<TypeState>], known: bool) -> Ty {
    (0..insns.len())
        .filter(|&i| insns[i].op == Op::Return && insns[i].frame == 0 && !insns[i].bail)
        .filter_map(|i| ins[i].as_ref())
        .map(|st| st.acc)
        .filter(|&t| !known || t != Ty::Any)
        .fold(Ty::Bot, Ty::join)
}

/// Forward dataflow from `entry` (instruction index) with `entry_state`.
/// Returns the joined in-state of every reachable instruction, and what
/// [`select`] picks there (each instruction's last visit had its final
/// in-state).
fn infer(
    insns: &[Insn],
    entry: usize,
    entry_state: TypeState,
    spec: &Spec,
) -> (Vec<Option<TypeState>>, Vec<Option<Sel>>) {
    let n = insns.len();
    let mut ins: Vec<Option<TypeState>> = vec![None; n];
    let mut sels: Vec<Option<Sel>> = (0..n).map(|_| None).collect();
    // Instructions on `work`, which holds each once, taken lowest first:
    // bytecode is laid out in about reverse postorder, so an instruction
    // mostly waits until its predecessors in the loop are done. Taken last
    // pushed first, zlib's 9,600-instruction `a1` took 180,000 visits (430
    // ms a compile), now 25,000 (70 ms).
    let mut queued = vec![false; n];
    ins[entry] = Some(entry_state);
    let mut work = std::collections::BinaryHeap::from([std::cmp::Reverse(entry)]);
    queued[entry] = true;
    while let Some(std::cmp::Reverse(i)) = work.pop() {
        queued[i] = false;
        let sel = select(&insns[i], i, ins[i].as_ref().unwrap(), spec);
        let succs = [
            insns[i].falls_through().then_some(i + 1),
            insns[i].jumps().then_some(insns[i].target),
        ];
        for s in succs.into_iter().flatten() {
            assert!(s < n, "control falls off the end of the bytecode");
            let mut out = sel.out.clone();
            if !spec.acc_live[s] {
                out.acc = Ty::Bot;
            }
            let live = &spec.reg_live[s];
            out.regs
                .map(|r, t| if rs_has(live, r) { t } else { Ty::Bot });
            if !out.same.is_empty() || out.acc_same.is_some() {
                leave_dead(&mut out, &spec.reg_live[s], spec.acc_live[s]);
            }
            let changed = match &mut ins[s] {
                Some(st) => st.join(&out),
                slot @ None => {
                    *slot = Some(out);
                    true
                }
            };
            if changed && !queued[s] {
                queued[s] = true;
                work.push(std::cmp::Reverse(s));
            }
        }
        sels[i] = Some(sel);
    }
    (ins, sels)
}

// ---- Register homes (milestone 3) ----

/// GPR slots `g0…` (boxed values) and FP slots `f0…` (unboxed doubles) in
/// the stencil signature; see `stencils/stencils.rs`.
#[cfg(target_arch = "x86_64")]
pub const GPR_SLOTS: usize = 6;
#[cfg(target_arch = "aarch64")]
pub const GPR_SLOTS: usize = 12;
pub const FP_SLOTS: usize = 7;

/// Where a VM register lives during a compiled region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Home {
    /// The register file in memory, boxed.
    Mem,
    /// GPR slot `k`, boxed.
    Gpr(u8),
    /// GPR slot `k`, an unboxed int32 (zero-extended). The register is only
    /// ever `I32` in the region.
    Int(u8),
    /// FP slot `k`, an unboxed double. The register is pinned as `F64`.
    Fpr(u8),
}

impl Home {
    fn loc(self) -> &'static str {
        const G: [&str; 16] = [
            "g0", "g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9", "g10", "g11", "g12", "g13",
            "g14", "g15",
        ];
        const U: [&str; 16] = [
            "u0", "u1", "u2", "u3", "u4", "u5", "u6", "u7", "u8", "u9", "u10", "u11", "u12", "u13",
            "u14", "u15",
        ];
        const F: [&str; 16] = [
            "f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
            "f14", "f15",
        ];
        match self {
            Home::Mem => "m",
            Home::Gpr(k) => G[k as usize],
            Home::Int(k) => U[k as usize],
            Home::Fpr(k) => F[k as usize],
        }
    }
}

/// A set of registers (they are `u8`s).
type RegSet = [u64; 4];

fn rs_has(s: &RegSet, r: usize) -> bool {
    s[r / 64] >> (r % 64) & 1 != 0
}

/// Equality, without a call to `memcmp` (what `==` on arrays becomes).
fn rs_eq(a: &RegSet, b: &RegSet) -> bool {
    (a[0] ^ b[0]) | (a[1] ^ b[1]) | (a[2] ^ b[2]) | (a[3] ^ b[3]) == 0
}

fn rs_add(s: &mut RegSet, r: usize) {
    s[r / 64] |= 1 << (r % 64);
}

fn rs_or(a: &mut RegSet, b: &RegSet) {
    a.iter_mut().zip(b).for_each(|(x, y)| *x |= y);
}

fn rs_of(l: &[bool]) -> RegSet {
    let mut s = [0; 4];
    l.iter()
        .enumerate()
        .filter(|e| *e.1)
        .for_each(|(r, _)| rs_add(&mut s, r));
    s
}

fn rs_iter(s: &RegSet) -> impl Iterator<Item = usize> + '_ {
    s.iter().enumerate().flat_map(|(k, &w)| {
        let mut w = w;
        std::iter::from_fn(move || {
            (w != 0).then(|| {
                let b = w.trailing_zeros() as usize;
                w &= w - 1;
                k * 64 + b
            })
        })
    })
}

/// Registers that can't share a slot (see [`assign_homes`]), from the
/// registers live into each instruction (exits included): both live into
/// one, or one written where the other is live after.
fn interference(insns: &[Insn], live: &[RegSet], nregs: usize) -> Vec<RegSet> {
    let mut m = vec![[0; 4]; nregs];
    for (i, here) in live.iter().enumerate() {
        for r in rs_iter(here).take_while(|&r| r < nregs) {
            rs_or(&mut m[r], here);
        }
        if insns[i].op == Op::Star {
            let mut after = [0; 4];
            for s in succs(insns, i).filter(|&s| s < live.len()) {
                rs_or(&mut after, &live[s]);
            }
            let w = insns[i].reg as usize;
            rs_or(&mut m[w], &after);
            for r in rs_iter(&after).take_while(|&r| r < nregs) {
                rs_add(&mut m[r], w);
            }
        }
    }
    m
}

/// Assign homes by static use count. Registers whose type is only ever
/// int32 or double (and at least once double) get FP slots; registers only
/// ever int32 get GPR slots unboxed (`Int`); other used registers get GPR
/// slots boxed; the rest stay in memory, as do registers from `mem_from`
/// (direct calls' frames). `no_fp` / `no_int` exclude registers from
/// pinning, as does `!pin_ok` (see [`pinnable`]).
///
/// With `interferes` ([`interference`]), registers share a slot unless they
/// interfere. Exits then write back only the registers live there.
#[allow(clippy::too_many_arguments)]
fn assign_homes(
    stats: &HomeStats,
    nregs: usize,
    no_fp: &[bool],
    no_int: &[bool],
    pin_ok: &[bool],
    mem_from: usize,
    interferes: Option<&[RegSet]>,
) -> Vec<Home> {
    let HomeStats { uses, join } = stats;
    let mut by_uses: Vec<usize> = (0..nregs.min(mem_from)).filter(|&r| uses[r] > 0).collect();
    by_uses.sort_by_key(|&r| (std::cmp::Reverse(uses[r]), r));
    let meets = |a: &RegSet, b: &RegSet| a.iter().zip(b).any(|(x, y)| x & y != 0);
    // Per slot, the registers in it.
    let mut fprs: Vec<RegSet> = vec![[0; 4]; FP_SLOTS];
    let mut gprs: Vec<RegSet> = vec![[0; 4]; GPR_SLOTS];
    let free = |slots: &[RegSet], r: usize| {
        slots.iter().position(|s| match interferes {
            Some(m) => !meets(s, &m[r]),
            None => rs_eq(s, &[0; 4]),
        })
    };
    let mut homes = vec![Home::Mem; nregs];
    for &r in &by_uses {
        if join[r] == 2
            && !no_fp[r]
            && pin_ok[r]
            && let Some(k) = free(&fprs, r)
        {
            homes[r] = Home::Fpr(k as u8);
            rs_add(&mut fprs[k], r);
        }
    }
    for &r in &by_uses {
        if homes[r] == Home::Mem
            && let Some(k) = free(&gprs, r)
        {
            homes[r] = if join[r] == 1 && !no_int[r] && pin_ok[r] {
                Home::Int(k as u8)
            } else {
                Home::Gpr(k as u8)
            };
            rs_add(&mut gprs[k], r);
        }
    }
    homes
}

/// What [`assign_homes`] chooses from: per register, its uses (weighted)
/// and the join of its types (`Bot` < `I32` < `F64` < anything else).
struct HomeStats {
    uses: Vec<usize>,
    join: Vec<u8>,
}

/// [`HomeStats`] for `insns` with the types `ins`. Uses inside loops count
/// 8 times per level (a loop is the code from a backward jump's target to
/// it).
fn home_stats(insns: &[Insn], ins: &[Option<TypeState>], nregs: usize) -> HomeStats {
    let mut depth = vec![0u32; insns.len() + 1];
    for (j, insn) in insns.iter().enumerate() {
        if insn.jumps() && insn.target <= j {
            depth[insn.target] += 1;
            depth[j + 1] -= 1;
        }
    }
    let mut d = 0u32;
    let weight: Vec<usize> = depth[..insns.len()]
        .iter()
        .map(|&x| {
            d = d.wrapping_add(x);
            8usize.pow(d.min(4))
        })
        .collect();
    let mut uses = vec![0usize; nregs];
    // As far as homes care: no shapes joined.
    let mut join = vec![0u8; nregs];
    // (States mostly share their registers' types with the one before:
    // each list once.)
    let mut last: *const Vec<(u8, Ty)> = std::ptr::null();
    for ((insn, st), w) in insns.iter().zip(ins).zip(&weight) {
        let Some(st) = st else { continue };
        if insn.reads_reg() {
            uses[insn.reg as usize] += w;
        }
        if Rc::as_ptr(&st.regs.tys) == last {
            continue;
        }
        last = Rc::as_ptr(&st.regs.tys);
        for (r, t) in st.regs.iter() {
            let j = &mut join[r];
            let k = match t {
                Ty::Bot => 0,
                Ty::I32 => 1,
                Ty::F64 => 2,
                _ => 3,
            };
            *j = (*j).max(k);
        }
    }
    HomeStats { uses, join }
}

/// The shift `s` and multiplier `m` with which `mod_smi_{u,i}_s<s>`
/// compute `a mod d` for 0 <= a < 2^31 and d >= 3 not a power of two: s =
/// 31 + ⌈log2 d⌉, m = ⌈2^s / d⌉ < 2^32, and q = ⌊a·m / 2^s⌋ = ⌊a / d⌋
/// (Granlund & Montgomery). With e = m·d - 2^s < d <= 2^(s-31), a·e <
/// 2^s, so a·m / 2^s exceeds a / d by less than 1/d, short of the next
/// integer.
fn mod_magic(d: i32) -> (u32, u64) {
    debug_assert!(d >= 3 && d & (d - 1) != 0);
    let s = 31 + (32 - (d as u32 - 1).leading_zeros());
    (s, (1u64 << s).div_ceil(d as u64))
}

/// Per instruction, whether `acc` is never a negative int32 there: it
/// holds a non-negative int32, or no int32 at all. From registers that
/// never do (all their stores store such values), for registers whose
/// values don't come from outside (not live at the start: parameters,
/// `this`, or the caller's registers outside a VM): constants, `Inc` and
/// adds of a non-negative constant (an int32 overflow leaves int32, it
/// doesn't wrap, but for the wrapping adds `wraps` marks, which only feed
/// a bitwise operation), remainders of such, masks with a non-negative
/// constant, booleans and the other non-numbers. Within straight-line
/// code: unknown at jump targets. For `ModSmi`, whose `_u` forms need no
/// sign handling; empty work if there is none.
fn nonneg_acc(
    insns: &[Insn],
    is_target: &[bool],
    nregs: usize,
    frames: &[FrameRegs],
    live_at_start: Option<&RegSet>,
) -> Vec<bool> {
    let n = insns.len();
    if !insns.iter().any(|i| i.op == Op::ModSmi && !i.bail) {
        return vec![false; n];
    }
    let start = match live_at_start {
        Some(l) => *l,
        None => reg_live_in(insns, &reachable(insns, 0), nregs, frames).swap_remove(0),
    };
    let mut ok: Vec<bool> = (0..nregs).map(|r| !rs_has(&start, r)).collect();
    loop {
        let mut acc = vec![false; n];
        for j in 1..n {
            let p = &insns[j - 1];
            if is_target[j] || !p.falls_through() {
                continue;
            }
            acc[j] = match p.op {
                Op::LdaSmi => p.imm >= 0,
                Op::LdaUndefined
                | Op::LdaNull
                | Op::LdaTrue
                | Op::LdaFalse
                | Op::TestLessThan
                | Op::TestEqual
                | Op::TestEqualSmi
                | Op::TestGreaterThan
                | Op::TestLessThanOrEqual
                | Op::TestGreaterThanOrEqual
                | Op::TestStrictEqual
                | Op::TestUndefined
                | Op::TestNull
                | Op::TestNullish => true,
                Op::Ldar => ok[p.reg as usize],
                Op::Inc => acc[j - 1],
                Op::AddSmi => p.imm >= 0 && acc[j - 1],
                Op::ModSmi => p.imm > 0 && acc[j - 1],
                Op::BitAndSmi => p.imm >= 0,
                // `acc` unchanged.
                Op::Star
                | Op::Nop
                | Op::Mov
                | Op::JumpIfTrue
                | Op::JumpIfFalse
                | Op::JumpIfToBooleanTrue
                | Op::JumpIfToBooleanFalse => acc[j - 1],
                op if is_compare_jump(op) => acc[j - 1],
                _ => false,
            };
        }
        let mut changed = false;
        for (j, insn) in insns.iter().enumerate() {
            let (dst, fine) = match insn.op {
                Op::Star => (insn.reg as usize, acc[j]),
                Op::Mov => (insn.reg2 as usize, ok[insn.reg as usize]),
                _ => continue,
            };
            if ok[dst] && !fine {
                ok[dst] = false;
                changed = true;
            }
        }
        if !changed {
            return acc;
        }
    }
}

/// Instructions reachable from `entry`.
fn reachable(insns: &[Insn], entry: usize) -> Vec<bool> {
    let mut seen = vec![false; insns.len()];
    let mut work = vec![entry];
    while let Some(i) = work.pop() {
        if i >= insns.len() || std::mem::replace(&mut seen[i], true) {
            continue;
        }
        if insns[i].falls_through() {
            work.push(i + 1);
        }
        if insns[i].jumps() {
            work.push(insns[i].target);
        }
    }
    seen
}

fn succs(insns: &[Insn], i: usize) -> impl Iterator<Item = usize> {
    let insn = &insns[i];
    [
        insn.falls_through().then_some(i + 1),
        insn.jumps().then_some(insn.target),
    ]
    .into_iter()
    .flatten()
}

/// Registers live into each reachable instruction (backward dataflow):
/// instructions with a register operand read it, except `star`, which
/// writes it.
/// A frame's (first register, register count, parent frame).
type FrameRegs = (usize, usize, usize);

/// The registers the interpreter may read after an exit from `frame`: its
/// own and its callers' (`frames` empty: one frame with all of them).
/// Other frames' registers, and the headers between frames, aren't
/// registers at the exit (they may hold anything).
fn exit_live(frames: &[FrameRegs], frame: u8, nregs: usize) -> Vec<bool> {
    let mut live = vec![false; nregs];
    if frames.is_empty() {
        live.iter_mut().for_each(|l| *l = true);
        return live;
    }
    let mut f = frame as usize;
    loop {
        let (base, n, parent) = frames[f];
        for l in live.iter_mut().skip(base).take(n) {
            *l = true;
        }
        if f == 0 {
            return live;
        }
        f = parent;
    }
}

/// Per instruction, the registers live into it (backward dataflow, over
/// the reachable instructions): read before being written on some path,
/// by an instruction or by the interpreter after a bail.
fn reg_live_in(insns: &[Insn], reach: &[bool], nregs: usize, frames: &[FrameRegs]) -> Vec<RegSet> {
    reg_live_in_mats(insns, reach, nregs, frames, None, true)
}

/// [`reg_live_in`] without the registers only a bail keeps live (every
/// register of its frames, which the interpreter may or may not read): the
/// registers compiled code reads, which hold values. One a bail alone keeps
/// live may hold anything (a call-window temporary not yet written in this
/// frame holds whatever the stack held: a stale frame header, say).
fn reg_read_in(insns: &[Insn], reach: &[bool], nregs: usize) -> Vec<RegSet> {
    reg_live_in_mats(insns, reach, nregs, &[], None, false)
}

/// [`reg_live_in`], with the fields of the objects exits allocate (`mats`,
/// see [`scalar_replace`]) read where they are allocated; with
/// `bail_reads` false, a bail reads no register.
fn reg_live_in_mats(
    insns: &[Insn],
    reach: &[bool],
    nregs: usize,
    frames: &[FrameRegs],
    mats: Option<&HashMap<usize, Vec<Rc<Mat>>>>,
    bail_reads: bool,
) -> Vec<RegSet> {
    let n = insns.len();
    // Per instruction, the registers it reads and those it writes, and
    // whether it bails (leaving every register of its frames live: see
    // below).
    let mut exit_sets: HashMap<u8, RegSet> = HashMap::default();
    let effects: Vec<(RegSet, RegSet)> = insns
        .iter()
        .map(|insn| {
            let (mut reads, mut writes) = ([0; 4], [0; 4]);
            if insn.op == Op::Star {
                rs_add(&mut writes, insn.reg as usize);
            } else if insn.op == Op::Mov {
                rs_add(&mut writes, insn.reg2 as usize);
                rs_add(&mut reads, insn.reg as usize);
            } else if insn.uses_reg_value() {
                rs_add(&mut reads, insn.reg as usize);
            }
            if insn.is_call() && !insn.bail {
                for r in insn.call_reads() {
                    rs_add(&mut reads, r);
                }
            }
            if insn.op == Op::SetKeyed && !insn.bail {
                rs_add(&mut reads, insn.reg2 as usize);
            }
            // The interpreter runs a bailed instruction and the rest of the
            // function, which may read any register: each must hold its
            // value there, so a register with a store of a non-int32 isn't
            // pinned as an int32 (which would truncate it).
            if insn.bail && !bail_reads {
                writes = [!0; 4];
            } else if insn.bail {
                let e = *exit_sets
                    .entry(insn.frame)
                    .or_insert_with(|| rs_of(&exit_live(frames, insn.frame, nregs)));
                // Live: all of them (the successors' don't matter).
                writes = [!0; 4];
                rs_or(&mut reads, &e);
            }
            (reads, writes)
        })
        .collect();
    let mut effects = effects;
    for (&at, ms) in mats.into_iter().flatten() {
        for &f in ms.iter().flat_map(|m| &m.fields) {
            rs_add(&mut effects[at].0, f as usize);
        }
    }
    let mut live_in: Vec<RegSet> = vec![[0; 4]; n];
    let mut changed = true;
    while changed {
        changed = false;
        for i in (0..n).rev().filter(|&i| reach[i]) {
            let mut live: RegSet = [0; 4];
            for s in succs(insns, i).filter(|&s| s < n) {
                rs_or(&mut live, &live_in[s]);
            }
            let (reads, writes) = &effects[i];
            for (l, (r, w)) in live.iter_mut().zip(reads.iter().zip(writes)) {
                *l = (*l & !w) | r;
            }
            if !rs_eq(&live, &live_in[i]) {
                live_in[i] = live;
                changed = true;
            }
        }
    }
    live_in
}

/// Registers that may be pinned (kept unboxed in a slot). A pinned register
/// can't hold an arbitrary value, so one that isn't live at entry (and so
/// isn't loaded) must be written on every path before any point where the
/// slots are spilled: a possible int32 overflow exit, or `return`.
/// Otherwise the spill would overwrite its untouched value in memory.
fn pinnable(
    insns: &[Insn],
    entry: usize,
    reach: &[bool],
    live_at_entry: &[bool],
    ins: &[Option<TypeState>],
    widened: &HashSet<usize>,
) -> Vec<bool> {
    let n = insns.len();
    let nregs = live_at_entry.len();
    // Definitely assigned on entry to each instruction (forward, must).
    let mut assigned: Vec<Option<RegSet>> = vec![None; n];
    let mut queued = vec![false; n];
    let mut at_entry = [0; 4];
    for r in (0..nregs).filter(|&r| live_at_entry[r]) {
        rs_add(&mut at_entry, r);
    }
    assigned[entry] = Some(at_entry);
    let mut work = vec![entry];
    queued[entry] = true;
    while let Some(i) = work.pop() {
        queued[i] = false;
        let mut out = assigned[i].unwrap();
        if insns[i].op == Op::Star {
            rs_add(&mut out, insns[i].reg as usize);
        }
        for s in succs(insns, i).filter(|&s| s < n) {
            let changed = match &mut assigned[s] {
                Some(a) => {
                    let mut meet = *a;
                    for (x, o) in meet.iter_mut().zip(&out) {
                        *x &= o;
                    }
                    let changed = !rs_eq(&meet, a);
                    *a = meet;
                    changed
                }
                slot @ None => {
                    *slot = Some(out);
                    true
                }
            };
            if changed && !queued[s] {
                queued[s] = true;
                work.push(s);
            }
        }
    }
    let may_spill = |i: usize| {
        let Some(st) = &ins[i] else { return false };
        let reg = st.regs.get(insns[i].reg as usize).copied();
        if insns[i].bail || insns[i].is_property() {
            return true;
        }
        match insns[i].op {
            Op::Return => true,
            Op::AddSmi | Op::Inc => st.acc == Ty::I32 && !widened.contains(&i),
            Op::ModSmi => st.acc == Ty::I32 && insns[i].imm > 0 && !widened.contains(&i),
            Op::Add => st.acc == Ty::I32 && reg == Some(Ty::I32) && !widened.contains(&i),
            _ => false,
        }
    };
    // Assigned at every spill point (an unreached one: nothing).
    let mut everywhere = [u64::MAX; 4];
    for i in (0..n).filter(|&i| reach[i] && may_spill(i)) {
        let a = assigned[i].unwrap_or([0; 4]);
        for (x, o) in everywhere.iter_mut().zip(&a) {
            *x &= o;
        }
    }
    (0..nregs)
        .map(|r| live_at_entry[r] || rs_has(&everywhere, r))
        .collect()
}

// ---- Layout and patching ----

/// Where a stencil's `toi_continue` / `toi_jump_target` goes.
#[derive(Clone, Copy)]
enum Target {
    /// Whatever is laid out right after this piece (lets the jump be trimmed).
    Next,
    /// The first piece of instruction `i`.
    Label(u32),
    /// Edge stub `n` (conversions, then a jump to its target).
    Stub(u32),
    /// Hot piece `k`.
    Piece(u32),
    /// Whatever is laid out right after hot piece `k`.
    AfterPiece(u32),
}

/// One stencil instance to emit.
struct Piece {
    stencil: Instance,
    /// Its `__slow` helper, if any (copied into the cold area).
    slow: Option<Instance>,
    imm: i32,
    /// The 64-bit immediates, for stencils that have them.
    imm64: u64,
    imm64b: u64,
    reg: u8,
    cont: Target,
    jump: Target,
    /// Instruction whose exit stub `toi_exit` goes to.
    exit: u32,
    /// Machine code of its own instead of the stencil's (`arch::direct_call`):
    /// 1 + its index in the region's list, or 0.
    raw: u32,
    raw_len: u32,
}

impl Piece {
    /// The stencil `name` with immediate `imm`. On arm64, use its `_k16`
    /// variant (one `movz` instead of `movz` + `movk`) when it has one and
    /// the immediate fits.
    fn with_imm(name: &str, imm: i32) -> Piece {
        if cfg!(target_arch = "aarch64") && (0..=0xFFFF).contains(&imm) {
            let k16 = format!("{name}_k16");
            if has_stencil(&k16) {
                return Piece {
                    imm,
                    ..Piece::new(&k16, 0)
                };
            }
        }
        Piece {
            imm,
            ..Piece::new(name, 0)
        }
    }

    fn new(name: &str, reg: u8) -> Piece {
        let stencil = Instance::get(name);
        let slow = (stencil.stencil().slow).then(|| match stencil.stencil().slow_at {
            0 => Instance::get(&format!("{name}__slow")),
            k => Instance {
                at: k - 1,
                regs: stencil.regs,
            },
        });
        Piece {
            slow,
            ..Piece::of(stencil, reg)
        }
    }

    /// The instance `stencil`, without its `__slow` helper (spills have
    /// none). On arm64, a family template's name isn't the instance's.
    fn of(stencil: Instance, reg: u8) -> Piece {
        Piece {
            stencil,
            slow: None,
            imm: 0,
            imm64: 0,
            imm64b: 0,
            reg,
            cont: Target::Next,
            jump: Target::Next,
            exit: u32::MAX,
            raw: 0,
            raw_len: 0,
        }
    }

    fn trims(&self) -> bool {
        if self.raw != 0 {
            return false;
        }
        let s = self.stencil.stencil();
        matches!(self.cont, Target::Next) && arch::ends_with_continue(s.code, s.relocs)
    }

    fn len(&self) -> usize {
        if self.raw != 0 {
            return self.raw_len as usize;
        }
        self.stencil.stencil().code.len() - if self.trims() { arch::TRAILING_JUMP } else { 0 }
    }

    fn uses(&self, symbol: Sym) -> bool {
        self.stencil.stencil().relocs.iter().any(|r| r.2 == symbol)
    }
}

/// Make the last piece of a sequence continue at `t` instead of falling
/// through.
fn ending_at(mut pieces: Vec<Piece>, t: Target) -> Vec<Piece> {
    if let Some(last) = pieces.last_mut() {
        last.cont = t;
    }
    pieces
}

/// Representation changes on an edge from `from` to `to` (types only
/// widen). The accumulator moves between `acc` (boxed) and `facc`
/// (unboxed double); a register in memory or a GPR slot converts its boxed
/// int32 to a boxed double. Pinned registers are always doubles.
fn edge_conversions(from: &TypeState, to: &TypeState, homes: &[Home]) -> Vec<Piece> {
    let mut out = Vec::new();
    match (from.acc, to.acc) {
        (Ty::I32, Ty::F64) => out.push(Piece::new("conv_acc_f", 0)),
        (Ty::F64, Ty::Any) => out.push(Piece::new("box_facc", 0)),
        (Ty::I32, Ty::Any) => out.push(Piece::new("box_iacc", 0)),
        _ => {}
    }
    for (r, b) in to.regs.iter() {
        let a = from.regs[r];
        if a == Ty::I32 && b == Ty::F64 && matches!(homes[r], Home::Mem | Home::Gpr(_)) {
            out.push(Piece::new(&format!("conv_{}_f", homes[r].loc()), r as u8));
        }
    }
    out
}

/// Entry: load cached registers from the register file (unboxing pinned
/// ones), then convert to the entry instruction's in-state. `actual` is the
/// exact entry state (`acc` arrives boxed); `actual_live` has dead
/// registers as `Bot`. Pinned registers that aren't live at entry aren't
/// loaded (see [`pinnable`]); boxed ones are if `exact`, so spills keep
/// the register file exact. In a VM frame that doesn't matter: a register
/// dead at entry is written before it is read, so it is dead wherever it
/// still holds what it held at entry, and spilling garbage there is
/// harmless.
fn entry_pieces(
    actual: &TypeState,
    actual_live: &TypeState,
    to: &TypeState,
    homes: &[Home],
    live_at_entry: &[bool],
    exact: bool,
) -> Vec<Piece> {
    let mut out = Vec::new();
    for (r, &h) in homes.iter().enumerate() {
        match h {
            Home::Mem => {}
            Home::Gpr(k) => {
                if exact || live_at_entry[r] {
                    out.push(Piece::new(&format!("enter_g{k}"), r as u8));
                }
            }
            _ if !live_at_entry[r] => {}
            Home::Int(k) => {
                assert_eq!(actual.regs[r], Ty::I32, "r{r} pinned as int32");
                out.push(Piece::new(&format!("enter_u{k}"), r as u8));
            }
            Home::Fpr(k) => {
                let kind = match actual.regs[r] {
                    Ty::I32 => "i",
                    Ty::F64 => "f",
                    t => panic!("register r{r} pinned as double but enters as {t:?}"),
                };
                out.push(Piece::new(&format!("enter_f{k}_{kind}"), r as u8));
            }
        }
    }
    match (actual.acc, to.acc) {
        (Ty::F64, Ty::F64) => out.push(Piece::new("unbox_facc", 0)),
        (Ty::I32, Ty::F64) => out.push(Piece::new("conv_acc_f", 0)),
        _ => {}
    }
    let mut regs_only = actual_live.clone();
    regs_only.acc = to.acc;
    out.extend(edge_conversions(&regs_only, to, homes));
    out
}

/// Write every cached register back to the register file, boxed.
fn spills(homes: &[Home], boxed: &[bool]) -> Vec<Piece> {
    homes
        .iter()
        .enumerate()
        .filter_map(|(r, &h)| spill_reg_piece(h, r as u8, boxed))
        .collect()
}

/// `spill_piece`, but for a register whose int32 slot may hold a value
/// compiled code doesn't read, boxed (`boxed`, see `star_u<k>_v`):
/// `spill_u<k>_v`.
fn spill_reg_piece(home: Home, r: u8, boxed: &[bool]) -> Option<Piece> {
    static SPILLS: std::sync::OnceLock<Vec<Instance>> = std::sync::OnceLock::new();
    match home {
        Home::Int(k) if boxed.get(r as usize).copied().unwrap_or(false) => {
            let table = SPILLS.get_or_init(|| {
                (0..GPR_SLOTS).map(|k| Instance::get(&format!("spill_u{k}_v"))).collect()
            });
            Some(Piece::of(table[k as usize], r))
        }
        _ => spill_piece(home, r),
    }
}

/// The stencil that writes a slot back to memory (`spill_g3`), if `home`
/// is one.
fn spill_name(home: Home) -> Option<String> {
    match home {
        Home::Mem => None,
        Home::Gpr(k) => Some(format!("spill_g{k}")),
        Home::Int(k) => Some(format!("spill_u{k}")),
        Home::Fpr(k) => Some(format!("spill_f{k}")),
    }
}

/// The piece that writes register `r`'s slot back to memory (`spill_g3`),
/// if it has one. (The stencils are looked up once: exits write back
/// every slot, many times a compilation.)
/// A [`stencil_for`] piece's exit before it's known: the instruction's.
const KEY_EXIT: u32 = u32::MAX - 1;

/// The pieces putting a keyed store's key, register `k`, into r11 / x15
/// (`key_*`, which exits if it isn't an int32), where the store reads it.
/// A double in an FP slot goes through memory (and exits there).
fn key_pieces(homes: &[Home], k: u8) -> Vec<Piece> {
    let mut v = Vec::new();
    let key = match homes[k as usize] {
        Home::Mem => Piece::new("key_m", k),
        Home::Gpr(j) => Piece::new(&format!("key_g{j}"), k),
        Home::Int(j) => Piece::new(&format!("key_u{j}"), k),
        h @ Home::Fpr(_) => {
            v.extend(spill_piece(h, k));
            Piece::new("key_m", k)
        }
    };
    v.push(Piece {
        exit: KEY_EXIT,
        ..key
    });
    v
}

fn spill_piece(home: Home, r: u8) -> Option<Piece> {
    static SPILLS: std::sync::OnceLock<Vec<Option<Instance>>> = std::sync::OnceLock::new();
    let table = SPILLS.get_or_init(|| {
        let at = |kind: &str, k: usize| Instance::find(&format!("spill_{kind}{k}"));
        let some = |n: usize, kind: &'static str| {
            (0..16).map(move |k| if k < n { at(kind, k) } else { None })
        };
        some(GPR_SLOTS, "g")
            .chain(some(GPR_SLOTS, "u"))
            .chain(some(FP_SLOTS, "f"))
            .collect()
    });
    let at = match home {
        Home::Mem => return None,
        Home::Gpr(k) => k as usize,
        Home::Int(k) => 16 + k as usize,
        Home::Fpr(k) => 32 + k as usize,
    };
    let stencil = table[at].unwrap_or_else(|| panic!("no spill stencil for {home:?}"));
    Some(Piece::of(stencil, r))
}

/// The piece that boxes an accumulator of type `acc`, if it isn't boxed:
/// `box_facc` for a double in `facc`, `box_iacc` for an int32 (whose high
/// bits may be 0 instead of the tag).
fn box_acc_piece(acc: Ty) -> Option<Piece> {
    match acc {
        Ty::F64 => Some(Piece::new("box_facc", 0)),
        Ty::I32 => Some(Piece::new("box_iacc", 0)),
        _ => None,
    }
}

/// The strings concatenated (stencil names: cheaper than `format!`).
macro_rules! cat {
    ($($s:expr),+ $(,)?) => {{
        let mut s = String::with_capacity(32);
        $(s.push_str($s);)+
        s
    }};
}

/// The stencil name for instruction `insn`, and pieces to run before it.
/// With `vm`, the code runs in a VM frame, which is dead once it returns,
/// but for `this`: a constructor's result may be it (`construct_result`).
/// `return_spills`: how many registers, from `r0`, `return` writes back.
fn stencil_for(
    insn: &Insn,
    sel: &Sel,
    st: &TypeState,
    homes: &[Home],
    boxed: &[bool],
    return_spills: usize,
) -> (Vec<Piece>, String) {
    let mut pre = Vec::new();
    let home = homes.get(insn.reg as usize).copied().unwrap_or(Home::Mem);
    let loc = home.loc();
    // Generic stencils (and `return`) take a boxed acc.
    let box_acc = |pre: &mut Vec<Piece>| pre.extend(box_acc_piece(st.acc));
    let name = match (sel.base, home) {
        ("ldar", Home::Fpr(k)) => format!("ldar_f{k}"),
        ("ldar", _) if sel.case == "f" => cat!("ldar_", loc, "_f"),
        ("ldar", _) => cat!("ldar_", loc),
        ("star", Home::Fpr(k)) if sel.case == "f" => format!("star_f{k}"),
        ("star", Home::Fpr(k)) => format!("star_i_f{k}"),
        ("star", _) if sel.case == "f" => cat!("star_", loc, "_f"),
        ("star", Home::Mem | Home::Gpr(_)) if sel.case == "i" => cat!("star_", loc, "_i"),
        // Into an int32 slot, a value compiled code never reads (it would
        // be int32 there) but the interpreter may, after an exit: kept
        // boxed, which `spill_u` writes back as it is.
        ("star", Home::Int(k)) if sel.case != "i" => {
            box_acc(&mut pre);
            format!("star_u{k}_v")
        }
        ("star", _) => cat!("star_", loc),
        ("check_callee", _) => cat!("check_callee_", loc),
        ("callee_jne", _) => cat!("callee_jne_", loc),
        (
            "guard_cell" | "shape_jne" | "shape_guard" | "family_jne" | "family_guard"
            | "check_shape",
            _,
        ) => {
            cat!(sel.base, "_", loc)
        }
        ("check_tail_callee", _) => cat!("check_tail_callee_", loc),
        // The callee reads its arguments from memory.
        // The receiver in its slot; a boxed argument unless `_i`.
        ("array_push" | "array_pop", _) => {
            let at = homes
                .get(slot_reg(insn, sel, homes) as usize)
                .copied()
                .unwrap_or(Home::Mem)
                .loc();
            if sel.base == "array_push" && !sel.case.starts_with('i') {
                box_acc(&mut pre);
            }
            if sel.case.is_empty() {
                cat!(sel.base, "_", at)
            } else {
                cat!(sel.base, "_", sel.case, "_", at)
            }
        }
        ("call" | "call_direct" | "construct_direct", _) => {
            for r in insn.call_reads() {
                pre.extend(spill_piece(homes[r], r as u8));
            }
            if sel.case == "nc" {
                cat!(sel.base, "_nc")
            } else {
                sel.base.into()
            }
        }
        ("get_own" | "get_oop" | "get_proto" | "get_length", _) if sel.case == "nc" => {
            cat!(sel.base, "_nc_", loc)
        }
        ("get_own" | "get_oop" | "get_proto" | "get_length", _) => cat!(sel.base, "_", loc),
        // Stores take a boxed `acc`; a number's (`_n`) needs no barrier.
        ("set_own" | "set_oop" | "set_tr", _) => {
            box_acc(&mut pre);
            let n = if matches!(st.acc, Ty::I32 | Ty::F64) { "_n" } else { "" };
            if sel.case == "nc" {
                cat!(sel.base, n, "_nc_", loc)
            } else {
                cat!(sel.base, n, "_", loc)
            }
        }
        ("sta_global" | "sta_global_oop", _) => {
            box_acc(&mut pre);
            sel.base.into()
        }
        ("set_keyed", _) => {
            if !sel.case.ends_with('i') {
                box_acc(&mut pre);
            }
            let k = insn.reg2;
            if sel.case.is_empty() {
                pre.extend(spill_piece(homes[k as usize], k));
            } else {
                pre.extend(key_pieces(homes, k));
            }
            if sel.case.is_empty() {
                cat!("set_keyed_", loc)
            } else {
                cat!("set_keyed_", sel.case, "_", loc)
            }
        }
        ("get_keyed_rt", _) => {
            box_acc(&mut pre);
            cat!("get_keyed_rt_", loc)
        }
        ("tget", _) => cat!("tget_", sel.case, "_", loc),
        // The key into r11 / x15 (`key_*`), where `tset` reads it.
        ("tset", _) => {
            pre.extend(key_pieces(homes, insn.reg2));
            cat!("tset_", sel.case, "_", loc)
        }
        ("set_keyed_rt", _) => {
            if sel.case.is_empty() {
                box_acc(&mut pre);
            }
            let k = insn.reg2;
            pre.extend(spill_piece(homes[k as usize], k));
            if sel.case.is_empty() {
                cat!("set_keyed_rt_", loc)
            } else {
                cat!("set_keyed_rt_", sel.case, "_", loc)
            }
        }
        ("get_keyed", _) if sel.case.starts_with('i') => cat!("get_keyed_", sel.case, "_", loc),
        ("get_keyed", _) => {
            box_acc(&mut pre);
            if sel.case.is_empty() {
                cat!("get_keyed_", loc)
            } else {
                cat!("get_keyed_", sel.case, "_", loc)
            }
        }
        // `mod`'s stencils only exist on memory, typed ones too (a
        // division costs more than the slot's write-back).
        ("mod", _) => {
            if sel.case.is_empty() {
                box_acc(&mut pre);
            }
            pre.extend(spill_piece(home, insn.reg));
            if sel.case.is_empty() { "mod_m".into() } else { cat!("mod_", sel.case, "_m") }
        }
        // Group 2's generic stencils only exist on memory: a slot is
        // written back first (they are the slow, untyped paths).
        (
            "sub" | "mul" | "div" | "jrel" | "test_rel" | "test_seq" | "test_instanceof" | "test_in"
            | "bit_and" | "bit_or" | "bit_xor" | "shl" | "sar" | "shr",
            _,
        ) if sel.case.is_empty() => {
            box_acc(&mut pre);
            pre.extend(spill_piece(home, insn.reg));
            cat!(sel.base, "_m")
        }
        ("add" | "jlt" | "test_lt" | "test_eq", _) if sel.case.is_empty() => {
            box_acc(&mut pre);
            cat!(sel.base, "_", loc)
        }
        (
            "add" | "sub" | "mul" | "div" | "jlt" | "test_lt" | "test_eq" | "tlt_jt" | "tlt_jf"
            | "jge" | "test_gt" | "test_le" | "test_ge" | "bit_and" | "bit_or" | "bit_xor" | "shl"
            | "sar" | "shr",
            _,
        ) => cat!(sel.base, "_", sel.case, "_", loc),
        ("test_id", _) => cat!("test_id_", loc),
        // Up to 8 slots, the count is part of the stencil.
        ("create_object", _) if sel.ops.unwrap().0 <= 8 => {
            format!("create_object_{}", sel.ops.unwrap().0)
        }
        // The count is part of the stencil: `sar_smi_i_3`.
        ("shl_smi" | "sar_smi" | "shr_smi", _) if sel.case == "i" => {
            format!("{}_i_{}", sel.base, insn.imm & 31)
        }
        ("return", _) => {
            box_acc(&mut pre);
            pre.extend(spills(&homes[..return_spills.min(homes.len())], boxed));
            "return".into()
        }
        // Generic stencils that read `acc` take it boxed.
        (base, _) if sel.case.is_empty() => {
            if !matches!(
                base,
                "nop"
                    | "jump"
                    | "lda_smi"
                    | "lda_true"
                    | "lda_false"
                    | "lda_undefined"
                    | "lda_null"
                    | "lda_global"
                    | "lda_global_oop"
                    | "create_array"
                    | "create_object"
                    | "lda_constant"
                    | "lda_upvalue"
                    | "lda_upvalue_c"
                    | "bail"
            ) {
                box_acc(&mut pre);
            }
            base.into()
        }
        // The shift of the divisor's magic number is part of the stencil.
        ("mod_smi", _) if matches!(sel.case, "u" | "i") => {
            format!("mod_smi_{}_s{}", sel.case, mod_magic(insn.imm).0)
        }
        (base, _) => cat!(base, "_", sel.case),
    };
    (pre, name)
}

// ---- Fused stencils ----

/// Whether the accumulator is live after each instruction (backward
/// dataflow over reachable instructions). An instruction reads `acc` unless
/// it is a load (`ldar`, `lda_smi`, `lda_true`, …), which overwrites it, or
/// `nop` / `jump` / `mov`, which pass it through.
fn acc_live_out(insns: &[Insn], ins: &[Option<TypeState>]) -> Vec<bool> {
    let reach: Vec<bool> = ins.iter().map(Option::is_some).collect();
    acc_liveness(insns, &reach).1
}

/// Accumulator liveness into and out of each reachable instruction.
fn acc_liveness(insns: &[Insn], reach: &[bool]) -> (Vec<bool>, Vec<bool>) {
    let n = insns.len();
    let kills = |op: Op| {
        matches!(
            op,
            Op::Ldar
                | Op::LdaSmi
                | Op::LdaTrue
                | Op::LdaFalse
                | Op::LdaUndefined
                | Op::LdaNull
                | Op::GetNamed
                | Op::LdaGlobal
                | Op::Call
                | Op::CallUndefined
                | Op::Construct
                | Op::CreateArray
                | Op::CreateObject
                | Op::LdaConstant
        )
    };
    let reads = |op: Op| !kills(op) && !matches!(op, Op::Nop | Op::Jump | Op::Mov);
    // A bail hands `acc` to the interpreter; an inlined `push` takes its
    // argument in it.
    // A code-less `Star` (`Insn::check_only`) reads nothing, a code-less
    // `Ldar` writes nothing.
    let (reads, kills) = (
        |insn: &Insn| insn.bail || (reads(insn.op) && !insn.check_only) || insn.native == 1,
        |insn: &Insn| !insn.bail && kills(insn.op) && !(insn.check_only && insn.op == Op::Ldar),
    );
    let mut live_in = vec![false; n];
    let mut live_out = vec![false; n];
    let mut changed = true;
    while changed {
        changed = false;
        for i in (0..n).rev() {
            if !reach[i] {
                continue;
            }
            let insn = &insns[i];
            let succs = [
                insn.falls_through().then_some(i + 1),
                insn.jumps().then_some(insn.target),
            ];
            let out = succs.into_iter().flatten().any(|s| live_in[s]);
            let inn = reads(insn) || (!kills(insn) && out);
            if out != live_out[i] || inn != live_in[i] {
                (live_out[i], live_in[i]) = (out, inn);
                changed = true;
            }
        }
    }
    (live_in, live_out)
}

/// One stencil of a fused group.
struct FusedStep {
    /// The slot holding `acc` at an exit from this step, if it isn't the
    /// group's `ldar` slot.
    exit_ldar: Option<String>,
    name: String,
    imm: i32,
    /// The register operand, for a stencil on a register in memory.
    reg: u8,
    /// The bytecode instruction an exit from this step happens before.
    insn: usize,
    /// For in-place ops on unboxed slots, which write the wrapped result
    /// before checking overflow: the stencil (and immediate) that undoes it
    /// in the exit stub.
    undo: Option<(String, i32)>,
}

/// A matched fused group: one or more stencils replacing `len` bytecode
/// instructions.
struct Fused {
    len: usize,
    steps: Vec<FusedStep>,
    /// `ldar` stencil for the slot holding the group's result, to
    /// rematerialize `acc` after the group or in an exit stub (where that
    /// slot holds the interpreter's `acc` at the exiting instruction).
    ldar: String,
    acc_live_after: bool,
}

impl Fused {
    fn single(
        len: usize,
        name: String,
        imm: i32,
        ldar: String,
        acc_live_after: bool,
        undo: Option<(String, i32)>,
        insn: usize,
    ) -> Fused {
        Fused {
            len,
            steps: vec![FusedStep {
                name,
                imm,
                reg: 0,
                insn,
                undo,
                exit_ldar: None,
            }],
            ldar,
            acc_live_after,
        }
    }
}

/// Matches fused-stencil patterns (see the fused section of
/// `stencils/stencils.rs`). A group needs its operands in slots with the
/// right types, no jump target (or the entry) inside it, and, for the loop
/// tail, a dead accumulator on both branches.
struct Fusion<'a> {
    insns: &'a [Insn],
    ins: &'a [Option<TypeState>],
    homes: &'a [Home],
    /// `None` when not specializing: no fusion.
    widened: Option<&'a HashSet<usize>>,
    live_out: &'a [bool],
    is_target: &'a [bool],
}

impl Fusion<'_> {
    /// A fused group at instruction `i`, if any: not one whose
    /// instructions name two registers sharing a slot (see
    /// `assign_homes`), which its patterns tell apart by register.
    fn at(&self, i: usize) -> Option<Fused> {
        let f = self.at_any(i)?;
        let mut seen: Vec<((bool, u8), u8)> = Vec::new();
        for insn in &self.insns[i..i + f.len] {
            if !insn.reads_reg() {
                continue;
            }
            let r = insn.reg;
            let slot = match self.homes[r as usize] {
                Home::Mem => continue,
                Home::Gpr(k) | Home::Int(k) => (false, k),
                Home::Fpr(k) => (true, k),
            };
            if seen.iter().any(|&(s, q)| s == slot && q != r) {
                return None;
            }
            seen.push((slot, r));
        }
        Some(f)
    }

    fn at_any(&self, i: usize) -> Option<Fused> {
        let widened = self.widened?;
        // Not over a code-less `Ldar` (see `Lowering::drop_setup`).
        let n = self.insns.len();
        if (i..(i + 4).min(n)).any(|j| self.insns[j].check_only && self.insns[j].op == Op::Ldar) {
            return None;
        }
        if let Some(f) = self.add_chain(i, widened) {
            return Some(f);
        }
        if let Some(f) = self.compare_jump(i) {
            return Some(f);
        }
        if let Some(f) = self.int_slot_ops(i) {
            return Some(f);
        }
        if let Some(f) = self.slot_tests_and_subs(i, widened) {
            return Some(f);
        }
        let insns = self.insns;
        let group = |len: usize| {
            i + len <= insns.len()
                && (i + 1..i + len).all(|j| !self.is_target[j] && self.ins[j].is_some())
        };
        if !group(3)
            || insns[i].op != Op::Ldar
            || insns[i + 2].op != Op::Star
            || insns[i + 2].reg != insns[i].reg
        {
            return None;
        }
        let r = insns[i].reg as usize;
        let st = self.ins[i].as_ref()?;
        let mid = &insns[i + 1];
        let int_ok = st.regs[r] == Ty::I32 && !widened.contains(&(i + 1));
        let dbl = st.regs[r] == Ty::F64;
        let home = self.homes[r];
        if home == Home::Mem {
            return None;
        }
        let ldar = format!("ldar_{}", home.loc());
        let fused = |len: usize, name: String, imm: i32| {
            let acc_live_after = self.live_out[i + len - 1];
            Some(Fused::single(
                len,
                name,
                imm,
                ldar.clone(),
                acc_live_after,
                None,
                i + 1,
            ))
        };
        let undone = |f: Option<Fused>, undo: String, imm: i32| {
            f.map(|mut f| {
                f.steps[0].undo = Some((undo, imm));
                f
            })
        };

        // Loop tail: ldar r; inc; star r; jlt_smi k | jlt r2.
        if mid.op == Op::Inc && int_ok && group(4) && !self.live_out[i + 3] {
            let tail = &insns[i + 3];
            let bound = match tail.op {
                Op::JumpIfLess => {
                    let r2 = tail.reg as usize;
                    (self.ins[i + 3].as_ref()?.regs[r2] == Ty::I32).then_some(self.homes[r2])
                }
                _ => None,
            };
            match (home, tail.op, bound) {
                (Home::Gpr(a), Op::JumpIfLessSmi, _) => {
                    return fused(4, format!("incjlt_g{a}"), tail.imm);
                }
                (Home::Gpr(a), Op::JumpIfLess, Some(Home::Gpr(b))) => {
                    return fused(4, format!("incjlt_g{a}_g{b}"), 0);
                }
                (Home::Int(a), Op::JumpIfLessSmi, _) => {
                    let f = fused(4, format!("incjlt_u{a}"), tail.imm);
                    return undone(f, format!("undo_addi_u{a}"), 1);
                }
                (Home::Int(a), Op::JumpIfLess, Some(Home::Int(b))) => {
                    let f = fused(4, format!("incjlt_u{a}_u{b}"), 0);
                    return undone(f, format!("undo_addi_u{a}"), 1);
                }
                _ => {}
            }
        }

        match (mid.op, home) {
            // ldar r; add_smi k | inc; star r
            (Op::AddSmi | Op::Inc, Home::Gpr(a)) if int_ok => {
                fused(3, format!("addi_g{a}"), mid.imm)
            }
            (Op::AddSmi | Op::Inc, Home::Int(a)) if int_ok => undone(
                fused(3, format!("addi_u{a}"), mid.imm),
                format!("undo_addi_u{a}"),
                mid.imm,
            ),
            (Op::AddSmi | Op::Inc, Home::Fpr(a)) if dbl => fused(3, format!("addi_f{a}"), mid.imm),
            _ => None,
        }
    }
}

impl Fusion<'_> {
    /// Whether instructions `i + 1 .. i + len` exist, are reachable and
    /// aren't jump targets, so `i .. i + len` can be one group.
    fn group(&self, i: usize, len: usize) -> bool {
        i + len <= self.insns.len()
            && (i + 1..i + len).all(|j| !self.is_target[j] && self.ins[j].is_some())
    }

    /// A comparison and the `JumpIfTrue` / `JumpIfFalse` consuming its
    /// boolean, as one compare-and-branch stencil (`tlt_jf_ii_u0`, …),
    /// when the boolean is dead after the jump. Also `BitAndSmi k;
    /// TestEqualSmi 0; JumpIf…` on an int32, as `test` + branch
    /// (`tstz_jt_i`). None of these exits.
    fn compare_jump(&self, i: usize) -> Option<Fused> {
        let insns = self.insns;
        let st = self.ins[i].as_ref()?;
        // A comparison's result is a boolean, so a ToBoolean jump on it
        // is the plain one (an inlined function returning `a < b` feeds
        // `JumpIfToBooleanFalse`).
        let polarity = |op: Op| match op {
            Op::JumpIfTrue | Op::JumpIfToBooleanTrue => Some("jt"),
            Op::JumpIfFalse | Op::JumpIfToBooleanFalse => Some("jf"),
            _ => None,
        };
        let step = |name: String, imm: i32, reg: u8| FusedStep {
            name,
            imm,
            reg,
            insn: i,
            undo: None,
            exit_ldar: None,
        };
        let fused = |len: usize, steps: Vec<FusedStep>| Fused {
            len,
            steps,
            ldar: String::new(),
            acc_live_after: false,
        };
        if insns[i].op == Op::BitAndSmi
            && st.acc == Ty::I32
            && self.group(i, 3)
            && insns[i + 1].op == Op::TestEqualSmi
            && insns[i + 1].imm == 0
            && let Some(pol) = polarity(insns[i + 2].op)
            && !self.live_out[i + 2]
        {
            return Some(fused(
                3,
                vec![step(format!("tstz_{pol}_i"), insns[i].imm, 0)],
            ));
        }
        if !self.group(i, 2) || self.live_out[i + 1] {
            return None;
        }
        let pol = polarity(insns[i + 1].op)?;
        let mut steps = Vec::new();
        let name = match insns[i].op {
            Op::TestEqual | Op::TestStrictEqual
                if identity_ty(st.acc) && identity_ty(st.regs[insns[i].reg as usize]) =>
            {
                let loc = self.homes[insns[i].reg as usize].loc();
                format!("tid_{pol}_{loc}")
            }
            Op::TestLessThan | Op::TestEqual => {
                let base = if insns[i].op == Op::TestLessThan {
                    "tlt"
                } else {
                    "teq"
                };
                let r = insns[i].reg;
                let loc = self.homes[r as usize].loc();
                match pair_case(st.acc, st.regs[r as usize]) {
                    "" => {
                        if let Some(b) = box_acc_piece(st.acc) {
                            steps.push(step(b.stencil.stencil().name.to_string(), 0, 0));
                        }
                        format!("{base}_{pol}_{loc}")
                    }
                    case => format!("{base}_{pol}_{case}_{loc}"),
                }
            }
            Op::TestEqualSmi => match num_case(st.acc) {
                "" => format!("teq_smi_{pol}"),
                case => format!("teq_smi_{pol}_{case}"),
            },
            // On int32s, as a `<` stencil with the operands either way
            // round (`jlt`: `acc < r`, `tlt`: `r < acc`) or its negation
            // (`jge`); otherwise the generic `jrel` / `jnrel`.
            Op::TestGreaterThan | Op::TestLessThanOrEqual | Op::TestGreaterThanOrEqual => {
                let r = insns[i].reg;
                let loc = self.homes[r as usize].loc();
                if pair_case(st.acc, st.regs[r as usize]) == "ii" {
                    let base = match (insns[i].op, pol) {
                        (Op::TestGreaterThan, "jt") | (Op::TestLessThanOrEqual, "jf") => "jlt",
                        (Op::TestGreaterThan, _) | (Op::TestLessThanOrEqual, _) => "jge",
                        (_, "jt") => "tlt_jf",
                        _ => "tlt_jt",
                    };
                    format!("{base}_ii_{loc}")
                } else {
                    if let Some(b) = box_acc_piece(st.acc) {
                        steps.push(step(b.stencil.stencil().name.to_string(), 0, 0));
                    }
                    if let Some(name) = spill_name(self.homes[r as usize]) {
                        steps.push(step(name, 0, r));
                    }
                    let base = if pol == "jt" { "jrel" } else { "jnrel" };
                    steps.push(step(format!("{base}_m"), rel_code(insns[i].op), r));
                    return Some(fused(2, steps));
                }
            }
            Op::TestStrictEqual => {
                let r = insns[i].reg;
                let loc = self.homes[r as usize].loc();
                if pair_case(st.acc, st.regs[r as usize]) == "ii" {
                    format!("teq_{pol}_ii_{loc}")
                } else {
                    if let Some(b) = box_acc_piece(st.acc) {
                        steps.push(step(b.stencil.stencil().name.to_string(), 0, 0));
                    }
                    if let Some(name) = spill_name(self.homes[r as usize]) {
                        steps.push(step(name, 0, r));
                    }
                    "tseq_".to_string() + pol + "_m"
                }
            }
            // Never true of a number or an object: no test.
            Op::TestUndefined | Op::TestNull | Op::TestNullish => {
                let base = match insns[i].op {
                    Op::TestUndefined => "tund",
                    Op::TestNull => "tnull",
                    _ => "tnsh",
                };
                match (never_nullish(st.acc), pol) {
                    (true, "jt") => return Some(fused(2, steps)),
                    (true, _) => "jump".to_string(),
                    (false, _) => format!("{base}_{pol}"),
                }
            }
            _ => return None,
        };
        steps.push(step(name, insns[i].imm, insns[i].reg));
        Some(fused(2, steps))
    }

    /// Groups starting with `ldar a` where `a` is an unboxed int32 slot,
    /// which work on the slots and skip `acc`:
    ///
    /// - `ldar a; TestLessThan b | TestEqual b; JumpIf…` with `b` an int32
    ///   slot too: `tlt_jf_u<a>_u<b>`, … (`cmp`, branch).
    /// - `ldar a; TestEqualSmi k; JumpIf…`: `teq_smi_jf_u<a>`, ….
    /// - `ldar a; BitAndSmi k; TestEqualSmi 0; JumpIf…`: `tstz_jf_u<a>`, ….
    ///
    /// each when `acc` is dead after the jump; and
    ///
    /// - `ldar a; BitAndSmi k | BitOrSmi k; star b` with `b` an int32 slot:
    ///   `mov_u<b>_u<a>` (unless `a` is `b`), then `andi_u<b>` / `ori_u<b>`
    ///   (nothing for `& -1` and `| 0`), then `ldar_u<b>` if `acc` is live.
    ///
    /// None of these exits.
    fn int_slot_ops(&self, i: usize) -> Option<Fused> {
        let insns = self.insns;
        if insns[i].op != Op::Ldar {
            return None;
        }
        let a = insns[i].reg as usize;
        let Home::Int(sa) = self.homes[a] else {
            return None;
        };
        let polarity = |k: usize| match insns.get(k).map(|n| n.op) {
            Some(Op::JumpIfTrue | Op::JumpIfToBooleanTrue) => Some("jt"),
            Some(Op::JumpIfFalse | Op::JumpIfToBooleanFalse) => Some("jf"),
            _ => None,
        };
        let step = |name: String, imm: i32| FusedStep {
            name,
            imm,
            reg: 0,
            insn: i,
            undo: None,
            exit_ldar: None,
        };
        let fused = |len: usize, steps: Vec<FusedStep>, ldar: String, live: bool| Fused {
            len,
            steps,
            ldar,
            acc_live_after: live,
        };
        let op1 = insns.get(i + 1).map(|n| n.op);
        let int_slot = |k: usize, r: usize| match self.homes[r] {
            Home::Int(sr) if self.ins[k].as_ref().is_some_and(|st| st.regs[r] == Ty::I32) => {
                Some(sr)
            }
            _ => None,
        };
        // A compare and branch, `acc` dead after.
        if self.group(i, 3)
            && let Some(pol) = polarity(i + 2)
            && !self.live_out[i + 2]
        {
            let name = match op1 {
                Some(Op::TestLessThan | Op::TestEqual | Op::TestStrictEqual) => {
                    let b = insns[i + 1].reg as usize;
                    let sb = int_slot(i + 1, b)?;
                    let base = if op1 == Some(Op::TestLessThan) {
                        "tlt"
                    } else {
                        "teq"
                    };
                    Some((format!("{base}_{pol}_u{sa}_u{sb}"), 0))
                }
                // `tlt_*_u<a>_u<b>` tests `b < a`; with `acc` = a and the
                // register operand b: `b > a` is `a < b` (operands
                // swapped), `b <= a` is `!(a < b)`, `b >= a` is `!(b < a)`.
                Some(
                    Op::TestGreaterThan | Op::TestLessThanOrEqual | Op::TestGreaterThanOrEqual,
                ) => {
                    let b = insns[i + 1].reg as usize;
                    let sb = int_slot(i + 1, b)?;
                    let negate = op1 != Some(Op::TestGreaterThan);
                    let pol = match (pol, negate) {
                        ("jt", false) | ("jf", true) => "jt",
                        _ => "jf",
                    };
                    let (x, y) = if op1 == Some(Op::TestGreaterThanOrEqual) {
                        (sa, sb)
                    } else {
                        (sb, sa)
                    };
                    Some((format!("tlt_{pol}_u{x}_u{y}"), 0))
                }
                Some(Op::TestEqualSmi) => Some((format!("teq_smi_{pol}_u{sa}"), insns[i + 1].imm)),
                _ => None,
            };
            if let Some((name, imm)) = name {
                return Some(fused(3, vec![step(name, imm)], String::new(), false));
            }
        }
        if self.group(i, 4)
            && op1 == Some(Op::BitAndSmi)
            && insns[i + 2].op == Op::TestEqualSmi
            && insns[i + 2].imm == 0
            && let Some(pol) = polarity(i + 3)
            && !self.live_out[i + 3]
        {
            let name = format!("tstz_{pol}_u{sa}");
            return Some(fused(
                4,
                vec![step(name, insns[i + 1].imm)],
                String::new(),
                false,
            ));
        }
        // A bitwise operation from slot to slot.
        if self.group(i, 3)
            && matches!(op1, Some(Op::BitAndSmi | Op::BitOrSmi))
            && insns[i + 2].op == Op::Star
        {
            let b = insns[i + 2].reg as usize;
            let Home::Int(sb) = self.homes[b] else {
                return None;
            };
            let k = insns[i + 1].imm;
            let mut steps = Vec::new();
            if sa != sb {
                steps.push(step(format!("mov_u{sb}_u{sa}"), 0));
            }
            match (op1, k) {
                (Some(Op::BitAndSmi), -1) | (Some(Op::BitOrSmi), 0) => {}
                (Some(Op::BitAndSmi), _) => steps.push(step(format!("andi_u{sb}"), k)),
                _ => steps.push(step(format!("ori_u{sb}"), k)),
            }
            return Some(fused(3, steps, format!("ldar_u{sb}"), self.live_out[i + 2]));
        }
        None
    }

    /// Two groups starting with `ldar a`:
    ///
    /// - `ldar a; TestUndefined | TestNull | TestNullish; JumpIf…` with `a` a
    ///   boxed slot and `acc` dead after the jump: the slot is compared
    ///   (`tund_jf_g<a>`, …).
    /// - `ldar y; Sub x; star x` (`x = x - y`) with `x`, `y` int32 slots:
    ///   `subr_ii_u<x>_u<y>`, in place. On overflow the exit stub undoes it
    ///   and loads `acc` from `y`, the interpreter's `acc` before the `Sub`.
    fn slot_tests_and_subs(&self, i: usize, widened: &HashSet<usize>) -> Option<Fused> {
        let insns = self.insns;
        if insns[i].op != Op::Ldar || !self.group(i, 3) {
            return None;
        }
        let a = insns[i].reg as usize;
        let (mid, last) = (&insns[i + 1], &insns[i + 2]);
        let pol = match last.op {
            Op::JumpIfTrue | Op::JumpIfToBooleanTrue => Some("jt"),
            Op::JumpIfFalse | Op::JumpIfToBooleanFalse => Some("jf"),
            _ => None,
        };
        if let (Home::Gpr(sa), Some(pol)) = (self.homes[a], pol)
            && !self.live_out[i + 2]
        {
            let base = match mid.op {
                Op::TestUndefined => "tund",
                Op::TestNull => "tnull",
                Op::TestNullish => "tnsh",
                _ => return None,
            };
            return Some(Fused {
                len: 3,
                steps: vec![FusedStep {
                    name: format!("{base}_{pol}_g{sa}"),
                    imm: 0,
                    reg: 0,
                    insn: i,
                    undo: None,
                    exit_ldar: None,
                }],
                ldar: String::new(),
                acc_live_after: false,
            });
        }
        if mid.op == Op::Sub && last.op == Op::Star && last.reg == mid.reg && mid.reg as usize != a
        {
            let x = mid.reg as usize;
            let st = self.ins[i + 1].as_ref()?;
            let (Home::Int(sx), Home::Int(sy)) = (self.homes[x], self.homes[a]) else {
                return None;
            };
            if st.acc != Ty::I32 || st.regs[x] != Ty::I32 || widened.contains(&(i + 1)) {
                return None;
            }
            return Some(Fused {
                len: 3,
                steps: vec![FusedStep {
                    name: format!("subr_ii_u{sx}_u{sy}"),
                    imm: 0,
                    reg: 0,
                    insn: i + 1,
                    undo: Some((format!("undo_subr_u{sx}_u{sy}"), 0)),
                    exit_ldar: Some(format!("ldar_u{sy}")),
                }],
                ldar: format!("ldar_u{sx}"),
                acc_live_after: self.live_out[i + 2],
            });
        }
        None
    }

    /// `ldar a; add b1; …; add bn; star c` (n ≥ 1) as `c = a` (a slot move,
    /// skipped when `c == a`) followed by in-place `c += b_k` stencils.
    ///
    /// If add k overflows, its stencil exits with the wrapped sum in `c`; the
    /// exit stub undoes it, so `c` holds `a + b1 + … + b(k-1)`, the
    /// interpreter's `acc` before add k, and rematerializes `acc` from it.
    /// `c` itself is dead there (the pending `star c` overwrites it), which
    /// requires that no `b_k` is `c`: that add would read `c`'s original
    /// value. With `c` in an FP slot the chain is computed in double from the
    /// start, which is exact for int32 operands too; the result must then be
    /// typed double, so a live `acc` stays consistent.
    fn add_chain(&self, i: usize, widened: &HashSet<usize>) -> Option<Fused> {
        let insns = self.insns;
        if insns[i].op != Op::Ldar {
            return None;
        }
        let mut j = i + 1;
        while j < insns.len()
            && insns[j].op == Op::Add
            && !self.is_target[j]
            && self.ins[j].is_some()
        {
            j += 1;
        }
        let n = j - i - 1;
        if n == 0
            || j >= insns.len()
            || insns[j].op != Op::Star
            || self.is_target[j]
            || self.ins[j].is_none()
        {
            return None;
        }
        let (a, c) = (insns[i].reg as usize, insns[j].reg as usize);
        let adds = i + 1..j;
        if adds.clone().any(|k| insns[k].reg as usize == c) {
            return None;
        }
        let reg_ty = |k: usize, r: usize| self.ins[k].as_ref().map(|st| st.regs[r]);
        let mut steps = Vec::new();
        let step = |name: String, insn: usize, undo: Option<(String, i32)>| FusedStep {
            name,
            imm: 0,
            reg: 0,
            insn,
            undo,
            exit_ldar: None,
        };
        match self.homes[c] {
            Home::Int(cs) => {
                if a != c {
                    let Home::Int(as_) = self.homes[a] else {
                        return None;
                    };
                    steps.push(step(format!("mov_u{cs}_u{as_}"), i + 1, None));
                }
                for k in adds {
                    let b = insns[k].reg as usize;
                    let st = self.ins[k].as_ref()?;
                    let Home::Int(bs) = self.homes[b] else {
                        return None;
                    };
                    if st.acc != Ty::I32 || st.regs[b] != Ty::I32 || widened.contains(&k) {
                        return None;
                    }
                    let undo = (format!("undo_addr_u{cs}_u{bs}"), 0);
                    steps.push(step(format!("addr_ii_u{cs}_u{bs}"), k, Some(undo)));
                }
            }
            Home::Fpr(cs) => {
                if self.ins[j].as_ref()?.acc != Ty::F64 {
                    return None;
                }
                if a != c {
                    let name = match (self.homes[a], reg_ty(i, a)?) {
                        (Home::Fpr(as_), Ty::F64) => format!("mov_f{cs}_f{as_}"),
                        (Home::Int(as_), Ty::I32) => format!("mov_f{cs}_u{as_}"),
                        _ => return None,
                    };
                    steps.push(step(name, i + 1, None));
                }
                for k in adds {
                    let b = insns[k].reg as usize;
                    let name = match (self.homes[b], reg_ty(k, b)?) {
                        (Home::Fpr(bs), Ty::F64) => format!("addr_ff_f{cs}_f{bs}"),
                        (Home::Int(bs), Ty::I32) => format!("addr_fi_f{cs}_u{bs}"),
                        (Home::Gpr(bs), Ty::I32) => format!("addr_fi_f{cs}_g{bs}"),
                        _ => return None,
                    };
                    steps.push(step(name, k, None));
                }
            }
            _ => return None,
        }
        Some(Fused {
            len: n + 2,
            steps,
            ldar: format!("ldar_{}", self.homes[c].loc()),
            acc_live_after: self.live_out[j],
        })
    }
}

/// Set in an exit record by a call's exit, which happens after the call, at
/// the next instruction: it says nothing about that instruction.
const AFTER_CALL: i32 = 1 << 23;

/// Set in an after-call exit record by the check of the call's result
/// against its prediction (see [`Ty::of_result`]): the call shouldn't be
/// predicted anymore.
const BAD_RESULT: i32 = 1 << 22;

/// The same bit without [`AFTER_CALL`]: the exit is a check moved before a
/// loop (see [`hoist_guards`]).
const HOISTED: i32 = BAD_RESULT;

/// Set in an exit record by warm code's loop header when the function's
/// heat ran out: compile it hot and continue there (see [`Tier`]).
const TIER_UP: i32 = 1 << 21;

/// Rounds of type inference that predict direct calls' results (see
/// [`JitCode::compile`]).
const MAX_RESULT_ROUNDS: u32 = 3;

/// Value of `ctx` while running: no exit taken.
const NO_EXIT: u32 = u32::MAX;

/// Call the generated code at `f` with the machine state: slots start empty
/// (`enter_*` stencils load them).
#[cfg(target_arch = "x86_64")]
unsafe fn call_entry(f: *const u8, acc: Value, regs: *mut Value, ctx: *mut u32) -> Value {
    type Entry = extern "rust-preserve-none" fn(
        Value,
        *mut Value,
        NumberTag,
        *mut u32,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
    ) -> Value;
    let entry: Entry = unsafe { std::mem::transmute(f) };
    let u = Value::UNDEFINED;
    entry(
        acc,
        regs,
        NumberTag::new(),
        ctx,
        u,
        u,
        u,
        u,
        u,
        u,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    )
}

#[cfg(target_arch = "aarch64")]
unsafe fn call_entry(f: *const u8, acc: Value, regs: *mut Value, ctx: *mut u32) -> Value {
    type Entry = extern "rust-preserve-none" fn(
        Value,
        *mut Value,
        NumberTag,
        *mut u32,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
    ) -> Value;
    let entry: Entry = unsafe { std::mem::transmute(f) };
    let u = Value::UNDEFINED;
    entry(
        acc,
        regs,
        NumberTag::new(),
        ctx,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    )
}

/// Machine code for one entry into a bytecode function.
pub struct JitCode {
    mem: ExecMem,
    entry: usize,
    /// Bytes of hot code, from the start (excluding cold code).
    main_len: usize,
    /// Bytes of code: hot, cold (stubs, helpers) and runtime-call wrappers.
    len: usize,
    /// The selected stencil per instruction (`None`: unreachable from the
    /// entry, or covered by the fused stencil of an earlier instruction),
    /// for inspection and tests.
    pub stencils: Vec<Option<String>>,
    /// Where each VM register lived.
    pub homes: Vec<Home>,
    /// What its `Return`s give (the join of `acc`'s types there), `Bot` if
    /// it never returns: a fact for every value the code returns itself.
    pub ret: Ty,
    /// Call sites (indices into the snapshot's) bound to this code itself:
    /// [`JitCode::into_rc`] gives them its address.
    self_sites: Vec<usize>,
    /// Copies of the snapshot's direct-call sites another version claimed
    /// with other types: this version's calls there go straight into a
    /// callee version for its own types, not through the runtime. Owned
    /// here: the code points to them.
    #[allow(clippy::vec_box, dead_code)]
    forks: Vec<Box<std::cell::UnsafeCell<CallSite>>>,
    /// For a VM function: the lowering compiled, which exits refer to and
    /// the code's call sites live in. Running code keeps it alive.
    snap: Option<Rc<Snapshot>>,
}

/// How a run of [`JitCode`] ended.
pub enum Outcome {
    Return(Value),
    /// Side exit before the instruction at bytecode offset `pc`, with `acc`;
    /// the register file is up to date.
    Exit {
        pc: usize,
        acc: Value,
    },
}

/// What compiling a VM function's lowered instructions needs besides them
/// (see [`lower`]); for other code, no properties or feedback, and no
/// inlined frames (`inline_base` is the register count).
struct VmCompile<'a> {
    props: &'a [Prop],
    polys: &'a [Cases],
    feedback: Option<&'a [Feedback]>,
    inline_base: usize,
    /// Where direct calls' frames start (see [`Lowered::direct_base`]):
    /// registers from there stay in memory.
    direct_base: usize,
    /// The lowering's call sites (`Prop::Call` names them by index).
    sites: &'a [Box<std::cell::UnsafeCell<CallSite>>],
    /// The frames' registers (empty: one frame, all registers).
    frames: &'a [FrameRegs],
    /// The function compiled (null for other code): its `ret_hint` follows
    /// the predictions for its own results.
    root: *const crate::vm::Code,
    /// Objects to allocate at exits (see [`Lowered::mats`]).
    mats: Option<&'a HashMap<usize, Vec<Rc<Mat>>>>,
    /// See [`Lowered::exit_live`].
    exit_live: Option<&'a [RegSet]>,
    /// Warm code: the function's heat, which its loop headers count down
    /// (see [`Tier`]); 0 for hot code.
    heat: u64,
}

/// Compilation options.
#[derive(Clone, Copy)]
struct Options {
    specialize: bool,
    register_homes: bool,
    fusion: bool,
    unboxed_ints: bool,
}

impl JitCode {
    /// Compile `insns` for entry at instruction `entry` with the exact types
    /// `actual` of the live values.
    fn compile(
        insns: &[Insn],
        entry: usize,
        live: &EntryLive,
        actual: TypeState,
        widened: &HashSet<usize>,
        opts: Options,
        vm: VmCompile,
    ) -> JitCode {
        timed(&COMPILE_NS, || {
            Self::compile_region(insns, entry, live, actual, widened, opts, vm)
        })
    }

    fn compile_region(
        insns: &[Insn],
        entry: usize,
        live: &EntryLive,
        actual: TypeState,
        widened: &HashSet<usize>,
        opts: Options,
        vm: VmCompile,
    ) -> JitCode {
        let VmCompile {
            props,
            polys,
            feedback,
            inline_base,
            direct_base,
            sites: vm_sites,
            frames,
            root,
            mats,
            exit_live,
            heat,
        } = vm;
        let n = insns.len();
        let nregs = actual.regs.len();
        // The collector scans the VM stack that far past a frame.
        let cur = crate::interp::current_vm();
        if !cur.is_null() {
            unsafe { (*cur).max_frame = (*cur).max_frame.max(nregs + interp::HEADER) };
        }
        let actual = if opts.specialize {
            actual
        } else {
            TypeState::any(nregs)
        };
        let widened = opts.specialize.then_some(widened);

        // Registers not live at entry are `Bot` there: their entry values
        // (typically `undefined`) are never read, so they shouldn't widen the
        // types the region's own stores give them.
        let (reach, live_sets) = (&live.reach, &live.regs);
        // Not inlined frames' registers: none has started at an entry (see
        // `JitFunction::entry_types`).
        let live_at_entry: Vec<bool> = (0..nregs)
            .map(|r| rs_has(&live_sets[entry], r) && (feedback.is_none() || r < inline_base))
            .collect();
        // In a VM frame, registers whose live ranges don't overlap share
        // slots (see `assign_homes`): live here also where an exit leaves
        // them to the interpreter (`exit_live`, which compiled code may not
        // read: a callee whose check it dropped, say), into an exit that
        // allocates an object scalar replacement left unallocated (its
        // fields), and `this` into the frame's `Return`s (it writes `this`
        // back).
        let share_live: Option<Vec<RegSet>> = exit_live
            .filter(|e| feedback.is_some() && opts.register_homes && e.len() == n && nregs <= 256)
            .map(|e| {
                let mut l = live_sets.clone();
                // Where an exit may leave: an instruction that can exit, and
                // the one after a call (it exits there).
                let quiet = |i: &Insn| {
                    !i.bail
                        && match i.op {
                            Op::Ldar
                            | Op::Star
                            | Op::LdaSmi
                            | Op::LdaUndefined
                            | Op::LdaNull
                            | Op::LdaTrue
                            | Op::LdaFalse
                            | Op::LdaConstant
                            | Op::Jump => true,
                            Op::Nop => {
                                i.guard == 0
                                    && i.alloc == 0
                                    && i.native == 0
                                    && i.hoisted.is_none()
                                    && !i.on_shape
                            }
                            _ => false,
                        }
                };
                for (k, (l, e)) in l.iter_mut().zip(e).enumerate() {
                    if !quiet(&insns[k]) || k > 0 && insns[k - 1].is_call() {
                        rs_or(l, e);
                    }
                }
                for (&at, ms) in mats.into_iter().flatten() {
                    for m in ms {
                        for &f in &m.fields {
                            rs_add(&mut l[at], f as usize);
                        }
                    }
                }
                for (i, insn) in insns.iter().enumerate() {
                    if insn.op == Op::Return && insn.frame == 0 && nregs > 0 {
                        rs_add(&mut l[i], 0);
                    }
                }
                // Those are uses: live back to where they were written.
                let uses = l.clone();
                let mut changed = true;
                while changed {
                    changed = false;
                    for k in (0..n).rev().filter(|&k| reach[k]) {
                        let mut out = [0; 4];
                        for s in succs(insns, k).filter(|&s| s < n) {
                            rs_or(&mut out, &l[s]);
                        }
                        if insns[k].op == Op::Star {
                            let w = insns[k].reg as usize;
                            out[w / 64] &= !(1 << (w % 64));
                        }
                        rs_or(&mut out, &uses[k]);
                        if !rs_eq(&out, &l[k]) {
                            l[k] = out;
                            changed = true;
                        }
                    }
                }
                l
            });
        let interferes = share_live.as_ref().map(|l| interference(insns, l, nregs));
        // The homes an exit before instruction `i` writes back; for an exit
        // after a call, `i` is the instruction after it, and those live
        // into the call too: the interpreter may make the call again (see
        // `Vm::redo_call`).
        let exit_homes = |homes: &[Home], i: usize| -> Vec<Home> {
            match &share_live {
                Some(l) => homes
                    .iter()
                    .enumerate()
                    .map(|(r, &h)| {
                        let call = i > 0 && insns[i - 1].is_call() && rs_has(&l[i - 1], r);
                        if rs_has(&l[i.min(n - 1)], r) || call {
                            h
                        } else {
                            Home::Mem
                        }
                    })
                    .collect(),
                None => homes.to_vec(),
            }
        };
        // The spills of an exit before instruction `i` (`exit_homes`'
        // slots), from the registers with slots.
        let exit_spills = |homes: &[Home], i: usize, boxed: &[bool]| -> Vec<Piece> {
            let live = |r: usize| match &share_live {
                Some(l) => {
                    rs_has(&l[i.min(n - 1)], r)
                        || (i > 0 && insns[i - 1].is_call() && rs_has(&l[i - 1], r))
                }
                None => true,
            };
            homes
                .iter()
                .enumerate()
                .filter(|&(r, &h)| h != Home::Mem && live(r))
                .filter_map(|(r, &h)| spill_reg_piece(h, r as u8, boxed))
                .collect()
        };
        // What entry loads into slots: the live registers, and in a VM frame
        // those an exit may leave to the interpreter (shared slots write
        // back only those, so they must hold their values).
        let load_at_entry: Vec<bool> = match &share_live {
            Some(l) => (0..nregs)
                .map(|r| rs_has(&l[entry], r) && r < inline_base)
                .collect(),
            None => live_at_entry.clone(),
        };
        let mut actual_live = actual.clone();
        for (r, &live) in live_at_entry.iter().enumerate() {
            if !live {
                actual_live.regs.set(r, Ty::Bot);
            }
        }

        // Pass 1: types without pinning; choose homes. Pass 2: types with
        // the FP-homed registers pinned as doubles. Pinning only widens
        // int32 to double, so it can't make an FP-pinned register `Any`, but
        // it can turn an int-pinned one double; then unpin it and retry.
        let unpinned = vec![false; nregs];
        let acc_live = &live.acc;
        // An add right before a bitwise operation, with no jump target in
        // between: the add's result is `acc`, which the bitwise operation
        // overwrites, so that is its only use.
        let is_target = {
            let mut t = vec![false; n];
            for insn in insns.iter().filter(|insn| insn.jumps()) {
                t[insn.target] = true;
            }
            t
        };
        let moved: Vec<bool> = (0..n)
            .map(|i| {
                insns[i].op == Op::GetNamed
                    && !insns[i].bail
                    && !insns[i].check_only
                    && i + 1 < n
                    && !is_target[i + 1]
                    && !insns[i + 1].bail
                    && insns[i + 1].op == Op::SetNamed
            })
            .collect();
        let wraps: Vec<bool> = (0..n)
            .map(|i| {
                matches!(insns[i].op, Op::Add | Op::AddSmi | Op::Inc | Op::Sub)
                    && !insns[i].bail
                    && i + 1 < n
                    && !is_target[i + 1]
                    && !insns[i + 1].bail
                    && matches!(
                        insns[i + 1].op,
                        Op::BitAndSmi
                            | Op::BitOrSmi
                            | Op::ShiftLeftSmi
                            | Op::ShiftRightSmi
                            | Op::ShiftRightLogicalSmi
                    )
            })
            .collect();
        let nonneg = nonneg_acc(
            insns,
            &is_target,
            nregs,
            frames,
            (entry == 0).then(|| &live_sets[0]),
        );
        // `this` and the arguments' types at call `i`, in state `st`.
        let call_types = |i: usize, st: &TypeState| {
            let insn = &insns[i];
            let ty = |r: usize| st.regs.get(r).copied().unwrap_or(Ty::Any);
            let w = insn.reg2 as usize;
            let (this, first) = match (insn.op, props.get(i)) {
                (Op::Call, _) => (ty(w), w + 1),
                // `new`: an object of the shape the site allocates.
                (Op::Construct, Some(Prop::Call { site, .. })) => {
                    let s = unsafe { (*vm_sites[*site as usize].get()).this_shape };
                    (if s == 0 { Ty::Any } else { Ty::Obj(s) }, w)
                }
                _ => (Ty::Any, w),
            };
            let args: Vec<Ty> = (first..first + insn.imm as usize).map(ty).collect();
            (this, args)
        };
        // A direct call's result is predicted from the callee version it
        // will bind to (see `Ty::of_result`), which depends on the
        // arguments' types, which may depend on earlier calls' results: run
        // the passes again until the predictions settle. They are checked
        // after the calls, so stopping before is safe. Calls whose check
        // failed before are in `widened`: no prediction.
        let mut rets = vec![Ty::Any; n];
        let mut round = 0;
        // No store through the runtime (see `Spec::stable_ok`): every named
        // store a cached one, no keyed store.
        let stable_ok = !props.is_empty()
            && insns.iter().enumerate().all(|(k, i)| {
                i.bail
                    || match i.op {
                        Op::SetKeyed => false,
                        Op::SetNamed => matches!(
                            props.get(k),
                            Some(
                                Prop::Own { .. }
                                    | Prop::Oop { .. }
                                    | Prop::Transition { .. }
                                    | Prop::Poly { .. }
                            )
                        ),
                        _ => true,
                    }
            });
        let (homes, ins, mut sel) = loop {
            let spec1 = Spec {
                widened,
                pinned: &unpinned,
                acc_live,
                reg_live: live_sets,
                props,
                polys,
                wraps: &wraps,
                nonneg: &nonneg,
                moved: &moved,
                feedback,
                rets: &rets,
                stable_ok,
                fixed_from: direct_base,
            };
            let (ins1, sels1) = infer(insns, entry, actual_live.clone(), &spec1);
            let no_widening = HashSet::default();
            // Registers of inlined frames (from `inline_base`) are assigned
            // when their frame starts, before anything there can exit, and mean
            // nothing outside it: they may always be pinned.
            let pin_ok: Vec<bool> = pinnable(
                insns,
                entry,
                reach,
                &live_at_entry,
                &ins1,
                widened.unwrap_or(&no_widening),
            )
            .into_iter()
            .enumerate()
            .map(|(r, ok)| ok || r >= inline_base)
            // Not one entry loads only for the interpreter (see `entry_pieces`).
            .zip(&load_at_entry)
            .enumerate()
            .map(|(r, (ok, &load))| ok && (!load || live_at_entry[r]))
            .collect();
            let (mut no_fp, mut no_int) = (vec![false; nregs], vec![!opts.unboxed_ints; nregs]);
            let stats = home_stats(insns, &ins1, nregs);
            // The types and stencils with registers pinned, if any is.
            let (homes, pinned_run) = loop {
                let homes = if opts.register_homes {
                    assign_homes(
                        &stats,
                        nregs,
                        &no_fp,
                        &no_int,
                        &pin_ok,
                        direct_base,
                        interferes.as_deref(),
                    )
                } else {
                    vec![Home::Mem; nregs]
                };
                let pinned: Vec<bool> = homes.iter().map(|h| matches!(h, Home::Fpr(_))).collect();
                let mut entry_state = actual_live.clone();
                for (r, &p) in pinned.iter().enumerate() {
                    if p && live_at_entry[r] {
                        entry_state.regs.set(r, Ty::F64);
                    }
                }
                // Nothing pinned: the types are those already inferred.
                let run = pinned.contains(&true).then(|| {
                    infer(
                        insns,
                        entry,
                        entry_state,
                        &Spec {
                            pinned: &pinned,
                            ..spec1
                        },
                    )
                });
                let ins = run.as_ref().map_or(&ins1, |r| &r.0);
                let strays = |r: usize, ok: Ty| {
                    ins.iter()
                        .flatten()
                        .any(|st| st.regs[r] != ok && st.regs[r] != Ty::Bot)
                };
                let mut retry = false;
                for (r, h) in homes.iter().enumerate() {
                    match h {
                        Home::Fpr(_) if strays(r, Ty::F64) => (no_fp[r], retry) = (true, true),
                        Home::Int(_) if strays(r, Ty::I32) => (no_int[r], retry) = (true, true),
                        _ => {}
                    }
                }
                if !retry {
                    break (homes, run);
                }
            };
            let (ins, sel) = pinned_run.unwrap_or((ins1, sels1));
            // Calls to this function are predicted to give what its own
            // `Return`s do, which depends on the prediction. The first round
            // assumes they give what the other `Return`s do (ignoring those
            // that are `Any`, as those of a self call's result are then);
            // later rounds check that.
            let own = Ty::of_result(returned(insns, &ins, round == 0));
            // Callees compiled for this round's direct calls predict calls
            // back to this function (mutual recursion) from it.
            if !root.is_null() {
                unsafe { (*root).ret_hint.set(own) };
            }
            let want: Vec<Ty> = (0..n)
                .map(|i| match (&ins[i], props.get(i), widened) {
                    (
                        Some(st),
                        Some(Prop::Call {
                            site, direct: true, ..
                        }),
                        Some(w),
                    ) if !w.contains(&i) => {
                        let site = vm_sites[*site as usize].get();
                        if unsafe { (*site).self_call } {
                            return own;
                        }
                        // `new` gives `this`, mostly: what the
                        // constructor's objects have been.
                        if insns[i].op == Op::Construct {
                            return unsafe { runtime_result((*site).cached, Op::Construct) };
                        }
                        let (this, args) = call_types(i, st);
                        match unsafe { direct_version(site, this, &args) } {
                            Version::Ready(code, _) => Ty::of_result(code.ret),
                            // Being compiled, further up: what its current
                            // round predicts.
                            Version::Busy => unsafe { (*(*site).callee).ret_hint.get() },
                            Version::None => Ty::Any,
                        }
                    }
                    (
                        Some(_),
                        Some(Prop::Call {
                            site,
                            direct: false,
                            ..
                        }),
                        Some(w),
                    ) if !w.contains(&i) => {
                        let site = vm_sites[*site as usize].get();
                        unsafe { runtime_result((*site).cached, insns[i].op) }
                    }
                    _ => Ty::Any,
                })
                .collect();
            round += 1;
            if want == rets || round == MAX_RESULT_ROUNDS {
                break (homes, ins, sel);
            }
            rets = want;
        };
        // A `Star` into a register nothing reads after, compiled code or
        // the interpreter after an exit, writes nothing (but into a direct
        // call's frame, which the callee reads).
        let mut dead_star = vec![false; n];
        if let Some(l) = &share_live {
            for (i, s) in sel.iter_mut().enumerate() {
                if let Some(s) = s
                    && insns[i].op == Op::Star
                    && s.base == "star"
                    && (insns[i].reg as usize) < direct_base
                    && i + 1 < n
                    && !rs_has(&l[i + 1], insns[i].reg as usize)
                {
                    s.base = "nop";
                    s.case = "";
                    dead_star[i] = true;
                }
            }
            // The load into `acc` such a `Star` stored, if nothing else
            // reads it.
            for i in 1..n {
                let pure = matches!(
                    sel[i - 1].as_ref().map(|s| s.base),
                    Some("ldar" | "lda_smi" | "lda_undefined" | "lda_null" | "lda_true")
                        | Some("lda_false" | "lda_constant")
                ) && sel[i - 1].as_ref().is_some_and(|s| s.check.is_none());
                if dead_star[i] && pure && !acc_live.get(i + 1).copied().unwrap_or(true) {
                    let s = sel[i - 1].as_mut().unwrap();
                    s.base = "nop";
                    s.case = "";
                }
            }
        }
        // Registers whose int32 slot a `Star` gives a boxed value
        // (`star_u<k>_v`): exits write them back with `spill_u<k>_v`.
        let mut boxed = vec![false; nregs];
        for (insn, s) in insns.iter().zip(&sel) {
            if let Some(s) = s
                && insn.op == Op::Star
                && s.base == "star"
                && s.case != "i"
                && matches!(homes.get(insn.reg as usize), Some(Home::Int(_)))
            {
                boxed[insn.reg as usize] = true;
            }
        }
        // Direct calls: bind each to the callee version for the arguments'
        // types here, or call through the runtime. A call to this function
        // binds to this code, if it is entered at the start and the
        // arguments fit its entry types (its entry pieces trust those);
        // this code's address is only known at the end.
        let at_start = insns[entry].frame == 0 && insns[entry].pc == 0;
        let fits_entry = |this: Ty, args: &[Ty], nparams: usize| {
            let fits = |a: Ty, v: Ty| {
                a == v
                    || v == Ty::Any
                    || v == Ty::Bot
                    || matches!((a, v), (Ty::Obj(x) | Ty::Ext(x), Ty::Ext(y))
                        if unsafe { extends(x as *const Shape, y as *const Shape) })
            };
            actual_live.acc == Ty::Bot
                && actual_live.regs.iter().all(|(r, v)| {
                    let a = match r {
                        0 => this,
                        r if r <= nparams => args.get(r - 1).copied().unwrap_or(Ty::Any),
                        _ => Ty::Any,
                    };
                    fits(a, v)
                })
        };
        let mut self_sites = Vec::new();
        #[allow(clippy::vec_box)]
        let mut forks: Vec<Box<std::cell::UnsafeCell<CallSite>>> = Vec::new();
        // Per instruction, the site its stencil uses if not the snapshot's.
        let mut forked: HashMap<usize, u64> = HashMap::default();
        for (i, s) in sel.iter_mut().enumerate() {
            if let (
                Some(s),
                Some(st),
                Some(Prop::Call {
                    site: k,
                    direct: true,
                    ..
                }),
            ) = (s.as_mut(), &ins[i], props.get(i))
            {
                let (this, args) = call_types(i, st);
                let site = vm_sites[*k as usize].get();
                let bound = unsafe {
                    if (*site).self_call {
                        let ok = at_start
                            && !(*site).bound
                            && fits_entry(this, &args, (*site).nparams as usize);
                        if ok {
                            (*site).bound = true;
                            (*site).end_off = 8 * ((*site).frame + nregs + interp::HEADER) as u64;
                            self_sites.push(*k as usize);
                        }
                        ok
                    } else if bind_direct(site, this, &args) {
                        true
                    } else if (*site)
                        .claim
                        .as_ref()
                        .is_some_and(|c| *c != (this, args.clone()))
                    {
                        // Claimed by another version with other types: a
                        // copy of its own.
                        let f = Box::new(std::cell::UnsafeCell::new((*site).fork()));
                        let ok = bind_direct(f.get(), this, &args);
                        if ok {
                            forked.insert(i, f.get() as u64);
                            forks.push(f);
                        }
                        ok
                    } else {
                        false
                    }
                };
                if !bound {
                    (s.base, s.case) = ("call", "");
                }
            }
        }

        // Hot code: entry pieces, then each reachable instruction in bytecode
        // order: its fall-through edge conversions, then (at its label) any
        // pieces it needs first, then its stencil. Jump edges needing
        // conversions get cold stubs; exits get cold spill-and-exit stubs.
        let mut hot: Vec<Piece> = Vec::with_capacity(2 * n);
        // Machine code of pieces of their own (`Piece::raw`), and per
        // direct call made so, (hot piece, stub, rel32 fields to patch; see
        // `RawCall`).
        let mut raws: Vec<Vec<u8>> = Vec::new();
        #[allow(clippy::type_complexity)]
        let mut raw_links: Vec<(usize, usize, Vec<(usize, usize)>, Vec<(usize, usize)>)> =
            Vec::new();
        let mut stubs: Vec<Vec<Piece>> = Vec::new();
        let mut exits: Vec<(usize, Vec<Piece>)> = Vec::new();
        let mut label_piece = vec![usize::MAX; n];
        let mut names: Vec<Option<String>> = vec![None; n];

        let entry_seq = entry_pieces(
            &actual,
            &actual_live,
            ins[entry].as_ref().unwrap(),
            &homes,
            &load_at_entry,
            feedback.is_none(),
        );
        // In a VM frame, the registers dead at entry are zeroed: their words
        // hold what an earlier frame there left (an inlined frame's, one
        // the code keeps in a machine register), which the collector would
        // take for references; one left for long can keep growing garbage
        // alive (Splay's removed nodes, each pointing at the next). Only
        // once per collection (a cold stub, run when the collection count
        // differs from the one the code last zeroed at): a word stale for
        // one collection keeps garbage alive for one cycle, and code that
        // is entered constantly (Richards' tasks) would otherwise pay at
        // every entry.
        let mut entry_seq = entry_seq;
        let mut clear = Vec::new();
        let cur = crate::interp::current_vm();
        if feedback.is_some() && !cur.is_null() {
            // (Not `this`, which the caller of a `new` frame reads after.)
            let mut r = 1;
            while r < nregs {
                if live_at_entry[r] || load_at_entry[r] {
                    r += 1;
                    continue;
                }
                let start = r;
                while r < nregs && !live_at_entry[r] && !load_at_entry[r] {
                    r += 1;
                }
                if r - start <= 4 {
                    clear.extend((start..r).map(|k| Piece::new("zero_m", k as u8)));
                } else {
                    clear.push(Piece {
                        imm: (r - start) as i32,
                        imm64: 8 * start as u64,
                        ..Piece::new("clear_m", 0)
                    });
                }
            }
        }
        if !clear.is_empty() {
            let count = unsafe { &raw const (*cur).heap.gc.count } as u64;
            let last = Box::leak(Box::new(u64::MAX)) as *mut u64 as u64;
            // The stub continues where `words_jne` does: the next entry
            // piece, or the entry instruction when there is none (what is
            // laid out after it then is the region's first instruction,
            // which the entry may be past: a loop nested in the region's).
            let cont = if entry_seq.is_empty() {
                Target::Label(entry as u32)
            } else {
                Target::AfterPiece(hot.len() as u32)
            };
            clear.push(Piece {
                imm64: count,
                imm64b: last,
                cont,
                ..Piece::new("copy_word", 0)
            });
            stubs.push(clear);
            entry_seq.insert(
                0,
                Piece {
                    imm64: count,
                    imm64b: last,
                    jump: Target::Stub(stubs.len() as u32 - 1),
                    ..Piece::new("words_jne", 0)
                },
            );
        }
        let has_entry_seq = !entry_seq.is_empty();
        hot.extend(ending_at(entry_seq, Target::Label(entry as u32)));
        let entry_len = hot.len();
        // What `return` writes back: in a VM frame only `this`, which the
        // caller of a `new` frame reads, and only if the code changes it
        // (`Star r0`, a self tail call's moves); otherwise everything.
        let return_spills = if feedback.is_none() {
            homes.len()
        } else {
            let writes_this = |i: &Insn| {
                i.frame == 0
                    && ((i.op == Op::Star && i.reg == 0) || (i.op == Op::Mov && i.reg2 == 0))
            };
            usize::from(
                insns
                    .iter()
                    .zip(&ins)
                    .any(|(i, st)| st.is_some() && writes_this(i)),
            )
        };

        let live_out = acc_live_out(insns, &ins);
        let mut is_target = vec![false; n];
        is_target[entry] = true;
        for (insn, st) in insns.iter().zip(&ins) {
            if st.is_some() && insn.jumps() {
                is_target[insn.target] = true;
            }
        }
        let fusion = Fusion {
            insns,
            ins: &ins,
            homes: &homes,
            widened: widened.filter(|_| opts.fusion),
            live_out: &live_out,
            is_target: &is_target,
        };

        // Transitions whose new shape only the next transition on the same
        // object sees: it adds a property to that shape (exactly, and
        // unchecked), and nothing between them can exit: only loads into
        // `acc`. Such a transition stores just the slot (`set_own_nc`), the
        // next one the final shape. (A jump into the run is fine: the next
        // transition's shape is exact there too.)
        // Instruction `i`'s transition, if a `set_tr_nc`: shape and new
        // shape (a polymorphic cache's case, if `select` picked one).
        let transition = |i: usize| -> Option<(u64, u64)> {
            let s = sel[i].as_ref()?;
            if (s.base, s.case) != ("set_tr", "nc") {
                return None;
            }
            match (s.ops, props.get(i).copied()) {
                (Some((_, shape, new_shape)), _)
                | (
                    None,
                    Some(Prop::Transition {
                        shape, new_shape, ..
                    }),
                ) => Some((shape, new_shape)),
                _ => None,
            }
        };
        let defer_shape: Vec<bool> = (0..n)
            .map(|i| {
                let Some((_, new_shape)) = transition(i) else {
                    return false;
                };
                let (frame, obj) = (insns[i].frame, insns[i].reg);
                for (j, insn) in insns.iter().enumerate().skip(i + 1) {
                    let Some(s) = &sel[j] else { return false };
                    if insn.frame != frame {
                        return false;
                    }
                    match insn.op {
                        Op::Ldar
                        | Op::LdaSmi
                        | Op::LdaUndefined
                        | Op::LdaNull
                        | Op::LdaTrue
                        | Op::LdaFalse => {}
                        Op::SetNamed => {
                            return insn.reg == obj
                                && s.check.is_none()
                                && transition(j).is_some_and(|(shape, _)| shape == new_shape);
                        }
                        _ => return false,
                    }
                }
                false
            })
            .collect();

        // The jump target of instruction `i` (with its out-state), going
        // through a conversion stub if the edge needs one.
        let jump_to = |i: usize, out: &TypeState, stubs: &mut Vec<Vec<Piece>>| {
            let target = insns[i].target;
            let cs = edge_conversions(out, ins[target].as_ref().unwrap(), &homes);
            if cs.is_empty() {
                Target::Label(target as u32)
            } else {
                stubs.push(ending_at(cs, Target::Label(target as u32)));
                Target::Stub(stubs.len() as u32 - 1)
            }
        };

        // Warm code: the function's start and loop headers count down its
        // heat.
        let mut loop_head = vec![false; n];
        if heat != 0 {
            for (j, insn) in insns.iter().enumerate() {
                if insn.jumps() && insn.target <= j && insns[insn.target].frame == 0 {
                    loop_head[insn.target] = true;
                }
            }
            if n > 0 && insns[0].pc == 0 {
                loop_head[0] = true;
            }
        }
        let mut i = 0;
        while i < n {
            let (Some(s), Some(st)) = (&sel[i], &ins[i]) else {
                i += 1;
                continue;
            };
            let insn = &insns[i];
            if i > 0
                && let Some(prev) = &sel[i - 1]
                && insns[i - 1].falls_through()
            {
                hot.extend(edge_conversions(&prev.out, st, &homes));
            }
            label_piece[i] = hot.len();
            if loop_head[i] {
                // Run out: leave at the header, to continue in hot code.
                let mut seq: Vec<Piece> = box_acc_piece(st.acc).into_iter().collect();
                seq.extend(exit_spills(&homes, i, &boxed));
                seq.push(Piece {
                    imm: insn.exit_record() | TIER_UP,
                    ..Piece::new("exit", 0)
                });
                exits.push((2 * n + i, seq));
                hot.push(Piece {
                    imm64: heat,
                    exit: (2 * n + i) as u32,
                    ..Piece::new("tier_count", 0)
                });
            }

            if s.base == "poly" {
                let at = exit_homes(&homes, i);
                emit_poly(i, insn, s, st, props, polys, &at, &boxed, &mut hot, &mut exits);
                names[i] = Some(format!(
                    "poly{}_{}",
                    if insn.op == Op::GetNamed {
                        "get"
                    } else {
                        "set"
                    },
                    hot[label_piece[i]..]
                        .iter()
                        .map(|p| p.stencil.stencil().name)
                        .collect::<Vec<_>>()
                        .join("+")
                ));
                i += 1;
                continue;
            }

            if let Some(f) = fusion.at(i) {
                let last = i + f.len - 1;
                let jump = if insns[last].jumps() {
                    jump_to(last, &sel[last].as_ref().unwrap().out, &mut stubs)
                } else {
                    Target::Next
                };
                let n_steps = f.steps.len();
                for (k, step) in f.steps.iter().enumerate() {
                    let piece = Piece {
                        jump: if k + 1 == n_steps { jump } else { Target::Next },
                        exit: step.insn as u32,
                        reg: step.reg,
                        ..Piece::with_imm(&step.name, step.imm)
                    };
                    if piece.uses(Sym::Exit) {
                        // Exit before the step's instruction: undo the
                        // in-place op, then `acc` is the result slot.
                        let mut seq = Vec::new();
                        if let Some((undo, imm)) = &step.undo {
                            seq.push(Piece {
                                imm: *imm,
                                ..Piece::new(undo, 0)
                            });
                        }
                        seq.push(Piece::new(step.exit_ldar.as_ref().unwrap_or(&f.ldar), 0));
                        seq.push(Piece::new("box_iacc", 0));
                        seq.extend(exit_spills(&homes, step.insn, &boxed));
                        seq.push(Piece {
                            imm: insns[step.insn].exit_record(),
                            ..Piece::new("exit", 0)
                        });
                        exits.push((step.insn, seq));
                    }
                    hot.push(piece);
                }
                if f.acc_live_after {
                    hot.push(Piece::new(&f.ldar, 0));
                }
                names[i] = Some(if f.steps.is_empty() {
                    "-".into()
                } else {
                    f.steps
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("+")
                });
                i += f.len;
                continue;
            }

            let (pre, name) = stencil_for(insn, s, st, &homes, &boxed, return_spills);
            let name = if defer_shape[i] {
                name.replacen("set_tr_nc_", "set_own_nc_", 1).replacen("set_tr_n_nc_", "set_own_n_nc_", 1)
            } else {
                name
            };
            let jump = if insn.jumps() {
                jump_to(i, &s.out, &mut stubs)
            } else {
                Target::Next
            };
            // (A `key_*` piece leaves through the instruction's exit.)
            hot.extend(pre.into_iter().map(|p| match p.exit {
                KEY_EXIT => Piece { exit: i as u32, ..p },
                _ => p,
            }));
            // A direct call to a function with upvalues: its frame's header
            // gets the function (`set_frame_func`), which the runtime's
            // calls write themselves.
            if matches!(name.as_str(), "call_direct" | "call_direct_nc" | "construct_direct")
                && let Some(Prop::Call { site, .. }) = props.get(i)
            {
                let site = unsafe { &*vm_sites[*site as usize].get() };
                if !site.callee.is_null() && !unsafe { (&(*site.callee).upvalues).is_empty() } {
                    hot.push(Piece {
                        imm64: site.closure,
                        ..Piece::with_imm(
                            "set_frame_func",
                            (8 * site.frame) as i32 - interp::FUNC_OFF as i32,
                        )
                    });
                }
            }
            // `mod_smi_pow2` takes the mask, k - 1; `mod_smi_{u,i}_s<s>` k
            // and its magic number (`mod_magic`); `mod_smi_iif` k and
            // ⌈2^64 / k⌉.
            let (imm, imm64, imm64b) = match (name.as_str(), props.get(i).copied()) {
                _ if s.ops.is_some() => s.ops.unwrap(),
                (n, _) if n.starts_with("mod_smi_pow2") => (insn.imm - 1, 0, 0),
                (n, _) if n.starts_with("mod_smi_u_s") || n.starts_with("mod_smi_i_s") => {
                    (insn.imm, mod_magic(insn.imm).1, 0)
                }
                ("mod_smi_iif", _) => (insn.imm, u64::MAX / insn.imm as u64 + 1, 0),
                ("bail", _) => (0, 0, 0),
                // `push` allocates an empty array's first elements from
                // the heap's free space.
                (n, _) if n.starts_with("array_push") => (
                    0,
                    feedback.map_or(0, |f| unsafe { (*f[0].vm).heap.bump() } as u64),
                    0,
                ),
                (_, Some(p)) if p.operands().is_some() => p.operands().unwrap(),
                (
                    _,
                    Some(Prop::Global {
                        shape, global, off, ..
                    }),
                ) => (off, shape, global),
                (_, Some(Prop::Keyed { shape, .. })) => (0, shape, 0),
                (
                    _,
                    Some(Prop::Call {
                        site,
                        epoch,
                        epoch_addr,
                        ..
                    }),
                ) => (
                    epoch as i32,
                    (forked.get(&i).copied()).unwrap_or(vm_sites[site as usize].get() as u64),
                    epoch_addr,
                ),
                _ => (insn.imm, 0, 0),
            };
            let main = Piece {
                // `push` and `pop` read the receiver's slot.
                reg: slot_reg(insn, s, &homes),
                jump,
                exit: i as u32,
                imm64,
                imm64b,
                ..Piece::with_imm(&name, imm)
            };
            // A store's check of the value against the field's type exits
            // before the instruction, like the stencil's own guard.
            let check = s.check.map(|(name, imm64, imm64b)| Piece {
                imm64,
                imm64b,
                exit: i as u32,
                ..Piece::new(name, 0)
            });
            let is_call = matches!(
                name.as_str(),
                "call" | "call_direct" | "call_direct_nc" | "construct_direct"
            );
            // The check of a call's result exits after the call too, through
            // a stub of its own, whose record says the check failed.
            let result = s.result.map(|(name, imm64)| {
                let mut seq = exit_spills(&homes, i + 1, &boxed);
                seq.push(Piece {
                    imm: insns[i + 1].exit_record() | AFTER_CALL | BAD_RESULT,
                    ..Piece::new("exit", 0)
                });
                exits.push((n + i, seq));
                Piece {
                    imm64,
                    exit: (n + i) as u32,
                    ..Piece::new(name, 0)
                }
            });
            // (An exit in the stencil's `__slow` helper uses the stub too.)
            let helper_exits = main
                .slow
                .is_some_and(|h| h.stencil().relocs.iter().any(|r| r.2 == Sym::Exit));
            if main.uses(Sym::Exit) || helper_exits || check.is_some() {
                // `add_smi_i` adds in place; undo it before anything else.
                let mut seq: Vec<Piece> = Vec::new();
                if name == "add_smi_i" {
                    seq.push(Piece {
                        imm: insn.imm,
                        ..Piece::new("undo_add_smi_i", 0)
                    });
                }
                // A call exits after it (on an error, or when the callee
                // changed the code epoch), with its result, boxed, in `acc`.
                let record = if is_call {
                    insns[i + 1].exit_record() | AFTER_CALL
                } else {
                    seq.extend(box_acc_piece(st.acc));
                    insn.exit_record() | if insn.hoisted.is_some() { HOISTED } else { 0 }
                };
                seq.extend(exit_spills(&homes, if is_call { i + 1 } else { i }, &boxed));
                if insn.exit_const != 0 {
                    seq.push(Piece {
                        imm64: insn.exit_const,
                        ..Piece::new("store_const_m", insn.reg)
                    });
                }
                seq.push(Piece {
                    imm: record,
                    ..Piece::new("exit", 0)
                });
                exits.push((i, seq));
            }
            hot.extend(check);
            // A direct call (in a VM frame, where exits' liveness is known)
            // without the `toi_rt_enter` wrapper: `arch::direct_call`, then
            // `call_done`'s checks.
            let main = match exit_live.filter(|e| e.len() == n) {
                Some(e) if matches!(name.as_str(), "call_direct" | "call_direct_nc") => {
                    let site = unsafe { &*(main.imm64 as *const CallSite) };
                    let (g, f) = live_slots(e, mats, live_sets, &homes, i, n);
                    let call = arch::direct_call(
                        &g,
                        &f,
                        main.imm64,
                        (name == "call_direct").then_some(site.f_off as u32),
                        (8 * site.frame) as u32,
                        &DirectConsts::new(),
                    );
                    let raw = |code: Vec<u8>, raws: &mut Vec<Vec<u8>>| {
                        raws.push(code);
                        Piece {
                            raw: raws.len() as u32,
                            raw_len: raws.last().unwrap().len() as u32,
                            exit: i as u32,
                            ..Piece::new("nop", 0)
                        }
                    };
                    raw_links.push((hot.len(), stubs.len(), call.hot_to_cold, call.cold_to_hot));
                    let cold = raw(call.cold, &mut raws);
                    stubs.push(vec![cold]);
                    let hot_part = raw(call.hot, &mut raws);
                    hot.push(hot_part);
                    Piece {
                        stencil: Instance::get("call_done"),
                        slow: None,
                        ..main
                    }
                }
                _ => main,
            };
            hot.push(main);
            hot.extend(result);
            if let Some(post) = s.post {
                hot.push(Piece::new(post, 0));
            }
            let mut full = match s.check {
                Some((c, ..)) => format!("{c}+{name}"),
                None => name,
            };
            for p in s.result.map(|r| r.0).into_iter().chain(s.post) {
                full = format!("{full}+{p}");
            }
            names[i] = Some(full);
            i += 1;
        }

        // The entry sequence falls through into the entry instruction when
        // that comes right after it.
        if has_entry_seq && label_piece[entry] == entry_len {
            hot[entry_len - 1].cont = Target::Next;
        }

        // Exits where objects scalar replacement didn't allocate are live
        // allocate them first, after the spills (an exit after a call: as
        // they are after it).
        if let Some(mats) = mats {
            for (k, seq) in exits.iter_mut() {
                let at = if *k >= n { *k - n + 1 } else { *k };
                let Some(ms) = mats.get(&at) else { continue };
                let pieces = ms.iter().map(|m| Piece {
                    imm64: Rc::as_ptr(m) as u64,
                    ..Piece::new("materialize", 0)
                });
                let last = seq.len() - 1;
                seq.splice(last..last, pieces);
            }
        }

        // Positions. Hot pieces back to back; then cold code: edge stubs,
        // exit stubs, helpers; then runtime-call wrappers.
        let mut pos = 0;
        let place = |pieces: &[Piece], pos: &mut usize| -> Vec<usize> {
            pieces
                .iter()
                .map(|p| {
                    let at = *pos;
                    *pos += p.len();
                    at
                })
                .collect()
        };
        // Counting (`TOI_COUNT`): each hot piece behind a counter of its
        // stencil's executions, where jumps to it and falls into it land.
        let prefix = count_prefix();
        let hot_at: Vec<usize> = hot
            .iter()
            .map(|p| {
                let at = pos;
                pos += prefix + p.len();
                at
            })
            .collect();
        let code_at: Vec<usize> = hot_at.iter().map(|&at| at + prefix).collect();
        let main_len = pos;
        let label = |i: usize| hot_at[label_piece[i]];
        let entry_at = if has_entry_seq { 0 } else { label(entry) };

        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let stub_ats: Vec<Vec<usize>> = stubs.iter().map(|s| place(s, &mut pos)).collect();
        let exit_ats: Vec<Vec<usize>> = exits.iter().map(|(_, s)| place(s, &mut pos)).collect();
        let exit_at: HashMap<usize, usize> = exits
            .iter()
            .zip(&exit_ats)
            .map(|((i, _), ats)| (*i, ats[0]))
            .collect();
        // `__slow` helpers, per hot piece: piece index -> offset.
        let mut helper_at: HashMap<usize, usize> = HashMap::default();
        let mut helper_list = Vec::new();
        for (k, p) in hot.iter().enumerate() {
            if let Some(h) = p.slow {
                helper_at.insert(k, pos);
                helper_list.push((k, h, pos));
                pos += h.stencil().code.len();
            }
        }
        // Runtime-call wrappers, saving the slot registers this region uses
        // (see `rt_call!` in the stencils). The FP accumulator counts if the
        // region ever holds a double in it.
        let uses_facc = ins.iter().flatten().any(|st| st.acc == Ty::F64)
            || sel.iter().flatten().any(|s| s.out.acc == Ty::F64);
        // Once per slot (registers may share one).
        let mut saved_gprs: Vec<u8> = homes
            .iter()
            .filter_map(|h| match h {
                Home::Gpr(k) | Home::Int(k) => arch::caller_saved_gpr_slot(*k),
                _ => None,
            })
            .collect();
        saved_gprs.sort_unstable();
        saved_gprs.dedup();
        let mut saved_fprs: Vec<u8> = uses_facc
            .then_some(0)
            .into_iter()
            .chain(homes.iter().filter_map(|h| match h {
                Home::Fpr(k) => Some(k + 1),
                _ => None,
            }))
            .collect();
        saved_fprs.sort_unstable();
        saved_fprs.dedup();
        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let mut tramp_at: HashMap<u8, usize> = HashMap::default();
        let mut wrappers: Vec<(usize, Vec<u8>, &str)> = Vec::new();
        let all_stencils = hot
            .iter()
            .chain(stubs.iter().flatten())
            .chain(exits.iter().flat_map(|(_, s)| s))
            .map(|p| p.stencil)
            .chain(helper_list.iter().map(|&(_, h, _)| h));
        for s in all_stencils {
            for r in s.stencil().relocs {
                let Sym::Rt(rt) = r.2 else { continue };
                tramp_at.entry(rt).or_insert_with(|| {
                    let name = RT_NAMES[rt as usize];
                    let code = if name == "toi_rt_enter" {
                        arch::enter_wrapper(&saved_gprs, &saved_fprs, &EnterConsts::new())
                    } else {
                        let target = runtime_function(name) as u64;
                        arch::runtime_wrapper(target, &saved_gprs, &saved_fprs)
                    };
                    let at = pos;
                    pos += code.len().next_multiple_of(8);
                    wrappers.push((at, code, name));
                    at
                });
            }
        }

        let len = pos;
        let mut mem = ExecMem::new(pos);
        let base = mem.ptr as u64;
        let buf = mem.bytes();
        for (at, code, _) in &wrappers {
            buf[*at..*at + code.len()].copy_from_slice(code);
        }
        // Profiling: with `TOI_PERF_MAP` set, name every piece in
        // `/tmp/perf-<pid>.map`, so `perf report` shows time per stencil.
        if std::env::var_os("TOI_PERF_MAP").is_some() {
            let mut map = String::new();
            // Empty pieces (`nop`) get no symbol: perf would give them the
            // samples of the piece at the same address.
            let mut sym = |at: usize, len: usize, name: &str| {
                if len > 0 {
                    map.push_str(&format!("{:x} {len:x} jit:{name}\n", base as usize + at));
                }
            };
            for (p, &at) in hot.iter().zip(&code_at) {
                let name = if p.raw != 0 { "direct_call" } else { p.stencil.stencil().name };
                sym(at, p.len(), name);
            }
            for (pieces, ats) in stubs
                .iter()
                .zip(&stub_ats)
                .chain(exits.iter().map(|(_, s)| s).zip(&exit_ats))
            {
                for (p, &at) in pieces.iter().zip(ats) {
                    let name = if p.raw != 0 { "direct_call" } else { p.stencil.stencil().name };
                    sym(at, p.len(), &format!("cold:{name}"));
                }
            }
            for &(_, h, at) in &helper_list {
                sym(at, h.stencil().code.len(), h.stencil().name);
            }
            for (at, code, name) in &wrappers {
                sym(*at, code.len(), &format!("wrapper:{name}"));
            }
            use std::io::Write;
            let path = format!("/tmp/perf-{}.map", std::process::id());
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            f.write_all(map.as_bytes()).unwrap();
        }

        let resolve = |t: Target, next: usize| match t {
            Target::Next => next,
            Target::Label(i) => label(i as usize),
            Target::Stub(s) => stub_ats[s as usize][0],
            Target::Piece(k) => hot_at[k as usize],
            Target::AfterPiece(k) => code_at[k as usize] + hot[k as usize].len(),
        };
        // Copy the first `len` bytes of `s` to `at` and patch it: register
        // fields of a family template, operands from `p`, continuation and
        // branch target at the given offsets, and the helper of hot piece
        // `key`.
        let raw_pieces = hot.iter().zip(&code_at).chain(
            stubs
                .iter()
                .zip(&stub_ats)
                .flat_map(|(s, ats)| s.iter().zip(ats)),
        );
        for (p, &at) in raw_pieces.filter(|(p, _)| p.raw != 0) {
            let code = &raws[p.raw as usize - 1];
            buf[at..at + code.len()].copy_from_slice(code);
        }
        for (k, s, to_cold, to_hot) in &raw_links {
            let (h, c) = (code_at[*k], stub_ats[*s][0]);
            for &(f, t) in to_cold {
                arch::link_raw(buf, h + f, c + t);
            }
            for &(f, t) in to_hot {
                arch::link_raw(buf, c + f, h + t);
            }
        }
        let mut emit = |s: Instance,
                        len: usize,
                        at: usize,
                        p: &Piece,
                        cont: usize,
                        jump: usize,
                        key: Option<usize>| {
            s.copy_to(&mut buf[at..], len);
            for r in s.stencil().relocs.iter().filter(|r| (r.0 as usize) < len) {
                let value = match r.2 {
                    Sym::HoleImm => p.imm as u32 as u64,
                    // A cell, or an address in one, that the code embeds
                    // (a folded constant, a callee it checks for, a
                    // prototype's slot): kept alive (see `gc`).
                    Sym::HoleImm64 | Sym::HoleImm64b => {
                        let v = if r.2 == Sym::HoleImm64 { p.imm64 } else { p.imm64b };
                        let vm = crate::interp::current_vm();
                        if !vm.is_null() {
                            unsafe { (*vm).heap.keep(v as usize) };
                        }
                        v
                    }
                    Sym::HoleReg => p.reg as u64 * size_of::<Value>() as u64,
                    Sym::Continue => base + cont as u64,
                    Sym::JumpTarget => base + jump as u64,
                    Sym::Exit => base + exit_at[&(p.exit as usize)] as u64,
                    Sym::Rt(rt) => base + tramp_at[&rt] as u64,
                    Sym::Slow => {
                        let k = key.expect("helper reference outside a hot piece");
                        base + helper_at[&k] as u64
                    }
                };
                let field = at + r.0 as usize;
                // arm64: a 64-bit hole without a fourth part (`imm64_of!`)
                // holds an address, 48 bits; `imm64_full_of!` has the part.
                if cfg!(target_arch = "aarch64") && r.1 == 268 {
                    let relocs = s.stencil().relocs;
                    let has_g3 = relocs.iter().any(|q| q.0 == r.0 + 4 && q.1 == 269 && q.2 == r.2);
                    assert!(has_g3 || value >> 48 == 0, "64-bit hole {value:#x} past 48 bits");
                }
                arch::patch(
                    &mut buf[field..],
                    r.1 as u32,
                    value,
                    r.3 as i64,
                    base + field as u64,
                );
            }
        };
        // Resolved continuation and branch target of each hot piece; its
        // helpers share them.
        let hot_targets: Vec<(usize, usize)> = hot
            .iter()
            .zip(&code_at)
            .map(|(p, &at)| (resolve(p.cont, at + p.len()), resolve(p.jump, at + p.len())))
            .collect();
        for (k, p) in hot.iter().enumerate() {
            if p.raw != 0 {
                continue;
            }
            let (cont, jump) = hot_targets[k];
            emit(p.stencil, p.len(), code_at[k], p, cont, jump, Some(k));
        }
        for &(k, h, at) in &helper_list {
            let (cont, jump) = hot_targets[k];
            emit(h, h.stencil().code.len(), at, &hot[k], cont, jump, Some(k));
        }
        let cold = stubs
            .iter()
            .zip(&stub_ats)
            .chain(exits.iter().map(|(_, s)| s).zip(&exit_ats));
        for (seq, ats) in cold {
            for (p, &at) in seq.iter().zip(ats).filter(|(p, _)| p.raw == 0) {
                emit(
                    p.stencil,
                    p.len(),
                    at,
                    p,
                    resolve(p.cont, at + p.len()),
                    0,
                    None,
                );
            }
        }
        if prefix > 0 {
            for (p, &at) in hot.iter().zip(&hot_at) {
                let name = if p.raw != 0 { "direct_call" } else { p.stencil.stencil().name };
                arch::count_prefix(&mut buf[at..at + prefix], piece_counter(name, p.len()));
            }
        }

        mem.seal();
        let ret = returned(insns, &ins, false);
        if !root.is_null() {
            unsafe { (*root).ret_hint.set(Ty::of_result(ret)) };
        }
        let code = JitCode {
            mem,
            entry: entry_at,
            main_len,
            len,
            stencils: names,
            homes,
            ret,
            self_sites,
            forks,
            snap: None,
        };
        for &k in &code.self_sites {
            unsafe { (*vm_sites[k].get()).entry = code.entry_addr() };
        }
        code
    }

    /// Run from the entry point over `regs`, starting with `acc`.
    ///
    /// # Safety
    /// `regs` must cover every register operand, and the code must have been
    /// compiled for the current types of `acc` and `regs`.
    unsafe fn enter(&self, acc: Value, regs: &mut [Value]) -> Outcome {
        let mut exit = NO_EXIT;
        let acc = unsafe {
            call_entry(
                self.mem.ptr.add(self.entry),
                acc,
                regs.as_mut_ptr(),
                &mut exit,
            )
        };
        match exit {
            NO_EXIT => Outcome::Return(acc),
            pc => Outcome::Exit {
                pc: pc as usize,
                acc,
            },
        }
    }

    /// Debugging: with `TOI_JIT_LOG` set, print the stencils of a VM
    /// function's code, entered at bytecode offset `pc`, and its size in
    /// bytes: hot code, and all of it (`scripts/sizes.sh` adds these up).
    fn log(&self, function: *const crate::vm::Code, pc: usize) {
        if std::env::var_os("TOI_JIT_LOG").is_some() {
            let names: Vec<&str> = self.stencils.iter().flatten().map(String::as_str).collect();
            eprintln!(
                "jit {} @{pc} ({} hot, {} bytes): {}",
                unsafe { &(*function).name },
                self.main_len,
                self.len,
                names.join(" ")
            );
        }
    }

    /// Share the code, telling the call sites bound to it where it is.
    fn into_rc(self) -> Rc<JitCode> {
        let code = Rc::new(self);
        if let Some(snap) = &code.snap {
            for &k in &code.self_sites {
                unsafe { (*snap.sites[k].get()).self_code = Rc::as_ptr(&code) };
            }
        }
        code
    }

    /// The address to call the code at (see `call_direct`).
    fn entry_addr(&self) -> u64 {
        self.mem.ptr as u64 + self.entry as u64
    }

    /// The hot machine code, for inspection.
    pub fn main_code(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.mem.ptr, self.main_len) }
    }
}

// ---- Driver ----

/// Counters, for tests and benchmarks.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct JitStats {
    pub compiles: u32,
    pub exits: u32,
    /// Interpreter runs that tiered up into JIT code at a loop header.
    pub osr_entries: u32,
    /// Runs (or parts of runs) spent in the interpreter.
    pub interp_runs: u32,
    /// Times an entry point became megamorphic and its versions were
    /// replaced by one generalized version.
    pub generalizations: u32,
}

/// When to tier up from the interpreter. The default: 1000 loop back edges
/// or 10 calls. At the interpreter's ~16 cycles per loop iteration, 1000
/// iterations cost a few µs, less than compiling a small function.
#[derive(Clone, Copy, Debug)]
pub struct Tiering {
    /// Loop back edges run in the interpreter, over the function's lifetime,
    /// before compiling at the loop header.
    pub back_edges: i64,
    /// Calls run in the interpreter before compiling at entry.
    pub calls: u32,
}

impl Default for Tiering {
    fn default() -> Tiering {
        Tiering {
            back_edges: 1000,
            calls: 10,
        }
    }
}

/// Versions kept per entry point before generalizing.
const MAX_VERSIONS: usize = 4;

/// A bytecode function run through the JIT.
///
/// Code is compiled for an entry point (an instruction) and the types of the
/// values live there. Entry types are facts, not speculation: inside the
/// region types only widen, and edges get conversions. The only speculation
/// is that int32 arithmetic doesn't overflow. When it does, the stencil
/// side-exits before the instruction; the driver records the instruction as
/// widened (it will produce a double), invalidates all versions, recompiles
/// for entry at that instruction with the current types, and resumes there.
///
/// **Versions** (milestone 4). Compiled code is cached per entry point,
/// keyed by its entry types with dead registers ignored. A version fits a
/// later entry if every live value's type is the same or narrower (an int32
/// where it expects a double is converted, in the register file, before
/// entering; JS numbers are just numbers). This check in the driver replaces
/// entry guards. After [`MAX_VERSIONS`] versions at one entry point, it is
/// megamorphic: one version for the join of all types seen replaces them.
///
/// **Tiering** (the default; see [`Tiering`]). The function starts in the
/// interpreter. Loop opcodes count back edges; when the budget runs out, the
/// interpreter leaves at the loop header (on-stack replacement) and the run
/// continues in code compiled there. After enough calls, later calls start
/// in JIT code. [`JitFunction::eager`] compiles on entry instead.
/// The instructions reachable from an entry instruction `entry`, which
/// registers are live into each and whether `acc` is; and which registers
/// and whether `acc` are live at `entry` itself.
struct EntryLive {
    reach: Vec<bool>,
    regs: Vec<RegSet>,
    acc: Vec<bool>,
    acc_at_entry: bool,
    at_entry: Vec<bool>,
    /// The registers of `at_entry`, in order (an entry checks only these).
    live_list: Vec<u8>,
    /// Of `at_entry`, those compiled code reads (see [`reg_read_in`]).
    read_at_entry: Vec<bool>,
}

impl EntryLive {
    fn new(insns: &[Insn], entry: usize, nregs: usize, frames: &[FrameRegs]) -> EntryLive {
        let reach = reachable(insns, entry);
        let regs = reg_live_in(insns, &reach, nregs, frames);
        let acc = acc_liveness(insns, &reach).0;
        let at_entry: Vec<bool> = (0..nregs).map(|r| rs_has(&regs[entry], r)).collect();
        let live_list = (0..nregs).filter(|&r| at_entry[r]).map(|r| r as u8).collect();
        let read = reg_read_in(insns, &reach, nregs);
        let read_at_entry = (0..nregs).map(|r| rs_has(&read[entry], r)).collect();
        EntryLive {
            read_at_entry,
            acc_at_entry: acc[entry],
            at_entry,
            live_list,
            reach,
            regs,
            acc,
        }
    }
}

pub struct JitFunction {
    code: Vec<u8>,
    /// The lowered instructions (shared with the [`Snapshot`]).
    insns: Rc<[Insn]>,
    nregs: usize,
    opts: Options,
    widened: HashSet<usize>,
    /// Compiled versions per entry instruction, with their entry types.
    versions: HashMap<usize, Vec<(TypeState, Rc<JitCode>)>>,
    /// Liveness for entering at an instruction, which compiling there
    /// uses too.
    live_at: HashMap<usize, Rc<EntryLive>>,
    /// The most recently used version: (entry instruction, index).
    last: Option<(usize, usize)>,
    tiering: Option<Tiering>,
    /// Back edges left before tiering up, and calls so far.
    back_edges_left: i64,
    calls: u32,
    pub stats: JitStats,
    /// For a function of a VM program (see [`JitFunction::for_vm`]).
    vm: Option<Box<VmState>>,
}

/// After this many compilations in one run, finish in the interpreter.
const MAX_COMPILES: u32 = 16;

/// A function's decoded instructions, with accumulator liveness after each.
type Decoded = Rc<(Vec<Insn>, Vec<bool>)>;

/// What the JIT keeps for a function in VM frames.
// The call sites are boxed: compiled code holds their addresses.
#[allow(clippy::vec_box)]
/// What lowering works out about functions' bytecode (which doesn't
/// change), kept by the VM for every function's lowerings (see
/// `Vm::jit_caches`).
#[derive(Default)]
pub(crate) struct Caches {
    /// Code call sites were bound to before, kept alive (see
    /// `CallSite::bind`).
    retired: Vec<Rc<JitCode>>,
    /// Decoded code of functions inlined, with accumulator liveness after
    /// each instruction.
    decoded: HashMap<*const crate::vm::Code, Decoded>,
    /// Per callee of a direct call, its registers live at entry (see
    /// [`Lowering::entry_live`]).
    entry_live: HashMap<*const crate::vm::Code, Rc<Vec<bool>>>,
    /// Per function inlined, its registers live into each instruction as
    /// the interpreter runs it, by bytecode offset (see [`interp_live`]).
    pc_live: HashMap<*const crate::vm::Code, Rc<PcLive>>,
}

impl Caches {
    /// Function `code` decoded, with accumulator liveness after each
    /// instruction.
    fn decoded(&mut self, code: *const crate::vm::Code) -> Decoded {
        self.decoded
            .entry(code)
            .or_insert_with(|| {
                let insns = decode(unsafe { &(*code).bytecode }, true);
                let live = acc_liveness(&insns, &vec![true; insns.len()]).1;
                Rc::new((insns, live))
            })
            .clone()
    }
}

struct VmState {
    /// The function's code, and the VM.
    root: *const crate::vm::Code,
    vm: *mut crate::vm::Vm,
    /// How many instructions the function decodes to (`JitFunction::insns`
    /// holds the lowered ones).
    root_len: usize,
    /// The current lowering (see [`lower`]): per instruction its inline
    /// cache's contents, the frames, their feedback, the runtime call
    /// sites, and where inlined frames' registers start.
    props: Vec<Prop>,
    polys: Vec<Cases>,
    fbs: Vec<Feedback>,
    snap: Rc<Snapshot>,
    inline_base: usize,
    direct_base: usize,
    /// The lowering needs redoing: compiled code exited (its caches may
    /// have changed since), or it was never done. Entries otherwise skip
    /// it, which matters for functions entered at every call.
    dirty: bool,
    /// The prototype epoch the versions were compiled under: a prototype's
    /// slot addresses are only valid while it holds.
    epoch: u32,
    /// The field epoch they were compiled under: the field types they
    /// assume hold until it changes.
    field_epoch: u32,
    /// Per instruction origin (code, bytecode offset), as the interpreter
    /// sees it: int32 operations that overflowed, exits at property
    /// accesses (past [`MAX_PROPERTY_EXITS`] given up), and exits at the
    /// guard of an inlined call (past it, the call isn't inlined). In
    /// `widened` too: calls whose result failed its predicted type's check
    /// (no prediction since).
    widened: HashSet<(usize, usize)>,
    property_exits: HashMap<(usize, usize), u32>,
    callee_exits: HashMap<(usize, usize), u32>,
    /// Exits through a bail per entry; past [`MAX_BAIL_EXITS`] the entry is
    /// left to the interpreter.
    bail_exits: HashMap<usize, u32>,
    no_entry: HashSet<usize>,
    /// Calls whose result their prediction missed (see `vm_finish`).
    bad_results: u32,
    /// Call entries left to the interpreter after a property or callee
    /// exit ([`COOL_ENTRIES`]), letting inline caches settle before the
    /// function is lowered again.
    cool: u32,
    /// The function's inline-cache misses (`Code::ic_misses`) when the
    /// cooling window began, and how many times it was extended: a window
    /// in which the caches missed again is followed by another, longer
    /// one (up to [`MAX_SETTLE`] times), so code is compiled again once
    /// its feedback settles.
    settle_mark: u32,
    settles: u32,
    /// Coolings so far: each one is longer ([`cooling`]).
    coolings: u32,
    /// Warm or hot, and for warm, what is left before hot (its heat):
    /// calls and loop back edges, counted down by warm code at its start
    /// and its loop headers (`tier_count`), at a stable address.
    tier: Tier,
    heat: Box<i64>,
    /// Whether the first compile chose the tier (see `inlining_pays`).
    classified: bool,
}

impl VmState {
    /// For compiling warm code, its heat's address; 0 when hot.
    fn heat_addr(&self) -> u64 {
        match self.tier {
            Tier::Warm => &raw const *self.heat as u64,
            Tier::Hot => 0,
        }
    }
}

/// Whether the function `code` calls or constructs, as its inline caches
/// recorded, a function small enough to inline and without loops: what
/// hot code does better than warm code (a method or constructor call costs
/// tens of times what its inlined body does). Calls to functions that loop
/// gain little from being inlined: warm code calls their code directly.
unsafe fn inlining_pays(code: *const crate::vm::Code) -> bool {
    let c = unsafe { &*code };
    let ics: *const crate::vm::Ic = c.ics.as_ptr().cast();
    decode(&c.bytecode, true).iter().any(|i| {
        if i.bail || !matches!(i.op, Op::Call | Op::CallUndefined | Op::Construct) {
            return false;
        }
        let fo = unsafe { (*ics.add(i.ic as usize * crate::vm::IC_WAYS)).holder };
        if fo.is_null() || unsafe { (*fo).class } != crate::object::Class::Function {
            return false;
        }
        let callee = decode(unsafe { &(*(*fo).code).bytecode }, true);
        let loops = callee
            .iter()
            .enumerate()
            .any(|(k, j)| j.jumps() && j.target <= k);
        callee.len() <= MAX_INLINE_INSNS && !loops
    })
}

/// How a function in VM frames is compiled, with the warm tier on
/// (`Vm::set_warm_tier`, `TOI_WARM`; off by default). **Warm**: the
/// function alone (no inlining; direct calls only into code compiled
/// already), its start and loop headers counting down its heat
/// ([`HOT_HEAT`], `tier_count`). **Hot**: everything (inlining, direct
/// calls, constant folding and checks moved out of loops, scalar
/// replacement, registers sharing slots). A function starts hot if
/// inlining is what hot code would bring ([`inlining_pays`]), else warm;
/// warm code running out of heat leaves at its start or a loop header, and
/// the run continues there in hot code (`vm_run`).
///
/// Off by default: on the microbenchmarks every function ends hot, so a
/// warm stage adds a compile and slower code before it (83 +6%, 30 +6%,
/// 68 +4%; 91 -5%). It is for code that is called often enough to compile
/// but never gets hot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    Warm,
    Hot,
}

const HOT_HEAT: i64 = 1_000;

const MAX_PROPERTY_EXITS: u32 = 4;
const MAX_BAIL_EXITS: u32 = 3;
/// Compilations per function in VM frames, over the program's run.
const MAX_VM_COMPILES: u32 = 32;

/// The compilations a function lowered to `n` instructions gets: up to
/// [`MAX_VM_COMPILES`], fewer past [`COMPILE_SIZE`] instructions, as each
/// costs about its size (at least 4). A large function whose feedback
/// keeps changing (TypeScript's scanner and emitter, which compiling
/// functions with upvalues made eligible) otherwise compiles 32 times.
/// Measured (scores against no limit): TypeScript +7%, zlib +9%, Box2D
/// +21%, Mandreel +15%, DeltaBlue and GameBoy unchanged; with 200, zlib
/// and GameBoy lost half.
fn max_compiles(n: usize) -> u32 {
    (MAX_VM_COMPILES as usize * COMPILE_SIZE / n.max(1)).clamp(4, MAX_VM_COMPILES as usize) as u32
}

/// See [`max_compiles`].
const COMPILE_SIZE: usize = 400;
/// Inlining limits: nesting, and the callee's size in instructions.
const MAX_INLINE_DEPTH: usize = 2;
const MAX_INLINE_INSNS: usize = 64;
/// Bytecode instructions a lowering inlines at call sites outside loops
/// (small leaves aside): past it, those calls stay calls (direct ones,
/// cheap now). Inlined chains of polymorphic methods outside loops were
/// most of DeltaBlue's compiled code (833 KB, 5.4 KB per compile; 66 of
/// its 74 ms for 10 runs went to compiling). Measured (perf.sh --ab):
/// 87 -48%, 85 -27%, 71 -8%; 91 +4%, 80 and 82 +2%. Without exempting
/// calls in loops, 80 lost 80%; 60 cost 91 17%.
const INLINE_BUDGET: usize = 80;
/// Functions inlined at any depth: calling nothing and at most this long.
const MAX_LEAF_INSNS: usize = 16;

/// A frame of compiled code: frame 0 is the function itself, the others
/// functions inlined into it (see [`lower`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct InlineFrame {
    /// Inlined for `new`: the interpreter's frame for it, if it exits
    /// there, is a `new` one (its `Return` gives `this`).
    construct: bool,
    code: *const crate::vm::Code,
    /// The function object it runs, if known (an inlined frame's callee,
    /// checked): an exit writes it into the frame's header, for its
    /// upvalues.
    func: *mut Object,
    /// Where its registers start, in registers from the compiled frame's.
    base: usize,
    parent: usize,
    /// Where the parent resumes after the call, in its bytecode.
    ret_pc: usize,
}

/// A call compiled code makes (`call` and `call_direct` stencils): `op`'s
/// operands, renamed into the compiled frame, and where a callee's frame
/// goes (past every register the compiled code uses). For a direct call,
/// the callee's compiled entry and what `call_direct` checks before calling
/// it: the first fields, which it reads (`DirectSite` in
/// `stencils/stencils.rs`).
#[repr(C)]
pub(crate) struct CallSite {
    /// Entry address of the callee's compiled code; 0: always call through
    /// the runtime.
    entry: u64,
    /// The function compiled: the callee register must hold it.
    closure: u64,
    /// The callee register's offset in bytes.
    f_off: u64,
    /// Where the callee's compiled frame (registers of its inlined frames
    /// included) ends, in bytes from the caller's registers: it must end
    /// before `stack_end`.
    end_off: u64,
    stack_end: u64,
    /// The VM's native stack limit (see `interp::native_stack_low`).
    native_limit: *const usize,
    /// For `new`: the shape `this` is allocated with, which the callee's
    /// version is bound for (0 if none is known).
    this_shape: u64,
    /// For a direct `new`: what `construct_direct` allocates `this` with.
    ctor: Option<Box<CtorSite>>,
    vm: *mut crate::vm::Vm,
    op: Op,
    f: usize,
    w: usize,
    argc: usize,
    frame: usize,
    nparams: u32,
    /// Exits of the callee's code at this site: past [`MAX_DIRECT_EXITS`],
    /// calls go through the runtime (`entry` = 0) until the callee is
    /// compiled again (`retry`).
    exits: u32,
    /// When calls went through the runtime for exits: the callee's compile
    /// count then. Once it has compiled since (with the feedback the exits
    /// gave it), the site is bound again (see [`toi_rt_call`]).
    retry: u32,
    /// The callee's code and compiled version, for a direct call.
    callee: *const crate::vm::Code,
    /// The function the call's inline cache names (bytecode or native), if
    /// any: a call through the runtime predicts its result from it.
    cached: *const crate::vm::Code,
    target: Option<Rc<JitCode>>,
    /// The callee is the function compiled: bound (if at all) to the code
    /// making the call, `self_code`, which `target` can't own.
    self_call: bool,
    self_code: *const JitCode,
    /// Bound to a version (`target` or `self_code`), for good.
    bound: bool,
    /// For a call to another function: the types of `this` and the
    /// arguments the site was claimed with, by the first caller version
    /// compiled to call directly. Only versions passing the same types may
    /// call directly, as the callee's version is bound for those.
    claim: Option<(Ty, Vec<Ty>)>,
    /// Claimed, but the callee was being compiled (mutual recursion): bind
    /// at a call ([`toi_rt_call`]).
    pending: bool,
}

/// How `construct_direct` allocates `this` for a direct `new` (read by the
/// stencil as `CtorAlloc`): from the VM heap's free space, with the root
/// shape the callee's version is bound for (`CallSite::this_shape`), as
/// long as what that root depends on (`Vm::ctor_root`) is unchanged: the
/// constructor's `prototype` slot and its objects' in-object capacity.
/// Otherwise, or when the heap's chunk is full, `toi_rt_construct`
/// allocates.
#[repr(C)]
#[derive(Clone, PartialEq)]
struct CtorSite {
    /// The heap's `ptr` and `end` (`Heap::bump`).
    heap: *mut [*mut u64; 2],
    /// The constructor's `prototype` slot, and its value then.
    proto_at: *const u64,
    proto: u64,
    /// The constructor's `Code::ctor_capacity`, and its value then.
    cap_at: *const u32,
    cap: u64,
    /// In-object slots of the shape, set to `undefined`.
    slots: u64,
    /// Where `this` goes: the callee frame's `r0`, in bytes from the
    /// caller's registers.
    this_off: u64,
    /// A shape for which `Vm::construct_done` has nothing to do: the
    /// constructor's objects end with it (see `toi_rt_construct_done`).
    done_shape: u64,
    /// The header of `this`.
    header: [u64; crate::object::INLINE_BASE / 8],
}

/// Whether two lowerings' instructions are the same. An inlined `new`'s
/// allocation site (`Insn::alloc`) is boxed afresh by each lowering: it is
/// compared by what it allocates.
fn same_insns(a: &[Insn], b: &[Insn]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            if x.alloc == y.alloc || x.alloc == 0 || y.alloc == 0 {
                return x == y;
            }
            let (s, t) = unsafe {
                (
                    &*(*(x.alloc as *const std::cell::UnsafeCell<CallSite>)).get(),
                    &*(*(y.alloc as *const std::cell::UnsafeCell<CallSite>)).get(),
                )
            };
            Insn { alloc: 0, ..*x } == Insn { alloc: 0, ..*y }
                && (s.closure, s.this_shape, s.callee) == (t.closure, t.this_shape, t.callee)
                && s.ctor.is_some()
                && s.ctor == t.ctor
        })
}

/// Call entries a function leaves to the interpreter after a property or
/// callee exit (see `VmState::cool`). Measured on DeltaBlue (10 runs): no
/// cooling 101 ms, 50 entries 77, 100 entries 72; doubling it at each new
/// lowering changed little.
const COOL_ENTRIES: u32 = 100;
/// Cooling windows added while a function's caches keep missing (see
/// `VmState::settle_mark`), for functions of at least [`SETTLE_SIZE`]
/// instructions. Measured (scores, against none): pdfjs +8%, TypeScript
/// +1%; for every function, DeltaBlue -2.5% (its methods are small).
const MAX_SETTLE: u32 = 4;
const SETTLE_SIZE: usize = 200;
/// Measured on 87-constraint-solver, whose phases change its types (compile
/// ms; 0: 26.6, 2: 24.5, 4 and 6: 25.5).
const MAX_COOL_SHIFT: u32 = 2;

/// The call entries of a function's next cooling, given how many it has
/// had (`coolings`, counted here): [`COOL_ENTRIES`], doubled at each
/// cooling up to [`MAX_COOL_SHIFT`] times, so a function whose code keeps
/// exiting is compiled again less often. Loop entries don't cool: a loop
/// left to the interpreter costs more than compiling it (87 +27% with
/// them cooling too).
fn cooling(coolings: &mut u32) -> u32 {
    let n = *coolings;
    *coolings += 1;
    COOL_ENTRIES << n.min(MAX_COOL_SHIFT)
}

/// How many times `callee` has been compiled (0 if it is being compiled
/// now: its state is borrowed).
fn callee_compiles(callee: *const crate::vm::Code) -> u32 {
    unsafe { (*callee).jit.try_borrow() }
        .ok()
        .and_then(|f| f.as_ref().map(|f| f.stats.compiles))
        .unwrap_or(0)
}

/// Exits of a direct callee before its call site stops calling directly.
const MAX_DIRECT_EXITS: u32 = 4;

/// Claim direct-call candidate `site` for a caller version passing `this`
/// and arguments of the types `this` and `args` (the caller's types when
/// compiling: facts, like entry types), and bind it to a version of its
/// callee entered with them; the callee's other live registers hold
/// `undefined`. True if the caller version may call directly:
///
/// - the site is unclaimed, and there is such a version, or the callee is
///   being compiled (mutual recursion: its state is borrowed), so that
///   binding waits for a call (`pending`);
/// - or the site was claimed with the same types.
///
/// False if it was claimed with other types (by another version of the
/// caller), or the callee won't compile: the call then goes through the
/// runtime.
///
/// # Safety
/// `site` must be a live call site of a VM function.
unsafe fn bind_direct(site: *mut CallSite, this: Ty, args: &[Ty]) -> bool {
    let site = unsafe { &mut *site };
    if let Some((t, a)) = &site.claim {
        return *t == this && a == args;
    }
    match unsafe { direct_version(site, this, args) } {
        Version::Ready(code, nregs) => {
            site.bind(code, nregs);
            site.claim = Some((this, args.to_vec()));
            true
        }
        Version::Busy => {
            site.claim = Some((this, args.to_vec()));
            site.pending = true;
            true
        }
        Version::None => false,
    }
}

impl CallSite {
    /// The site unbound and unclaimed, for a caller version passing other
    /// types than the one that claimed it (see `JitCode::forks`).
    fn fork(&self) -> CallSite {
        CallSite {
            entry: 0,
            ctor: self.ctor.clone(),
            exits: 0,
            target: None,
            self_code: std::ptr::null(),
            bound: false,
            claim: None,
            pending: false,
            retry: 0,
            ..*self
        }
    }

    fn bind(&mut self, code: Rc<JitCode>, nregs: usize) {
        self.entry = code.entry_addr();
        // (The version's own frame: it zeroes its dead registers at entry.)
        let nregs = nregs.max(code.homes.len());
        self.end_off = 8 * (self.frame + nregs + interp::HEADER) as u64;
        // The code bound before may be running: a direct call into it is
        // on the native stack below this rebinding (a pending site bound
        // by `toi_rt_call` from inside its callee, say). It stays alive
        // with the VM, as dropping it would free code returned into.
        if let Some(old) = self.target.replace(code) {
            unsafe { (*self.vm).jit_caches.retired.push(old) };
        }
        self.bound = true;
    }

    /// A pending site (see [`bind_direct`]): bind it if its callee isn't
    /// being compiled anymore.
    unsafe fn bind_pending(&mut self) {
        let (this, args) = self.claim.clone().expect("a claimed site");
        match unsafe { direct_version(self, this, &args) } {
            Version::Ready(code, nregs) => {
                self.bind(code, nregs);
                self.pending = false;
            }
            Version::Busy => {}
            Version::None => self.pending = false,
        }
    }
}

/// The prediction for the result of a call through the runtime to the
/// function `cached` (null: unknown) with `op`: for `new`, an object of the
/// shape the constructor's objects have had (`Code::ctor_result`); for a
/// call, the latest prediction of a bytecode function for its results, or
/// a native's declared type (`Code::ret_hint`). Checked after the call.
///
/// # Safety
/// `cached` must be null or a live function's code.
unsafe fn runtime_result(cached: *const crate::vm::Code, op: Op) -> Ty {
    if cached.is_null() {
        return Ty::Any;
    }
    let code = unsafe { &*cached };
    if op == Op::Construct {
        let shape = code.ctor_result.get();
        if shape.is_null() || shape == crate::vm::MANY_SHAPES {
            Ty::Any
        } else {
            Ty::Obj(shape as u64)
        }
    } else {
        code.ret_hint.get()
    }
}

/// What [`direct_version`] found.
enum Version {
    Ready(Rc<JitCode>, usize),
    /// The callee's state is borrowed: it is being compiled.
    Busy,
    /// The callee won't compile.
    None,
}

/// The version of direct-call candidate `site`'s callee that [`bind_direct`]
/// would bind it to, with the register count of its frame; compiled if
/// needed.
///
/// # Safety
/// As [`bind_direct`].
unsafe fn direct_version(site: *const CallSite, this: Ty, args: &[Ty]) -> Version {
    let site = unsafe { &*site };
    let (callee, vm) = (site.callee, site.vm);
    let Ok(mut slot) = (unsafe { (*callee).jit.try_borrow_mut() }) else {
        return Version::Busy;
    };
    let f = slot.get_or_insert_with(|| Box::new(JitFunction::for_vm(callee, vm)));
    // Cooling after an exit: binding waits (calls meanwhile go through the
    // interpreter, which counts the cooling down).
    if let Some(v) = &f.vm
        && v.dirty
        && v.cool > 0
    {
        return Version::Busy;
    }
    let nparams = site.nparams as usize;
    let make_state = |live: &[bool], acc_live: bool| {
        let ty = |r: usize| match r {
            0 => this,
            r if r <= nparams => args.get(r - 1).copied().unwrap_or(Ty::Any),
            _ => Ty::Any,
        };
        TypeState {
            acc: if acc_live { Ty::Any } else { Ty::Bot },
            regs: Regs::from_dense(
                (live.iter().enumerate()).map(|(r, &l)| if l { ty(r) } else { Ty::Bot }),
            ),
            same: Copies::new(),
            acc_same: None,
        }
    };
    let field_epoch = unsafe { (*vm).field_epoch.wrapping_add((*site.callee).field_deps.get()) };
    match unsafe { f.entry_version((*vm).proto_epoch, field_epoch, make_state) } {
        Some((code, nregs)) => Version::Ready(code, nregs),
        None => Version::None,
    }
}

/// A lowering as compiled (see [`JitCode::snap`]).
#[allow(clippy::vec_box)]
pub(crate) struct Snapshot {
    insns: Rc<[Insn]>,
    /// See [`Lowered::mats`].
    mats: HashMap<usize, Vec<Rc<Mat>>>,
    frames: Vec<InlineFrame>,
    /// Per frame, (first register, register count, parent frame): the
    /// registers an exit from it leaves to the interpreter (see
    /// [`reg_live_in`]).
    frame_regs: Vec<FrameRegs>,
    /// See [`Lowered::aliases`]: an exit copies each argument into its
    /// parameter, for the interpreter.
    aliases: Vec<(u8, u8, u8)>,
    restores: Vec<Restore>,
    exit_live: Vec<RegSet>,
    /// Owned here so they live as long as code pointing to them.
    sites: Vec<Box<std::cell::UnsafeCell<CallSite>>>,
}

/// Emit polymorphic access `i` ([`Prop::Poly`]): a store's boxing and
/// check, the receiver's cell check, one compare per shape, branching to
/// that shape's access (the last one's exits on a mismatch and falls
/// through to its own), the accesses, and the load's `post`. The accesses
/// but the last laid out jump past it.
#[allow(clippy::too_many_arguments)]
fn emit_poly(
    i: usize,
    insn: &Insn,
    s: &Sel,
    st: &TypeState,
    props: &[Prop],
    polys: &[Cases],
    homes: &[Home],
    boxed: &[bool],
    hot: &mut Vec<Piece>,
    exits: &mut Vec<(usize, Vec<Piece>)>,
) {
    let Some(Prop::Poly { n, at }) = props.get(i).copied() else {
        unreachable!("a polymorphic selection without its cases")
    };
    let cases = poly_cases(polys, n, at);
    let m = cases.len();
    let load = insn.op == Op::GetNamed;
    let loc = homes
        .get(insn.reg as usize)
        .copied()
        .unwrap_or(Home::Mem)
        .loc();
    let at = |name: &str| Piece {
        reg: insn.reg,
        exit: i as u32,
        ..Piece::new(&format!("{name}_{loc}"), 0)
    };
    if !load {
        hot.extend(box_acc_piece(st.acc));
    }
    if let Some((c, imm64, imm64b)) = s.check {
        hot.push(Piece {
            imm64,
            imm64b,
            exit: i as u32,
            ..Piece::new(c, 0)
        });
    }
    hot.push(at("guard_cell"));
    // Only the dispatch for a load whose value is unused: a match jumps
    // past the last compare.
    if insn.check_only {
        let last = hot.len() + m - 1;
        for (c, case) in cases.iter().enumerate() {
            let is_last = c + 1 == m;
            hot.push(Piece {
                imm64: case.base(),
                jump: if is_last {
                    Target::Next
                } else {
                    Target::AfterPiece(last as u32)
                },
                ..at(&case.guard(if is_last { "guard" } else { "jeq" }))
            });
        }
    } else {
        // The distinct accesses, the last case's first (it falls through to
        // it), and each case's.
        let mut bodies = vec![cases[m - 1].access(load)];
        let group: Vec<usize> = cases
            .iter()
            .map(|c| {
                let a = c.access(load);
                bodies.iter().position(|&b| b == a).unwrap_or_else(|| {
                    bodies.push(a);
                    bodies.len() - 1
                })
            })
            .collect();
        let first = hot.len() + m;
        let end = first + bodies.len() - 1;
        for (c, case) in cases.iter().enumerate() {
            let last = c + 1 == m;
            hot.push(Piece {
                imm64: case.base(),
                jump: if last {
                    Target::Next
                } else {
                    Target::Piece((first + group[c]) as u32)
                },
                ..at(&case.guard(if last { "guard" } else { "jeq" }))
            });
        }
        for (base, imm, imm64b) in bodies {
            let k = hot.len();
            hot.push(Piece {
                imm64b,
                reg: insn.reg,
                exit: i as u32,
                cont: if k == end {
                    Target::Next
                } else {
                    Target::AfterPiece(end as u32)
                },
                ..Piece::with_imm(&format!("{base}_nc_{loc}"), imm)
            });
        }
        if let Some(post) = s.post {
            hot.push(Piece::new(post, 0));
        }
    }
    let mut seq: Vec<Piece> = box_acc_piece(st.acc).into_iter().collect();
    seq.extend(spills(homes, boxed));
    seq.push(Piece {
        imm: insn.exit_record(),
        ..Piece::new("exit", 0)
    });
    exits.push((i, seq));
}

/// The result of [`lower`].
#[allow(clippy::vec_box)]
struct Lowered {
    insns: Vec<Insn>,
    props: Vec<Prop>,
    /// The cases of the polymorphic accesses in `props`.
    polys: Vec<Cases>,
    /// Per instruction, the objects scalar replacement left unallocated
    /// that are live there: an exit before it allocates them (see
    /// [`scalar_replace`]).
    mats: HashMap<usize, Vec<Rc<Mat>>>,
    frames: Vec<InlineFrame>,
    fbs: Vec<Feedback>,
    sites: Vec<Box<std::cell::UnsafeCell<CallSite>>>,
    nregs: usize,
    /// Where direct calls' frames start: registers from there are set up
    /// for callees, which read them from memory (see [`lower`]). Or where
    /// the registers of loads moved out of loops start, just below them,
    /// which stay in memory too (see [`hoist_loads`]).
    direct_base: usize,
    /// Parameters of inlined frames read straight from the caller's
    /// argument registers: `(frame, parameter, argument)`, registers
    /// renamed (see [`Lowering::arg_moves`]).
    aliases: Vec<(u8, u8, u8)>,
    /// Registers a call's setup no longer writes, which an exit resuming
    /// in its frame within its setup sets for the interpreter (see
    /// [`Lowering::drop_setup`]).
    restores: Vec<Restore>,
    /// Per instruction, the registers an exit before it leaves the
    /// interpreter to read (see [`interp_live`]).
    exit_live: Vec<RegSet>,
}

/// Register `reg` (renamed) of frame `frame` holds `value` for the
/// interpreter at bytecode offsets `after < pc <= upto` of that frame.
#[derive(Clone, Copy, Debug)]
struct Restore {
    frame: u8,
    after: usize,
    upto: usize,
    reg: u8,
    value: Restored,
}

#[derive(Clone, Copy, Debug)]
enum Restored {
    /// What register (renamed) holds.
    Reg(u8),
    Const(u64),
}

/// A direct call as first lowered, its frame setup's registers relative to
/// the callee's frame: the call (`reg2` is its window), the stores into the
/// frame (`Star`s), and the original window.
struct DirectSetup {
    site: usize,
    call: usize,
    stores: Vec<usize>,
    window: u8,
}

/// An instruction made by [`lower`], at bytecode offset `pc` of `frame`.
fn synth(op: Op, reg: u8, frame: usize, pc: usize) -> Insn {
    Insn {
        pc,
        op,
        imm: 0,
        reg,
        reg2: 0,
        target: 0,
        bail: false,
        frame: frame as u8,
        ic: 0,
        guard: 0,
        tail: false,
        next_case: false,
        assume: 0,
        on_shape: false,
        alloc: 0,
        native: 0,
        check_only: false,
        exit_const: 0,
        sub: false,
        hoisted: None,
        nullable_this: false,
    }
}

/// For the call at `insns[k]`, the registers of its window (`this` and the
/// arguments) that hold a copy of another register `x` until the call: set
/// by `Mov x, w` or `Ldar x; Star w`, with no jump into the code from there
/// to the call and nothing there writing `x` or calling. An inlined callee
/// may read `x` for such a `w` (see [`Lowering::arg_moves`]); `w` is still
/// set, for the interpreter. As `(w, x)`.
fn window_sources(insns: &[Insn], is_target: &[bool], k: usize) -> Vec<(u8, u8)> {
    let call = &insns[k];
    let n = call.imm as usize + usize::from(call.op == Op::Call);
    let writes = |j: &Insn| match j.op {
        Op::Star => Some(j.reg),
        Op::Mov => Some(j.reg2),
        _ => None,
    };
    let mut out = Vec::new();
    for w in (call.reg2 as usize..call.reg2 as usize + n).map(|w| w as u8) {
        let Some(s) = (0..k).rev().find(|&s| writes(&insns[s]) == Some(w)) else {
            continue;
        };
        let x = match insns[s].op {
            Op::Mov if !insns[s].bail => insns[s].reg,
            Op::Star
                if s > 0 && !is_target[s] && insns[s - 1].op == Op::Ldar && !insns[s - 1].bail =>
            {
                insns[s - 1].reg
            }
            _ => continue,
        };
        let clear = (s + 1..k).all(|j| {
            let j = &insns[j];
            !j.bail && !j.is_call() && writes(j) != Some(x)
        });
        if clear && (s + 1..=k).all(|j| !is_target[j]) {
            out.push((w, x));
        }
    }
    out
}

/// What [`lower`] reads and builds.
struct Lowering<'a> {
    state: &'a mut VmState,
    epoch: u32,
    out: Lowered,
    direct: Vec<DirectSetup>,
    /// Per function, the registers live into each of its instructions, in
    /// its own frame (see [`Lowering::dead_after`]).
    live: HashMap<*const crate::vm::Code, Rc<Vec<RegSet>>>,
    /// For the call being lowered, registers of its window that are
    /// copies of another register, with that register, both renamed (see
    /// [`window_sources`]).
    fwd: Vec<(u8, u8)>,
    /// Bytecode instructions inlined so far, small leaves aside (see
    /// [`INLINE_BUDGET`]).
    inlined: usize,
    /// The call being considered for inlining is in a loop of its frame:
    /// not limited by [`INLINE_BUDGET`].
    call_in_loop: bool,
    /// The `Star`s and `Ldar`s that keep `acc` in a scratch register
    /// around a `Mov` (see `emit_frame`): the register is given once the
    /// frames are laid out.
    mov_scratch: Vec<usize>,
}

/// The function's instructions as compiled, from its code and inline caches:
///
/// - `Mov a, b` becomes `Ldar a; Star b` when `acc` is dead after it
///   (compilers emit it to set up calls), else a bail.
/// - A `Call` or `CallUndefined` whose cache names a small bytecode function
///   (not inlined twice in one chain, within [`MAX_INLINE_DEPTH`] and
///   [`MAX_INLINE_INSNS`]) is inlined: a guard that the callee register
///   holds that function (`check_callee`), moves of `this` and the
///   arguments into the callee's registers (missing ones and locals
///   `undefined`), then the callee's instructions, its registers renamed and
///   each `Return` a jump past it. Its registers go exactly where its VM
///   frame would be, just past the caller's registers and a frame header,
///   so an exit inside it only has to write that header for the
///   interpreter to resume there (see [`vm_osr`]).
/// - Other calls go through the runtime (`call`), with a [`CallSite`].
///
/// Every instruction keeps its frame and bytecode offset, which exits
/// record.
/// Time spent lowering and compiling, in nanoseconds, over the process
/// (`toi` prints it with `TOI_JIT_TIME` set).
pub static LOWER_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static COMPILE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// Lowerings and compilations running (a call binding compiles its
    /// callee within its caller's): only the outermost is timed.
    static TIMING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Run `f`, adding its time to `total` unless it runs within another timed
/// call.
pub static LOWERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Profiling: with `TOI_COUNT` set, hot code counts each stencil's
/// executions (exactly, unlike sampling, whose skid moves samples to the
/// next piece), reported by [`count_report`]. x86-64 only.
fn count_prefix() -> usize {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("TOI_COUNT").is_some()) {
        arch::COUNT_PREFIX
    } else {
        0
    }
}

/// Per stencil name: executions (boxed: compiled code increments it in
/// place) and code bytes.
type Counts = HashMap<&'static str, (Box<u64>, usize)>;
static COUNTS: std::sync::Mutex<Option<Counts>> = std::sync::Mutex::new(None);

fn piece_counter(name: &'static str, len: usize) -> *mut u64 {
    let mut counts = COUNTS.lock().unwrap();
    let (n, bytes) = counts
        .get_or_insert_with(HashMap::default)
        .entry(name)
        .or_insert_with(|| (Box::new(0), len));
    *bytes = (*bytes).max(len);
    &mut **n
}

/// The counts `TOI_COUNT` collected, most executed bytes first: stencil,
/// executions, bytes, executions times bytes.
pub fn count_report() -> String {
    let counts = COUNTS.lock().unwrap();
    let mut rows: Vec<_> = counts
        .iter()
        .flatten()
        .map(|(name, (n, bytes))| (**n * *bytes as u64, **n, *bytes, *name))
        .collect();
    rows.sort_unstable_by(|a, b| b.cmp(a));
    let total: u64 = rows.iter().map(|r| r.0).sum::<u64>().max(1);
    rows.iter()
        .map(|(w, n, bytes, name)| {
            format!("{:6.2}% {n:>12} x {bytes:>4} B  {name}\n", 100.0 * *w as f64 / total as f64)
        })
        .collect()
}

pub static COMPILES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn timed<T>(total: &std::sync::atomic::AtomicU64, f: impl FnOnce() -> T) -> T {
    if std::ptr::eq(total, &LOWER_NS) {
        LOWERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    } else {
        COMPILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let depth = TIMING.with(|d| d.replace(d.get() + 1));
    let t = std::time::Instant::now();
    let r = f();
    if depth == 0 {
        total.fetch_add(
            t.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    TIMING.with(|d| d.set(depth));
    r
}

fn lower(state: &mut VmState, epoch: u32) -> Lowered {
    timed(&LOWER_NS, || lower_frames(state, epoch))
}

fn lower_frames(state: &mut VmState, epoch: u32) -> Lowered {
    let root = state.root;
    let root_len = state.root_len;
    let mut cx = Lowering {
        state,
        epoch,
        out: Lowered {
            // (Inlined calls make it longer.)
            insns: Vec::with_capacity(4 * root_len),
            props: Vec::with_capacity(4 * root_len),
            polys: Vec::new(),
            mats: HashMap::default(),
            frames: vec![InlineFrame {
                construct: false,
                code: root,
                // (The root's is in its frame's header already.)
                func: std::ptr::null_mut(),
                base: 0,
                parent: 0,
                ret_pc: 0,
            }],
            fbs: Vec::new(),
            sites: Vec::new(),
            nregs: unsafe { (*root).nregs } as usize,
            direct_base: usize::MAX,
            aliases: Vec::new(),
            restores: Vec::new(),
            exit_live: Vec::new(),
        },
        direct: Vec::new(),
        live: HashMap::default(),
        fwd: Vec::new(),
        inlined: 0,
        call_in_loop: false,
        mov_scratch: Vec::new(),
    };
    cx.out.fbs.push(cx.feedback(root));
    let rets = cx.emit_frame(0, 0);
    debug_assert!(rets.is_empty());
    if !cx.mov_scratch.is_empty() {
        let t = cx.out.nregs as u8;
        cx.out.nregs += 1;
        for k in std::mem::take(&mut cx.mov_scratch) {
            cx.out.insns[k].reg = t;
        }
    }
    // Registers for the loads `hoist_loads` moves out of loops, below the
    // direct calls' frames.
    let spare_base = cx.out.nregs;
    let spare = hoist_candidates(&cx.out).min(255usize.saturating_sub(spare_base + interp::HEADER));
    cx.out.nregs += spare;
    cx.place_direct_frames();
    // They stay in memory (`Lowered::direct_base` on): a slot each for the
    // whole loop took slots from what the loop computes (71, 82, 87 3-8%
    // slower), while a load from the frame is one load, not a chain.
    if spare > 0 {
        cx.out.direct_base = cx.out.direct_base.min(spare_base);
    }
    fold_constant_tests(&mut cx.out.insns);
    reuse_global_loads(&mut cx.out);
    fold_constant_loads(&mut cx.out, &cx.state.property_exits);
    hoist_guards(&mut cx.out);
    // An int32 constant needing no check is an `LdaSmi`, which typed code
    // and fused groups handle as an immediate.
    for (i, p) in cx.out.insns.iter_mut().zip(cx.out.props.iter_mut()) {
        if let Prop::Const { value, at: 0, .. } = *p
            && i.op != Op::Nop
            && Value::from_bits(value).is_int32()
        {
            let k = Value::from_bits(value).as_i32_unchecked();
            (i.op, i.imm, *p) = (Op::LdaSmi, k, Prop::None);
        }
    }
    scalar_replace(&mut cx.out);
    // After scalar replacement: loads from objects it removed are no loads.
    // Innermost loops first, then out of the loops around them (`0`: no
    // round to go).
    for _ in 0..if spare > 0 { 4 } else { 0 } {
        if hoist_loads(&mut cx.out, &cx.state.property_exits, (spare_base, spare)) == 0 {
            break;
        }
    }
    if cx.state.tier == Tier::Warm {
        return cx.out;
    }
    for k in 0..cx.out.frames.len() {
        cx.pc_live(cx.out.frames[k].code);
    }
    let pc_live = unsafe { &(*cx.state.vm).jit_caches.pc_live };
    cx.out.exit_live = interp_live(&cx.out, pc_live);
    cx.out
}

/// A function's registers live into each instruction, by bytecode offset.
type PcLive = HashMap<usize, RegSet>;

/// Per instruction of `out`, the registers the interpreter may read after
/// an exit before it: each frame's live registers where it resumes (the
/// exit's frame at the instruction, its callers past their calls), as its
/// bytecode reads them, renamed; for a parameter read from an argument's
/// register, that register, and for a register a call's setup no longer
/// writes, the register it is restored from (see `materialize`).
fn interp_live(
    out: &Lowered,
    pc_live: &HashMap<*const crate::vm::Code, Rc<PcLive>>,
) -> Vec<RegSet> {
    if out.nregs > 256 {
        return Vec::new();
    }
    // Per frame, its registers read from others.
    let mut aliases: Vec<Vec<(usize, usize)>> = vec![Vec::new(); out.frames.len()];
    for &(g, p, a) in &out.aliases {
        aliases[g as usize].push((p as usize, a as usize));
    }
    // What frame `g` reads at `pc` (as its bytecode does, renamed), and
    // registers restored for it there.
    let local = |g: usize, pc: usize| {
        let f = out.frames[g];
        let mut set = [0; 4];
        // (Past the end of the bytecode: nothing.)
        if let Some(live) = pc_live[&f.code].get(&pc) {
            for r in rs_iter(live) {
                rs_add(&mut set, f.base + r);
            }
        }
        // A restored register's source, then a parameter's argument (the
        // source may be a parameter: see `materialize`).
        for r in out.restores.iter().filter(|r| r.frame as usize == g) {
            if let Restored::Reg(x) = r.value
                && r.after < pc
                && pc <= r.upto
            {
                rs_add(&mut set, x as usize);
            }
        }
        for &(p, a) in &aliases[g] {
            if rs_has(&set, p) {
                rs_add(&mut set, a);
            }
        }
        set
    };
    // Per frame, what its callers read past their calls (a frame's parent
    // comes before it).
    let mut callers: Vec<RegSet> = Vec::with_capacity(out.frames.len());
    for (g, f) in out.frames.iter().enumerate() {
        let mut set = [0; 4];
        if g != 0 {
            set = local(f.parent, f.ret_pc);
            rs_or(&mut set, &callers[f.parent]);
        }
        callers.push(set);
    }
    let mut memo: HashMap<(u8, usize), RegSet> = HashMap::default();
    memo.reserve(out.insns.len());
    out.insns
        .iter()
        .map(|insn| {
            *memo.entry((insn.frame, insn.pc)).or_insert_with(|| {
                let g = insn.frame as usize;
                let mut set = local(g, insn.pc);
                rs_or(&mut set, &callers[g]);
                set
            })
        })
        .collect()
}

/// An object [`scalar_replace`] didn't allocate, as an exit allocates it:
/// of shape `shape`, slot `k` holding register `fields[k]`, and put in the
/// registers `regs` (those holding it there).
pub(crate) struct Mat {
    shape: *const Shape,
    fields: Vec<u8>,
    regs: Vec<u8>,
}

/// Scalar replacement of inlined `new`s whose object never escapes: no
/// call, store, comparison or return sees it, only loads and stores of
/// its own properties, on every path from the allocation while a register
/// holding it is live (not into a loop's header, where code may be entered
/// with a real object). The allocation becomes `ctor_check` (what the
/// object's shape depends on unchanged, else leave before), each property a
/// register: a store `Star`s it, a load `Ldar`s it. Where it is live, an
/// exit allocates it first (`Lowered::mats`, `materialize`), with the shape
/// it has there, storing its fields through `Vm::store_field`, so their
/// types stay right.
fn scalar_replace(out: &mut Lowered) {
    let allocs: Vec<usize> = (0..out.insns.len())
        .filter(|&a| out.insns[a].alloc != 0 && !out.insns[a].bail)
        .collect();
    let mut live = None;
    for a in allocs {
        let l = live.get_or_insert_with(|| {
            let frames: Vec<FrameRegs> = out
                .frames
                .iter()
                .map(|f| (f.base, unsafe { (*f.code).nregs } as usize, f.parent))
                .collect();
            // An object replaced already is a use of the registers of
            // its fields where an exit allocates it: one of them may hold
            // the object this allocation makes.
            reg_live_in_mats(
                &out.insns,
                &reachable(&out.insns, 0),
                out.nregs,
                &frames,
                Some(&out.mats),
                true,
            )
        });
        if let Some(plan) = replacement(out, a, l) {
            apply_replacement(out, a, plan);
            live = None;
        }
    }
}

/// What [`replacement`] found: the rewrites (instruction, slot, load or
/// store), the slot count, and per instruction where the object is live,
/// its shape and the live registers holding it.
struct Replacement {
    rewrites: Vec<(usize, usize, bool)>,
    slots: usize,
    live: Vec<(usize, *const Shape, Vec<u8>)>,
}

fn replacement(out: &Lowered, a: usize, live: &[RegSet]) -> Option<Replacement> {
    const NO: u8 = 0;
    const YES: u8 = 1;
    const MAYBE: u8 = 2;
    /// Which registers hold the object for sure (`yes`), or on some
    /// paths only (`maybe`).
    #[derive(Clone, PartialEq)]
    struct St {
        yes: RegSet,
        maybe: RegSet,
        acc: u8,
        shape: *const Shape,
    }
    let get = |st: &St, r: usize| {
        if rs_has(&st.maybe, r) {
            MAYBE
        } else if rs_has(&st.yes, r) {
            YES
        } else {
            NO
        }
    };
    let set = |st: &mut St, r: usize, v: u8| {
        let bit = 1u64 << (r % 64);
        st.yes[r / 64] &= !bit;
        st.maybe[r / 64] &= !bit;
        match v {
            YES => st.yes[r / 64] |= bit,
            MAYBE => st.maybe[r / 64] |= bit,
            _ => {}
        }
    };
    let meets = |a: &RegSet, b: &RegSet| a.iter().zip(b).any(|(x, y)| x & y != 0);
    let insns = &out.insns;
    let n = insns.len();
    let nregs = out.nregs;
    let mut header = vec![false; n];
    for (j, i) in insns.iter().enumerate() {
        if i.jumps() && i.target <= j {
            header[i.target] = true;
        }
    }
    let site = unsafe { &*(insns[a].alloc as *const CallSite) };
    let root = site.this_shape as *const Shape;
    // Forward over the whole function, from its start, so that a path
    // that doesn't come from the allocation counts where paths join.
    let start = St {
        yes: [0; 4],
        maybe: [0; 4],
        acc: NO,
        shape: root,
    };
    let mut ins: Vec<Option<St>> = vec![None; n];
    let mut rewrites: HashMap<usize, (usize, bool)> = HashMap::default();
    let mut slots = 0usize;
    let mut work = vec![(0, start)];
    let held =
        |st: &St, live: &RegSet| st.acc != NO || meets(&st.yes, live) || meets(&st.maybe, live);
    while let Some((i, st)) = work.pop() {
        if i >= n {
            return None;
        }
        let st = match &ins[i] {
            Some(old) => {
                let mut j = old.clone();
                for k in 0..4 {
                    let maybe = j.maybe[k] | st.maybe[k] | (j.yes[k] ^ st.yes[k]);
                    j.yes[k] &= st.yes[k] & !maybe;
                    j.maybe[k] = maybe;
                }
                if j.acc != st.acc {
                    j.acc = MAYBE;
                }
                if j.shape != st.shape {
                    // Paths disagree on its shape: fine if nothing holds it.
                    if held(&j, &live[i]) || held(&st, &live[i]) {
                        return None;
                    }
                    j.shape = root;
                }
                if &j == old {
                    continue;
                }
                j
            }
            None => st,
        };
        ins[i] = Some(st.clone());
        let insn = insns[i];
        let is_held = held(&st, &live[i]);
        if is_held && (i == a || header[i] || insn.bail && st.acc != NO) {
            return None;
        }
        // Registers holding it that the interpreter would read after an
        // exit here must hold it for sure.
        if meets(&st.maybe, &live[i]) {
            return None;
        }
        let r = insn.reg as usize;
        let reg_held = r < nregs && get(&st, r) != NO;
        let mut next = st.clone();
        let fb = &out.fbs[insn.ic_frame()];
        match insn.op {
            // The allocation: `acc` holds a new object (nothing holds the
            // previous one any more, checked above).
            _ if i == a => {
                (next.yes, next.maybe) = ([0; 4], [0; 4]);
                next.acc = YES;
                next.shape = root;
            }
            Op::Ldar => next.acc = if r < nregs { get(&st, r) } else { NO },
            Op::Star => set(&mut next, r, st.acc),
            Op::GetNamed if reg_held => {
                if get(&st, r) != YES {
                    return None;
                }
                let shape = unsafe { &*st.shape };
                let slot = shape.lookup(fb.name(&insn))? as usize;
                if shape.attrs[slot] & crate::object::ACCESSOR != 0 {
                    return None;
                }
                if shape.location(slot as u32) < 0 {
                    return None;
                }
                rewrites.insert(i, (slot, true));
                next.acc = NO;
            }
            Op::SetNamed if reg_held => {
                if get(&st, r) != YES || st.acc != NO {
                    return None;
                }
                let shape = unsafe { &*st.shape };
                let name = fb.name(&insn);
                let slot = match shape.lookup(name) {
                    Some(k) if shape.attrs[k as usize] & crate::object::ACCESSOR != 0 => {
                        return None;
                    }
                    Some(k) => k as usize,
                    None => {
                        let Prop::Transition {
                            shape: from,
                            new_shape,
                            ..
                        } = out.props[i]
                        else {
                            return None;
                        };
                        if from != st.shape as u64 {
                            return None;
                        }
                        next.shape = new_shape as *const Shape;
                        unsafe { (*next.shape).lookup(name)? as usize }
                    }
                };
                if unsafe { (*next.shape).location(slot as u32) } < 0 {
                    return None;
                }
                slots = slots.max(slot + 1);
                rewrites.insert(i, (slot, false));
            }
            _ => {
                // Any other use of it is an escape.
                let reads_held = (insn.reads_reg() && reg_held)
                    || (insn.is_call()
                        && insn.call_reads().any(|q| q < nregs && get(&st, q) != NO))
                    || (insn.op == Op::SetKeyed && get(&st, insn.reg2 as usize) != NO)
                    || (st.acc != NO && reads_acc(&insn));
                if reads_held || insn.assume != 0 && reg_held {
                    return None;
                }
                if writes_acc(&insn).is_some() {
                    next.acc = NO;
                }
            }
        }
        if insn.falls_through() {
            work.push((i + 1, next.clone()));
        }
        if insn.jumps() {
            work.push((insn.target, next));
        }
    }
    let live = (0..n)
        .filter_map(|i| {
            let st = ins[i].as_ref()?;
            let regs: Vec<u8> = rs_iter(&st.yes)
                .filter(|&q| q < nregs && rs_has(&live[i], q))
                .map(|q| q as u8)
                .collect();
            (!regs.is_empty()).then_some((i, st.shape, regs))
        })
        .collect();
    let rewrites = rewrites
        .into_iter()
        .map(|(i, (k, load))| (i, k, load))
        .collect::<Vec<_>>();
    (!rewrites.is_empty() && ins[a].is_some() && out.nregs + slots <= 256).then_some(Replacement {
        rewrites,
        slots,
        live,
    })
}

/// Whether `insn` reads `acc` (conservatively: unless it is known not to).
fn reads_acc(insn: &Insn) -> bool {
    insn.native == 1
        || !matches!(
            insn.op,
            Op::Ldar
                | Op::LdaSmi
                | Op::LdaUndefined
                | Op::LdaNull
                | Op::LdaTrue
                | Op::LdaFalse
                | Op::LdaGlobal
                | Op::LdaConstant
                | Op::Jump
                | Op::Nop
                | Op::GetNamed
                | Op::CreateArray
                | Op::CreateObject
                | Op::CreateClosure
                | Op::Call
                | Op::CallUndefined
                | Op::Construct
        )
}

fn apply_replacement(out: &mut Lowered, a: usize, plan: Replacement) {
    let base = out.nregs;
    out.nregs += plan.slots;
    let h = |slot: usize| (base + slot) as u8;
    for (i, slot, load) in plan.rewrites {
        let j = out.insns[i];
        out.insns[i] = synth(
            if load { Op::Ldar } else { Op::Star },
            h(slot),
            j.frame as usize,
            j.pc,
        );
        out.props[i] = Prop::None;
    }
    // The allocation: only its checks (`acc` = `undefined`).
    out.insns[a].native = 3;
    for (i, shape, regs) in plan.live {
        let fields = (0..unsafe { (*shape).keys.len() }).map(h).collect();
        // Before the objects replaced earlier: one may hold this one in a
        // field (a slot register this allocation writes). (Not the other
        // way: an object stored into one replaced later escapes there.)
        out.mats.entry(i).or_default().insert(
            0,
            Rc::new(Mat {
                shape,
                fields,
                regs,
            }),
        );
    }
}

/// A global loaded again in straight-line code, its earlier load still in
/// a register: `Ldar` that register, a copy (see [`TypeState::same`]), so
/// a shape guard on one holds for both. Not past a jump target, a store
/// to a global, a call not inlined (which may store one), a bail, or a
/// property store that might be to the global object (its receiver's
/// shape unknown, or the global's).
fn reuse_global_loads(out: &mut Lowered) {
    let n = out.insns.len();
    let mut target = vec![false; n + 1];
    for i in out.insns.iter().filter(|i| i.jumps()) {
        if i.target <= n {
            target[i.target] = true;
        }
    }
    let global_shape = out
        .fbs
        .first()
        .map_or(0, |f| unsafe { (*f.global).shape } as u64);
    let may_store_global = |p: &Prop| match *p {
        Prop::Own { shape, .. } | Prop::Oop { shape, .. } | Prop::Transition { shape, .. } => {
            shape == global_shape
        }
        Prop::Poly { n, at } => poly_cases(&out.polys, n, at)
            .iter()
            .any(|c| c.is_family() || c.shape == global_shape),
        _ => true,
    };
    // (name, register holding its value), and the name `acc` holds.
    let mut known: Vec<(Atom, u8)> = Vec::new();
    let mut acc: Option<Atom> = None;
    for k in 0..n {
        if target[k] {
            known.clear();
            acc = None;
        }
        let i = out.insns[k];
        if i.bail {
            known.clear();
            acc = None;
            continue;
        }
        match i.op {
            Op::LdaGlobal => {
                let name = out.fbs[i.ic_frame()].name(&i);
                if let Some(&(_, r)) = known.iter().find(|(m, _)| *m == name) {
                    out.insns[k] = synth(Op::Ldar, r, i.frame as usize, i.pc);
                    out.props[k] = Prop::None;
                }
                acc = Some(name);
            }
            Op::Star => {
                known.retain(|&(_, r)| r != i.reg);
                if let Some(name) = acc {
                    known.push((name, i.reg));
                }
            }
            Op::Ldar => acc = known.iter().find(|&&(_, r)| r == i.reg).map(|&(m, _)| m),
            Op::StaGlobal | Op::Call | Op::CallUndefined | Op::Construct => {
                known.clear();
                acc = None;
            }
            Op::SetNamed | Op::SetKeyed
                if may_store_global(&out.props[k]) && i.op == Op::SetNamed =>
            {
                known.clear();
                acc = None;
            }
            _ => {
                if writes_acc(&i).is_some() {
                    acc = None;
                }
            }
        }
    }
}

/// Loads of objects that mostly stay what they are: a global's (a
/// function, say) and properties of an object so loaded, or loaded so
/// (`C.parent.prototype.m`), and int32s of int32 fields so found
/// (`Dir.FORWARD`). Each becomes the value it has now
/// ([`Prop::Const`]), after a check that its slot still holds it (or, for
/// a prototype's property, that the receiver's shape is the one the cache
/// found it under); none for a slot of a prototype, which holds its value
/// while the prototype epoch the code is compiled under holds (see
/// `Vm::store_field`). A guard on the callee then needs no code if its
/// register holds the function. Only in-object slots (out-of-line ones
/// move when they grow), in straight-line code (registers' values known
/// from the last jump target on), not where such a check exited, and not
/// a global the code stores to. None for a constant field (see
/// `FieldCell`): a store that changes it drops the code, which notices
/// after calls; so not one the code itself may store to (by name, or any
/// with a keyed store), through the runtime.
fn fold_constant_loads(out: &mut Lowered, exits: &HashMap<(usize, usize), u32>) {
    use crate::object::{Object, PROTOTYPE};
    let n = out.insns.len();
    let mut target = vec![false; n + 1];
    for i in out.insns.iter().filter(|i| i.jumps()) {
        if i.target <= n {
            target[i.target] = true;
        }
    }
    let word = |at: u64| unsafe { *(at as *const u64) };
    let object = |v: u64| Value::from_bits(v).is_cell().then_some(v as *mut Object);
    let proto = |o: *mut Object| unsafe { (*o).flags & PROTOTYPE != 0 };
    // Fields the code may store to. A keyed store's stencils write an
    // array's elements, or go through the runtime for a typed array (which
    // may add it a named property); only `set_keyed_rt` writes any object's.
    let keyed_store = |rt: bool| {
        (0..n).any(|k| out.insns[k].op == Op::SetKeyed && (out.props[k] == Prop::Runtime) == rt)
    };
    let (keyed, keyed_typed) = (keyed_store(true), keyed_store(false));
    let set: Vec<Atom> = (out.insns.iter())
        .filter(|i| i.op == Op::SetNamed)
        .map(|i| out.fbs[i.ic_frame()].name(i))
        .collect();
    // The cell of `o`'s field `name`, if constant and not stored to.
    let constant = |o: *mut Object, name: Atom| unsafe {
        let s = &*(*o).shape;
        let typed = (*o).class == crate::object::Class::TypedArray;
        (s.lookup(name).map(|slot| s.field_cell(slot)))
            .filter(|c| c.is_constant() && !keyed && !(typed && keyed_typed) && !set.contains(&name))
    };
    // Globals the code stores to: likely to change.
    let stored: Vec<Atom> = (out.insns.iter())
        .filter(|i| i.op == Op::StaGlobal)
        .map(|i| out.fbs[i.ic_frame()].name(i))
        .collect();
    // Registers known to hold a value, and what `acc` holds.
    let mut known: Vec<(u8, u64)> = Vec::new();
    let mut acc: Option<u64> = None;
    for k in 0..n {
        let i = out.insns[k];
        if target[k] || i.bail {
            known.clear();
            acc = None;
        }
        if i.bail {
            continue;
        }
        let exited = || exits.contains_key(&(out.frames[i.frame as usize].code as usize, i.pc));
        let of = |r: u8| known.iter().find(|&&(x, _)| x == r).map(|&(_, v)| v);
        // (value, the word checked and what it must be, field type, the
        // object).
        let folded = match (i.op, out.props[k]) {
            _ if i.check_only => None,
            (
                Op::LdaGlobal,
                Prop::Global {
                    global,
                    off,
                    oop,
                    ft,
                    ..
                },
            ) => {
                let at = global + off as u64;
                let name = out.fbs[i.ic_frame()].name(&i);
                // An out-of-line slot moves when the slots grow: only
                // without a check (a constant field).
                let at = if oop {
                    unsafe { *((global + 16) as *const u64) + off as u64 }
                } else {
                    at
                };
                let check = if constant(global as *mut Object, name).is_some() { 0 } else { at };
                (!(oop && check != 0) && !stored.contains(&name))
                    .then(|| (word(at), check, word(at), ft, global))
            }
            (Op::GetNamed, Prop::Own { shape, off, ft }) => of(i.reg)
                .and_then(object)
                .filter(|&o| word(o as u64) == shape)
                .map(|o| {
                    let at = o as u64 + off as u64;
                    let name = out.fbs[i.ic_frame()].name(&i);
                    let fixed = proto(o) || constant(o, name).is_some();
                    if proto(o) {
                        rely_on_slot(at);
                    }
                    (
                        word(at),
                        if fixed { 0 } else { at },
                        word(at),
                        ft,
                        o as u64,
                    )
                }),
            (Op::GetNamed, Prop::Proto { shape, addr, ft }) => of(i.reg)
                .and_then(object)
                .filter(|&o| word(o as u64) == shape)
                .map(|o| {
                    rely_on_slot(addr);
                    (
                        word(addr),
                        if proto(o) { 0 } else { o as u64 },
                        shape,
                        ft,
                        o as u64,
                    )
                }),
            _ => None,
        };
        let folded = folded
            .filter(|&(value, _, _, ft, _)| {
                // An object, or an int32 of an int32 field.
                let int = Value::from_bits(value).is_int32() && ft == FieldType::Int;
                (object(value).is_some() || int) && !exited()
            })
            .map(|(value, at, expect, ft, obj)| Prop::Const {
                value,
                at,
                expect: if at == 0 { 0 } else { expect },
                ft,
                obj,
            });
        match i.op {
            Op::LdaGlobal | Op::GetNamed => {
                acc = None;
                if let Some(p @ Prop::Const { value, at, obj, .. }) = folded {
                    // Without a check: a store that changes it must drop
                    // the code.
                    if at == 0
                        && let Some(c) = constant(obj as *mut Object, out.fbs[i.ic_frame()].name(&i))
                    {
                        c.fold();
                    }
                    out.props[k] = p;
                    acc = Some(value);
                }
            }
            Op::Star => {
                known.retain(|&(r, _)| r != i.reg);
                if let Some(v) = acc
                    && !i.check_only
                {
                    known.push((i.reg, v));
                }
            }
            Op::Ldar => acc = of(i.reg),
            Op::Nop if i.guard != 0 && !i.next_case && !i.on_shape && !i.tail && i.alloc == 0 => {
                if of(i.reg) == Some(i.guard) {
                    out.insns[k].guard = 0;
                }
            }
            _ => {
                if writes_acc(&i).is_some() || i.is_call() {
                    acc = None;
                }
            }
        }
    }
}

/// The checks of constant loads ([`Prop::Const`]) in a loop that nothing
/// in the loop can make fail, moved before it: the load in the loop is the
/// constant alone. Not in a loop that calls (anything may change then),
/// stores through the runtime or by key, or stores into objects with the
/// checked object's shape or one extending it (`StaGlobal`: into the
/// global object). The checks go where the loop is entered: before its
/// header if that is entered by falling into it (and jumps from outside
/// to it go to them), else before the jump into the loop (a `for` jumping
/// to its condition), with a copy before the header for code entered
/// there (compiled code is entered at a loop's header, by its bytecode
/// offset, which the checks carry: see `JitFunction::index_of`). Not into
/// a loop holding another loop of the function itself (entered at its
/// header, it would skip the checks), nor one jumped into elsewhere.
/// A check failing exits before the loop (`HOISTED`), and its load isn't
/// compiled as a constant again.
fn hoist_guards(out: &mut Lowered) {
    let n = out.insns.len();
    let insns = &out.insns;
    // Loops: header, last backward jump to it.
    let mut loops: Vec<(usize, usize)> = Vec::new();
    for (j, i) in insns.iter().enumerate() {
        if i.jumps() && i.target <= j {
            match loops.iter_mut().find(|l| l.0 == i.target) {
                Some(l) => l.1 = l.1.max(j),
                None => loops.push((i.target, j)),
            }
        }
    }
    loops.sort_by_key(|&(t, e)| e - t);
    let global = out.fbs.first().map_or(0, |f| f.global as u64);
    let word = |at: u64| unsafe { *(at as *const u64) };
    // Insertions before an instruction: the checks, and which jumps to
    // the instruction go to them (`Some(loop)`: from outside the loop;
    // `None`: all, or with `false` none).
    struct Group {
        at: usize,
        checks: Vec<usize>,
        frame: u8,
        pc: usize,
        enter: Option<(usize, usize)>,
        all: bool,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut taken = vec![false; n];
    for &(t, e) in &loops {
        let inside = |k: usize| t <= k && k <= e;
        // Another loop of the function's own frame inside: entered there.
        if loops
            .iter()
            .any(|&(u, _)| u != t && inside(u) && insns[u].frame == 0)
        {
            continue;
        }
        // What the loop may store into.
        let (mut all, mut shapes, mut globals) = (false, Vec::new(), false);
        for i in &insns[t..=e] {
            if i.bail {
                continue;
            }
            if i.is_call() && i.native == 0 {
                all = true;
            }
        }
        for k in t..=e {
            let i = &insns[k];
            if i.bail {
                continue;
            }
            match (i.op, out.props[k]) {
                (
                    Op::SetNamed,
                    Prop::Own { shape, .. }
                    | Prop::Oop { shape, .. }
                    | Prop::Transition { shape, .. },
                ) => shapes.push(shape),
                (Op::SetNamed, Prop::Poly { n, at }) => {
                    shapes.extend(poly_cases(&out.polys, n, at).iter().map(|c| c.shape))
                }
                (Op::SetNamed | Op::SetKeyed, _) => all = true,
                (Op::StaGlobal, _) => globals = true,
                _ => {}
            }
        }
        if all {
            continue;
        }
        // Entered by falling into the header (`A`), or by a jump just
        // before it (`B`); from outside, nothing else jumps in.
        let fall = t == 0 || insns[t - 1].falls_through() && !insns[t - 1].jumps();
        let jump = t > 0
            && insns[t - 1].op == Op::Jump
            && !insns[t - 1].bail
            && inside(insns[t - 1].target);
        if !fall && !jump {
            continue;
        }
        let jumps_in = insns.iter().enumerate().any(|(j, i)| {
            i.jumps()
                && !inside(j)
                && inside(i.target)
                && !(fall && i.target == t)
                && !(jump && j == t - 1)
        });
        if jumps_in {
            continue;
        }
        let checks: Vec<usize> = (t..=e)
            .filter(|&k| {
                !taken[k]
                    && !insns[k].bail
                    && matches!(out.props[k], Prop::Const { at, obj, .. } if at != 0 && obj != 0
                    && !(globals && obj == global)
                    && !shapes.iter().any(|&s| {
                        let o = word(obj) as *const Shape;
                        s == o as u64 || unsafe { extends(s as *const Shape, o) }
                    }))
            })
            .collect();
        if checks.is_empty() {
            continue;
        }
        // (where, from outside which loop jumps enter, whether all do).
        type Place = (usize, Option<(usize, usize)>, bool);
        let places: Vec<Place> = if fall {
            vec![(t, Some((t, e)), false)]
        } else if insns[t].frame == 0 {
            vec![(t - 1, None, true), (t, None, false)]
        } else {
            vec![(t - 1, None, true)]
        };
        if places.iter().any(|p| groups.iter().any(|g| g.at == p.0)) {
            continue;
        }
        for &k in &checks {
            taken[k] = true;
        }
        for (at, enter, all) in places {
            groups.push(Group {
                at,
                checks: checks.clone(),
                frame: insns[at].frame,
                pc: insns[at].pc,
                enter,
                all,
            });
        }
    }
    if groups.is_empty() {
        return;
    }
    groups.sort_by_key(|g| g.at);
    // Rebuild, with the checks inserted.
    let mut new_of = vec![0; n + 1];
    let mut start_of: HashMap<usize, usize> = HashMap::default();
    let extra: usize = groups.iter().map(|g| g.checks.len()).sum();
    let (mut insns2, mut props2) = (Vec::with_capacity(n + extra), Vec::with_capacity(n + extra));
    let mut g = 0;
    for k in 0..=n {
        while g < groups.len() && groups[g].at == k {
            start_of.insert(k, insns2.len());
            for &c in &groups[g].checks {
                let mut check = synth(Op::Nop, 0, groups[g].frame as usize, groups[g].pc);
                check.hoisted = Some((out.insns[c].frame, out.insns[c].pc as u32));
                insns2.push(check);
                props2.push(out.props[c]);
            }
            g += 1;
        }
        new_of[k] = insns2.len();
        if k < n {
            insns2.push(out.insns[k]);
            props2.push(out.props[k]);
        }
    }
    // The loads keep only their constants.
    for g in &groups {
        for &c in &g.checks {
            if let Prop::Const { at, expect, .. } = &mut props2[new_of[c]] {
                (*at, *expect) = (0, 0);
            }
        }
    }
    for (k, i) in out.insns.iter().enumerate() {
        if !(i.jumps() || i.bail && i.target > 0) {
            continue;
        }
        let t = i.target;
        let to = match groups.iter().find(|g| g.at == t) {
            Some(g) if g.all => start_of[&t],
            Some(g) => match g.enter {
                Some((a, b)) if !(a <= k && k <= b) => start_of[&t],
                _ => new_of[t],
            },
            None => new_of[t],
        };
        insns2[new_of[k]].target = to;
    }
    out.insns = insns2;
    out.props = props2;
}

/// Loads of constant fields (see `object::FieldCell`) from objects a loop
/// doesn't change, moved before the loop into registers of their own
/// (the first and how many, reserved by [`hoist_candidates`]): the loop
/// reads the register. In DeltaBlue's `Plan.execute`, `this.v`
/// and `v.elms` were loaded every iteration (by hand, moving them out of
/// the loop: -5.2% cycles). The value can't change while the code runs: a
/// store changing a constant field drops code that took its value
/// (`FieldCell::fold`), which a running loop notices after its calls; and
/// the loop must not store to such a field itself (a store through the
/// runtime checks nothing after). Only fields of numbers or objects, which
/// never held `undefined` (filling one keeps it constant).
///
/// The receiver: a register the loop doesn't write, or one written just
/// before the load (no jump target between) with a copy of such a register
/// or of a hoisted load's value (the copies an inlined call makes of its
/// receiver). Placed as `hoist_guards` places checks, after them and after
/// scalar replacement: an exit from a moved load allocates what one at the
/// loop's header would. A load failing its check there exits before the
/// loop (`HOISTED`), and isn't moved again.
/// An upper bound on what [`hoist_loads`] moves, in one pass, to reserve
/// registers and to skip it where it would find nothing (it went over the
/// code several times: lowering 0.8 ms more on 87): twice the loads in
/// loops of a field it could move from a register written just before
/// with such a load's value (the chains it moves, with their heads).
fn hoist_candidates(out: &Lowered) -> usize {
    let insns = &out.insns;
    let n = insns.len();
    // How many loops each instruction is in (back jumps' ranges).
    let mut depth = vec![0i32; n + 1];
    let mut any = false;
    for (j, i) in insns.iter().enumerate() {
        if i.jumps() && i.target <= j {
            depth[i.target] += 1;
            depth[j + 1] -= 1;
            any = true;
        }
    }
    if !any {
        return 0;
    }
    let movable = |k: usize| {
        let i = &insns[k];
        i.op == Op::GetNamed
            && !i.bail
            && !i.check_only
            && i.guard == 0
            && matches!(
                out.props[k],
                Prop::Own { ft: FieldType::Int | FieldType::Num | FieldType::Obj(_), .. }
                    | Prop::Oop { ft: FieldType::Int | FieldType::Num | FieldType::Obj(_), .. }
            )
    };
    let mut from_load = [false; 256];
    let (mut count, mut inside) = (0, 0);
    for k in 0..n {
        inside += depth[k];
        let i = &insns[k];
        if inside > 0 && movable(k) && from_load[i.reg as usize] {
            count += 2;
        }
        match i.op {
            _ if i.bail => {}
            Op::Star => from_load[i.reg as usize] = k > 0 && movable(k - 1),
            Op::Mov => from_load[i.reg2 as usize] = false,
            _ => {}
        }
    }
    count
}

fn hoist_loads(
    out: &mut Lowered,
    exits: &HashMap<(usize, usize), u32>,
    (base, max): (usize, usize),
) -> usize {
    let n = out.insns.len();
    let insns = &out.insns;
    // One pass: the loops (header, last backward jump to it), the jumps,
    // jump targets, the fields the code may change by name (not by a
    // transition, which adds the field to an object that hadn't it:
    // filling it, see `FieldCell`), and the registers earlier rounds took.
    let spare_regs = base..base + max;
    let mut loops: Vec<(usize, usize)> = Vec::new();
    let mut jumps: Vec<(usize, usize)> = Vec::new();
    let mut target = vec![false; n + 1];
    let mut set: Vec<Atom> = Vec::new();
    let mut used = 0;
    for (j, i) in insns.iter().enumerate() {
        if i.jumps() {
            jumps.push((j, i.target));
            target[i.target.min(n)] = true;
            if i.target <= j {
                match loops.iter_mut().find(|l| l.0 == i.target) {
                    Some(l) => l.1 = l.1.max(j),
                    None => loops.push((i.target, j)),
                }
            }
        } else if i.bail && i.target > 0 {
            target[i.target.min(n)] = true;
        }
        match i.op {
            _ if i.bail => {}
            Op::SetNamed if !matches!(out.props[j], Prop::Transition { .. }) => {
                set.push(out.fbs[i.ic_frame()].name(i))
            }
            Op::Star if spare_regs.contains(&(i.reg as usize)) => {
                used = used.max(i.reg as usize + 1 - base)
            }
            _ => {}
        }
    }
    if loops.is_empty() {
        return 0;
    }
    // Innermost first: a load moves once, out of its innermost loop.
    loops.sort_by_key(|&(t, e)| e - t);
    // A hoisted load's value: from a register, or another hoisted load's.
    #[derive(Clone, Copy, PartialEq)]
    enum Src {
        Reg(u8),
        Load(usize),
    }
    struct Group {
        at: usize,
        loads: Vec<(usize, Src)>,
        frame: u8,
        pc: usize,
        enter: Option<(usize, usize)>,
        all: bool,
    }
    // A load an earlier round moved before an inner loop (`GetNamed;
    // Star` into a register of its own): moving it again keeps its
    // register, and the pair before the inner loop goes.
    let moved = |k: usize| -> Option<u8> {
        let (i, next) = (insns[k], insns.get(k + 1)?);
        (i.hoisted.is_some() && next.op == Op::Star && spare_regs.contains(&(next.reg as usize)))
            .then_some(next.reg)
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut taken = vec![false; n];
    let mut count = 0;
    // Where compiled code entered at loop header `u` (by its bytecode
    // offset, `JitFunction::index_of`) starts: the first of the copies
    // earlier rounds put before it, which carry its offset.
    let entry_at = |u: usize| {
        let mut k = u;
        while k > 0 && insns[k - 1].frame == 0 && insns[k - 1].pc == insns[u].pc && !target[k] {
            k -= 1;
        }
        k
    };
    for &(t, e) in &loops {
        let inside = |k: usize| t <= k && k <= e;
        // Loops of the function's own frame inside, where compiled code
        // may be entered (tiering up there): each gets a copy of the loads
        // where that entry lands. Only loops entered by a jump to their
        // condition, so nothing else gets there: a loop entered by falling
        // into it would run the copy at every iteration around it.
        let mut nested: Vec<usize> = Vec::new();
        let mut ok = true;
        for &(u, _) in loops.iter().filter(|&&(u, _)| u != t && inside(u) && insns[u].frame == 0) {
            let at = entry_at(u);
            let jumped = at > 0
                && insns[at - 1].op == Op::Jump
                && !insns[at - 1].bail
                && (at..=u).all(|k| k == u || !target[k]);
            if !jumped {
                ok = false;
                break;
            }
            nested.push(at);
        }
        if !ok {
            continue;
        }
        // Entered as `hoist_guards` requires.
        let fall = t == 0 || insns[t - 1].falls_through() && !insns[t - 1].jumps();
        let jump = t > 0
            && insns[t - 1].op == Op::Jump
            && !insns[t - 1].bail
            && inside(insns[t - 1].target);
        if !fall && !jump {
            continue;
        }
        let jumps_in = jumps.iter().any(|&(j, to)| {
            !inside(j) && inside(to) && !(fall && to == t) && !(jump && j == t - 1)
        });
        if jumps_in {
            continue;
        }
        // One pass over the loop: the registers it writes, whether it has
        // loads, and no call through the runtime, which puts its callee's
        // frame at the call's window, below the moved loads' registers: the
        // callee's registers run over them. Direct calls' frames are past
        // them (`place_direct_frames`), as are their callees' after an exit.
        let mut wset: RegSet = [0; 4];
        let (mut loads_in, mut runtime_call) = (false, false);
        for k in t..=e {
            let i = &insns[k];
            if i.bail {
                continue;
            }
            match i.op {
                Op::Star => rs_add(&mut wset, i.reg as usize),
                Op::Mov => rs_add(&mut wset, i.reg2 as usize),
                Op::GetNamed => loads_in = true,
                _ if i.is_call() => {
                    runtime_call |= !matches!(out.props[k], Prop::Call { direct: true, .. })
                }
                _ => {}
            }
        }
        if !loads_in || runtime_call {
            continue;
        }
        let written = |r: u8| rs_has(&wset, r as usize);
        let mut loads: Vec<(usize, Src)> = Vec::new();
        // Going through the loop: each register's last write, and the
        // last jump target, so far.
        let mut last_write = [usize::MAX; 256];
        let mut last_target = t;
        for k in t..=e {
            let i = insns[k];
            if target[k] {
                last_target = k;
            }
            let wrote = (!i.bail).then(|| match i.op {
                Op::Star => Some(i.reg),
                Op::Mov => Some(i.reg2),
                _ => None,
            });
            let record = |last_write: &mut [usize; 256]| {
                if let Some(Some(r)) = wrote {
                    last_write[r as usize] = k;
                }
            };
            if taken[k] || i.op != Op::GetNamed || i.bail || i.check_only || i.guard != 0 {
                record(&mut last_write);
                continue;
            }
            let (shape, ft) = match out.props[k] {
                Prop::Own { shape, ft, .. } | Prop::Oop { shape, ft, .. } => (shape, ft),
                _ => continue,
            };
            if !matches!(ft, FieldType::Int | FieldType::Num | FieldType::Obj(_)) {
                continue;
            }
            let name = out.fbs[i.ic_frame()].name(&i);
            let sh = unsafe { &*(shape as *const Shape) };
            let constant = sh
                .lookup(name)
                .is_some_and(|slot| sh.field_cell(slot).is_constant());
            let origin = match i.hoisted {
                Some((f, pc)) => (out.frames[f as usize].code as usize, pc as usize),
                None => (out.frames[i.frame as usize].code as usize, i.pc),
            };
            if !constant || set.contains(&name) || exits.contains_key(&origin) {
                continue;
            }
            let src = if !written(i.reg) {
                Src::Reg(i.reg)
            } else {
                // The write just before it, in the same block.
                let j = last_write[i.reg as usize];
                if j == usize::MAX || last_target > j || j == t || target[j] {
                    continue;
                }
                let (w, prev) = (insns[j], insns[j - 1]);
                match (w.op, prev.op) {
                    (Op::Star, Op::Ldar) if !prev.bail && !written(prev.reg) => Src::Reg(prev.reg),
                    (Op::Star, Op::GetNamed) if loads.iter().any(|l| l.0 == j - 1) => {
                        Src::Load(j - 1)
                    }
                    (Op::Mov, _) if !written(w.reg) => Src::Reg(w.reg),
                    _ => continue,
                }
            };
            loads.push((k, src));
        }
        // Only chains: a load from a hoisted load's value (`this.v.elms`),
        // and those it reads. One load from a register the loop keeps
        // would be one load from the frame instead: nothing gained (31
        // 20% slower, the stores to the frame in the loop).
        let mut keep: Vec<bool> = loads.iter().map(|l| matches!(l.1, Src::Load(_))).collect();
        for k in (0..loads.len()).rev() {
            if let (true, Src::Load(j)) = (keep[k], loads[k].1)
                && let Some(p) = loads.iter().position(|l| l.0 == j)
            {
                keep[p] = true;
            }
        }
        let mut kept = keep.iter();
        loads.retain(|_| *kept.next().unwrap());
        if loads.is_empty() {
            continue;
        }
        type Place = (usize, Option<(usize, usize)>, bool);
        let mut places: Vec<Place> = if fall {
            vec![(t, Some((t, e)), false)]
        } else if insns[t].frame == 0 {
            vec![(t - 1, None, true), (entry_at(t), None, false)]
        } else {
            vec![(t - 1, None, true)]
        };
        places.extend(nested.iter().map(|&at| (at, None, false)));
        if places.iter().any(|p| groups.iter().any(|g| g.at == p.0)) {
            continue;
        }
        let new = loads.iter().filter(|l| moved(l.0).is_none()).count();
        if used + count + new > max {
            continue;
        }
        count += new;
        for &(k, _) in &loads {
            taken[k] = true;
        }
        for (at, enter, all) in places {
            groups.push(Group {
                at,
                loads: loads.clone(),
                frame: insns[at].frame,
                pc: insns[at].pc,
                enter,
                all,
            });
        }
    }
    if groups.is_empty() {
        return 0;
    }
    // Each hoisted load's register: its own, if moved before.
    let mut reg_of: HashMap<usize, u8> = HashMap::default();
    let mut next = base + used;
    for g in &groups {
        for &(k, _) in &g.loads {
            reg_of.entry(k).or_insert_with(|| {
                moved(k).unwrap_or_else(|| {
                    next += 1;
                    (next - 1) as u8
                })
            });
        }
    }
    // The `Star`s of loads moved again go with them.
    let gone: HashSet<usize> = (reg_of.keys())
        .filter(|&&k| moved(k).is_some())
        .map(|&k| k + 1)
        .collect();
    for g in &groups {
        for &(k, _) in &g.loads {
            let i = &out.insns[k];
            let name = out.fbs[i.ic_frame()].name(i);
            if let Prop::Own { shape, .. } | Prop::Oop { shape, .. } = out.props[k] {
                let sh = unsafe { &*(shape as *const Shape) };
                if let Some(slot) = sh.lookup(name) {
                    sh.field_cell(slot).fold();
                }
            }
        }
    }
    groups.sort_by_key(|g| g.at);
    let mut new_of = vec![0; n + 1];
    let mut start_of: HashMap<usize, usize> = HashMap::default();
    let extra: usize = groups.iter().map(|g| 2 * g.loads.len()).sum();
    let (mut insns2, mut props2) = (Vec::with_capacity(n + extra), Vec::with_capacity(n + extra));
    let mut g = 0;
    for k in 0..=n {
        while g < groups.len() && groups[g].at == k {
            start_of.insert(k, insns2.len());
            let (frame, pc) = (groups[g].frame as usize, groups[g].pc);
            // (Copies of one load, before two entries of an inner loop,
            // load once.)
            let mut done: Vec<u8> = Vec::new();
            for &(l, src) in &groups[g].loads {
                if done.contains(&reg_of[&l]) {
                    continue;
                }
                done.push(reg_of[&l]);
                let orig = out.insns[l];
                let mut load = Insn {
                    frame: frame as u8,
                    pc,
                    hoisted: orig.hoisted.or(Some((orig.frame, orig.pc as u32))),
                    ..orig
                };
                load.reg = match src {
                    Src::Reg(r) => r,
                    Src::Load(j) => reg_of[&j],
                };
                insns2.push(load);
                props2.push(out.props[l]);
                insns2.push(synth(Op::Star, reg_of[&l], frame, pc));
                props2.push(Prop::None);
            }
            g += 1;
        }
        new_of[k] = insns2.len();
        if k < n {
            let i = out.insns[k];
            match reg_of.get(&k) {
                // Moved again, with its `Star`: nothing left here.
                Some(_) if moved(k).is_some() => {
                    insns2.push(synth(Op::Nop, 0, i.frame as usize, i.pc));
                    props2.push(Prop::None);
                }
                _ if gone.contains(&k) => {
                    insns2.push(synth(Op::Nop, 0, i.frame as usize, i.pc));
                    props2.push(Prop::None);
                }
                // The loop reads the register.
                Some(&r) => {
                    insns2.push(synth(Op::Ldar, r, i.frame as usize, i.pc));
                    props2.push(Prop::None);
                }
                None => {
                    insns2.push(i);
                    props2.push(out.props[k]);
                }
            }
        }
    }
    for (k, i) in out.insns.iter().enumerate() {
        if !(i.jumps() || i.bail && i.target > 0) {
            continue;
        }
        let t = i.target;
        let to = match groups.iter().find(|g| g.at == t) {
            Some(g) if g.all => start_of[&t],
            Some(g) => match g.enter {
                Some((a, b)) if !(a <= k && k <= b) => start_of[&t],
                _ => new_of[t],
            },
            None => new_of[t],
        };
        insns2[new_of[k]].target = to;
    }
    // Objects to allocate at exits: by the new indices, and for the moved
    // loads those of the instruction they go before.
    let mut mats: HashMap<usize, Vec<Rc<Mat>>> = HashMap::default();
    for (k, m) in std::mem::take(&mut out.mats) {
        if let Some(&start) = start_of.get(&k) {
            for j in start..new_of[k] {
                mats.insert(j, m.clone());
            }
        }
        mats.insert(new_of[k], m);
    }
    // Another round only if loads went before a loop inside another.
    let nested = groups
        .iter()
        .any(|g| loops.iter().any(|&(u, v)| u < g.at && g.at <= v));
    out.mats = mats;
    out.insns = insns2;
    out.props = props2;
    if nested { reg_of.len() } else { 0 }
}

/// Jumps on a value known where they are: taken or not whatever happens,
/// so they become `Jump` or nothing. Known: what `LdaTrue`, `LdaFalse`,
/// `LdaUndefined`, `LdaNull` and `LdaSmi` load, through `Star`, `Ldar` and
/// `Mov`, along straight-line code: an inlined `return true` tested by the
/// caller, or a parameter an inlined call leaves `undefined`
/// (05-logical-or-default's `initial || 0`). Not past where control also
/// arrives from elsewhere (a jump target, which loop entries are), a bail,
/// or a call. `JumpIfTrue` and `JumpIfFalse` fold on booleans only.
fn fold_constant_tests(insns: &mut [Insn]) {
    // A value's truth, and whether it's a boolean.
    #[derive(Clone, Copy)]
    struct Known {
        truth: bool,
        boolean: bool,
    }
    let mut target = vec![false; insns.len()];
    for i in insns.iter().filter(|i| i.jumps()) {
        if let Some(t) = target.get_mut(i.target) {
            *t = true;
        }
    }
    let mut regs: Vec<Option<Known>> = vec![None; 256];
    let mut acc: Option<Known> = None;
    let constant = |truth, boolean| Some(Known { truth, boolean });
    for k in 0..insns.len() {
        let i = insns[k];
        if target[k] || i.bail || i.is_call() {
            regs.fill(None);
            acc = None;
        }
        if i.bail {
            continue;
        }
        match i.op {
            Op::LdaTrue => acc = constant(true, true),
            Op::LdaFalse => acc = constant(false, true),
            Op::LdaUndefined | Op::LdaNull => acc = constant(false, false),
            Op::LdaSmi => acc = constant(i.imm != 0, false),
            Op::Ldar => acc = regs[i.reg as usize],
            Op::Star => regs[i.reg as usize] = acc,
            Op::Mov => regs[i.reg2 as usize] = regs[i.reg as usize],
            Op::Nop if i.alloc == 0 => {}
            Op::JumpIfToBooleanTrue
            | Op::JumpIfToBooleanFalse
            | Op::JumpIfTrue
            | Op::JumpIfFalse => {
                let on = matches!(i.op, Op::JumpIfToBooleanTrue | Op::JumpIfTrue);
                let to_boolean = matches!(i.op, Op::JumpIfToBooleanTrue | Op::JumpIfToBooleanFalse);
                if let Some(v) = acc
                    && (to_boolean || v.boolean)
                {
                    insns[k].op = if v.truth == on { Op::Jump } else { Op::Nop };
                }
            }
            _ => acc = None,
        }
        if !insns[k].falls_through() {
            regs.fill(None);
            acc = None;
        }
    }
}

impl Lowering<'_> {
    fn feedback(&self, code: *const crate::vm::Code) -> Feedback {
        let code = unsafe { &*code };
        Feedback {
            ics: code.ics.as_ptr().cast(),
            names: code.ic_names.as_ptr(),
            global: unsafe { (*self.state.vm).global },
            vm: self.state.vm,
            constants: code.constants.as_ptr(),
        }
    }

    /// Function `code`'s registers live into each instruction as the
    /// interpreter runs it, by bytecode offset (see [`bytecode_live`]).
    fn pc_live(&mut self, code: *const crate::vm::Code) -> Rc<PcLive> {
        if let Some(l) = self.caches().pc_live.get(&code) {
            return l.clone();
        }
        let d = self.decoded(code);
        let live = bytecode_live(unsafe { &(*code).bytecode }, &d.0);
        let l: Rc<PcLive> = Rc::new(d.0.iter().map(|i| i.pc).zip(live).collect());
        self.caches().pc_live.insert(code, l.clone());
        l
    }

    /// The registers function `code` may read before writing them, when
    /// called: those a direct call must set (`this`, the parameters, and
    /// locals read as `undefined`).
    fn entry_live(&mut self, code: *const crate::vm::Code) -> Rc<Vec<bool>> {
        if let Some(l) = self.caches().entry_live.get(&code) {
            return l.clone();
        }
        let first = self.pc_live(code).get(&0).copied().unwrap_or([0; 4]);
        let nregs = unsafe { (*code).nregs } as usize;
        let l: Rc<Vec<bool>> = Rc::new((0..nregs).map(|r| rs_has(&first, r)).collect());
        self.caches().entry_live.insert(code, l.clone());
        l
    }

    /// The VM's [`Caches`].
    fn caches(&mut self) -> &mut Caches {
        unsafe { &mut (*self.state.vm).jit_caches }
    }

    fn decoded(&mut self, code: *const crate::vm::Code) -> Decoded {
        self.caches().decoded(code)
    }

    fn push(&mut self, insn: Insn, prop: Prop) -> usize {
        self.out.insns.push(insn);
        self.out.props.push(prop);
        self.out.insns.len() - 1
    }

    /// Emit frame `frame`'s instructions; returns the indices of the jumps
    /// its `Return`s became (an inlined frame's), to point past it.
    fn emit_frame(&mut self, frame: usize, depth: usize) -> Vec<usize> {
        let f = self.out.frames[frame];
        let code_key = f.code as usize;
        let d = self.decoded(f.code);
        let (insns, acc_out) = (&d.0, &d.1);
        let base = f.base;
        let mut start = vec![0usize; insns.len() + 1];
        let (mut fix, mut rets) = (Vec::new(), Vec::new());
        let aliases: Vec<(u8, u8)> = (self.out.aliases.iter())
            .filter(|a| a.0 as usize == frame)
            .map(|a| (a.1, a.2))
            .collect();
        let mut is_target = vec![false; insns.len() + 1];
        for j in insns.iter().filter(|j| j.jumps()) {
            is_target[j.target] = true;
        }
        for (k, orig) in insns.iter().enumerate() {
            start[k] = self.out.insns.len();
            let mut i = *orig;
            i.frame = frame as u8;
            let renamed = |r: u8| {
                let r = (r as usize + base) as u8;
                aliases.iter().find(|a| a.0 == r).map_or(r, |a| a.1)
            };
            match i.op {
                Op::Ldar
                | Op::Star
                | Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::Mod
                | Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::ShiftLeft
                | Op::ShiftRight
                | Op::ShiftRightLogical
                | Op::JumpIfLess
                | Op::JumpIfLessEqual
                | Op::JumpIfGreater
                | Op::JumpIfGreaterEqual
                | Op::TestLessThan
                | Op::TestGreaterThan
                | Op::TestLessThanOrEqual
                | Op::TestGreaterThanOrEqual
                | Op::TestEqual
                | Op::TestStrictEqual
                | Op::TestInstanceOf
                | Op::TestIn
                | Op::GetNamed
                | Op::SetNamed
                | Op::GetKeyed => i.reg = renamed(i.reg),
                Op::Mov | Op::Call | Op::CallUndefined | Op::Construct | Op::SetKeyed => {
                    (i.reg, i.reg2) = (renamed(i.reg), renamed(i.reg2))
                }
                _ => {}
            }
            self.fwd = match i.op {
                Op::Call | Op::CallUndefined | Op::Construct if !i.bail => {
                    window_sources(insns, &is_target, k)
                        .into_iter()
                        .map(|(w, x)| (renamed(w), renamed(x)))
                        .collect()
                }
                _ => Vec::new(),
            };
            let origin = (code_key, i.pc);
            // `f.call(that, …args)` (see `vm::VIA_CALL`): a guard that the
            // callee is `Function.prototype.call`, then a call of `f`, the
            // window one register on.
            if i.op == Op::Call && !i.bail {
                let ic = unsafe { *self.out.fbs[frame].ic(i.ic as usize) };
                if ic.loc == crate::vm::VIA_CALL && !ic.holder.is_null() {
                    let call = unsafe { (*self.state.vm).function_call_fn };
                    if !self.proven_callee(frame, i.reg, call) {
                        let mut guard = synth(Op::Nop, i.reg, frame, i.pc);
                        guard.guard = call as u64;
                        self.push(guard, Prop::None);
                    }
                    i.reg = i.reg2;
                    i.reg2 += 1;
                    // `X.call(this, …)` passes the caller's `this`, never
                    // `undefined` or `null` (sloppy mode).
                    let this = renamed(0);
                    i.nullable_this = !self.fwd.iter().any(|&(w, x)| w == i.reg2 && x == this);
                    if i.imm == 0 {
                        i.op = Op::CallUndefined;
                    } else {
                        i.imm -= 1;
                    }
                }
            }
            // `a.push(v)` and `a.pop()`, when that is all the call has
            // called: a guard on the callee (unless `proven_callee`), then
            // inline (see `array_push` in the stencils). Not after exits
            // there (a receiver not an array).
            if i.op == Op::Call && !i.bail {
                let ic = self.out.fbs[frame].ic(i.ic as usize);
                let (fo, other) = unsafe { ((*ic).holder, (*ic.add(1)).holder) };
                let vm = unsafe { &*self.state.vm };
                let native = match (fo, i.imm) {
                    (f, 1) if f == vm.array_push_fn => 1,
                    (f, 0) if f == vm.array_pop_fn => 2,
                    _ => 0,
                };
                let exits = self.state.callee_exits.get(&origin).copied().unwrap_or(0);
                if native != 0 && other.is_null() && exits < MAX_PROPERTY_EXITS {
                    // `push` takes its argument in `acc` (there already if
                    // the call's setup stored it last).
                    let stored = self.out.insns.last().is_some_and(|l| {
                        l.op == Op::Star && l.reg == i.reg2 + 1 && l.frame == i.frame && !l.bail
                    });
                    if !self.proven_callee(frame, i.reg, fo) {
                        let mut guard = synth(Op::Nop, i.reg, frame, i.pc);
                        guard.guard = fo as u64;
                        self.push(guard, Prop::None);
                    } else if self.dead_after(f.code, k, orig.reg)
                        && self.drop_callee_load(frame, i.reg)
                    {
                        i.exit_const = crate::object::object_value(fo).to_bits();
                    }
                    if native == 1 && !stored {
                        self.push(synth(Op::Ldar, i.reg2 + 1, frame, i.pc), Prop::None);
                    }
                    i.native = native;
                    self.push(i, Prop::None);
                    continue;
                }
            }
            // `new Array()`: a guard on the callee (unless proven), then a
            // new empty array, as `[]` makes (DeltaBlue's collections make
            // one each, which went through the runtime's `construct`).
            if i.op == Op::Construct && i.imm == 0 && !i.bail {
                let ic = self.out.fbs[frame].ic(i.ic as usize);
                let (fo, other) = unsafe { ((*ic).holder, (*ic.add(1)).holder) };
                let array = unsafe { (*self.state.vm).array_fn };
                let exits = self.state.callee_exits.get(&origin).copied().unwrap_or(0);
                if !array.is_null() && fo == array && other.is_null() && exits < MAX_PROPERTY_EXITS
                {
                    if !self.proven_callee(frame, i.reg, fo) {
                        let mut guard = synth(Op::Nop, i.reg, frame, i.pc);
                        guard.guard = fo as u64;
                        self.push(guard, Prop::None);
                    }
                    self.push(synth(Op::CreateArray, 0, frame, i.pc), Prop::None);
                    continue;
                }
            }
            match i.op {
                _ if i.bail => {
                    self.push(i, Prop::None);
                }
                // The function returns from its compiled code: the caller of
                // the code (`vm_run`) finishes the return.
                Op::Return if frame == 0 => {
                    self.push(i, Prop::None);
                }
                Op::Return => {
                    // An inlined frame returns: `acc` is the result.
                    let mut j = synth(Op::Jump, 0, frame, i.pc);
                    j.target = usize::MAX;
                    rets.push(self.push(j, Prop::None));
                }
                Op::Mov if !acc_out[k] => {
                    self.push(synth(Op::Ldar, i.reg, frame, i.pc), Prop::None);
                    self.push(synth(Op::Star, i.reg2, frame, i.pc), Prop::None);
                }
                // `acc` is live across it: kept in a scratch register.
                Op::Mov if self.out.nregs + interp::HEADER < 240 => {
                    let k = self.push(synth(Op::Star, 0, frame, i.pc), Prop::None);
                    self.push(synth(Op::Ldar, i.reg, frame, i.pc), Prop::None);
                    self.push(synth(Op::Star, i.reg2, frame, i.pc), Prop::None);
                    let l = self.push(synth(Op::Ldar, 0, frame, i.pc), Prop::None);
                    self.mov_scratch.extend([k, l]);
                }
                Op::Mov => {
                    i.bail = true;
                    self.push(i, Prop::None);
                }
                Op::Call | Op::CallUndefined
                    if frame == 0 && self.tail_call(&i, insns.get(k + 1)).is_some() =>
                {
                    let fo = self.tail_call(&i, insns.get(k + 1)).unwrap();
                    let jump = self.emit_tail_call(&i, fo);
                    fix.push(jump);
                }
                // A call that never ran: nothing to inline or call directly
                // yet. The interpreter runs it, filling its cache, and the
                // exit makes the next entry lower the function again.
                Op::Call | Op::CallUndefined
                    if unsafe { (*self.out.fbs[frame].ic(i.ic as usize)).holder.is_null() } =>
                {
                    i.bail = true;
                    self.push(i, Prop::None);
                }
                Op::Call | Op::CallUndefined if self.inline_targets(frame, &i, depth).len() > 1 => {
                    let fos = self.inline_targets(frame, &i, depth);
                    let dead = self.dead_after(f.code, k, orig.reg);
                    self.inline_poly(frame, &i, &fos, depth, dead);
                }
                Op::Call | Op::CallUndefined if self.inline_target(frame, &i, depth).is_some() => {
                    let callee = self.inline_target(frame, &i, depth).unwrap();
                    let fwd = self.fwd.clone();
                    let guard = self.inline(frame, &i, callee, depth);
                    self.drop_setup(frame, k, orig.reg, &i, guard, callee, &fwd);
                }
                Op::Construct if self.inline_ctor(frame, &i, depth).is_some() => {
                    let callee = self.inline_ctor(frame, &i, depth).unwrap();
                    self.inline_construct(frame, &i, callee, depth);
                }
                Op::Call | Op::CallUndefined if self.cached_callees(frame, &i).len() > 1 => {
                    // Several callees, not inlined: a direct call to each,
                    // dispatched on the callee; the last one's site calls
                    // any other through the runtime. (Its guards jump to
                    // the next case, never exit: exits that gave up
                    // inlining here don't count. With them, DeltaBlue's
                    // `plan.execute()` called its second test's `execute`
                    // through the runtime: 135 calls a run.)
                    let fos = self.cached_callees(frame, &i);
                    let mut ends = Vec::new();
                    for (n, &fo) in fos.iter().enumerate() {
                        let last = n + 1 == fos.len();
                        let guard = (!last).then(|| {
                            let mut g = synth(Op::Nop, i.reg, frame, i.pc);
                            (g.guard, g.next_case) = (fo as u64, true);
                            self.push(g, Prop::None)
                        });
                        self.emit_call(frame, i, fo, guard.is_some());
                        if let Some(g) = guard {
                            let mut j = synth(Op::Jump, 0, frame, i.pc);
                            j.target = usize::MAX;
                            ends.push(self.push(j, Prop::None));
                            self.out.insns[g].target = self.out.insns.len();
                        }
                    }
                    self.return_past(ends);
                }
                Op::Call | Op::CallUndefined | Op::Construct => {
                    let fo = unsafe { (*self.out.fbs[frame].ic(i.ic as usize)).holder };
                    self.emit_call(frame, i, fo, false);
                }
                _ if i.jumps() => {
                    let x = self.push(i, Prop::None);
                    fix.push(x);
                }
                // An upvalue: in an inlined frame, of its (checked) callee,
                // whose cells are known; in the root frame, through the
                // frame's function (`lda_upvalue`).
                Op::LdaUpvalue | Op::StaUpvalue => {
                    let func = self.out.frames[frame].func;
                    let prop = if func.is_null() {
                        Prop::None
                    } else {
                        let cell = unsafe { *crate::vm::Vm::upvalues(func).add(i.imm as usize) };
                        Prop::Upvalue { cell: cell as u64 }
                    };
                    self.push(i, prop);
                }
                _ => {
                    let given_up = self
                        .state
                        .property_exits
                        .get(&origin)
                        .is_some_and(|&n| n >= MAX_PROPERTY_EXITS);
                    let prop = match i.op {
                        // (Any key: a name, a typed array's index, ….)
                        Op::GetKeyed if given_up => Prop::Runtime,
                        Op::SetKeyed if given_up => Prop::Runtime,
                        _ if given_up && i.is_property() => {
                            i.bail = true;
                            Prop::None
                        }
                        _ => self.out.fbs[frame].prop(&i, self.epoch, &mut self.out.polys),
                    };
                    self.push(i, prop);
                }
            }
        }
        start[insns.len()] = self.out.insns.len();
        for x in fix {
            let t = self.out.insns[x].target;
            self.out.insns[x].target = start[t];
        }
        rets
    }

    /// Whether call `i` of frame `frame` may go straight into its callee's
    /// compiled code: its cache names a bytecode function. Small ones,
    /// which calls from the interpreter don't enter compiled (see
    /// `vm::has_loop`), are worth it here too: a direct call
    /// costs much less than one through the runtime. Fills `site` in with
    /// the callee; [`bind_direct`] picks the version when compiling, from the
    /// arguments' types, or for this function `JitCode::compile` binds the
    /// version it compiles, if they fit it. Notes the function the cache
    /// names in `site.cached` either way, natives included.
    /// A call site for call `i` of frame `frame`, its callee unknown yet.
    fn call_site(&self, frame: usize, i: &Insn) -> CallSite {
        let f = self.out.frames[frame];
        let vm = self.state.vm;
        CallSite {
            entry: 0,
            closure: 0,
            f_off: 8 * i.reg as u64,
            end_off: 0,
            stack_end: unsafe { (*vm).stack_end } as u64,
            native_limit: unsafe { &raw const (*vm).native_limit },
            this_shape: 0,
            ctor: None,
            vm,
            op: i.op,
            f: i.reg as usize,
            w: i.reg2 as usize,
            argc: i.imm as usize,
            frame: f.base + unsafe { (*f.code).nregs } as usize + interp::HEADER,
            nparams: 0,
            exits: 0,
            callee: std::ptr::null(),
            cached: std::ptr::null(),
            target: None,
            self_call: false,
            self_code: std::ptr::null(),
            bound: false,
            claim: None,
            pending: false,
            retry: 0,
        }
    }

    /// How `new fo` allocates `this` now (see [`CtorSite`]), and the shape
    /// it gives it.
    fn ctor_site(&mut self, fo: *mut Object) -> (Box<CtorSite>, *const Shape) {
        unsafe {
            let vm = &mut *self.state.vm;
            let callee = (*fo).code;
            let shape = vm.ctor_root(fo);
            let proto_at = Object::inline(fo, 0).cast::<u64>();
            vm.heap.keep(fo as usize);
            vm.heap.keep(*proto_at as usize);
            let cap_at = (*callee).ctor_capacity.as_ptr();
            let site = Box::new(CtorSite {
                heap: vm.heap.bump(),
                proto_at,
                proto: *proto_at,
                cap_at,
                cap: *cap_at as u64,
                slots: (*shape).capacity.get() as u64,
                // Set when the frame is placed.
                this_off: 0,
                done_shape: 0,
                header: crate::object::object_header(&*shape),
            });
            (site, shape)
        }
    }

    /// Call `i` of frame `frame` through a site of its own: straight into
    /// `fo`'s compiled code if it may (see [`Lowering::direct_target`]),
    /// else through the runtime.
    /// `guarded`: a guard that the callee register holds `fo` comes just
    /// before.
    fn emit_call(&mut self, frame: usize, i: Insn, fo: *mut Object, guarded: bool) {
        let mut site = self.call_site(frame, &i);
        let direct = self.direct_target(&i, fo, &mut site) && i.imm as u32 <= site.nparams;
        let proven = direct && (guarded || self.proven_callee(frame, i.reg, fo));
        let p = Prop::Call {
            site: self.out.sites.len() as u32,
            direct,
            proven,
            // (The compiled function's own epoch: see `Code::field_deps`.)
            epoch: unsafe { (*self.state.root).field_deps.get() },
            epoch_addr: unsafe { (*self.state.root).field_deps.as_ptr() } as u64,
        };
        let callee = site.callee;
        self.out
            .sites
            .push(Box::new(std::cell::UnsafeCell::new(site)));
        if direct {
            self.direct_setup(frame, i, callee, p);
        } else {
            self.push(i, p);
        }
    }

    /// The bytecode functions call `i`'s cache has seen, if more than one
    /// and all of them are (natives, `f.call`, … go through the runtime).
    fn cached_callees(&self, frame: usize, i: &Insn) -> Vec<*mut Object> {
        use crate::object::Class;
        let ic = self.out.fbs[frame].ic(i.ic as usize);
        let mut fos = Vec::new();
        for k in 0..crate::vm::IC_WAYS {
            let w = unsafe { *ic.add(k) };
            if w.holder.is_null() {
                break;
            }
            if unsafe { (*w.holder).class } != Class::Function || w.loc == crate::vm::VIA_CALL {
                return Vec::new();
            }
            if !fos.contains(&w.holder) {
                fos.push(w.holder);
            }
        }
        if fos.len() > 1 { fos } else { Vec::new() }
    }

    fn direct_target(&mut self, i: &Insn, fo: *mut Object, site: &mut CallSite) -> bool {
        use crate::object::Class;
        if fo.is_null() {
            return false;
        }
        let class = unsafe { (*fo).class };
        if matches!(class, Class::Function | Class::Native) {
            site.cached = unsafe { (*fo).code };
        }
        if class != Class::Function {
            return false;
        }
        let callee = unsafe { (*fo).code };
        // Not compiled: its exception handlers need interpreted frames.
        // Nor a `that` of `f.call(that, …)` to convert: the runtime does.
        if unsafe { (*callee).interpreted } || self.nullable_this(i, fo) {
            return false;
        }
        // A constructor's `this` is allocated by the call
        // (`toi_rt_construct`), with the shape its root has now.
        if i.op == Op::Construct {
            let (ctor, shape) = self.ctor_site(fo);
            site.this_shape = shape as u64;
            site.ctor = Some(ctor);
        }
        site.self_call = callee == self.state.root && i.op != Op::Construct;
        // Warm code calls directly only code compiled already (itself
        // included): binding compiles the callee, which a warm caller
        // shouldn't pay for (its callees tier up on their own).
        if self.state.tier == Tier::Warm && !site.self_call {
            let compiled = unsafe { (*callee).jit.try_borrow() }
                .ok()
                .is_some_and(|f| f.as_ref().is_some_and(|f| !f.versions.is_empty()));
            if !compiled {
                return false;
            }
        }
        site.closure = fo as u64;
        unsafe { (*self.state.vm).heap.keep(fo as usize) };
        site.nparams = unsafe { (*callee).nparams };
        site.callee = callee;
        true
    }

    /// Whether call `i` of the function compiled (frame 0), followed by
    /// `next`, is a self tail call to compile as a jump to its start: its
    /// result is returned at once, its cache names this function, it passes
    /// no more arguments than there are parameters, and (for `Call`) the
    /// receiver isn't one of the registers the arguments go to. Not after
    /// [`MAX_PROPERTY_EXITS`] exits at its guard. Returns the function.
    fn tail_call(&self, i: &Insn, next: Option<&Insn>) -> Option<*mut Object> {
        use crate::object::Class;
        let root = self.state.root;
        if next.is_none_or(|n| n.op != Op::Return || n.bail)
            || i.imm as u32 > unsafe { (*root).nparams }
            || (i.op == Op::Call && i.reg2 as i32 <= i.imm)
            || (i.op == Op::Call && i.nullable_this)
            || self
                .state
                .callee_exits
                .get(&(root as usize, i.pc))
                .is_some_and(|&n| n >= MAX_PROPERTY_EXITS)
        {
            return None;
        }
        let fo = unsafe { (*self.out.fbs[0].ic(i.ic as usize)).holder };
        // (Not for a function with upvalues: another closure of the same
        // code has other cells, and the frame's header would keep this
        // one's.)
        let ok = !fo.is_null()
            && unsafe {
                (*fo).class == Class::Function && (*fo).code == root && (&(*root).upvalues).is_empty()
            };
        ok.then_some(fo)
    }

    /// A self tail call `i` (see [`Lowering::tail_call`]) to `fo`: a guard
    /// that the callee register holds `fo` and the frame isn't a `new`
    /// one, the arguments moved into the parameters, `this` set, the
    /// registers the function may read before writing (`entry_live`) set
    /// to `undefined`, and a jump to the start, whose index it returns for
    /// fixing up. The frame is reused: deep tail recursion runs in constant
    /// space. The moves need no temporaries: the arguments are consecutive
    /// registers, moved in the order that reads each before it is
    /// overwritten.
    fn emit_tail_call(&mut self, i: &Insn, fo: *mut Object) -> usize {
        let root = self.state.root;
        let live = self.entry_live(root);
        let (pc, argc) = (i.pc, i.imm as usize);
        let mut guard = synth(Op::Nop, i.reg, 0, pc);
        guard.guard = fo as u64;
        guard.tail = true;
        self.push(guard, Prop::None);
        let mov = |cx: &mut Self, src: Option<usize>, dst: usize| {
            match src {
                Some(r) => cx.push(synth(Op::Ldar, r as u8, 0, pc), Prop::None),
                None => cx.push(synth(Op::LdaUndefined, 0, 0, pc), Prop::None),
            };
            cx.push(synth(Op::Star, dst as u8, 0, pc), Prop::None);
        };
        let w = i.reg2 as usize;
        let first = w + usize::from(i.op == Op::Call);
        // Arguments from below the parameters move last-first, from above
        // first-first (they don't overlap `this`: see `tail_call`).
        let order: Vec<usize> = if first == 0 {
            (0..argc).rev().collect()
        } else if first > 1 {
            (0..argc).collect()
        } else {
            Vec::new()
        };
        for a in order {
            mov(self, Some(first + a), 1 + a);
        }
        if i.op == Op::Call {
            mov(self, Some(w), 0);
        } else if live[0] {
            self.global_this(0, pc);
            self.push(synth(Op::Star, 0, 0, pc), Prop::None);
        }
        for (r, &l) in live.iter().enumerate().skip(1 + argc) {
            if l {
                mov(self, None, r);
            }
        }
        let mut jump = synth(Op::Jump, 0, 0, pc);
        jump.target = 0;
        self.push(jump, Prop::None)
    }

    /// The function to inline for call `i` of frame `frame`, if any.
    fn inline_target(&mut self, frame: usize, i: &Insn, depth: usize) -> Option<*mut Object> {
        if self.state.tier == Tier::Warm {
            return None;
        }
        let f = self.out.frames[frame];
        let ic = unsafe { *self.out.fbs[frame].ic(i.ic as usize) };
        self.call_in_loop = self.in_loop(f.code, i.pc);
        if (depth >= MAX_INLINE_DEPTH && !self.small_leaf(ic.holder))
            || self.out.frames.len() >= 255
            || self
                .state
                .callee_exits
                .get(&(f.code as usize, i.pc))
                .is_some_and(|&n| n >= MAX_PROPERTY_EXITS)
        {
            return None;
        }
        if self.nullable_this(i, ic.holder) {
            return None;
        }
        self.inlinable(frame, ic.holder).then_some(ic.holder)
    }

    /// Whether call `i` may pass `fo` an `undefined` or `null` `this`
    /// (`f.call(null, …)`) that `fo` reads, and would see as the global
    /// object: then made through the runtime, which converts it.
    fn nullable_this(&mut self, i: &Insn, fo: *mut Object) -> bool {
        i.op == Op::Call
            && i.nullable_this
            && !fo.is_null()
            && unsafe { (*fo).class } == crate::object::Class::Function
            && self.entry_live(unsafe { (*fo).code })[0]
    }

    /// Load the global object into `acc`: `this` of a call without a
    /// receiver (sloppy mode).
    fn global_this(&mut self, frame: usize, pc: usize) {
        let g = unsafe { (*self.state.vm).global };
        let prop = Prop::Const {
            value: crate::object::object_value(g).to_bits(),
            at: 0,
            expect: 0,
            ft: FieldType::Any,
            obj: 0,
        };
        self.push(synth(Op::LdaConstant, 0, frame, pc), prop);
    }

    /// Whether the instruction at bytecode offset `pc` of `code` is in a
    /// loop of `code` (between a backward jump and its target).
    fn in_loop(&mut self, code: *const crate::vm::Code, pc: usize) -> bool {
        let d = self.decoded(code);
        let insns = &d.0;
        let Some(k) = insns.iter().position(|j| j.pc == pc) else {
            return false;
        };
        (insns.iter().enumerate()).any(|(b, j)| j.jumps() && j.target <= k && k <= b)
    }

    /// Whether `fo` is a bytecode function of at most [`MAX_LEAF_INSNS`]
    /// instructions that calls nothing: inlined past [`MAX_INLINE_DEPTH`],
    /// as it costs little code and inlines nothing further.
    fn small_leaf(&mut self, fo: *mut Object) -> bool {
        if fo.is_null() || unsafe { (*fo).class } != crate::object::Class::Function {
            return false;
        }
        let d = self.decoded(unsafe { (*fo).code });
        d.0.len() <= MAX_LEAF_INSNS && !d.0.iter().any(|j| j.is_call())
    }

    /// The functions to inline for call `i` of frame `frame` when its
    /// cache recorded several callees: all of them (up to four), if each
    /// can be inlined (see [`Lowering::inline_target`]).
    fn inline_targets(&mut self, frame: usize, i: &Insn, depth: usize) -> Vec<*mut Object> {
        let Some(first) = self.inline_target(frame, i, depth) else {
            return Vec::new();
        };
        // At the last level, a callee's own calls couldn't be inlined: a
        // direct call into its compiled code, which inlines them, is
        // better than inlining it. Calls to small leaves are inlined at
        // any depth, so they don't count (`EqualityConstraint.execute`'s
        // `this.output()` in DeltaBlue: counting them left a call in
        // `chainTest`'s hottest loop, 87M direct calls a run).
        let calls = |cx: &mut Self, fo: *mut Object| {
            let code = unsafe { (*fo).code };
            let fb = cx.feedback(code);
            let d = cx.decoded(code);
            d.0.iter().filter(|j| j.is_call()).any(|j| {
                let ic = fb.ic(j.ic as usize);
                if unsafe { (*ic).loc } == crate::vm::VIA_CALL {
                    return true;
                }
                let holders: Vec<*mut Object> = (0..crate::vm::IC_WAYS)
                    .map(|k| unsafe { (*ic.add(k)).holder })
                    .take_while(|h| !h.is_null())
                    .collect();
                holders.is_empty() || !holders.iter().all(|&h| cx.small_leaf(h))
            })
        };
        let last_level = depth + 1 >= MAX_INLINE_DEPTH;
        let ic = self.out.fbs[frame].ic(i.ic as usize);
        let mut fos = vec![first];
        for k in 1..crate::vm::IC_WAYS {
            let fo = unsafe { (*ic.add(k)).holder };
            if fo.is_null() {
                break;
            }
            if !self.inlinable(frame, fo)
                || self.nullable_this(i, fo)
                || self.out.frames.len() + fos.len() >= 255
            {
                return Vec::new();
            }
            fos.push(fo);
        }
        if last_level && fos.len() > 1 && fos.iter().any(|&fo| calls(self, fo)) {
            return Vec::new();
        }
        fos
    }

    /// Whether `fo` is a bytecode function call sites in frame `frame` may
    /// inline: not recursively more than one level deep, and small enough.
    fn inlinable(&mut self, frame: usize, fo: *mut Object) -> bool {
        use crate::object::Class;
        let f = self.out.frames[frame];
        if fo.is_null() || unsafe { (*fo).class } != Class::Function {
            return false;
        }
        let callee = unsafe { (*fo).code };
        // Exceptions unwind only through interpreted frames.
        if unsafe { (*callee).interpreted } {
            return false;
        }
        if !self.call_in_loop
            && self.inlined + self.decoded(callee).0.len() > INLINE_BUDGET
            && !self.small_leaf(fo)
        {
            return false;
        }
        // Recursively only one level deep: below that, calls to it are
        // direct (to the code being compiled, for this function).
        let mut seen = false;
        let mut g = frame;
        loop {
            if self.out.frames[g].code == callee {
                if seen {
                    return false;
                }
                seen = true;
            }
            if g == 0 {
                break;
            }
            g = self.out.frames[g].parent;
        }
        let base = f.base + unsafe { (*f.code).nregs } as usize + interp::HEADER;
        let nregs = unsafe { (*callee).nregs } as usize;
        base + nregs <= 256 && self.decoded(callee).0.len() <= MAX_INLINE_INSNS
    }

    /// Direct call `i` (with `p`) of frame `frame` to `callee`: store `this`
    /// (for `Call`) and the arguments into the callee's frame, and
    /// `undefined` into the other registers the callee may read before
    /// writing ([`entry_live`]), then call with the frame as the window. The
    /// stores' registers are relative to the frame until
    /// [`Lowering::place_direct_frames`] places it. The direct call then
    /// copies nothing, and the runtime path finds the arguments in the
    /// window. (There are no more arguments than parameters: the fills
    /// would overwrite those the runtime path needs.)
    fn direct_setup(&mut self, frame: usize, mut i: Insn, callee: *const crate::vm::Code, p: Prop) {
        let live = self.entry_live(callee);
        let pc = i.pc;
        let (w, argc) = (i.reg2 as usize, i.imm as usize);
        let has_this = i.op == Op::Call;
        let mut stores = Vec::new();
        // `src`: a register, `undefined` (`None`), or the global object
        // (`usize::MAX`: `this` of a call without a receiver).
        let mut store = |cx: &mut Self, src: Option<usize>, j: usize| {
            match src {
                Some(usize::MAX) => cx.global_this(frame, pc),
                Some(r) => {
                    cx.push(synth(Op::Ldar, r as u8, frame, pc), Prop::None);
                }
                None => {
                    cx.push(synth(Op::LdaUndefined, 0, frame, pc), Prop::None);
                }
            };
            stores.push(cx.push(synth(Op::Star, j as u8, frame, pc), Prop::None));
        };
        if has_this {
            store(self, Some(w), 0);
        } else if live[0] && i.op != Op::Construct {
            store(self, Some(usize::MAX), 0);
        }
        let args = w + usize::from(has_this);
        for a in 0..argc {
            store(self, Some(args + a), 1 + a);
        }
        for (r, &l) in live.iter().enumerate().skip(1 + argc) {
            if l {
                store(self, None, r);
            }
        }
        let window = i.reg2;
        i.reg2 = u8::from(!has_this);
        let call = self.push(i, p);
        self.direct.push(DirectSetup {
            site: match p {
                Prop::Call { site, .. } => site as usize,
                _ => unreachable!(),
            },
            call,
            stores,
            window,
        });
    }

    /// Place direct calls' frames past every register the compiled code
    /// uses (and a frame header), where they can't overlap an inlined
    /// frame's registers, which may live in slots. If that's past the 256
    /// registers instructions can name, the calls go through the runtime
    /// from their original windows instead.
    fn place_direct_frames(&mut self) {
        if self.direct.is_empty() {
            return;
        }
        let base = self.out.nregs + interp::HEADER;
        let extent = self
            .direct
            .iter()
            .flat_map(|d| d.stores.iter().map(|&k| self.out.insns[k].reg as usize + 1))
            .max()
            .unwrap_or(0);
        let fits = base + extent.max(1) <= 256;
        for d in std::mem::take(&mut self.direct) {
            let site = unsafe { &mut *self.out.sites[d.site].get() };
            if fits {
                for &k in &d.stores {
                    self.out.insns[k].reg += base as u8;
                }
                self.out.insns[d.call].reg2 += base as u8;
                site.frame = base;
                site.w = self.out.insns[d.call].reg2 as usize;
                if let Some(c) = &mut site.ctor {
                    c.this_off = 8 * base as u64;
                }
            } else {
                for &k in &d.stores {
                    // The `Ldar` / `LdaUndefined` before each store too.
                    self.out.insns[k - 1].op = Op::Nop;
                    self.out.insns[k].op = Op::Nop;
                }
                self.out.insns[d.call].reg2 = d.window;
                if let Prop::Call { direct, .. } = &mut self.out.props[d.call] {
                    *direct = false;
                }
            }
        }
        if fits {
            self.out.direct_base = base;
            self.out.nregs = base + extent;
        }
    }

    /// The constructor to inline for `new` `i` of frame `frame`, if any: as
    /// for a call ([`Lowering::inline_target`]), and one whose objects
    /// have settled (see `Code::ctor_settled`) on one shape that
    /// `construct_done` has nothing to do for (see `toi_rt_construct_done`),
    /// and that returns nothing (`this` is the result).
    fn inline_ctor(&mut self, frame: usize, i: &Insn, depth: usize) -> Option<*mut Object> {
        let fo = self.inline_target(frame, i, depth)?;
        let code = unsafe { &*(*fo).code };
        let seen = code.ctor_result.get();
        let fits = !seen.is_null()
            && seen != crate::vm::MANY_SHAPES
            && unsafe { (*seen).keys.len() } as u32 <= code.ctor_capacity.get();
        if !code.ctor_settled.get() || !fits {
            return None;
        }
        let d = self.decoded(code);
        let only_undefined = d.0.iter().enumerate().all(|(k, j)| {
            j.op != Op::Return || (k > 0 && d.0[k - 1].op == Op::LdaUndefined && !d.0[k - 1].bail)
        });
        only_undefined.then_some(fo)
    }

    /// `new` `i` of frame `frame` inlined: the guard on the constructor,
    /// `this` allocated (`guard_word` + `create_object`, leaving before
    /// the `new` if the constructor's prototype changed, which would change
    /// its shape) into the callee's `r0`, the arguments, the body, then
    /// `this`.
    /// `construct_done` is left out: the objects have the shape it has
    /// nothing to do for (see [`Lowering::inline_ctor`]); one that ends
    /// with another only misses growing the capacity for later objects.
    fn inline_construct(&mut self, frame: usize, i: &Insn, fo: *mut Object, depth: usize) {
        let f = self.out.frames[frame];
        let base = f.base + unsafe { (*f.code).nregs } as usize + interp::HEADER;
        let mut site = self.call_site(frame, i);
        let (ctor, shape) = self.ctor_site(fo);
        site.ctor = Some(ctor);
        site.this_shape = shape as u64;
        site.closure = fo as u64;
        unsafe { (*self.state.vm).heap.keep(fo as usize) };
        site.callee = unsafe { (*fo).code };
        let site = Box::new(std::cell::UnsafeCell::new(site));
        let alloc = site.get() as u64;
        self.out.sites.push(site);
        let (_, rets) = self.inline_case(frame, i, fo, depth, false, None, alloc);
        self.return_past(rets);
        self.push(synth(Op::Ldar, base as u8, frame, i.pc), Prop::None);
    }

    /// Returns the index of the callee's guard.
    fn inline(&mut self, frame: usize, i: &Insn, fo: *mut Object, depth: usize) -> usize {
        let (guard, rets) = self.inline_case(frame, i, fo, depth, false, None, 0);
        self.return_past(rets);
        guard
    }

    /// After call `i` (instruction `k` of frame `frame`'s code, its callee
    /// register `orig_reg` there) was inlined behind the guard at `guard`
    /// to `fo`: drop the setup only an exit would read. Its callee's load,
    /// if the guard needed no check (`proven_callee`) and the register is
    /// dead after the call: the load only checks the receiver's shape and
    /// its `Star` emits nothing (`Insn::check_only`). A window register
    /// that is a copy of another, `x` (`fwd`, see [`window_sources`]),
    /// which the callee reads instead, if it is dead after the call and the
    /// callee doesn't read it: its `Star` emits nothing. An exit resuming
    /// in the frame between the dropped `Star` and the call gives the
    /// interpreter the function, or `x`'s value ([`Restore`],
    /// `materialize`); one in the callee or after it resumes past the call.
    #[allow(clippy::too_many_arguments)]
    fn drop_setup(
        &mut self,
        frame: usize,
        k: usize,
        orig_reg: u8,
        i: &Insn,
        guard: usize,
        fo: *mut Object,
        fwd: &[(u8, u8)],
    ) {
        let code = self.out.frames[frame].code;
        let insns = &self.out.insns;
        let reads = |j: &Insn, r: u8| {
            (j.uses_reg_value() && j.reg == r)
                || (j.is_call() && !j.bail && j.call_reads().any(|x| x == r as usize))
                || (j.op == Op::SetKeyed && j.reg2 == r)
                || (j.op == Op::Mov && j.reg == r)
        };
        let star = |r: u8| {
            insns[..guard]
                .iter()
                .rposition(|j| j.frame as usize == frame && j.op == Op::Star && j.reg == r)
                .filter(|&s| !insns[s].bail && !insns[s].check_only)
        };
        let body = guard..insns.len();
        // The first instruction after `s` (past code-less `Nop`s) sets
        // `acc` without reading it: a value the dropped code left in `acc`
        // (or didn't) is never seen.
        let then_sets_acc = |s: usize| {
            insns[s + 1..]
                .iter()
                .find(|j| !(j.op == Op::Nop && j.guard == 0 && j.alloc == 0 && j.assume == 0))
                .is_some_and(|j| {
                    !j.bail
                        && j.native == 0
                        && matches!(
                            j.op,
                            Op::Ldar
                                | Op::LdaSmi
                                | Op::LdaUndefined
                                | Op::LdaNull
                                | Op::LdaTrue
                                | Op::LdaFalse
                                | Op::LdaConstant
                                | Op::LdaGlobal
                                | Op::GetNamed
                        )
                })
        };
        let mut drops = Vec::new();
        // The callee's load.
        if insns[guard].guard == 0
            && let Some(s) = star(i.reg)
            && s > 0
            && insns[s - 1].op == Op::GetNamed
            && !insns[s - 1].bail
            && insns[s + 1..].iter().all(|j| !reads(j, i.reg))
            && then_sets_acc(s)
        {
            let value = Restored::Const(crate::object::object_value(fo).to_bits());
            drops.push((s, Some(s - 1), insns[s - 1].pc, orig_reg, i.reg, value));
        }
        for &(w, x) in fwd {
            let Some(s) = star(w) else { continue };
            if insns[s + 1..].iter().any(|j| reads(j, w))
                || !insns[body.clone()].iter().any(|j| reads(j, x))
            {
                continue;
            }
            let orig_w = w - (self.out.frames[frame].base as u8);
            // Its `Ldar x` too, if `acc` isn't read after.
            let ldar = (s > 0
                && insns[s - 1].op == Op::Ldar
                && insns[s - 1].reg == x
                && insns[s - 1].frame as usize == frame
                && !insns[s - 1].bail
                && then_sets_acc(s))
            .then_some(s - 1);
            drops.push((s, ldar, insns[s].pc, orig_w, w, Restored::Reg(x)));
        }
        for (s, get, after, orig, reg, value) in drops {
            if !self.dead_after(code, k, orig) {
                continue;
            }
            self.out.insns[s].check_only = true;
            if let Some(g) = get {
                self.out.insns[g].check_only = true;
            }
            self.out.restores.push(Restore {
                frame: frame as u8,
                after,
                upto: i.pc,
                reg,
                value,
            });
        }
    }

    /// Call `i` inlined for each of the functions `fos`: a case per
    /// function, as [`Lowering::inline`] does it, but for the last one's a
    /// guard that jumps to the next case on another callee (`next_case`),
    /// so only the last exits. The cases' frames share registers (one runs),
    /// so `this` and the arguments move into them once, before the cases
    /// (as many as any callee has parameters; each case sets its other
    /// registers).
    ///
    /// When each callee implies its own receiver shape and nothing reads
    /// the callee's register after the call (`dead`), the load of the
    /// callee goes, and the cases dispatch on the receiver's shape (a cell
    /// check, `shape_jne`, the last `shape_guard`): one dispatch, not two.
    /// The guards then exit to the load, which the interpreter redoes;
    /// exits there count against this.
    fn inline_poly(
        &mut self,
        frame: usize,
        i: &Insn,
        fos: &[*mut Object],
        depth: usize,
        dead: bool,
    ) {
        let (by_callee, load) = self.receiver_shapes(frame, i, fos);
        // The receiver's shape, where just one finds the callee. (Inlining a
        // callee found under several shapes once per shape, to dispatch on
        // the shape alone, cost 87 more compiling than it saved: +15%.)
        let shapes: Vec<u64> = (by_callee.iter())
            .map(|ss| if ss.len() == 1 { ss[0] } else { 0 })
            .collect();
        let mut fused = None;
        if dead
            && let Some((get, star)) = load
            && shapes.iter().all(|&s| s != 0)
            && shapes
                .iter()
                .enumerate()
                .all(|(a, s)| !shapes[..a].contains(s))
        {
            let g = self.out.insns[get];
            let code = self.out.frames[frame].code as usize;
            let exits = self.state.callee_exits.get(&(code, g.pc)).copied();
            if exits.is_none_or(|n| n < MAX_PROPERTY_EXITS) {
                for x in [get, star] {
                    self.out.insns[x] = synth(Op::Nop, 0, frame, g.pc);
                    self.out.props[x] = Prop::None;
                }
                fused = Some(g.pc);
            }
        }
        let nparams = fos
            .iter()
            .map(|&fo| unsafe { (*(*fo).code).nparams } as usize)
            .max();
        let codes: Vec<_> = fos.iter().map(|&fo| unsafe { (*fo).code }).collect();
        let aliases = self.arg_moves(frame, i, nparams.unwrap_or(0), &codes);
        if let Some(pc) = fused {
            let mut cell = synth(Op::Nop, i.reg2, frame, pc);
            (cell.guard, cell.on_shape) = (1, true);
            self.push(cell, Prop::None);
        }
        let mut rets = Vec::new();
        for (k, &fo) in fos.iter().enumerate() {
            let last = k + 1 == fos.len();
            let (guard, r) =
                self.inline_case(frame, i, fo, depth, !last, Some((shapes[k], &aliases)), 0);
            if let Some(pc) = fused {
                let g = &mut self.out.insns[guard];
                (g.reg, g.guard, g.on_shape, g.pc) = (i.reg2, shapes[k], true, pc);
            }
            rets.extend(r);
            if !last {
                if self.out.insns.last().is_some_and(|j| j.falls_through()) {
                    let mut j = synth(Op::Jump, 0, frame, i.pc);
                    j.target = usize::MAX;
                    rets.push(self.push(j, Prop::None));
                }
                self.out.insns[guard].target = self.out.insns.len();
            }
        }
        self.return_past(rets);
    }

    /// For a polymorphic call `i` (`Call`) of frame `frame` to `fos`: per
    /// callee, the shapes its receiver must have one of (none if not known).
    /// The callee was
    /// loaded from the receiver, unchanged since, by a polymorphic access
    /// whose shapes all find it in a prototype: a prototype's slots hold
    /// the same values while the prototype epoch the code is compiled
    /// under holds (see `Vm::store_field`), so the shapes a callee is found
    /// under are the ones the receiver may have. Only with nothing between
    /// that may add properties to the receiver (a call), which would leave
    /// it a descendant of that shape, not that shape.
    /// Also the indices of the load and of the `Star` of its result.
    fn receiver_shapes(
        &self,
        frame: usize,
        i: &Insn,
        fos: &[*mut Object],
    ) -> (Vec<Vec<u64>>, Option<(usize, usize)>) {
        let none = (vec![Vec::new(); fos.len()], None);
        if i.op != Op::Call {
            return none;
        }
        let insns = &self.out.insns;
        let Some(k) = insns
            .iter()
            .rposition(|j| j.frame as usize == frame && j.op == Op::Star && j.reg == i.reg)
        else {
            return none;
        };
        let Some(get) = k.checked_sub(1) else {
            return none;
        };
        let (g, Prop::Poly { n, at }) = (insns[get], self.out.props[get]) else {
            return none;
        };
        if g.op != Op::GetNamed || g.frame as usize != frame || g.bail {
            return none;
        }
        // The call's `this` is the receiver: the same register, or one it
        // was copied to since (`Mov r8, r10` of `s.m()`, `s` a local, which
        // lowers to `Ldar r8; Star r10`), and not written again.
        let writes = |j: &Insn, r: u8| {
            j.frame as usize == frame
                && (j.op == Op::Star && j.reg == r || j.op == Op::Mov && j.reg2 == r)
        };
        let copied = i.reg2 != g.reg
            && insns[k + 1..]
                .iter()
                .rposition(|j| writes(j, i.reg2))
                .is_some_and(|m| {
                    let (j, prev) = (insns[k + 1 + m], insns[k + m]);
                    j.op == Op::Mov && j.reg == g.reg
                        || j.op == Op::Star
                            && m > 0
                            && prev.op == Op::Ldar
                            && prev.reg == g.reg
                            && prev.frame == j.frame
                });
        if i.reg2 != g.reg && !copied {
            return none;
        }
        // Nothing between rewrites the receiver or calls.
        if insns[k + 1..]
            .iter()
            .any(|j| j.is_call() || j.guard != 0 || j.bail || writes(j, g.reg))
        {
            return none;
        }
        let cases = poly_cases(&self.out.polys, n, at);
        let mut values = Vec::with_capacity(cases.len());
        for c in cases {
            let CaseLoc::Proto(addr) = c.loc else {
                return none;
            };
            rely_on_slot(addr);
            values.push(unsafe { *(addr as *const Value) });
        }
        let shapes = fos
            .iter()
            .map(|&fo| {
                let f = crate::object::object_value(fo);
                (values.iter().zip(cases))
                    .filter(|(v, _)| **v == f)
                    .map(|(_, c)| c.shape)
                    .collect()
            })
            .collect();
        (shapes, Some((get, k)))
    }

    /// Whether register `r` of `code`'s frame is dead after its instruction
    /// `k` (no path reads it before writing it; an exit to the interpreter
    /// later reads only what it would).
    fn dead_after(&mut self, code: *const crate::vm::Code, k: usize, r: u8) -> bool {
        let d = self.decoded(code);
        let live = self
            .live
            .entry(code)
            .or_insert_with(|| {
                let insns = &d.0;
                let nregs = unsafe { (*code).nregs } as usize;
                Rc::new(reg_live_in(insns, &reachable(insns, 0), nregs, &[]))
            })
            .clone();
        live.get(k + 1).is_some_and(|l| !rs_has(l, r as usize))
    }

    /// Whether register `r` of frame `frame` holds `fo` for sure: its last
    /// write was the result of a load from a prototype's slot (whose shape
    /// guard passed) that holds `fo`. A prototype's slots keep their values
    /// while the prototype epoch the code is compiled under holds (see
    /// `Vm::store_field`), which code checks at entry and after calls.
    fn proven_callee(&self, frame: usize, r: u8, fo: *mut Object) -> bool {
        let insns = &self.out.insns;
        let Some(k) = insns
            .iter()
            .rposition(|j| j.frame as usize == frame && j.op == Op::Star && j.reg == r)
        else {
            return false;
        };
        let Some(get) = k.checked_sub(1) else {
            return false;
        };
        let g = insns[get];
        let holds = |addr: u64| {
            rely_on_slot(addr);
            (unsafe { *(addr as *const Value) }) == crate::object::object_value(fo)
        };
        // A load from a prototype slot holding it, for every receiver shape.
        let proven = match self.out.props[get] {
            Prop::Proto { addr, .. } => holds(addr),
            Prop::Poly { n, at } => poly_cases(&self.out.polys, n, at)
                .iter()
                .all(|c| matches!(c.loc, CaseLoc::Proto(addr) if holds(addr))),
            _ => false,
        };
        proven && g.op == Op::GetNamed && g.frame as usize == frame && !g.bail
    }

    /// For a call whose callee register `r` (of frame `frame`) is
    /// `proven_callee` and dead after it: make its load check only the
    /// receiver's shape, and drop the `Star` to `r`, if nothing since reads
    /// `r` (the call's setup moves only other registers), and emit nothing
    /// for the `Star`. The call must then store the callee to `r` in its
    /// exits (`Insn::exit_const`).
    fn drop_callee_load(&mut self, frame: usize, r: u8) -> bool {
        let insns = &self.out.insns;
        let Some(k) = insns
            .iter()
            .rposition(|j| j.frame as usize == frame && j.op == Op::Star && j.reg == r)
        else {
            return false;
        };
        let moves_only = insns[k + 1..].iter().all(|j| {
            j.frame as usize == frame
                && !j.bail
                && matches!(j.op, Op::Ldar | Op::Star | Op::LdaUndefined)
                && j.reg != r
        });
        if !moves_only || k == 0 {
            return false;
        }
        self.out.insns[k - 1].check_only = true;
        self.out.insns[k].check_only = true;
        true
    }

    /// Point the jumps `rets` (inlined `Return`s) past the inlined code,
    /// which ends here; a last one falling through to it needs no jump.
    fn return_past(&mut self, rets: Vec<usize>) {
        let mut cont = self.out.insns.len();
        if let Some(&last) = rets.last()
            && last + 1 == cont
        {
            self.out.insns.pop();
            self.out.props.pop();
            cont -= 1;
        }
        for r in rets {
            if r < cont {
                self.out.insns[r].target = cont;
            }
        }
    }

    /// `acc = src` (a register, or `undefined`), then `dst = acc`, for call
    /// setup at `pc` in frame `frame`.
    fn mov(&mut self, frame: usize, pc: usize, src: Option<u8>, dst: u8) {
        match src {
            Some(r) => self.push(synth(Op::Ldar, r, frame, pc), Prop::None),
            None => self.push(synth(Op::LdaUndefined, 0, frame, pc), Prop::None),
        };
        self.push(synth(Op::Star, dst, frame, pc), Prop::None);
    }

    /// For call `i` of frame `frame` inlined: move `this` and the first
    /// `nparams` arguments (`undefined` for missing ones) into the
    /// callee's registers, which start past the caller's and a header.
    /// Not a parameter that none of `callees` writes or passes in a call
    /// window ([`Lowering::read_only`]): the callee reads the argument's
    /// register instead, which nothing writes while the callee runs (it
    /// is the caller's), and an exit copies it over for the interpreter
    /// (see [`materialize`]). Returns those, as (parameter, argument).
    fn arg_moves(
        &mut self,
        frame: usize,
        i: &Insn,
        nparams: usize,
        callees: &[*const crate::vm::Code],
    ) -> Vec<(u8, u8)> {
        let f = self.out.frames[frame];
        let base = f.base + unsafe { (*f.code).nregs } as usize + interp::HEADER;
        let (w, argc) = (i.reg2, i.imm as usize);
        let (this, args) = match i.op {
            Op::Call => (Some(w), w as usize + 1),
            _ => (None, w as usize),
        };
        let mut aliases = Vec::new();
        let mut arg = |cx: &mut Self, p: usize, src: Option<u8>| match src {
            Some(a) if callees.iter().all(|&c| cx.read_only(c, p)) => {
                let a = cx.fwd.iter().find(|f| f.0 == a).map_or(a, |f| f.1);
                aliases.push(((base + p) as u8, a));
            }
            _ => cx.mov(frame, i.pc, src, (base + p) as u8),
        };
        // `new` allocated `this` already; a call without a receiver passes
        // the global object.
        if i.op == Op::CallUndefined && callees.iter().any(|&c| self.entry_live(c)[0]) {
            self.global_this(frame, i.pc);
            self.push(synth(Op::Star, base as u8, frame, i.pc), Prop::None);
        } else if i.op != Op::Construct {
            arg(self, 0, this);
        }
        for p in 0..nparams {
            arg(self, 1 + p, (p < argc).then_some((args + p) as u8));
        }
        aliases
    }

    /// Whether register `r` of the function `code` is `this` or a
    /// parameter that it never writes nor passes in a call window (which
    /// must stay contiguous).
    fn read_only(&mut self, code: *const crate::vm::Code, r: usize) -> bool {
        if r > unsafe { (*code).nparams } as usize {
            return false;
        }
        let d = self.decoded(code);
        d.0.iter().all(|j| match j.op {
            Op::Star => j.reg as usize != r,
            Op::Mov => j.reg2 as usize != r,
            Op::Call | Op::CallUndefined | Op::Construct => {
                let n = j.imm as usize + usize::from(j.op == Op::Call);
                !(j.reg2 as usize..j.reg2 as usize + n).contains(&r)
            }
            _ => true,
        })
    }

    /// Inline call `i` of frame `frame` to `fo`, in a new frame: the guard
    /// (jumping on a mismatch if `next_case`; its index is returned), the
    /// moves into the callee's registers (`this` and the arguments only if
    /// not `hoisted` before, which gives the receiver's shape the callee
    /// implies, or 0), its instructions. Returns the
    /// jumps its `Return`s became, still to point past it.
    #[allow(clippy::too_many_arguments)]
    fn inline_case(
        &mut self,
        frame: usize,
        i: &Insn,
        fo: *mut Object,
        depth: usize,
        next_case: bool,
        hoisted: Option<(u64, &[(u8, u8)])>,
        alloc: u64,
    ) -> (usize, Vec<usize>) {
        let f = self.out.frames[frame];
        let callee = unsafe { (*fo).code };
        let c = unsafe { &*callee };
        let base = f.base + unsafe { (*f.code).nregs } as usize + interp::HEADER;
        let id = self.out.frames.len();
        if !self.small_leaf(fo) {
            self.inlined += self.decoded(callee).0.len();
        }
        unsafe { (*self.state.vm).heap.keep(fo as usize) };
        self.out.frames.push(InlineFrame {
            construct: i.op == Op::Construct,
            code: callee,
            func: fo,
            base,
            parent: frame,
            ret_pc: i.pc + i.op.size(),
        });
        self.out.fbs.push(self.feedback(callee));
        self.out.nregs = self.out.nregs.max(base + c.nregs as usize);
        let pc = i.pc;
        let mut guard = synth(Op::Nop, i.reg, frame, pc);
        guard.guard = fo as u64;
        guard.next_case = next_case;
        // A callee loaded from a prototype holding it needs no check.
        if !next_case && self.proven_callee(frame, i.reg, fo) {
            guard.guard = 0;
        }
        let guard = self.push(guard, Prop::None);
        // `new`: `this`, allocated (see `Lowering::inline_construct`).
        if alloc != 0 {
            let mut a = synth(Op::Nop, i.reg, frame, pc);
            (a.guard, a.alloc) = (fo as u64, alloc);
            self.push(a, Prop::None);
            self.push(synth(Op::Star, base as u8, frame, pc), Prop::None);
        }
        let aliases = match hoisted {
            None => self.arg_moves(frame, i, c.nparams as usize, &[callee]),
            Some((_, aliases)) => aliases.to_vec(),
        };
        // The receiver's shape, if this callee implies it: in the window
        // and in the callee's `this` (unless that is the window's).
        if let Some((shape, _)) = hoisted
            && shape != 0
            && shape & FAMILY == 0
        {
            let mut regs = vec![i.reg2];
            if !aliases.iter().any(|a| a.0 == base as u8) {
                regs.push(base as u8);
            }
            for r in regs {
                let mut a = synth(Op::Nop, r, frame, pc);
                a.assume = shape;
                self.push(a, Prop::None);
            }
        }
        (self.out.aliases).extend(aliases.iter().map(|&(p, a)| (id as u8, p, a)));
        // Locals start `undefined` only if the callee may read them before
        // writing them (`entry_live`), which also covers the interpreter
        // after an exit in it: it runs the same bytecode from there.
        let live = self.entry_live(callee);
        for r in 1 + c.nparams as usize..c.nregs as usize {
            if live[r] {
                self.mov(frame, pc, None, (base + r) as u8);
            }
        }
        self.push(synth(Op::LdaUndefined, 0, frame, pc), Prop::None);
        (guard, self.emit_frame(id, depth + 1))
    }
}

/// `found` (a version, the accumulator for it, the register count), if
/// the version's frame, its inlined frames' registers included, fits the
/// VM stack from `regs`: its entry zeroes the registers dead there and
/// its inlined calls write theirs, unchecked. Checked at every entry,
/// with the version's own count: entered near the end of the stack
/// (unbounded recursion), the zeroing wrote past it, into the next
/// allocation (another VM's stack, in the tests run in parallel).
fn fits_stack(
    found: (Rc<JitCode>, Value, usize),
    regs: *mut Value,
    stack_end: *mut Value,
) -> Option<(Rc<JitCode>, Value, usize)> {
    let words = found.0.homes.len().max(found.2) + interp::HEADER;
    (regs.wrapping_add(words) <= stack_end).then_some(found)
}

impl JitFunction {
    /// Prepare `code`, to start in the interpreter and tier up with the
    /// default [`Tiering`]. Panics on malformed bytecode.
    pub fn new(code: &[u8]) -> JitFunction {
        let insns = decode(code, false);
        assert!(!insns.is_empty(), "empty bytecode");
        let nregs = insns
            .iter()
            .filter(|i| i.reads_reg())
            .map(|i| i.reg as usize + 1)
            .max()
            .unwrap_or(0);
        JitFunction::new_from(code, insns, nregs)
    }

    fn new_from(code: &[u8], insns: Vec<Insn>, nregs: usize) -> JitFunction {
        JitFunction {
            code: code.to_vec(),
            insns: insns.into(),
            nregs,
            opts: Options {
                specialize: true,
                register_homes: true,
                fusion: true,
                unboxed_ints: true,
            },
            widened: HashSet::default(),
            versions: HashMap::default(),
            live_at: HashMap::default(),
            last: None,
            tiering: Some(Tiering::default()),
            back_edges_left: Tiering::default().back_edges,
            calls: 0,
            stats: JitStats::default(),
            vm: None,
        }
    }

    /// Prepare `code`, a function of a VM program, for on-stack replacement
    /// from its frames ([`JitFunction::vm_enter`]). Its calls are compiled,
    /// inlined or through the runtime; instructions the JIT doesn't compile
    /// (allocation, `return`) become exits to the interpreter (see
    /// [`lower`]).
    pub(crate) fn for_vm(code: *const crate::vm::Code, vm: *mut crate::vm::Vm) -> JitFunction {
        // (Its own epoch moves with the VM's from now on: see
        // `Vm::bump_code_epoch`.)
        unsafe { (*vm).jitted.push(code) };
        let root_insns = unsafe { (*vm).jit_caches.decoded(code) }.0.clone();
        let nregs = unsafe { (*code).nregs } as usize;
        let root_len = root_insns.len();
        let mut f = JitFunction::new_from(unsafe { &(*code).bytecode }, root_insns, nregs);
        f.tiering = None;
        f.vm = Some(Box::new(VmState {
            root: code,
            vm,
            root_len,
            props: Vec::new(),
            polys: Vec::new(),
            fbs: Vec::new(),
            snap: Rc::new(Snapshot {
                insns: Rc::from([]),
                mats: HashMap::default(),
                frames: Vec::new(),
                frame_regs: Vec::new(),
                aliases: Vec::new(),
                restores: Vec::new(),
                exit_live: Vec::new(),
                sites: Vec::new(),
            }),
            inline_base: nregs,
            direct_base: usize::MAX,
            dirty: true,
            epoch: 0,
            field_epoch: 0,
            widened: HashSet::default(),
            property_exits: HashMap::default(),
            callee_exits: HashMap::default(),
            bail_exits: HashMap::default(),
            no_entry: HashSet::default(),
            bad_results: 0,
            cool: 0,
            settle_mark: 0,
            settles: 0,
            coolings: 0,
            tier: if unsafe { (*vm).warm_tier } {
                Tier::Warm
            } else {
                Tier::Hot
            },
            heat: Box::new(HOT_HEAT),
            classified: false,
        }));
        f
    }

    /// Prepare to enter compiled code for a VM frame whose registers start
    /// at `regs`, at bytecode offset `pc` (a loop header, or 0 at a call)
    /// with `acc`, compiling a version if needed: the code to run, the
    /// accumulator to give it (converted if the version expects a double),
    /// and the register count it uses. `None` if this entry isn't compiled.
    /// `epoch` and `field_epoch` are the VM's prototype and field epochs:
    /// versions compiled under others are dropped. `stack_end` bounds the
    /// registers of inlined frames.
    ///
    /// The function is lowered again ([`lower`]) after exits and epoch
    /// changes; if that gives something else, the versions are dropped.
    ///
    /// # Safety
    /// `regs` must be the frame's registers, holding values of the program
    /// whose inline caches and objects the `Feedback` points to.
    unsafe fn vm_prepare(
        &mut self,
        pc: usize,
        mut acc: Value,
        regs: *mut Value,
        stack_end: *mut Value,
        epoch: u32,
        field_epoch: u32,
    ) -> Option<(Rc<JitCode>, Value, usize)> {
        // Field types read while lowering and compiling are this function's.
        let _compiling = Compiling::of(self.vm.as_ref().expect("a VM function").root);
        {
            let vm = self.vm.as_mut().expect("a VM function");
            // The common case, entering at every call: nothing to redo, and
            // a version that fits the live values, found without allocating.
            if !vm.dirty
                && vm.epoch == epoch
                && vm.field_epoch == field_epoch
                && let Some(found) = unsafe { self.fitting_version(pc, acc, regs) }
            {
                return fits_stack(found, regs, stack_end);
            }
            let vm = self.vm.as_mut().unwrap();
            if vm.no_entry.contains(&pc) {
                return None;
            }
            // Out of compilations: the versions there are still run, unless
            // the epochs they assumed changed (an exit only made the function
            // `dirty`, for a lowering that won't come). Dropping them left
            // zlib's `a1` interpreted after its fourth exit.
            if self.stats.compiles >= max_compiles(vm.root_len) {
                if vm.epoch != epoch || vm.field_epoch != field_epoch {
                    return None;
                }
                return unsafe { self.fitting_version(pc, acc, regs) }
                    .and_then(|found| fits_stack(found, regs, stack_end));
            }
            // First compile: hot right away if inlining is what hot code
            // would bring (see `inlining_pays`).
            if vm.tier == Tier::Warm && !vm.classified {
                vm.classified = true;
                if unsafe { inlining_pays(vm.root) } {
                    vm.tier = Tier::Hot;
                    vm.dirty = true;
                }
            }
            if vm.dirty || vm.epoch != epoch || vm.field_epoch != field_epoch {
                self.refresh(epoch, field_epoch);
            }
            // Inlined frames' registers must fit the VM stack.
            if unsafe { regs.add(self.nregs + interp::HEADER) } > stack_end {
                return None;
            }
        }
        let entry = self.index_of(pc);
        let regs = unsafe { std::slice::from_raw_parts_mut(regs, self.nregs) };
        let actual = self.entry_types(entry, acc, regs);
        let index = self.version(entry, &actual, &mut acc, regs);
        fits_stack(
            (self.versions[&entry][index].1.clone(), acc, self.nregs),
            regs.as_mut_ptr(),
            stack_end,
        )
    }

    /// Lower the function again ([`lower`]); if that gives something else,
    /// or the epochs changed, drop the versions and take the new lowering.
    fn refresh(&mut self, epoch: u32, field_epoch: u32) {
        let vm = self.vm.as_mut().unwrap();
        let low = lower(vm, epoch);
        vm.dirty = false;
        // With no versions left, nothing new uses the call sites: a new
        // snapshot gives new versions unbound ones, so direct calls bind to
        // the callee's current versions.
        if vm.epoch != epoch
            || vm.field_epoch != field_epoch
            || self.versions.is_empty()
            || !same_insns(&low.insns, &self.insns)
            || low.props != vm.props
            || low.polys != vm.polys
            || low.frames != vm.snap.frames
        {
            self.versions.clear();
            self.live_at.clear();
            vm.epoch = epoch;
            vm.field_epoch = field_epoch;
            self.widened = low
                .insns
                .iter()
                .enumerate()
                .filter(|(_, i)| {
                    vm.widened
                        .contains(&(low.frames[i.frame as usize].code as usize, i.pc))
                })
                .map(|(k, _)| k)
                .collect();
            let insns: Rc<[Insn]> = low.insns.into();
            self.insns = insns.clone();
            self.nregs = low.nregs;
            vm.direct_base = low.direct_base;
            vm.props = low.props;
            vm.polys = low.polys;
            vm.fbs = low.fbs;
            let frame_regs = low
                .frames
                .iter()
                .map(|f| (f.base, unsafe { (*f.code).nregs } as usize, f.parent))
                .collect();
            vm.snap = Rc::new(Snapshot {
                insns,
                mats: low.mats,
                frames: low.frames,
                frame_regs,
                aliases: low.aliases,
                restores: low.restores,
                exit_live: low.exit_live,
                sites: low.sites,
            });
        }
    }

    /// A version entered at instruction 0 that values of the types
    /// `make_state` gives (from which registers, and whether `acc`, are live
    /// there) fit, compiled if needed, with the register count its frame
    /// uses. Direct calls from other compiled code use it: they pick the
    /// version when compiling, from the arguments' types.
    unsafe fn entry_version(
        &mut self,
        epoch: u32,
        field_epoch: u32,
        make_state: impl Fn(&[bool], bool) -> TypeState,
    ) -> Option<(Rc<JitCode>, usize)> {
        let vm = self.vm.as_mut().unwrap();
        if vm.no_entry.contains(&0) || self.stats.compiles >= max_compiles(vm.root_len) {
            return None;
        }
        let _compiling = Compiling::of(vm.root);
        if vm.dirty || vm.epoch != epoch || vm.field_epoch != field_epoch {
            self.refresh(epoch, field_epoch);
        }
        let entry = self.index_of(0);
        let (insns, nregs) = (&self.insns, self.nregs);
        let snap = self.vm.as_ref().map(|v| v.snap.clone());
        let frames: &[FrameRegs] = snap.as_ref().map_or(&[], |s| &s.frame_regs);
        let live = self
            .live_at
            .entry(entry)
            .or_insert_with(|| Rc::new(EntryLive::new(insns, entry, nregs, frames)))
            .clone();
        let state = make_state(&live.at_entry, live.acc_at_entry);
        let fits = |a: Ty, v: Ty| a == v || a == Ty::Bot || v == Ty::Any;
        if let Some((_, code)) = self.versions.get(&entry).and_then(|vs| {
            vs.iter().find(|(st, _)| {
                fits(state.acc, st.acc) && state.regs.iter().all(|(r, a)| fits(a, st.regs[r]))
            })
        }) {
            return Some((code.clone(), self.nregs));
        }
        self.stats.compiles += 1;
        let vm = self.vm.as_ref().unwrap();
        let mut code = JitCode::compile(
            &self.insns,
            entry,
            &live,
            state.clone(),
            &self.widened,
            self.opts,
            VmCompile {
                props: &vm.props,
                polys: &vm.polys,
                feedback: Some(&vm.fbs[..]),
                inline_base: vm.inline_base,
                direct_base: vm.direct_base,
                sites: &vm.snap.sites,
                frames: &vm.snap.frame_regs,
                root: vm.root,
                mats: Some(&vm.snap.mats),
                exit_live: Some(&vm.snap.exit_live),
                heat: vm.heat_addr(),
            },
        );
        code.snap = Some(vm.snap.clone());
        code.log(vm.root, 0);
        let code = code.into_rc();
        self.versions
            .entry(entry)
            .or_default()
            .push((state, code.clone()));
        Some((code, self.nregs))
    }

    /// An existing version for the entry at bytecode offset `pc` that the
    /// live values in `acc` and `regs` fit exactly (no int32 → double
    /// conversion), without building their `TypeState`.
    unsafe fn fitting_version(
        &self,
        pc: usize,
        acc: Value,
        regs: *mut Value,
    ) -> Option<(Rc<JitCode>, Value, usize)> {
        // The entry among those with versions (keyed by `index_of`, so
        // the first instruction at `pc`): a few, against every instruction
        // of the function. Calls that find no version (a function out of
        // compilations, entered by every call) cost a scan of it each.
        let entry = *self.versions.keys().find(|&&e| {
            let i = &self.insns[e];
            i.pc == pc && i.frame == 0
        })?;
        let l = self.live_at.get(&entry)?;
        let (acc_live, live) = (&l.acc_at_entry, &l.live_list);
        let fits = |t: Ty, v: Value| match t {
            Ty::Bot => false,
            Ty::Any => true,
            Ty::I32 => v.is_int32(),
            Ty::F64 => v.is_double(),
            Ty::Obj(s) | Ty::Ext(s) => v.is_cell() && unsafe { *(v.to_bits() as *const u64) } == s,
        };
        let versions = self.versions.get(&entry)?;
        // The live registers against the version's types, both in order
        // (a live register without one is `Bot`: no fit).
        versions.iter().find_map(|(st, code)| {
            let tys = &st.regs.tys;
            let mut k = 0;
            let ok = (!*acc_live || fits(st.acc, acc))
                && live.iter().all(|&r| {
                    while k < tys.len() && tys[k].0 < r {
                        k += 1;
                    }
                    k < tys.len()
                        && tys[k].0 == r
                        && fits(tys[k].1, unsafe { *regs.add(r as usize) })
                });
            ok.then(|| (code.clone(), acc, self.nregs))
        })
    }

    /// Account for an exit of `code` (prepared at entry `pc`): at an int32
    /// operation, the instruction is widened (as in [`JitFunction::run`]);
    /// at a property access or an inlined call's guard, the exit counts
    /// towards giving the access or the inlining up; at a bail, towards
    /// giving the entry up; at the check of a call's result, the call loses
    /// its prediction. Any exit makes the next entry lower the function
    /// again. Returns the exit's frame and bytecode offset.
    fn vm_finish(&mut self, code: &JitCode, pc: usize, record: usize) -> (usize, usize) {
        self.stats.exits += 1;
        let after_call = record & AFTER_CALL as usize != 0;
        let bad_result = after_call && record & BAD_RESULT as usize != 0;
        let hoisted = !after_call && record & HOISTED as usize != 0;
        let (frame, exit_pc) = (record >> 24, record & (TIER_UP as usize - 1));
        let snap = code.snap.as_ref().expect("a VM function's code");
        // Warm code ran hot: compile hot (the caller enters it there).
        if record & TIER_UP as usize != 0 {
            // (Once: warm code still running, deeper in recursion say,
            // leaves the same way.)
            let vm = self.vm.as_mut().unwrap();
            if vm.tier == Tier::Warm {
                vm.tier = Tier::Hot;
                vm.dirty = true;
                self.versions.clear();
            }
            return (frame, exit_pc);
        }
        let vm = self.vm.as_mut().unwrap();
        // The call is the instruction before the one an after-call exit
        // resumes at.
        let call_before = || {
            let k = snap
                .insns
                .iter()
                .position(|i| i.frame as usize == frame && i.pc == exit_pc)
                .expect("an exit's instruction")
                - 1;
            debug_assert!(snap.insns[k].is_call());
            k
        };
        // A call that found the native stack too deep: the interpreter
        // makes it. Nothing the code assumed changed, so the next entry
        // needn't lower the function again (past the limit, every frame
        // leaves at its first call).
        if after_call && std::mem::take(unsafe { &mut (*vm.vm).redo_call }) {
            return (frame, snap.insns[call_before()].pc);
        }
        let was_dirty = std::mem::replace(&mut vm.dirty, true);
        // The function may have been lowered differently since (a nested
        // entry recompiled it): then only resume. So, while it cools, for
        // code still running from before the exit that started it: what
        // it saw counts once.
        if !Rc::ptr_eq(snap, &vm.snap) || (after_call && !bad_result) || (was_dirty && vm.cool > 0)
        {
            return (frame, exit_pc);
        }
        if bad_result {
            // Compile the call without a prediction from now on; after a
            // second miss, every call (each miss costs a compilation:
            // Raytrace's `rayTrace` missed at 20 calls, one at a time, and
            // ran out of compilations).
            let k = call_before();
            vm.bad_results += 1;
            let calls: Vec<usize> = if vm.bad_results >= 2 {
                (0..snap.insns.len()).filter(|&j| snap.insns[j].is_call()).collect()
            } else {
                vec![k]
            };
            for j in calls {
                let call = snap.insns[j];
                vm.widened
                    .insert((snap.frames[call.frame as usize].code as usize, call.pc));
                self.widened.insert(j);
            }
            self.versions.clear();
            return (frame, exit_pc);
        }
        // A check moved before a loop: its loads aren't compiled as
        // constants anymore (which of the checks there failed isn't told).
        if hoisted {
            let at = |i: &&Insn| i.frame as usize == frame && i.pc == exit_pc;
            for i in snap.insns.iter().filter(at) {
                if let Some((f, opc)) = i.hoisted {
                    let origin = (snap.frames[f as usize].code as usize, opc as usize);
                    *vm.property_exits.entry(origin).or_insert(0) += 1;
                }
            }
            return (frame, exit_pc);
        }
        let k = snap
            .insns
            .iter()
            .position(|i| i.frame as usize == frame && i.pc == exit_pc && i.hoisted.is_none())
            .expect("an exit's instruction");
        let insn = snap.insns[k];
        let origin = (snap.frames[frame].code as usize, exit_pc);
        if insn.bail {
            let n = vm.bail_exits.entry(pc).or_insert(0);
            *n += 1;
            if *n >= MAX_BAIL_EXITS {
                vm.no_entry.insert(pc);
            }
        } else if insn.guard != 0 || insn.native != 0 {
            vm.cool = cooling(&mut vm.coolings);
            (vm.settle_mark, vm.settles) = (unsafe { (*vm.root).ic_misses.get() }, 0);
            *vm.callee_exits.entry(origin).or_insert(0) += 1;
        } else if insn.is_property() {
            vm.cool = cooling(&mut vm.coolings);
            (vm.settle_mark, vm.settles) = (unsafe { (*vm.root).ic_misses.get() }, 0);
            *vm.property_exits.entry(origin).or_insert(0) += 1;
        } else if !insn.is_call() {
            vm.widened.insert(origin);
            self.widened.insert(k);
            // Out of compilations, the versions stay (see `vm_prepare`):
            // they exit here again, but run the rest.
            if self.stats.compiles < max_compiles(vm.root_len) {
                self.versions.clear();
            }
        }
        (frame, exit_pc)
    }

    /// Tier up with custom thresholds (see [`Tiering`]).
    pub fn tiered(mut self, tiering: Tiering) -> JitFunction {
        self.tiering = Some(tiering);
        self.back_edges_left = tiering.back_edges;
        self
    }

    /// Compile on entry instead of starting in the interpreter.
    pub fn eager(mut self) -> JitFunction {
        self.tiering = None;
        self
    }

    /// Use only generic stencils, as milestone 1 did.
    pub fn unspecialized(mut self) -> JitFunction {
        self.opts = Options {
            specialize: false,
            register_homes: false,
            fusion: false,
            unboxed_ints: false,
        };
        self
    }

    /// Don't use fused stencils.
    pub fn without_fusion(mut self) -> JitFunction {
        self.opts.fusion = false;
        self.opts.unboxed_ints = false;
        self
    }

    /// Keep int32 values boxed in slots.
    pub fn without_unboxed_ints(mut self) -> JitFunction {
        self.opts.unboxed_ints = false;
        self
    }

    /// Keep every VM register in memory, as milestone 2 did.
    pub fn without_register_homes(mut self) -> JitFunction {
        self.opts.register_homes = false;
        self
    }

    /// The instruction at bytecode offset `pc` of the function itself
    /// (frame 0).
    fn index_of(&self, pc: usize) -> usize {
        self.insns
            .iter()
            .position(|i| i.pc == pc && i.frame == 0)
            .expect("an instruction boundary")
    }

    /// The types at entry instruction `entry`, with values that are dead
    /// there (never read before being overwritten) as `Bot`, so they don't
    /// split versions.
    fn entry_types(&mut self, entry: usize, acc: Value, regs: &[Value]) -> TypeState {
        let (insns, nregs) = (&self.insns, self.nregs);
        let snap = self.vm.as_ref().map(|v| v.snap.clone());
        let frames: &[FrameRegs] = snap.as_ref().map_or(&[], |s| &s.frame_regs);
        let l = self
            .live_at
            .entry(entry)
            .or_insert_with(|| Rc::new(EntryLive::new(insns, entry, nregs, frames)));
        let (acc_live, live, read) = (&l.acc_at_entry, &l.at_entry, &l.read_at_entry);
        // Only live values are looked at: dead registers may hold anything.
        // So may those live only into a bail (see `reg_read_in`): `Any`,
        // not looked at either.
        // So may inlined frames' registers, live or not: code is entered in
        // the function's own frame, before any inlined frame starts (they
        // seem live there to a bail inside one, which leaves them all to
        // the interpreter).
        let inline_base = self.vm.as_ref().map_or(usize::MAX, |vm| vm.inline_base);
        let ty = |v: Value, live: bool| if live { Ty::of(v) } else { Ty::Bot };
        // At a function's start, `this` and the parameters are objects of
        // their shapes or shapes extending them (`Ext`): what callers'
        // loads from fields give, so its own calls to itself fit (see
        // `fits_entry` in `JitCode::compile`).
        let params = match &self.vm {
            Some(vm) if entry == 0 => (unsafe { (*vm.root).nparams }) as usize + 1,
            _ => 0,
        };
        TypeState {
            acc: ty(acc, *acc_live),
            regs: Regs::from_dense(regs.iter().zip(live.iter()).enumerate().map(
                |(r, (&v, &l))| {
                    let l = l && r < inline_base;
                    match ty(v, l && read[r]) {
                        _ if l && !read[r] => Ty::Any,
                        Ty::Obj(s) if r < params => Ty::Ext(s),
                        t => t,
                    }
                },
            )),
            same: Copies::new(),
            acc_same: None,
        }
    }

    /// A version for `entry` fitting `actual`, compiling one if needed;
    /// returns its index. May convert int32 values (in `acc` and `regs`) to
    /// doubles, where the version expects doubles.
    fn version(
        &mut self,
        entry: usize,
        actual: &TypeState,
        acc: &mut Value,
        regs: &mut [Value],
    ) -> usize {
        let fits = |a: Ty, v: Ty| {
            a == v
                || a == Ty::Bot
                || v == Ty::Any
                || (a == Ty::I32 && v == Ty::F64)
                || matches!((a, v), (Ty::Obj(x) | Ty::Ext(x), Ty::Ext(y))
                    if unsafe { extends(x as *const Shape, y as *const Shape) })
        };
        let fits_all = |v: &TypeState| {
            fits(actual.acc, v.acc) && actual.regs.iter().all(|(r, a)| fits(a, v.regs[r]))
        };
        let found = self
            .versions
            .get(&entry)
            .and_then(|vs| vs.iter().position(|(st, _)| fits_all(st)));
        let index = match found {
            Some(i) => i,
            None => {
                let seen = self.versions.remove(&entry).unwrap_or_default();
                let (state, keep) = if seen.len() >= MAX_VERSIONS {
                    // Megamorphic: one version for everything seen.
                    self.stats.generalizations += 1;
                    let mut joined = actual.clone();
                    for (st, _) in &seen {
                        joined.join(st);
                    }
                    (joined, Vec::new())
                } else {
                    (actual.clone(), seen)
                };
                self.stats.compiles += 1;
                let code = {
                    let (props, feedback, inline_base, direct_base, sites, root): (
                        &[Prop],
                        _,
                        _,
                        _,
                        &[_],
                        _,
                    ) = match &self.vm {
                        Some(vm) => (
                            &vm.props,
                            Some(&vm.fbs[..]),
                            vm.inline_base,
                            vm.direct_base,
                            &vm.snap.sites,
                            vm.root,
                        ),
                        None => (&[], None, self.nregs, usize::MAX, &[], std::ptr::null()),
                    };
                    let frames: &[FrameRegs] = match &self.vm {
                        Some(vm) => &vm.snap.frame_regs,
                        None => &[],
                    };
                    let mats = self.vm.as_ref().map(|vm| &vm.snap.mats);
                    let exit_live = self.vm.as_ref().map(|vm| &vm.snap.exit_live[..]);
                    let heat = self.vm.as_ref().map_or(0, |vm| vm.heat_addr());
                    JitCode::compile(
                        &self.insns,
                        entry,
                        &self.live_at[&entry].clone(),
                        state.clone(),
                        &self.widened,
                        self.opts,
                        VmCompile {
                            props,
                            polys: self.vm.as_ref().map_or(&[], |vm| &vm.polys[..]),
                            feedback,
                            inline_base,
                            direct_base,
                            sites,
                            frames,
                            root,
                            mats,
                            exit_live,
                            heat,
                        },
                    )
                };
                let mut code = code;
                if let Some(vm) = &self.vm {
                    code.snap = Some(vm.snap.clone());
                    code.log(vm.root, self.insns[entry].pc);
                }
                let vs = self.versions.entry(entry).or_insert(keep);
                vs.push((state, code.into_rc()));
                vs.len() - 1
            }
        };
        self.last = Some((entry, index));
        // Make the values match the version: int32 → double where it expects
        // a double.
        let st = &self.versions[&entry][index].0;
        let widen = |v: &mut Value, t: Ty| {
            if t == Ty::F64 && v.is_int32() {
                *v = Value::from_f64(v.as_i32_unchecked() as f64);
            }
        };
        widen(acc, st.acc);
        for (r, t) in st.regs.iter() {
            widen(&mut regs[r], t);
        }
        index
    }

    /// Run over `regs` and return the accumulator at `Return`. The
    /// accumulator starts as `undefined`.
    ///
    /// # Safety
    /// As [`crate::interp::run`]: register operands must be `< regs.len()`.
    pub unsafe fn run(&mut self, regs: &mut [Value]) -> Value {
        assert!(regs.len() >= self.nregs, "register file too small");
        let mut frame = Frame::new(regs);
        let v = unsafe { self.run_frame(&mut frame) };
        regs.copy_from_slice(frame.regs());
        v
    }

    unsafe fn run_frame(&mut self, frame: &mut Frame) -> Value {
        let (mut pc, mut acc) = (0, Value::UNDEFINED);
        self.calls += 1;
        let compiles_at_start = self.stats.compiles;
        let mut in_jit = match self.tiering {
            None => true,
            Some(t) => self.calls > t.calls || self.back_edges_left <= 0,
        };
        loop {
            if !in_jit {
                self.stats.interp_runs += 1;
                let (run, left) =
                    unsafe { interp::run_frame(&self.code, pc, acc, frame, self.back_edges_left) };
                self.back_edges_left = left;
                match run {
                    Run::Return(v) => return v,
                    Run::Osr { pc: p, acc: a } => {
                        self.stats.osr_entries += 1;
                        (pc, acc, in_jit) = (p, a, true);
                    }
                }
            }
            if self.stats.compiles - compiles_at_start >= MAX_COMPILES {
                return unsafe { interp::run_from(&self.code, pc, acc, frame.regs()) };
            }
            let entry = self.index_of(pc);
            let regs = &mut frame.regs()[..self.nregs];
            let actual = self.entry_types(entry, acc, regs);
            let index = self.version(entry, &actual, &mut acc, regs);
            let code = &self.versions[&entry][index].1;
            match unsafe { code.enter(acc, frame.regs()) } {
                Outcome::Return(v) => return v,
                Outcome::Exit {
                    pc: exit_pc,
                    acc: exit_acc,
                } => {
                    self.stats.exits += 1;
                    // Only int32 arithmetic exits: widen it, invalidate all
                    // versions (they would exit there too), recompile.
                    let k = self.index_of(exit_pc);
                    self.widened.insert(k);
                    self.versions.clear();
                    (pc, acc) = (exit_pc, exit_acc);
                    // Resume at a preceding `ldar r` if `acc` is just `r`.
                    // Re-running it changes nothing (acc is dead before an
                    // `ldar`), and the resume point then starts a fusible
                    // group (`ldar r; add …; star r`) instead of landing
                    // inside one, where nothing could be fused.
                    if k > 0
                        && let prev = &self.insns[k - 1]
                        && prev.op == Op::Ldar
                        && frame.regs()[prev.reg as usize] == exit_acc
                    {
                        pc = prev.pc;
                    }
                }
            }
        }
    }

    /// The most recently used code.
    pub fn last_code(&self) -> Option<&JitCode> {
        let Some((entry, index)) = self.last else {
            // Only compiled for direct calls: the latest entry version.
            if self.versions.is_empty() {
                return None;
            }
            let vs = self.versions.get(&self.index_of(0))?;
            return vs.last().map(|(_, c)| &**c);
        };
        self.versions.get(&entry)?.get(index).map(|(_, c)| &**c)
    }

    /// Direct calls of a VM function's current lowering that are bound to
    /// a callee's code (for tests).
    pub fn bound_direct_calls(&self) -> usize {
        let Some(vm) = &self.vm else { return 0 };
        let bound = |s: &std::cell::UnsafeCell<CallSite>| unsafe { (*s.get()).entry != 0 };
        vm.snap.sites.iter().filter(|s| bound(s)).count()
    }

    /// Versions currently compiled, over all entry points.
    pub fn version_count(&self) -> usize {
        self.versions.values().map(Vec::len).sum()
    }
}

/// Run `code` once through a fresh [`JitFunction`] (so with the default
/// tiering: it starts in the interpreter). See [`JitFunction::run`].
///
/// # Safety
/// As [`crate::interp::run`].
pub unsafe fn run(code: &[u8], regs: &mut [Value]) -> Value {
    unsafe { JitFunction::new(code).run(regs) }
}

/// How [`vm_run`] ended.
pub(crate) enum VmRun {
    /// No compiled code for this entry: run the frame in the interpreter.
    NotCompiled,
    /// Run this call in the interpreter: the native stack is low, so its
    /// compiled code would leave at its first call anyway.
    Interpret,
    /// The function returned `acc` from compiled code; the caller finishes
    /// the return (`construct_result`, the return pc).
    Returned(Value),
    /// The code exited: the interpreter resumes there.
    Exit(interp::Resume),
}

/// Run `code`'s compiled code for its VM frame `regs` from bytecode offset
/// `pc` (a loop header, for on-stack replacement, or 0, entering a call)
/// with `acc`, compiling it if needed.
///
/// The `JitFunction` isn't borrowed while the code runs: the code may call,
/// through the runtime, functions entering compiled code, this one
/// included. The version is an `Rc`, so it outlives a recompilation by such
/// a nested entry.
///
/// An exit inside a function the code had inlined resumes in that
/// function's frame, whose registers are already in place: its header is
/// written (and the headers of the frames between), so that its `Return`
/// continues the caller.
///
/// # Safety
/// `regs` must be a live frame of `code` in `vm`.
pub(crate) unsafe fn vm_run(
    vm: &mut crate::vm::Vm,
    code: &crate::vm::Code,
    regs: *mut Value,
    pc: usize,
    acc: Value,
) -> VmRun {
    let r = unsafe { vm_run_(vm, code, regs, pc, acc) };
    if *EXITS_ON.get_or_init(|| std::env::var_os("TOI_EXITS").is_some()) {
        let how = match &r {
            VmRun::Returned(_) => "returned",
            VmRun::Exit(_) => "exit",
            VmRun::Interpret => "interpret",
            VmRun::NotCompiled => "not compiled",
        };
        let key = format!("{} enter @{pc}: {how}", code.name);
        *EXITS.lock().unwrap().entry(key).or_default() += 1;
    }
    r
}

unsafe fn vm_run_(
    vm: &mut crate::vm::Vm,
    code: &crate::vm::Code,
    regs: *mut Value,
    pc: usize,
    acc: Value,
) -> VmRun {
    if pc == 0 && interp::native_stack_low(vm) {
        return VmRun::Interpret;
    }
    let vm_ptr: *mut crate::vm::Vm = vm;
    let (mut pc, mut acc) = (pc, acc);
    // After a tier-up exit: where the interpreter resumes if hot code can't
    // be entered there.
    let mut left: Option<(Rc<JitCode>, usize)> = None;
    loop {
        let prepared = {
            let mut slot = code.jit.borrow_mut();
            let f = slot.get_or_insert_with(|| Box::new(JitFunction::for_vm(code, vm_ptr)));
            // Cooling after an exit: calls run in the interpreter (not
            // `NotCompiled`, which would stop entering for good).
            if left.is_none()
                && pc == 0
                && let Some(v) = f.vm.as_mut()
                && v.dirty
                && v.cool > 0
            {
                v.cool -= 1;
                // The window ends: another, longer one if the caches
                // missed meanwhile.
                let misses = code.ic_misses.get();
                if v.cool == 0
                    && misses != v.settle_mark
                    && v.settles < MAX_SETTLE
                    && v.root_len >= SETTLE_SIZE
                {
                    v.settles += 1;
                    v.cool = COOL_ENTRIES << v.settles;
                    v.settle_mark = misses;
                }
                return VmRun::Interpret;
            }
            let compiles = f.stats.compiles;
            let prepared = unsafe {
                f.vm_prepare(
                    pc,
                    acc,
                    regs,
                    vm.stack_end,
                    vm.proto_epoch,
                    vm.field_epoch.wrapping_add(code.field_deps.get()),
                )
            };
            // Debugging: `TOI_JIT_DUMP=<file>` writes the last compilation's
            // hot code (`TOI_JIT_LOG`: see `JitCode::log`).
            if f.stats.compiles != compiles
                && let Some(c) = f.last_code()
                && let Some(path) = std::env::var_os("TOI_JIT_DUMP")
            {
                std::fs::write(path, c.main_code()).expect("writing TOI_JIT_DUMP");
            }
            prepared
        };
        let Some((jit_code, entry_acc, nregs)) = prepared else {
            return match left {
                Some((code, frame)) => {
                    VmRun::Exit(unsafe { materialize(vm, regs, &code, frame, pc, acc) })
                }
                None => VmRun::NotCompiled,
            };
        };
        let regs_slice = unsafe { std::slice::from_raw_parts_mut(regs, nregs) };
        let (record, exit_acc) = match unsafe { jit_code.enter(entry_acc, regs_slice) } {
            Outcome::Return(v) => return VmRun::Returned(v),
            Outcome::Exit { pc: record, acc } => (record, acc),
        };
        let (frame, exit_pc) = {
            let mut slot = code.jit.borrow_mut();
            slot.as_mut().unwrap().vm_finish(&jit_code, pc, record)
        };
        // Warm code ran out of heat at a loop header: continue there in hot
        // code (its registers are in the frame, as for the interpreter).
        if record & TIER_UP as usize != 0 && frame == 0 {
            (pc, acc) = (exit_pc, exit_acc);
            left = Some((jit_code, frame));
            continue;
        }
        if *EXITS_ON.get_or_init(|| std::env::var_os("TOI_EXITS").is_some()) {
            let c = unsafe { &*jit_code.snap.as_ref().unwrap().frames[frame].code };
            let op = Op::from_u8(c.bytecode[exit_pc]).map_or("?", |o| o.name());
            let key = format!("{} @{exit_pc} {op}", c.name);
            *EXITS.lock().unwrap().entry(key).or_default() += 1;
        }
        return VmRun::Exit(unsafe { materialize(vm, regs, &jit_code, frame, exit_pc, exit_acc) });
    }
}

static EXITS_ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static EXITS: std::sync::Mutex<std::collections::BTreeMap<String, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Debugging: what `TOI_EXITS` counted, most frequent first: exits from
/// compiled code (function, bytecode offset, instruction), entries into
/// it from the interpreter (`enter`, with how they ended) and calls
/// through the runtime (`rt_call`).
pub fn exits_report() -> String {
    let exits = EXITS.lock().unwrap();
    let mut rows: Vec<_> = exits.iter().map(|(k, n)| (*n, k)).collect();
    rows.sort_unstable_by(|a, b| b.cmp(a));
    rows.iter().map(|(n, k)| format!("{n:>10}  {k}\n")).collect()
}

/// Where the interpreter resumes after `code` (compiled for the frame
/// `regs`) exited in its frame `frame` at bytecode offset `pc` with `acc`:
/// an inlined frame gets its header written (and so do the frames between),
/// so that its `Return` continues the caller.
///
/// # Safety
/// `regs` must be the frame `code` ran over.
unsafe fn materialize(
    vm: &crate::vm::Vm,
    regs: *mut Value,
    code: &JitCode,
    frame: usize,
    exit_pc: usize,
    acc: Value,
) -> interp::Resume {
    let frames = &code.snap.as_ref().unwrap().frames;
    // Headers for the inlined frames from the exit's outwards.
    let mut g = frame;
    while g != 0 {
        let fr = frames[g];
        let parent = frames[fr.parent];
        unsafe {
            interp::write_frame_header(
                vm,
                regs.add(fr.base),
                fr.code,
                (*parent.code).bytecode.as_ptr().add(fr.ret_pc),
                regs.add(parent.base),
                if fr.construct { interp::CONSTRUCT } else { 0 },
            );
            interp::set_frame_func(regs.add(fr.base), fr.func);
        }
        g = fr.parent;
    }
    // From the outermost frame in, each frame's parameters read from their
    // arguments' registers (final in its caller by then), then registers
    // a call's setup didn't write, for a frame resuming in it (which may
    // copy a parameter).
    let snap = code.snap.as_ref().unwrap();
    let mut chain = vec![(frame, exit_pc)];
    while chain.last().unwrap().0 != 0 {
        let g = chain.last().unwrap().0;
        chain.push((frames[g].parent, frames[g].ret_pc));
    }
    for &(g, pc) in chain.iter().rev() {
        for &(_, p, a) in snap.aliases.iter().filter(|a| a.0 as usize == g && g != 0) {
            unsafe { *regs.add(p as usize) = *regs.add(a as usize) };
        }
        for r in snap.restores.iter().filter(|r| r.frame as usize == g) {
            if r.after < pc && pc <= r.upto {
                let v = match r.value {
                    Restored::Reg(x) => unsafe { *regs.add(x as usize) },
                    Restored::Const(bits) => Value::from_bits(bits),
                };
                unsafe { *regs.add(r.reg as usize) = v };
            }
        }
    }
    let fr = frames[frame];
    interp::Resume {
        regs: unsafe { regs.add(fr.base) },
        pc: unsafe { (*fr.code).bytecode.as_ptr().add(exit_pc) },
        acc,
    }
}

/// `call_direct` stencils, once their checks passed: call the compiled
/// entry of the callee of `site` (a `CallSite`), whose frame the compiled
/// frame `regs` has set up (see `Lowering::direct_setup`). `EMPTY` on an
/// error.
///
/// Compiled code doesn't call this function: a region's wrapper for it
/// does the same inline, more cheaply (`arch::enter_wrapper`). This one
/// serves `toi_rt_construct`, a Rust caller.
///
/// Its region's wrapper saves the slot registers the region uses, as for
/// any runtime call. The callee's code uses every register freely, so this
/// trampoline saves the C ABI's callee-saved ones. It sets up what the code
/// expects (`acc` = `undefined`, the frame, the tag, an exit cell on its
/// stack; the slots start with garbage, as the code's entry loads those it
/// uses from the frame) and calls the entry. If the code exited,
/// [`toi_direct_exit`] finishes the call. In asm, it costs about a third of
/// a Rust function doing the same through inline asm, which had to align the
/// stack and step over the red zone around the call.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
extern "C" fn toi_rt_enter(regs: Value, site: Value) -> Value {
    core::arch::naked_asm!(
        // Too deep in native calls: through the runtime (a tail call).
        "mov rax, [rsi + {limit}]",
        "cmp rsp, [rax]",
        "jb {call}",
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // Aligned again: [rsp] the exit cell, then `regs` and `site`.
        "sub rsp, 24",
        "mov [rsp + 8], rdi",
        "mov [rsp + 16], rsi",
        "mov dword ptr [rsp], {no_exit}",
        "mov rax, [rsi + {frame}]",
        "lea r13, [rdi + rax * 8]",
        // The frame's flags: not a `new` frame (its header isn't written
        // otherwise; see `check_tail_callee`). For `new`, either:
        // `toi_rt_construct` makes the result from its own `this`.
        "mov qword ptr [r13 - {flags}], 0",
        "movabs r14, {tag}",
        "mov r15, rsp",
        "mov r12d, {undefined}",
        "call qword ptr [rsi]",
        "cmp dword ptr [rsp], {no_exit}",
        "jne 3f",
        "2:",
        "add rsp, 24",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
        "3:",
        "mov rcx, rax",
        "mov edx, [rsp]",
        "mov rsi, [rsp + 16]",
        "mov rdi, [rsp + 8]",
        "call {exit}",
        "jmp 2b",
        no_exit = const NO_EXIT as i32,
        flags = const 8 * interp::FLAGS,
        limit = const std::mem::offset_of!(CallSite, native_limit),
        call = sym toi_rt_call,
        frame = const std::mem::offset_of!(CallSite, frame),
        tag = const Value::NUMBER_TAG,
        undefined = const Value::UNDEFINED.to_bits(),
        exit = sym toi_direct_exit,
    )
}

/// The arm64 `toi_rt_enter`, with the too-deep branch and a stub for it
/// as the OS needs: Linux branches to `toi_rt_call` directly (ELF
/// `CONDBR19`); Mach-O can't relocate a conditional branch to another
/// function, so there it goes through a local `b` placed after the cold
/// tail. The hot path is the same on both.
#[cfg(target_arch = "aarch64")]
macro_rules! toi_rt_enter_arm64 {
    ($too_deep:literal $(, $stub:literal)*) => {
#[unsafe(naked)]
extern "C" fn toi_rt_enter(regs: Value, site: Value) -> Value {
    core::arch::naked_asm!(
        // Too deep in native calls: through the runtime (a tail call).
        "ldr x9, [x1, #{limit}]",
        "ldr x9, [x9]",
        "mov x10, sp",
        "cmp x10, x9",
        $too_deep,
        "stp x29, x30, [sp, #-192]!",
        "mov x29, sp",
        "stp x19, x20, [sp, #16]",
        "stp x21, x22, [sp, #32]",
        "stp x23, x24, [sp, #48]",
        "stp x25, x26, [sp, #64]",
        "stp x27, x28, [sp, #80]",
        "stp d8, d9, [sp, #96]",
        "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]",
        "stp d14, d15, [sp, #144]",
        // `regs` and `site`, then the exit cell.
        "stp x0, x1, [sp, #160]",
        "mov w9, #-1",
        "str w9, [sp, #176]",
        "ldr x9, [x1, #{frame}]",
        "add x21, x0, x9, lsl #3",
        // The frame's flags: not a `new` frame (its header isn't written
        // otherwise; see `check_tail_callee`). For `new`, either:
        // `toi_rt_construct` makes the result from its own `this`.
        "stur xzr, [x21, #-{flags}]",
        "mov x22, #{tag_hi}",
        "lsl x22, x22, #48",
        "add x23, sp, #176",
        "mov x20, #{undefined}",
        "ldr x9, [x1]",
        "blr x9",
        "ldr w9, [sp, #176]",
        "cmn w9, #1",
        "b.ne 3f",
        "2:",
        "ldp d14, d15, [sp, #144]",
        "ldp d12, d13, [sp, #128]",
        "ldp d10, d11, [sp, #112]",
        "ldp d8, d9, [sp, #96]",
        "ldp x27, x28, [sp, #80]",
        "ldp x25, x26, [sp, #64]",
        "ldp x23, x24, [sp, #48]",
        "ldp x21, x22, [sp, #32]",
        "ldp x19, x20, [sp, #16]",
        "ldp x29, x30, [sp], #192",
        "ret",
        "3:",
        "mov x3, x0",
        "mov w2, w9",
        "ldp x0, x1, [sp, #160]",
        "bl {exit}",
        "b 2b",
        $($stub,)*
        limit = const std::mem::offset_of!(CallSite, native_limit),
        call = sym toi_rt_call,
        flags = const 8 * interp::FLAGS,
        frame = const std::mem::offset_of!(CallSite, frame),
        tag_hi = const Value::NUMBER_TAG >> 48,
        undefined = const Value::UNDEFINED.to_bits(),
        exit = sym toi_direct_exit,
    )
}
    };
}
#[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
toi_rt_enter_arm64!("b.lo {call}");
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
toi_rt_enter_arm64!("b.lo 4f", "4:", "b {call}");

/// What the inline version of [`toi_rt_enter`] in a region's wrapper for
/// it (`arch::enter_wrapper`) needs: `CallSite` offsets, constants and
/// the functions it calls.
/// The slot registers to keep across call `i`, as (GPRs, xmm registers):
/// those of registers live after it, to compiled code (`live`) or to the
/// interpreter after an exit there (`exit_live`, and the fields of objects
/// an exit there allocates). All of them: the callee's code preserves no
/// slot register, unlike a C function.
fn live_slots(
    exit_live: &[RegSet],
    mats: Option<&HashMap<usize, Vec<Rc<Mat>>>>,
    live: &[RegSet],
    homes: &[Home],
    i: usize,
    n: usize,
) -> (Vec<u8>, Vec<u8>) {
    let mut set = [0; 4];
    for k in [i, i + 1].into_iter().filter(|&k| k < n) {
        rs_or(&mut set, &exit_live[k]);
        for m in mats.and_then(|m| m.get(&k)).into_iter().flatten() {
            for &f in &m.fields {
                rs_add(&mut set, f as usize);
            }
        }
    }
    if i + 1 < n {
        rs_or(&mut set, &live[i + 1]);
    }
    let (mut g, mut f): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    for r in rs_iter(&set).filter(|&r| r < homes.len()) {
        match homes[r] {
            Home::Gpr(k) | Home::Int(k) => g.push(arch::slot_gpr(k)),
            Home::Fpr(k) => f.push(k + 1),
            Home::Mem => {}
        }
    }
    g.sort_unstable();
    g.dedup();
    f.sort_unstable();
    f.dedup();
    (g, f)
}

/// [`arch::direct_call`]'s code: the hot part, a piece of its own, and
/// the cold part, a stub; and the jumps to link once they're placed, each
/// as (where in its part, as [`arch::link_raw`] takes it, offset in the
/// other part).
struct RawCall {
    hot: Vec<u8>,
    cold: Vec<u8>,
    hot_to_cold: Vec<(usize, usize)>,
    cold_to_hot: Vec<(usize, usize)>,
}

/// What [`arch::direct_call`] needs: offsets in a `CallSite`, the frame
/// header's flags slot, `undefined`, and the runtime functions it calls.
struct DirectConsts {
    closure: usize,
    end_off: usize,
    stack_end: usize,
    limit: usize,
    flags: usize,
    undefined: u64,
    call: u64,
    exit: u64,
}

impl DirectConsts {
    fn new() -> Self {
        const { assert!(NO_EXIT == u32::MAX) };
        DirectConsts {
            closure: std::mem::offset_of!(CallSite, closure),
            end_off: std::mem::offset_of!(CallSite, end_off),
            stack_end: std::mem::offset_of!(CallSite, stack_end),
            limit: std::mem::offset_of!(CallSite, native_limit),
            flags: 8 * interp::FLAGS,
            undefined: Value::UNDEFINED.to_bits(),
            call: toi_rt_call as *const () as u64,
            exit: toi_direct_exit as *const () as u64,
        }
    }
}

struct EnterConsts {
    limit: usize,
    frame: usize,
    flags: usize,
    undefined: u64,
    call: u64,
    exit: u64,
}

impl EnterConsts {
    fn new() -> Self {
        const { assert!(NO_EXIT == u32::MAX) };
        EnterConsts {
            limit: std::mem::offset_of!(CallSite, native_limit),
            frame: std::mem::offset_of!(CallSite, frame),
            flags: 8 * interp::FLAGS,
            undefined: Value::UNDEFINED.to_bits(),
            call: toi_rt_call as *const () as u64,
            exit: toi_direct_exit as *const () as u64,
        }
    }
}

/// `construct_direct` stencils, once their checks passed, when they can't
/// allocate `this` themselves (see [`CtorSite`]): `new` of the
/// callee of `site` (a `CallSite`), whose frame the compiled frame `regs`
/// has set up but for `this`. Allocate `this`, and if it has the shape the
/// callee's version was bound for, enter that (`toi_rt_enter`); otherwise
/// (the constructor's root changed: its prototype, or its objects' size)
/// go through the runtime, from now on. The result is the constructor's,
/// if an object, else `this`. `EMPTY` on an error.
///
/// The frame isn't marked as a `new` one: this function (or the stencil)
/// keeps `this` and makes the result, also after an exit (the interpreter
/// finishes the frame as a plain call) and after a self tail call reused
/// the frame (whose `this` is then the tail call's).
extern "C" fn toi_rt_construct(regs: Value, site: Value) -> Value {
    unsafe {
        let s = site.to_bits() as *mut CallSite;
        let vm = &mut *(*s).vm;
        vm.safepoint(regs.to_bits() as *mut Value);
        let this = vm.construct_this((*s).closure as *mut Object);
        let o = crate::object::as_object(this);
        if (*s).this_shape != 0 && (*o).shape as u64 != (*s).this_shape {
            (*s).entry = 0;
            return toi_rt_call(regs, site);
        }
        *(regs.to_bits() as *mut Value).add((*s).frame) = this;
        let v = toi_rt_enter(regs, site);
        if v.to_bits() == 0 || v.is_cell() {
            // An error, or an object: the constructor's, or (through the
            // runtime, the native stack being low) `new`'s already.
            v
        } else {
            crate::vm::Vm::construct_done(&*(*s).callee, o);
            this
        }
    }
}

/// `materialize` stencils, in an exit: allocate the object `mat` (a
/// [`Mat`]) describes, its fields from the frame `regs` (spilled), and
/// put it in its registers.
extern "C" fn toi_rt_materialize(regs: Value, mat: Value) -> u64 {
    unsafe {
        let m = &*(mat.to_bits() as *const Mat);
        let regs = regs.to_bits() as *mut Value;
        let vm = &mut *crate::interp::current_vm();
        let o = vm.heap.alloc_object(&*m.shape);
        for (k, &r) in m.fields.iter().enumerate() {
            vm.store_field(o, k as u32, *regs.add(r as usize));
        }
        for &r in &m.regs {
            *regs.add(r as usize) = crate::object::object_value(o);
        }
        0
    }
}

/// `array_push` stencils, when the value doesn't fit the elements' type or
/// the elements are full: `Array.prototype.push` of `v` onto `a` (an
/// array), its new length.
extern "C" fn toi_rt_array_push(a: Value, v: Value) -> Value {
    let vm = unsafe { &mut *crate::interp::current_vm() };
    vm.safepoint(std::ptr::null_mut());
    crate::vm::array_push(vm, a, &[v]).expect("a push onto an array")
}

/// `construct_direct` stencils, after a constructor they allocated `this`
/// for returned it: `Vm::construct_done`. If that will have nothing to
/// do for objects of `this`'s shape from now on (their keys fit the
/// capacity, which only grows, and the constructor's result is that
/// shape or already several), the stencil skips the call for that shape.
extern "C" fn toi_rt_construct_done(site: Value, this: Value) -> u64 {
    unsafe {
        let s = &mut *(site.to_bits() as *mut CallSite);
        let o = crate::object::as_object(this);
        let code = &*s.callee;
        crate::vm::Vm::construct_done(code, o);
        let shape = (*o).shape;
        let seen = code.ctor_result.get();
        if code.ctor_settled.get()
            && (*shape).keys.len() as u32 <= code.ctor_capacity.get()
            && (seen == shape || seen == crate::vm::MANY_SHAPES)
        {
            s.ctor.as_mut().unwrap().done_shape = shape as u64;
        }
        0
    }
}

/// [`toi_rt_enter`]: the callee's code exited with `record` and `acc`.
extern "C" fn toi_direct_exit(
    regs: *mut Value,
    site: *mut CallSite,
    record: u32,
    acc: Value,
) -> Value {
    unsafe { direct_exit(&mut *(*site).vm, regs, &mut *site, record, acc) }
}

/// A direct call's callee code exited with `record` and `acc`. Account for
/// the exit, as an entry through the driver would, write the callee frame's
/// header (it returns to native code), and finish the call in the
/// interpreter. `EMPTY` on an error.
///
/// # Safety
/// `caller` must be the frame making the call at `site`, whose callee's
/// code just exited.
unsafe fn direct_exit(
    vm: &mut crate::vm::Vm,
    caller: *mut Value,
    site: &mut CallSite,
    record: u32,
    acc: Value,
) -> Value {
    unsafe {
        if vm.error.is_some() {
            return Value::EMPTY;
        }
        // Through the runtime from now on, or for a site claimed by types,
        // until the next call binds it again: the exit makes the callee
        // lower its code again, and maybe compile another version (a call
        // that hadn't run has run now). An exit to make a call the native
        // stack had no room for says nothing about the callee's code.
        // A warm callee tiering up says nothing either: the next call binds
        // its hot code.
        if !vm.redo_call {
            if record & TIER_UP as u32 == 0 {
                site.exits += 1;
            }
            site.entry = 0;
            site.pending = site.claim.is_some() && site.exits < MAX_DIRECT_EXITS;
            if site.exits == MAX_DIRECT_EXITS {
                site.retry = callee_compiles(site.callee);
            }
        }
        let callee_regs = caller.add(site.frame);
        // A call to the code itself: that code is running (it made the
        // call), so it is alive.
        let code: &JitCode = match &site.target {
            Some(code) => code,
            None => &*site.self_code,
        };
        let (frame, exit_pc) = {
            let mut slot = (*site.callee).jit.borrow_mut();
            slot.as_mut().unwrap().vm_finish(code, 0, record as usize)
        };
        // Not a `new` frame, even for `new`: `toi_rt_construct` makes the
        // result.
        interp::write_frame_header(vm, callee_regs, site.callee, std::ptr::null(), caller, 0);
        let r = materialize(vm, callee_regs, code, frame, exit_pc, acc);
        interp::drive(vm, r.pc, r.acc, r.regs)
    }
}

/// `call` stencils: run the call `site` (a `CallSite`) describes, over the
/// compiled frame `regs`. `EMPTY` on an error.
extern "C" fn toi_rt_call(regs: Value, site: Value) -> Value {
    unsafe {
        let site = &mut *(site.to_bits() as *mut CallSite);
        (*site.vm).safepoint(regs.to_bits() as *mut Value);
        // `TOI_EXITS`: also the calls through the runtime, by callee and why.
        if *EXITS_ON.get_or_init(|| std::env::var_os("TOI_EXITS").is_some()) && !site.callee.is_null() {
            let key = format!(
                "rt_call {} entry={} pending={} exits={} claim={} bound={}",
                (*site.callee).name,
                site.entry != 0,
                site.pending,
                site.exits,
                site.claim.is_some(),
                site.bound
            );
            *EXITS.lock().unwrap().entry(key).or_default() += 1;
        }
        // A site that stopped calling directly after its callee's exits:
        // bind it again once the callee compiled since. (Warm-up exits, at
        // stores to a constructor's `this` whose shapes differ by subclass,
        // say, made constructors run in the interpreter for good.)
        if site.claim.is_some()
            && !site.pending
            && site.exits >= MAX_DIRECT_EXITS
            && callee_compiles(site.callee) != site.retry
        {
            site.exits = 0;
            site.pending = true;
        }
        // A direct call whose callee was being compiled: bind it now, for
        // the next calls.
        if site.pending {
            site.bind_pending();
        }
        interp::call_from_jit(
            &mut *site.vm,
            regs.to_bits() as *mut Value,
            site.op,
            site.f,
            site.w,
            site.argc,
            site.frame,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
        })
    }

    /// `mod_magic`'s quotients are exact for every dividend below 2^31:
    /// checked for every divisor up to 5000 and some larger ones, at the
    /// dividends around each multiple near the ends, and a spread between.
    #[test]
    fn mod_magic_is_exact() {
        let big = [
            65535,
            65537,
            1 << 20 | 1,
            1_000_000_007,
            (1 << 30) + 1,
            i32::MAX,
        ];
        for d in (3..5000).chain(big).filter(|d| d & (d - 1) != 0) {
            let (s, m) = mod_magic(d);
            assert!(m < 1 << 32, "{d}");
            let d64 = d as u64;
            let top = (1u64 << 31) - 1;
            let near = |q: u64| [q * d64, q * d64 + d64 - 1, q * d64 + 1];
            let ends = (0..4)
                .flat_map(near)
                .chain(((top / d64).saturating_sub(2)..=top / d64).flat_map(near));
            let spread = (0..2000u64).map(|k| k * 1_073_741 % (top + 1));
            for a in ends.chain(spread).filter(|&a| a <= top) {
                assert_eq!((a * m) >> s, a / d64, "{a} / {d}");
            }
        }
    }

    /// Every stencil variant LLVM compiled is reproduced exactly by the JIT's    /// Every stencil variant LLVM compiled is reproduced exactly by the JIT's
    /// lookup and copy, including (on arm64) those rebuilt from a family
    /// template by patching register fields.
    #[test]
    fn every_stencil_instance_matches_its_compiled_bytes() {
        for &(name, hash) in STENCIL_HASHES {
            let inst = Instance::get(name);
            let len = inst.stencil().code.len();
            let mut buf = vec![0; len];
            inst.copy_to(&mut buf, len);
            assert_eq!(fnv1a(&buf), hash, "{name} (from {})", inst.stencil().name);
        }
    }
}
