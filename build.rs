//! Build the copy-and-patch stencils (see `stencils/stencils.rs`).
//!
//! 1. Compile `stencils/stencils.rs` on its own into an object file for the
//!    target, with the static relocation model, so that holes and
//!    continuations are plain relocations, not GOT/PLT indirections.
//! 2. For every `cloison_stencil_*` function, extract its machine code and
//!    relocations.
//! 3. On arm64, fold trailing branch stubs ([`fold_arm64_stubs`]) and
//!    invert conditional branches over lone jumps
//!    ([`invert_arm64_branch_over_jump`]). On x86-64, fold the trailing stubs
//!    that `asm goto` labels become ([`fold_x86_stubs`]), and make a final
//!    two-way branch end in the continuation ([`swap_x86_final_branch`]);
//!    a stencil whose exits still go through stubs takes the code of a
//!    second compile with every branch long, where they fold (`main`).
//! 4. On arm64, compact each family of per-slot variants into one template
//!    whose register fields the JIT patches ([`compact_arm64`]).
//! 5. Check every stencil's code ([`verify`]): a violation fails the build.
//! 6. Write `$OUT_DIR/stencils.rs`, included by `src/jit.rs`.
//!
//! The object file is kept as `$OUT_DIR/stencils.o` for inspection:
//! `scripts/stencils-asm.sh` disassembles it.

#[path = "build/verify.rs"]
mod verify;

use object::{Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationTarget, SymbolKind};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

const SOURCE: &str = "stencils/stencils.rs";
const PREFIX: &str = "cloison_stencil_";

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed=src/value.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/verify.rs");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let obj_path = out_dir.join("stencils.o");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let mut extracted = extract(&compile(&rustc, &target, &obj_path, &[]), &arch);
    // x86-64: LLVM jumps to an exit or a helper with a short `jcc` to a
    // stub after the hot path's final `jmp cloison_continue`, which then can't
    // be trimmed. Assembled with every branch long (`-mc-relax-all`), the
    // stubs fold away (`fold_x86_stubs`). That lengthens every other local
    // jump: a stencil takes its relaxed form only where that lets it end in
    // `jmp cloison_continue` and makes it no longer (DeltaBlue's `teq_jt`, 147
    // to 158 bytes of mostly 6-byte `jcc`s, cost it 2.5%).
    if arch == "x86_64" {
        let relaxed_path = out_dir.join("stencils-relaxed.o");
        let relaxed = extract(
            &compile(&rustc, &target, &relaxed_path, &["-Cllvm-args=-mc-relax-all"]),
            &arch,
        );
        let mut relaxed: std::collections::HashMap<String, Extracted> =
            relaxed.into_iter().map(|e| (e.name.clone(), e)).collect();
        for e in &mut extracted {
            if !ends_with_continue(e)
                && let Some(r) = relaxed.remove(&e.name)
                && ends_with_continue(&r)
                && r.code.len() <= e.code.len()
            {
                *e = r;
            }
        }
    }
    assert!(
        !extracted.is_empty(),
        "no {PREFIX}* symbols in {}",
        obj_path.display()
    );
    let violations: Vec<String> = extracted
        .iter()
        .flat_map(|e| {
            let relocs: Vec<verify::Reloc> = e
                .relocs
                .iter()
                .map(|r| verify::Reloc { offset: r.offset as usize, r_type: r.r_type, symbol: &r.symbol })
                .collect();
            verify::check(&e.name, &arch, &e.code, &relocs)
                .into_iter()
                .map(move |v| format!("  {}: {v}", e.name))
        })
        .collect();
    assert!(
        violations.is_empty(),
        "stencils the JIT can't copy as they are:\n{}",
        violations.join("\n")
    );

    let mut entries = if arch == "aarch64" {
        compact_arm64(extracted)
    } else {
        extracted.into_iter().map(Entry::concrete).collect()
    };
    // In name order, so the tables don't depend on the object file's.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    fs::write(out_dir.join("stencils.rs"), emit(&entries)).unwrap();
}


/// Compile the stencils into the object file `obj_path`, with `extra`
/// codegen options; returns its bytes.
fn compile(rustc: &str, target: &str, obj_path: &std::path::Path, extra: &[&str]) -> Vec<u8> {
    let mut cmd = Command::new(rustc);
    cmd.args([
        "--edition=2024",
        "--crate-type=lib",
        "--crate-name=cloison_stencils",
    ])
    .args(["-Copt-level=3", "-Cpanic=abort", "-Ccodegen-units=1"])
    .args(["-Crelocation-model=static", "-Ccode-model=small"])
    // Cargo doesn't show a build script's compiler warnings: as errors,
    // they can't pile up unseen (the nightly is pinned, so new lints come
    // with a deliberate bump). Edition 2024 warns of an unsafe operation
    // outside an `unsafe` block in an `unsafe fn`.
    .arg("-Dwarnings");
    // Darwin's default CPU is the host's (`apple-m1`), whose scheduling
    // model orders instructions differently; `generic` is Linux's default,
    // so the stencils come out the same bytes on both.
    if target.contains("apple") {
        cmd.arg("-Ctarget-cpu=generic");
    }
    let status = cmd
        .args(extra)
        .args(["--target", target])
        .arg(format!("--emit=obj={}", obj_path.display()))
        .arg(SOURCE)
        .status()
        .expect("failed to run rustc for the stencils");
    assert!(status.success(), "compiling {SOURCE} failed");
    fs::read(obj_path).unwrap()
}

/// Whether a stencil ends with its hot path's `jmp cloison_continue` (which
/// the JIT trims when the next stencil follows).
fn ends_with_continue(e: &Extracted) -> bool {
    let n = e.code.len();
    match e.code.len() {
        0..5 => false,
        _ if e.code[n - 5] == 0xE9 => e
            .relocs
            .iter()
            .any(|r| r.offset == (n - 4) as u64 && r.symbol == "cloison_continue"),
        _ => false,
    }
}

/// Every `cloison_stencil_*` function of the object file `data`: its code and
/// relocations, after this target's rewrites.
fn extract(data: &[u8], arch: &str) -> Vec<Extracted> {
    let file = object::File::parse(data).expect("stencil object");
    // Mach-O (macOS) symbol names carry a leading underscore.
    let macho = file.format() == object::BinaryFormat::MachO;
    fn plain(macho: bool, n: &str) -> &str {
        if macho { n.strip_prefix('_').unwrap_or(n) } else { n }
    }

    let text: Vec<_> = file
        .symbols()
        .filter(|s| s.kind() == SymbolKind::Text && s.is_definition())
        .collect();
    // Mach-O symbols have no size: a function ends where the next one in
    // its section starts.
    let mut starts: Vec<(usize, u64)> = text
        .iter()
        .map(|s| (s.section_index().unwrap().0, s.address()))
        .collect();
    starts.sort_unstable();
    let end_of = |sym: &object::Symbol| -> u64 {
        if sym.size() != 0 {
            return sym.address() + sym.size();
        }
        let sec = sym.section_index().unwrap();
        starts
            .iter()
            .find(|&&(s, a)| s == sec.0 && a > sym.address())
            .map(|&(_, a)| a)
            .unwrap_or_else(|| file.section_by_index(sec).unwrap().size())
    };

    let mut symbols: Vec<_> = text
        .iter()
        .filter_map(|s| Some((plain(macho, s.name().ok()?).strip_prefix(PREFIX)?.to_owned(), s)))
        .collect();
    symbols.sort_by(|a, b| a.0.cmp(&b.0));

    let mut extracted: Vec<Extracted> = Vec::new();
    for (name, sym) in symbols {
        let section = file.section_by_index(sym.section_index().unwrap()).unwrap();
        // In a relocatable object, a symbol's address is its section offset.
        let start = sym.address();
        let end = end_of(sym);
        let section_data = section.data().unwrap();
        let code = &section_data[start as usize..end as usize];

        let mut relocs: Vec<Reloc> = Vec::new();
        for (offset, reloc) in section.relocations() {
            if offset < start || offset >= end {
                continue;
            }
            let r_type = match reloc.flags() {
                RelocationFlags::Elf { r_type } => r_type.0,
                // Mach-O has one type for `b` and `bl`, which ELF tells
                // apart (and the JIT patches alike).
                RelocationFlags::MachO { r_type, r_pcrel: true, r_length: 2 }
                    if arch == "aarch64" && r_type.0 == object::macho::ARM64_RELOC_BRANCH26.0 =>
                {
                    let at = (offset - start) as usize;
                    let i = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
                    if i >> 31 == 1 { R_AARCH64_CALL26 } else { R_AARCH64_JUMP26 }
                }
                // A `@PAGEOFF` operand: ELF's `:lo12:`, typed by the
                // instruction (a load or store's size, or `add`).
                RelocationFlags::MachO { r_type, r_pcrel: false, r_length: 2 }
                    if arch == "aarch64" && r_type.0 == object::macho::ARM64_RELOC_PAGEOFF12.0 =>
                {
                    let at = (offset - start) as usize;
                    let i = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
                    if i & 0x3B00_0000 == 0x3900_0000 {
                        // Load/store, unsigned offset: size in bits 31:30.
                        [R_AARCH64_LDST8_ABS_LO12_NC, R_AARCH64_LDST16_ABS_LO12_NC,
                            R_AARCH64_LDST32_ABS_LO12_NC, R_AARCH64_LDST64_ABS_LO12_NC]
                            [(i >> 30) as usize]
                    } else if i & 0x7F80_0000 == 0x1100_0000 {
                        R_AARCH64_ADD_ABS_LO12_NC
                    } else {
                        panic!("{name}: PAGEOFF12 on {i:#010x} at {offset:#x}")
                    }
                }
                _ => panic!("{name}: unsupported relocation kind at {offset:#x}"),
            };
            let RelocationTarget::Symbol(idx) = reloc.target() else {
                panic!("{name}: relocation at {offset:#x} has no symbol target");
            };
            let target = file.symbol_by_index(idx).unwrap();
            let target_name = plain(macho, target.name().unwrap());
            // Only holes, continuations, runtime functions and the stencil's
            // own `__slow` helper may be referenced. Anything else (rodata,
            // panics, section-relative references) would need copying data
            // the JIT doesn't have.
            let allowed = target_name.starts_with("cloison_hole_")
                || target_name.starts_with("cloison_rt_")
                || target_name == format!("{PREFIX}{name}__slow")
                || target_name == "cloison_continue"
                || target_name == "cloison_jump_target"
                || target_name == "cloison_exit";
            assert!(
                allowed,
                "stencil {name}: unsupported relocation to `{target_name}` \
                 (type {r_type}) at {offset:#x}; stencils must stay self-contained"
            );
            relocs.push(Reloc {
                offset: offset - start,
                r_type,
                symbol: if target_name.starts_with(PREFIX) {
                    "@slow".into()
                } else {
                    target_name.to_owned()
                },
                addend: reloc.addend(),
            });
        }

        let mut code = code.to_vec();
        if macho && arch == "aarch64" {
            macho_holes(&name, &mut code, &mut relocs);
        }
        match arch {
            "aarch64" => {
                fold_arm64_stubs(&name, &mut code, &mut relocs);
                retarget_arm64_jumps(&mut code, &mut relocs);
                invert_arm64_branch_over_jump(&mut code, &mut relocs);
                widen_arm64_tbz(&mut code, &mut relocs);
            }
            "x86_64" => {
                fold_x86_stubs(&mut code, &mut relocs);
                swap_x86_final_branch(&mut code, &mut relocs);
            }
            _ => {}
        }
        relocs.sort_by_key(|r| r.offset);
        extracted.push(Extracted { name, code, relocs });
    }
    extracted
}

/// A stencil as extracted from the object file.
struct Extracted {
    name: String,
    code: Vec<u8>,
    relocs: Vec<Reloc>,
}

/// A table entry: a concrete stencil, or (arm64) a family template whose
/// slot registers the JIT patches in (`fields`).
struct Entry {
    /// The concrete name, or the family key with `{}` for each slot number.
    name: String,
    code: Vec<u8>,
    relocs: Vec<Reloc>,
    /// (instruction offset, bit position, parameter index): 5-bit register
    /// fields holding the register of the family's parameter.
    fields: Vec<(usize, u32, usize)>,
    /// For a template: the slot numbers of the members it covers.
    covers: Vec<Vec<u8>>,
    /// Every stencil this entry stands for, with a hash of its code as
    /// compiled (for a test that the JIT reproduces it).
    members: Vec<(String, u64)>,
}

impl Entry {
    fn concrete(e: Extracted) -> Entry {
        let members = vec![(e.name.clone(), fnv1a(&e.code))];
        Entry {
            name: e.name,
            code: e.code,
            relocs: e.relocs,
            fields: Vec::new(),
            covers: Vec::new(),
            members,
        }
    }
}

/// FNV-1a, 64-bit. Must match `fnv1a` in the tests of `src/jit.rs`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// Write `$OUT_DIR/stencils.rs`.
fn emit(entries: &[Entry]) -> String {
    let mut out = String::from("// @generated by build.rs from stencils/stencils.rs\n\n");
    let mut rt_names: Vec<&str> = entries
        .iter()
        .flat_map(|e| &e.relocs)
        .map(|r| r.symbol.as_str())
        .filter(|s| s.starts_with("cloison_rt_"))
        .collect();
    rt_names.sort();
    rt_names.dedup();
    writeln!(out, "/// Runtime functions, indexed by `Sym::Rt`.\npub(crate) static RT_NAMES: &[&str] = &{rt_names:?};\n").unwrap();
    // Concrete stencils by name: an open-addressing table (linear probing,
    // at most 2/3 full) of `STENCILS` indexes + 1, at the FNV-1a hash of
    // the name. Built here, so the JIT has nothing to build at startup.
    let concrete: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].covers.is_empty())
        .collect();
    let size = (3 * concrete.len()).div_ceil(2).next_power_of_two();
    let mut table = vec![0u16; size];
    for &i in &concrete {
        let mut h = fnv1a(entries[i].name.as_bytes()) as usize & (size - 1);
        while table[h] != 0 {
            h = (h + 1) & (size - 1);
        }
        table[h] = u16::try_from(i + 1).expect("too many stencils");
    }
    writeln!(out, "/// Concrete stencils by name: `STENCILS` index + 1 at `fnv1a(name)`, linear probing (0: empty).\npub(crate) static BY_NAME: [u16; {size}] = {table:?};\n").unwrap();
    out.push_str("pub(crate) static STENCILS: &[Stencil] = &[\n");
    for e in entries {
        // A template's members each have a `__slow` helper if the family does
        // (helpers are their own family, or explicit).
        let slow_name = format!("{}__slow", e.name);
        let slow = entries
            .iter()
            .any(|h| h.name == slow_name || slot_params(&h.name).0 == slow_name);
        // The helper's index + 1 when it is the one stencil (or template)
        // named so, which an instance's registers fit too; else 0 (looked
        // up by name: a family's templates share a name).
        let named: Vec<usize> = (0..entries.len())
            .filter(|&k| entries[k].name == slow_name)
            .collect();
        let slow_at = match named[..] {
            [k] if entries[k].covers.is_empty() == e.covers.is_empty() => k + 1,
            _ => 0,
        };
        let mut relocs = String::new();
        for r in &e.relocs {
            let symbol = match r.symbol.as_str() {
                "cloison_hole_imm" => "Sym::HoleImm".to_string(),
                "cloison_hole_imm64" => "Sym::HoleImm64".to_string(),
                "cloison_hole_imm64b" => "Sym::HoleImm64b".to_string(),
                "cloison_hole_reg" => "Sym::HoleReg".to_string(),
                "cloison_continue" => "Sym::Continue".to_string(),
                "cloison_jump_target" => "Sym::JumpTarget".to_string(),
                "cloison_exit" => "Sym::Exit".to_string(),
                "@slow" => "Sym::Slow".to_string(),
                rt => format!(
                    "Sym::Rt({})",
                    rt_names.iter().position(|n| *n == rt).unwrap()
                ),
            };
            write!(
                relocs,
                "R({}, {}, {symbol}, {}), ",
                r.offset, r.r_type, r.addend
            )
            .unwrap();
        }
        let template = if e.covers.is_empty() {
            "None".to_string()
        } else {
            let fields: Vec<String> = e
                .fields
                .iter()
                .map(|(o, sh, p)| format!("({o}, {sh}, {p})"))
                .collect();
            let covers: Vec<String> = e
                .covers
                .iter()
                .map(|ks| format!("[{}, {}]", ks[0], ks.get(1).copied().unwrap_or(0)))
                .collect();
            format!(
                "Some(&Template {{ fields: &[{}], covers: &[{}] }})",
                fields.join(", "),
                covers.join(", ")
            )
        };
        writeln!(
            out,
            "    Stencil {{ name: {:?}, slow: {slow}, slow_at: {slow_at}, code: &{:?}, relocs: &[{relocs}], template: {template} }},",
            e.name, e.code
        )
        .unwrap();
    }
    out.push_str("];\n\n");
    out.push_str("/// Every stencil variant as compiled, with a hash of its code.\n");
    out.push_str("#[cfg(test)]\npub(crate) static STENCIL_HASHES: &[(&str, u64)] = &[\n");
    for e in entries {
        for (name, hash) in &e.members {
            writeln!(out, "    ({name:?}, {hash:#x}),").unwrap();
        }
    }
    out.push_str("];\n");
    out
}

/// Split a stencil name into its family key and slot parameters: each
/// `_g<k>`, `_u<k>` or `_f<k>` token (followed by `_` or the end) becomes
/// `_g{}` etc. E.g. `addr_ii_u3_u5` → (`addr_ii_u{}_u{}`, [('u', 3), ('u', 5)]).
/// Must match `slot_params` in `src/jit.rs`.
fn slot_params(name: &str) -> (String, Vec<(char, u8)>) {
    let mut key = String::new();
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

/// The arm64 register of a slot parameter: GPR slots 0–4 are x24–x28, 5–11
/// are x0–x6; FP slot k is d(k+1). Must match `arch::slot_reg` in
/// `src/jit.rs`.
fn arm64_slot_reg((class, k): (char, u8)) -> u32 {
    match class {
        'f' => k as u32 + 1,
        _ if k < 5 => 24 + k as u32,
        _ => k as u32 - 5,
    }
}

/// arm64: replace each family of slot variants (`ldar_g0` … `ldar_g11`) by
/// templates plus the positions of their register fields, which the JIT
/// patches. Instructions are 32 bits with registers in 5-bit fields at bits
/// 0, 5, 10 and 16 (Rd/Rt, Rn, Ra/Rt2, Rm), so a register is a bit-field
/// patch, like a hole.
///
/// A family can need several templates: LLVM may lay out a commutative
/// operation differently depending on the register numbers (`fadd d1, d1,
/// d2` vs `fadd d1, d2, d1`). So members are clustered:
/// 1. Take a template from the remaining members (preferring distinct
///    registers, so each field's parameter is unambiguous). Its candidate
///    fields are the 5-bit fields holding a parameter's register.
/// 2. Confirm them against a second member whose registers all differ from
///    the template's: that rules out coincidences such as a zero immediate
///    looking like `x0`.
/// 3. The cluster is every remaining member that the template rebuilds
///    byte-for-byte, with the same relocations. Members in no cluster of
///    two or more stay explicit.
fn compact_arm64(stencils: Vec<Extracted>) -> Vec<Entry> {
    use std::collections::BTreeMap;
    type Member = (Vec<(char, u8)>, Extracted);
    let mut families: BTreeMap<String, Vec<Member>> = BTreeMap::new();
    for e in stencils {
        let (key, params) = slot_params(&e.name);
        families.entry(key).or_default().push((params, e));
    }
    let word = |code: &[u8], at: usize| u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
    let field = |code: &[u8], at: usize, shift: u32| (word(code, at) >> shift) & 31;
    let same_relocs = |a: &[Reloc], b: &[Reloc]| {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                (x.offset, x.r_type, &x.symbol, x.addend)
                    == (y.offset, y.r_type, &y.symbol, y.addend)
            })
    };
    let regs = |ps: &[(char, u8)]| ps.iter().map(|&p| arm64_slot_reg(p)).collect::<Vec<u32>>();
    let distinct = |rs: &[u32]| (0..rs.len()).all(|i| !rs[..i].contains(&rs[i]));

    let mut entries = Vec::new();
    for (key, mut rest) in families {
        if rest.len() < 2 || rest[0].0.is_empty() {
            entries.extend(rest.into_iter().map(|(_, e)| Entry::concrete(e)));
            continue;
        }
        let n_params = rest[0].0.len();
        while !rest.is_empty() {
            let t = rest
                .iter()
                .position(|(ps, _)| distinct(&regs(ps)))
                .unwrap_or(0);
            let (tps, template) = rest.swap_remove(t);
            let (tregs, len) = (regs(&tps), template.code.len());
            let mut fields: Vec<(usize, u32, usize)> = Vec::new();
            for at in (0..len).step_by(4) {
                for shift in [0, 5, 10, 16] {
                    for (p, &r) in tregs.iter().enumerate() {
                        if field(&template.code, at, shift) == r {
                            fields.push((at, shift, p));
                        }
                    }
                }
            }
            // Confirm with a member of the same shape whose registers all
            // differ from the template's.
            let witness = rest.iter().find(|(ps, e)| {
                let rs = regs(ps);
                e.code.len() == len
                    && same_relocs(&e.relocs, &template.relocs)
                    && rs.iter().zip(&tregs).all(|(a, b)| a != b)
            });
            if let Some((wps, w)) = witness {
                let wregs = regs(wps);
                fields.retain(|&(at, shift, p)| field(&w.code, at, shift) == wregs[p]);
            }
            let rebuild = |rs: &[u32]| {
                let mut code = template.code.clone();
                for &(at, shift, p) in &fields {
                    let w = (word(&code, at) & !(31 << shift)) | rs[p] << shift;
                    code[at..at + 4].copy_from_slice(&w.to_le_bytes());
                }
                code
            };
            let (cluster, others): (Vec<Member>, Vec<Member>) =
                std::mem::take(&mut rest).into_iter().partition(|(ps, e)| {
                    e.code == rebuild(&regs(ps)) && same_relocs(&e.relocs, &template.relocs)
                });
            rest = others;
            if cluster.is_empty() || witness_missing(n_params, &fields) {
                entries.push(Entry::concrete(template));
                entries.extend(cluster.into_iter().map(|(_, e)| Entry::concrete(e)));
                continue;
            }
            let mut covers: Vec<Vec<u8>> = vec![tps.iter().map(|p| p.1).collect()];
            covers.extend(
                cluster
                    .iter()
                    .map(|(ps, _)| ps.iter().map(|p| p.1).collect()),
            );
            let mut members = vec![(template.name.clone(), fnv1a(&template.code))];
            members.extend(
                cluster
                    .iter()
                    .map(|(_, e)| (e.name.clone(), fnv1a(&e.code))),
            );
            entries.push(Entry {
                name: key.clone(),
                code: template.code,
                relocs: template.relocs,
                fields,
                covers,
                members,
            });
        }
    }
    entries
}

/// Whether a template's fields can't cover every parameter (a parameter
/// with no field would be silently ignored).
fn witness_missing(n_params: usize, fields: &[(usize, u32, usize)]) -> bool {
    (0..n_params).any(|p| !fields.iter().any(|f| f.2 == p))
}

#[derive(Clone)]
struct Reloc {
    offset: u64,
    r_type: u32,
    symbol: String,
    addend: i64,
}

const R_AARCH64_MOVW_UABS_G0_NC: u32 = 264;
const R_AARCH64_MOVW_UABS_G1_NC: u32 = 266;
const R_AARCH64_MOVW_UABS_G2_NC: u32 = 268;
const R_AARCH64_MOVW_UABS_G3: u32 = 269;
const R_AARCH64_ADD_ABS_LO12_NC: u32 = 277;
const R_AARCH64_LDST8_ABS_LO12_NC: u32 = 278;
const R_AARCH64_TSTBR14: u32 = 279;
const R_AARCH64_CONDBR19: u32 = 280;
const R_AARCH64_JUMP26: u32 = 282;
const R_AARCH64_CALL26: u32 = 283;
const R_AARCH64_LDST16_ABS_LO12_NC: u32 = 284;
const R_AARCH64_LDST32_ABS_LO12_NC: u32 = 285;
const R_AARCH64_LDST64_ABS_LO12_NC: u32 = 286;

/// Mach-O has no relocation for a `movz`/`movk` immediate field (ELF's
/// `MOVW_UABS_G*`), so on macOS the stencils write each hole as a magic
/// immediate `0xB0<hole><part>` (`hole16!` in `stencils/stencils.rs`):
/// hole 1 `cloison_hole_imm`, 2 `cloison_hole_reg`, 3 `cloison_hole_imm64`, 4
/// `cloison_hole_imm64b`; part 0–3 the 16 bits at `16 * part`. Record each as
/// the relocation it stands for and zero the field, so the code is what
/// the ELF build extracts.
fn macho_holes(name: &str, code: &mut [u8], relocs: &mut Vec<Reloc>) {
    const HOLES: [&str; 4] = ["cloison_hole_imm", "cloison_hole_reg", "cloison_hole_imm64", "cloison_hole_imm64b"];
    const MOVW: [u32; 4] = [
        R_AARCH64_MOVW_UABS_G0_NC,
        R_AARCH64_MOVW_UABS_G1_NC,
        R_AARCH64_MOVW_UABS_G2_NC,
        R_AARCH64_MOVW_UABS_G3,
    ];
    for off in (0..code.len()).step_by(4) {
        let i = u32::from_le_bytes(code[off..off + 4].try_into().unwrap());
        let imm = (i >> 5) & 0xFFFF;
        let (hole, part) = ((imm >> 4) & 0xF, imm & 0xF);
        if imm & 0xFF00 != 0xB000 || !(1..=4).contains(&hole) || part > 3 {
            continue;
        }
        // `movz` or `movk`, 32- or 64-bit, shifted by the part.
        let movw = matches!(i & 0x7F80_0000, 0x5280_0000 | 0x7280_0000);
        assert!(
            movw && (i >> 21) & 3 == part,
            "stencil {name}: {i:#010x} at {off:#x} has a hole's magic immediate \
             but isn't a movz/movk of part {part}"
        );
        relocs.push(Reloc {
            offset: off as u64,
            r_type: MOVW[part as usize],
            symbol: HOLES[hole as usize - 1].into(),
            addend: 0,
        });
        code[off..off + 4].copy_from_slice(&(i & !(0xFFFF << 5)).to_le_bytes());
    }
}

/// AArch64 has no conditional tail call. For `if slow { become helper(..) }`
/// LLVM emits a conditional branch to a local stub, `b.cc stub`, and puts
/// `stub: b helper` at the very end, after the hot path's `b cloison_continue`.
/// So the JIT couldn't drop that `b cloison_continue`, and the hot path would
/// pay an extra taken branch per operation.
///
/// Fold such stubs: while the stencil ends in `b <symbol>`, retarget every
/// branch that points at it straight to `<symbol>`, with a new relocation,
/// and drop the stub. Gives up (leaving the stub) if anything else might
/// reach it.
fn fold_arm64_stubs(name: &str, code: &mut Vec<u8>, relocs: &mut Vec<Reloc>) {
    let insn = |code: &[u8], at: usize| u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
    loop {
        let n = code.len();
        if n < 8 {
            return;
        }
        let stub = (n - 4) as u64;
        let Some(stub_reloc) = relocs
            .iter()
            .position(|r| r.offset == stub && r.r_type == R_AARCH64_JUMP26)
        else {
            return;
        };
        // Don't fold the hot path's own `b cloison_continue`.
        if relocs[stub_reloc].symbol == "cloison_continue" {
            return;
        }
        // Find the branches targeting the stub.
        let mut users = Vec::new();
        for at in (0..n - 4).step_by(4) {
            let i = insn(code, at);
            let (kind, disp) = if i & 0xFF00_0010 == 0x5400_0000 || i & 0x7E00_0000 == 0x3400_0000 {
                // b.cond, cbz/cbnz: imm19 at [23:5]
                (
                    R_AARCH64_CONDBR19,
                    (((i >> 5) & 0x7FFFF) as i64) << 45 >> 43,
                )
            } else if i & 0x7E00_0000 == 0x3600_0000 {
                // tbz/tbnz: imm14 at [18:5]
                (R_AARCH64_TSTBR14, (((i >> 5) & 0x3FFF) as i64) << 50 >> 48)
            } else if i & 0xFC00_0000 == 0x1400_0000
                && !relocs.iter().any(|r| r.offset == at as u64)
            {
                // b with a local target: imm26 at [25:0]
                (R_AARCH64_JUMP26, ((i & 0x03FF_FFFF) as i64) << 38 >> 36)
            } else {
                continue;
            };
            if at as i64 + disp == stub as i64 {
                users.push((at as u64, kind));
            }
        }
        if users.is_empty() {
            // Nothing branches to it: a fall-through, so it can't go.
            return;
        }
        // Branched to, and also fallen into (the instruction before it
        // isn't an unconditional branch, `ret` or `br`): it must stay.
        let prev = insn(code, n - 8);
        let ends = prev & 0xFC00_0000 == 0x1400_0000 // b
            || prev & 0xFFFF_FC1F == 0xD65F_0000 // ret
            || prev & 0xFFFF_FC1F == 0xD61F_0000; // br
        if !ends {
            return;
        }
        let target = relocs.remove(stub_reloc);
        for (at, kind) in users {
            relocs.push(Reloc {
                offset: at,
                r_type: kind,
                symbol: target.symbol.clone(),
                addend: target.addend,
            });
        }
        code.truncate(n - 4);
        let _ = name;
    }
}

/// LLVM merges a stencil's tail calls to one symbol into one `b <symbol>`
/// that the other exits branch to, so an early exit (a barrier finding
/// nothing to record, say) takes two branches. Aim every local branch to
/// a `b <symbol>` at the symbol itself, with a relocation of its own
/// (`invert_arm64_branch_over_jump` can then fold the `b` the fall-through
/// path still takes).
fn retarget_arm64_jumps(code: &mut [u8], relocs: &mut Vec<Reloc>) {
    // Not the final `b`: the JIT drops it when the next piece follows, so
    // branching to it is branching to that piece, and falling into it is
    // free.
    let last = code.len() - 4;
    let stubs: Vec<(usize, String, i64)> = relocs
        .iter()
        .filter(|r| r.r_type == R_AARCH64_JUMP26 && !r.symbol.starts_with('@') && r.offset as usize != last)
        .map(|r| (r.offset as usize, r.symbol.clone(), r.addend))
        .collect();
    for (y, symbol, addend) in stubs {
        for at in (0..code.len()).step_by(4) {
            if relocs.iter().any(|r| r.offset == at as u64) {
                continue;
            }
            let Some((disp, mask, shift)) = local_branch(code, at) else {
                continue;
            };
            if at as i64 + disp != y as i64 {
                continue;
            }
            let i = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
            let r_type = match (mask, shift) {
                (0x7FFFF, 5) => R_AARCH64_CONDBR19,
                (0x3FFF, 5) => R_AARCH64_TSTBR14,
                _ => R_AARCH64_JUMP26,
            };
            code[at..at + 4].copy_from_slice(&(i & !(mask << shift)).to_le_bytes());
            relocs.push(Reloc {
                offset: at as u64,
                r_type,
                symbol: symbol.clone(),
                addend,
            });
        }
    }
}

/// AArch64 branch displacement of the (relocation-free) local branch at `at`,
/// in bytes, with the field's mask and shift, if it is one.
fn local_branch(code: &[u8], at: usize) -> Option<(i64, u32, u32)> {
    let i = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
    if i & 0xFF00_0010 == 0x5400_0000 || i & 0x7E00_0000 == 0x3400_0000 {
        Some(((((i >> 5) & 0x7FFFF) as i64) << 45 >> 43, 0x7FFFF, 5)) // b.cond, cbz/cbnz
    } else if i & 0x7E00_0000 == 0x3600_0000 {
        Some(((((i >> 5) & 0x3FFF) as i64) << 50 >> 48, 0x3FFF, 5)) // tbz/tbnz
    } else if i & 0xFC00_0000 == 0x1400_0000 {
        Some((((i & 0x03FF_FFFF) as i64) << 38 >> 36, 0x03FF_FFFF, 0)) // b
    } else {
        None
    }
}

/// LLVM often branches over a lone jump: `x: b.cond L; y: b <symbol>;
/// L: …`. A conditional `become` produces one, either in the middle of a
/// stencil (`b.vc L; b cloison_exit`) or at its end
/// (`b.ge z; b cloison_jump_target; z: b cloison_continue`, a loop's back edge).
/// Taking the jump then costs a not-taken `b.cond` plus a taken `b`, and at
/// the end it also blocks trimming the final `b cloison_continue`.
///
/// Rewrite each one to `x: b.!cond <symbol>` (a new relocation) and delete
/// `y`, fixing up local branches that cross it. Skips sites where another
/// branch targets `y`.
fn invert_arm64_branch_over_jump(code: &mut Vec<u8>, relocs: &mut Vec<Reloc>) {
    let reloc_at = |relocs: &[Reloc], at: usize| relocs.iter().position(|r| r.offset == at as u64);
    'search: loop {
        for x in (0..code.len().saturating_sub(8)).step_by(4) {
            let y = x + 4;
            let xi = u32::from_le_bytes(code[x..x + 4].try_into().unwrap());
            let is_bcond = xi & 0xFF00_0010 == 0x5400_0000 && (xi & 0xF) < 0xE;
            let Some(ry) = reloc_at(relocs, y) else {
                continue;
            };
            if !is_bcond
                || reloc_at(relocs, x).is_some()
                || relocs[ry].r_type != R_AARCH64_JUMP26
                || local_branch(code, x).map(|b| b.0) != Some(8)
            {
                continue;
            }
            let targets_y = (0..code.len()).step_by(4).any(|at| {
                at != x
                    && reloc_at(relocs, at).is_none()
                    && local_branch(code, at).is_some_and(|(d, _, _)| at as i64 + d == y as i64)
            });
            if targets_y {
                continue;
            }
            // Invert the condition (flip its low bit), clear the displacement,
            // and aim it at `y`'s target through a relocation.
            code[x..x + 4].copy_from_slice(&((xi ^ 1) & !(0x7FFFF << 5)).to_le_bytes());
            let target = relocs.remove(ry);
            relocs.push(Reloc {
                offset: x as u64,
                r_type: R_AARCH64_CONDBR19,
                symbol: target.symbol,
                addend: target.addend,
            });
            delete_insn(code, relocs, y);
            continue 'search;
        }
        return;
    }
}

/// `tbz`/`tbnz` reach 32 KiB; a stencil's exits, helpers and jump
/// targets lie in the JIT's cold area, after all the hot code, which grows
/// past that in large functions. Rewrite each one with a relocation, `tbz
/// Rt, #b, sym`, to `tst Rt, #(1 << b); b.eq sym` (`b.ne` for `tbnz`), which
/// reaches 1 MiB.
fn widen_arm64_tbz(code: &mut Vec<u8>, relocs: &mut [Reloc]) {
    while let Some(k) = relocs.iter().position(|r| r.r_type == R_AARCH64_TSTBR14) {
        let at = relocs[k].offset as usize;
        let i = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
        let bit = (i >> 31) << 5 | (i >> 19) & 31;
        let rt = i & 31;
        let tbnz = (i >> 24) & 1 == 1;
        // `ands xzr, xn, #(1 << bit)` (`wzr`, `wn` below 32): a logical
        // immediate of one set bit, rotated right by `size - bit`.
        let tst = if bit >= 32 {
            0xF240_0000 | ((64 - bit) % 64) << 16 | rt << 5 | 31
        } else {
            0x7200_0000 | ((32 - bit) % 32) << 16 | rt << 5 | 31
        };
        code[at..at + 4].copy_from_slice(&tst.to_le_bytes());
        // b.eq / b.ne, the displacement left to the relocation.
        let bcond: u32 = 0x5400_0000 | if tbnz { 1 } else { 0 };
        insert_insn(code, relocs, at + 4, bcond);
        relocs[k].r_type = R_AARCH64_CONDBR19;
        relocs[k].offset = (at + 4) as u64;
    }
}

/// Insert instruction `insn` at `y`, moving later code and relocations
/// down and fixing local branches across it.
fn insert_insn(code: &mut Vec<u8>, relocs: &mut [Reloc], y: usize, insn: u32) {
    for at in (0..code.len()).step_by(4) {
        if relocs.iter().any(|r| r.offset == at as u64) {
            continue;
        }
        let Some((disp, mask, shift)) = local_branch(code, at) else {
            continue;
        };
        let t = (at as i64 + disp) as usize;
        let moved = |p: usize| if p >= y { p + 4 } else { p };
        let new_disp = moved(t) as i64 - moved(at) as i64;
        if new_disp != disp {
            let i = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
            let field = ((new_disp >> 2) as u32 & mask) << shift;
            code[at..at + 4].copy_from_slice(&((i & !(mask << shift)) | field).to_le_bytes());
        }
    }
    for r in relocs.iter_mut().filter(|r| r.offset >= y as u64) {
        r.offset += 4;
    }
    code.splice(y..y, insn.to_le_bytes());
}

/// Delete the instruction at `y`, moving later code and relocations up and
/// fixing local branches across it. Nothing may branch to `y` itself.
fn delete_insn(code: &mut Vec<u8>, relocs: &mut [Reloc], y: usize) {
    for at in (0..code.len()).step_by(4) {
        if at == y || relocs.iter().any(|r| r.offset == at as u64) {
            continue;
        }
        let Some((disp, mask, shift)) = local_branch(code, at) else {
            continue;
        };
        let t = (at as i64 + disp) as usize;
        assert_ne!(t, y, "branch to a deleted instruction");
        let moved = |p: usize| if p > y { p - 4 } else { p };
        let new_disp = moved(t) as i64 - moved(at) as i64;
        if new_disp != disp {
            let i = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
            let field = ((new_disp >> 2) as u32 & mask) << shift;
            code[at..at + 4].copy_from_slice(&((i & !(mask << shift)) | field).to_le_bytes());
        }
    }
    for r in relocs.iter_mut().filter(|r| r.offset > y as u64) {
        r.offset -= 4;
    }
    code.drain(y..y + 4);
}

const R_X86_64_PC32: u32 = 2;
const R_X86_64_PLT32: u32 = 4;

/// A stencil that branches both ways at its end, `jcc cloison_continue; jmp X`,
/// can't have its final jump trimmed: it goes to `X`, not the continuation.
/// LLVM produces it for `if !c { become X(..) } become cloison_continue(..)`,
/// where it keeps the positive comparison and swaps the successors. Rewrite
/// it to `j!cc X; jmp cloison_continue` by flipping the condition (the low bit
/// of the `0F 8x` opcode) and swapping the two relocations: the same branch
/// with its arms exchanged, valid for any condition. Skipped if a local jump
/// targets the final `jmp`.
fn swap_x86_final_branch(code: &mut [u8], relocs: &mut [Reloc]) {
    use verify::x86::{decode, is_rel32_jump};
    let Ok(insns) = decode(code) else { return };
    let [.., jcc, jmp] = &insns[..] else { return };
    if !(is_rel32_jump(code, jcc) && code[jcc.at] == 0x0F && is_rel32_jump(code, jmp) && code[jmp.at] == 0xE9) {
        return;
    }
    let at = |off: usize| relocs.iter().position(|r| r.offset == off as u64);
    let (Some(jcc_r), Some(jmp_r)) = (at(jcc.at + 2), at(jmp.at + 1)) else {
        return;
    };
    if relocs[jcc_r].symbol != "cloison_continue" || relocs[jmp_r].symbol == "cloison_continue" {
        return;
    }
    // Nothing else may jump to the final `jmp`.
    let local = |i: &verify::Insn| !relocs.iter().any(|r| (i.at..i.at + i.len).contains(&(r.offset as usize)));
    if insns.iter().any(|i| local(i) && i.target == Some(jmp.at as i64)) {
        return;
    }
    code[jcc.at + 1] ^= 1;
    let (a, b) = (relocs[jcc_r].symbol.clone(), relocs[jmp_r].symbol.clone());
    relocs[jcc_r].symbol = b;
    relocs[jmp_r].symbol = a;
}

/// x86-64 counterpart of [`fold_arm64_stubs`], for stencils using `asm goto`
/// (immediates folded into `add` / `cmp`, see `stencils/stencils.rs`). A
/// label block that tail-calls becomes a stub at the end, `L: jmp <symbol>`,
/// after the hot path's `jmp cloison_continue`, which then can't be trimmed. The
/// asm jumps to it with a forced long form (`{disp32} jo L`), so retarget
/// that `rel32` straight at `<symbol>` with a new relocation and drop the
/// stub.
///
/// On decoded instructions ([`verify::x86::decode`]): it folds a stub
/// reached by `jcc rel32` / `jmp rel32` alone, not fallen into; a short
/// jump to it (whose field can't reach a symbol) leaves it. LLVM emits
/// long local jumps to such stubs only where told to: the `asm goto`
/// sites, and the stencils assembled with `-mc-relax-all` (see `main`).
fn fold_x86_stubs(code: &mut Vec<u8>, relocs: &mut Vec<Reloc>) {
    use verify::x86::{decode, is_rel32_jump};
    let pc32 = |r: &Reloc| matches!(r.r_type, R_X86_64_PC32 | R_X86_64_PLT32);
    loop {
        let Ok(insns) = decode(code) else { return };
        let [.., before, stub] = &insns[..] else { return };
        if !(is_rel32_jump(code, stub) && code[stub.at] == 0xE9) {
            return;
        }
        let Some(sr) = relocs
            .iter()
            .position(|r| r.offset == (stub.at + 1) as u64 && pc32(r) && r.symbol != "cloison_continue")
        else {
            return;
        };
        // Nothing may fall into the stub: the instruction before it must be
        // an unconditional `jmp rel32` to a symbol (the hot path's `jmp
        // cloison_continue`, or another stub's).
        let jumps_away = code[before.at] == 0xE9
            && is_rel32_jump(code, before)
            && relocs.iter().any(|r| r.offset == (before.at + 1) as u64 && pc32(r));
        if !jumps_away {
            return;
        }
        // Every local jump to the stub, each of which must have a rel32
        // field to take the relocation (a short one can't reach a symbol).
        let local = |i: &verify::Insn| !relocs.iter().any(|r| (i.at..i.at + i.len).contains(&(r.offset as usize)));
        let users: Vec<&verify::Insn> = insns
            .iter()
            .filter(|i| local(i) && i.target == Some(stub.at as i64))
            .collect();
        if users.is_empty() || !users.iter().all(|i| is_rel32_jump(code, i)) {
            return;
        }
        let target = relocs.remove(sr);
        for i in users {
            relocs.push(Reloc {
                offset: (i.at + i.len - 4) as u64,
                r_type: target.r_type,
                symbol: target.symbol.clone(),
                addend: target.addend,
            });
        }
        code.truncate(stub.at);
    }
}
