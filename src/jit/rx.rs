//! Compiled regular expressions: a pattern's program (`regexp::Insn`) as
//! native code, copied from the `rx_*` stencils (at the end of
//! `stencils/stencils.rs`, which describes their machine state) and
//! patched. `regexp` picks the pieces; this lays them out and patches them.

use super::{ExecMem, Instance, Sym, arch};

/// Where a piece continues, or where it branches (a failing one: to
/// `rx_fail`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum To {
    /// The next piece.
    Next,
    Piece(usize),
}

/// A piece's 64-bit operand: a value, or a piece's address.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Imm64 {
    Value(u64),
    Piece(usize),
}

/// One stencil to copy, with its operands.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Piece {
    pub name: &'static str,
    pub imm: u32,
    pub imm64: Imm64,
    pub imm64b: u64,
    pub cont: To,
    pub jump: To,
}

impl Piece {
    pub(crate) fn new(name: &'static str, jump: To) -> Piece {
        Piece {
            name,
            imm: 0,
            imm64: Imm64::Value(0),
            imm64b: 0,
            cont: To::Next,
            jump,
        }
    }
}

pub(crate) use crate::layout::rx::{BAIL, Class, Ctx, Entry, Head, NOMATCH, NONE, Run};

/// Pieces laid out and patched.
pub(crate) struct Code {
    _mem: ExecMem,
    /// Each piece's address.
    pub at: Vec<usize>,
}

impl Code {
    pub(crate) fn assemble(pieces: &[Piece]) -> Code {
        let stencils: Vec<Instance> = pieces.iter().map(|p| Instance::get(p.name)).collect();
        let target = |i: usize, t: To| match t {
            To::Next => i + 1,
            To::Piece(k) => k,
        };
        // A piece continuing at the one after it drops its final jump.
        let lens: Vec<usize> = stencils
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let st = s.stencil();
                let trims = target(i, pieces[i].cont) == i + 1 && arch::ends_with_continue(st.code, st.relocs);
                st.code.len() - if trims { arch::TRAILING_JUMP } else { 0 }
            })
            .collect();
        let mut offs = Vec::with_capacity(pieces.len() + 1);
        let mut pos = 0;
        for &len in &lens {
            offs.push(pos);
            pos += len;
        }
        offs.push(pos);
        let mut mem = ExecMem::new(pos);
        let base = mem.ptr as usize;
        let at: Vec<usize> = offs.iter().map(|o| base + o).collect();
        let buf = mem.bytes();
        for (i, (p, s)) in pieces.iter().zip(&stencils).enumerate() {
            let o = offs[i];
            s.copy_to(&mut buf[o..], lens[i]);
            for r in s.stencil().relocs.iter().filter(|r| (r.0 as usize) < lens[i]) {
                let value = match r.2 {
                    Sym::HoleImm => p.imm as u64,
                    Sym::HoleImm64 => match p.imm64 {
                        Imm64::Value(v) => v,
                        Imm64::Piece(k) => at[k] as u64,
                    },
                    Sym::HoleImm64b => p.imm64b,
                    Sym::Continue => at[target(i, p.cont)] as u64,
                    Sym::JumpTarget => at[target(i, p.jump)] as u64,
                    s => panic!("{}: relocation to {s:?}", p.name),
                };
                let field = o + r.0 as usize;
                arch::patch(&mut buf[field..], r.1 as u32, value, r.3 as i64, (base + field) as u64);
            }
        }
        // Profiling: as the JIT's pieces (`CLOISON_PERF_MAP`).
        if std::env::var_os("CLOISON_PERF_MAP").is_some() {
            use std::io::Write;
            let mut map = String::new();
            for (i, p) in pieces.iter().enumerate() {
                if lens[i] > 0 {
                    map.push_str(&format!("{:x} {:x} jit:{}\n", at[i], lens[i], p.name));
                }
            }
            let path = format!("/tmp/perf-{}.map", std::process::id());
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
            f.write_all(map.as_bytes()).unwrap();
        }
        mem.seal();
        Code { _mem: mem, at }
    }

    pub(crate) fn entry(&self) -> Entry {
        unsafe { std::mem::transmute::<usize, Entry>(self.at[0]) }
    }
}
