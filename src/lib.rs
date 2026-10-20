//! cloison — a JavaScript interpreter in Rust.
//!
//! The core of the design is a threaded dispatcher: every opcode handler is an
//! `extern "rust-preserve-none"` function that ends by tail-calling (`become`)
//! the handler of the next opcode. No central loop, no `match`, no stack
//! growth, and — thanks to `preserve_none` — no callee-saved register spills in
//! handler prologues.

#![feature(explicit_tail_calls)]
#![feature(rust_preserve_none_cc)]
#![feature(fn_align)]
#![allow(incomplete_features)]

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("cloison supports x86_64 and aarch64 only (dispatch is tuned per architecture)");

pub mod asm;
mod builtins;
mod array;
mod intrinsics;
mod symbol;
mod iterator;
mod typed;
mod dispose;
mod base64;
mod bigint;
mod reflect;
mod generator;
mod collections;
mod date;
mod json;
mod promise;
mod proxy;
pub mod props;
pub mod regexp;
mod unicode_props;
mod unicode_strings;
mod stack;
pub mod bytecode;
pub mod gc;
pub mod heap;
pub mod interp;
pub mod jit;
pub mod js;
pub mod math;
pub mod object;
pub mod value;
pub mod vm;
