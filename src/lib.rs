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
// Clippy's style lints this code departs from on purpose, so that
// `cargo clippy --all-targets` reports only what deserves a look:
// - index loops whose bodies need `&mut self` too (the collector's roots),
//   or index two slices at once (bignum and fdlibm arithmetic);
// - the front end's and the JIT's helpers taking many arguments, and
//   tuple types spelled out where they are used;
// - `props`'s `to_*`/`from_*` names for conversions of descriptors that
//   consume or don't take `self` as the convention expects;
// - nested `if`s and identical branches kept apart for their comments or
//   for the spec's steps they follow;
// - `(&(*p).field).len()`: the reference through a raw pointer written
//   out, as rustc's `dangerous_implicit_autorefs` asks.
#![allow(
    clippy::needless_borrow,
    clippy::needless_range_loop,
    clippy::explicit_counter_loop,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::wrong_self_convention,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::if_same_then_else
)]

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("cloison supports x86_64 and aarch64 only (dispatch is tuned per architecture)");

pub mod asm;
mod builtins;
mod array;
pub mod far;
mod intrinsics;
mod symbol;
mod iterator;
mod typed;
mod shared;
mod racy;
// (Some of it only the stencils use.)
#[allow(dead_code)]
mod layout;
// The checks `build.rs` makes of the stencils, for their tests.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../build/verify.rs"]
mod stencil_verify;
mod agent;
mod temporal;
#[cfg(feature = "intl")]
mod intl;
mod dispose;
mod module;
mod base64;
mod bigint;
mod reflect;
mod generator;
mod collections;
mod date;
mod json;
mod promise;
mod proxy;
mod realm;
mod shadow;
pub mod props;
pub mod regexp;
mod normalize;
mod unicode_norm;
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
