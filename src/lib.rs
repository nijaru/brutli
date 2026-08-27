//! Brutli is an idiomatic, high-performance Brotli implementation in Rust.
//!
//! The initial implementation target is an incremental RFC 7932 decoder.
//! Public API surface will remain deliberately small until the decoder state
//! machine and its error semantics are proven.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod decode;
