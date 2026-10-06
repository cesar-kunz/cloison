//! toi — a JavaScript interpreter in Rust.
//!
//! The core of the design is a threaded dispatcher: every opcode handler is an
//! `extern "rust-preserve-none"` function that ends by tail-calling (`become`)
//! the handler of the next opcode. No central loop, no `match`, no stack
//! growth, and — thanks to `preserve_none` — no callee-saved register spills in
//! handler prologues.

#![feature(explicit_tail_calls)]
#![feature(rust_preserve_none_cc)]
#![allow(incomplete_features)]

pub mod bytecode;
pub mod interp;
