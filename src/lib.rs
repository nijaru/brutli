//! Brutli is an idiomatic, high-performance Brotli implementation in Rust.
//!
//! The initial implementation target is an incremental RFC 7932 decoder.
//! Public API surface will remain deliberately small until the decoder state
//! machine and its error semantics are proven.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod decode;

/// Drives the internal decoder with bounded resources for coverage-guided fuzzing.
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn fuzz_decode(input: &[u8]) {
    decode::fuzz_decode(input);
}
