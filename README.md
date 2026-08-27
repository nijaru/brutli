# Brutli

An idiomatic, high-performance Brotli implementation in Rust.

Brutli is a ground-up implementation of the Brotli format based on the specification rather than a translation of the reference C implementation. The initial goal is a complete, robust RFC 7932 decoder with a clean streaming core; encoding follows once the decoder is correct and competitive.

> **Status:** early development. Brutli is not yet ready for production use.

## Goals

- Correct RFC 7932 decoding.
- Idiomatic stable Rust and Rust 2024 Edition.
- A low-overhead buffer-to-buffer streaming core that does not depend on `std::io`.
- Safe public APIs, with any future `unsafe` restricted to small, measured optimization kernels with documented invariants and portable fallbacks.
- Minimal mandatory dependencies and allocation-conscious hot paths.
- Differential and fuzz testing against mature Brotli implementations.
- Performance competitive with `google/brotli` and `rust-brotli` without inheriting C-shaped architecture.
- An architecture that can grow into the RFC 9841 Brotli extensions without redesigning the core.

## Non-goals for the initial decoder

- API compatibility with `rust-brotli`.
- Async-runtime-specific APIs. Async adapters can drive the same streaming core externally.
- `no_std` as an initial compatibility promise. The core should avoid unnecessary `std` coupling so this can be evaluated later.
- SIMD before profiling demonstrates a useful target.

## Plan

1. Build and validate the scalar streaming decoder.
2. Establish differential, malformed-input, corpus, and fuzz tests.
3. Benchmark against `google/brotli` and `rust-brotli`.
4. Optimize measured hot paths while preserving a portable scalar implementation.
5. Build the encoder, starting with correctness and then improving ratio and throughput.
6. Add RFC 9841 functionality after RFC 7932 compatibility is mature.

See [DESIGN.md](DESIGN.md) for architecture and implementation constraints.

## License

MIT
