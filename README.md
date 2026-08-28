# Brutli

An idiomatic Brotli implementation.

Brutli is a ground-up implementation of the Brotli format based on the specification rather than a translation of the reference C implementation.

> **Status:** the RFC 7932 decoder is functional and under compatibility and performance validation. Brutli is not yet ready for production use.

## Current decoder

- Incremental buffer-to-buffer decoding with explicit consumed/produced counts.
- One-shot bounded decompression.
- `std::io::Read` adapter over `BufRead` without swallowing bytes after the Brotli stream.
- Configurable decoded-output and Brotli window limits.
- Static dictionary support, including all RFC 7932 transforms.
- Differential tests across reference encoder quality and window settings.
- Malformed/truncated-input tests and coverage-guided fuzzing.
- Stable Rust 1.98 and Rust 2024 Edition.
- No mandatory third-party dependencies.

### One-shot

```rust
let compressed = /* Brotli bytes */;
let decoded = brutli::decompress(compressed, 16 * 1024 * 1024)?;
# Ok::<(), brutli::DecodeError>(())
```

The second argument is the maximum allowed decoded size. Trailing bytes after the Brotli stream are rejected.

### Incremental

```rust
use brutli::{DecodeStatus, Decoder};

let mut decoder = Decoder::with_limits(Some(16 * 1024 * 1024), 22);
let mut input = /* Brotli bytes */;
let mut output = [0_u8; 8192];

loop {
    let progress = decoder.process(input, &mut output)?;
    input = &input[progress.consumed..];
    // Consume output[..progress.produced].

    match progress.status {
        DecodeStatus::NeedInput => {
            // Supply more input, or call decoder.finish(...) at EOF.
            break;
        }
        DecodeStatus::NeedOutput => {}
        DecodeStatus::Done => break,
    }
}
# Ok::<(), brutli::DecodeError>(())
```

RFC 7932 defines `WBITS` from 10 through 24. The window limit is checked immediately after the stream header, before history growth.

### `std::io`

`DecoderReader<R>` implements `Read` for `R: BufRead`:

```rust
use std::io::{Cursor, Read};

let source = Cursor::new(/* Brotli bytes */);
let mut reader = brutli::DecoderReader::new(source);
let mut decoded = Vec::new();
reader.read_to_end(&mut decoded)?;
# Ok::<(), std::io::Error>(())
```

Using `BufRead` lets the adapter stop exactly at the end of the first Brotli stream while preserving already-buffered trailing bytes in the underlying reader.

## Goals

- Correct RFC 7932 decoding.
- Idiomatic stable Rust and Rust 2024 Edition.
- A low-overhead buffer-to-buffer streaming core independent of `std::io`.
- Safe public APIs, with any future `unsafe` restricted to small, measured optimization kernels with documented invariants and portable fallbacks.
- Minimal mandatory dependencies and allocation-conscious hot paths.
- Differential and fuzz testing against mature Brotli implementations.
- Performance competitive with `google/brotli` and `rust-brotli` without inheriting C-shaped architecture.
- An architecture that can grow into the RFC 9841 Brotli extensions without redesigning the core.

## Validation

The decoder currently includes:

- reference-produced compatibility fixtures,
- a generated differential matrix covering qualities 0 through 11 and multiple window sizes,
- deterministic malformed-input and truncation tests,
- single-bit mutation tests,
- a `cargo-fuzz` libFuzzer target with CI smoke runs and scheduled longer runs.

Run the normal validation with:

```text
cargo test --all-features
cargo clippy --all-targets --all-features
```

## Benchmarks

The initial decoder benchmark compares Brutli with `rust-brotli` on text, highly repetitive data, and incompressible binary data:

```text
cargo bench --bench decode
```

Benchmarks use Divan and report decoded-byte throughput. Google Brotli comparison and profile-guided optimization follow after the Rust baseline is established.

## Non-goals for the initial decoder

- API compatibility with `rust-brotli`.
- Async-runtime-specific APIs. Async adapters can drive the same streaming core externally.
- `no_std` as an initial compatibility promise. The core should avoid unnecessary `std` coupling so this can be evaluated later.
- SIMD before profiling demonstrates a useful target.

## Plan

1. Finish RFC 7932 compatibility and adversarial validation.
2. Establish decoder performance and allocation baselines against `rust-brotli` and Google Brotli.
3. Optimize measured hot paths while preserving a portable scalar implementation.
4. Build the encoder, starting with correctness and then improving ratio and throughput.
5. Add RFC 9841 functionality after RFC 7932 compatibility is mature.

See [DESIGN.md](DESIGN.md) for architecture and implementation constraints.

## License

MIT
