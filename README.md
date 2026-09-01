# Brutli

An idiomatic Brotli implementation.

Brutli is a ground-up implementation of the Brotli format based on the specification rather than a translation of the reference C implementation.

> **Status:** the RFC 7932 decoder is functional and the one-shot encoder now produces both compressed and stored streams. Brutli remains under compatibility, corpus, and performance validation and is not yet ready for production use.

## Current decoder

- Incremental buffer-to-buffer decoding with explicit consumed/produced counts.
- One-shot bounded decompression.
- `std::io::Read` adapter over `BufRead` without swallowing bytes after the Brotli stream.
- Configurable decoded-output and Brotli window limits.
- Static dictionary support, including all RFC 7932 transforms.
- Differential tests across reference encoder quality and window settings.
- Large-stream tests that repeatedly wrap small Brotli history windows.
- A deterministic bytewise reference model for bulk history-copy semantics.
- Malformed/truncated-input tests and coverage-guided fuzzing.
- Stable Rust 1.98 and Rust 2024 Edition.
- No mandatory third-party dependencies.

### One-shot decoding

```rust
let compressed = /* Brotli bytes */;
let decoded = brutli::decompress(compressed, 16 * 1024 * 1024)?;
# Ok::<(), brutli::DecodeError>(())
```

The second argument is the maximum allowed decoded size. Trailing bytes after the Brotli stream are rejected.

### Incremental decoding

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

## Current encoder

The public encoder is currently a one-shot API. The default uses `WBITS=22`:

```rust
let compressed = brutli::compress(b"Brotli data");
let smaller_window = brutli::compress_with_window_bits(b"Brotli data", 16)?;
# Ok::<(), brutli::EncodeError>(())
```

`compress_with_window_bits` accepts the RFC 7932 range `10..=24` and returns
`EncodeError` for invalid values.

Its current baseline includes:

- RFC 7932 stream and meta-block framing, including multi-metablock streams
  for inputs larger than the 16 MiB single-metablock cap.
- A bounded two-candidate-per-hash LZ77 matcher with compact `u32` positions.
- One-step lazy matching for short copies using an approximate format-bit cost.
- Frequency-weighted canonical Huffman generation with a valid 15-bit fallback.
- Simple and complex prefix-tree serialization.
- General backward-distance encoding plus Brotli recent-distance short codes.
- Specialized short-period encoding for highly repetitive input.
- Stored-block fallback when compression is not beneficial.
- Round-trip fuzzing and interoperability tests against an independent Brotli decoder.

Quality levels `0..=11` are accepted through `compress_with_quality`, but the current implementation only varies the match-search budget and does not yet match upstream quality-specific strategies. `EncoderMode::Font` selects the upstream font distance parameters (`NPOSTFIX=1`, `NDIRECT=12` at quality 4 and above); other modes currently behave like `Generic`. Inputs above 16 MiB are compressed as a sequence of greedy metablocks, each choosing its compressed or stored form, with the match-finder window and recent-distance state carried across metablock boundaries. Streaming operations remain future RFC 7932 work.

## Goals

- Correct RFC 7932 decoding and encoding.
- Idiomatic stable Rust and Rust 2024 Edition.
- A low-overhead buffer-to-buffer streaming decoder core independent of `std::io`.
- Safe public APIs, with any future `unsafe` restricted to small, measured optimization kernels with documented invariants and portable fallbacks.
- Minimal mandatory dependencies and allocation-conscious hot paths.
- Differential and fuzz testing against mature Brotli implementations.
- Performance competitive with `google/brotli` and `rust-brotli` without inheriting C-shaped architecture.
- An architecture that can grow into the RFC 9841 Brotli extensions without redesigning the core.

## Validation

The project currently includes:

- reference-produced decoder compatibility fixtures,
- a generated differential matrix covering reference qualities 0 through 11 and multiple window sizes,
- multi-megabyte streams exercising repeated history-window wraparound,
- a bytewise reference model for validating optimized ring/history operations,
- deterministic malformed-input and truncation tests,
- single-bit mutation tests,
- external-decoder interoperability tests for Brutli encoder output,
- separate decoder and encoder-round-trip `cargo-fuzz` targets with CI smoke runs and scheduled longer runs.

Run the normal validation with:

```text
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

## Benchmarks

Decoder and encoder benchmarks compare Brutli with the official Google Brotli C implementation and `rust-brotli`:

```text
cargo bench --bench decode
cargo bench --bench encode
```

The decoder harness reports decoded-byte throughput across direct incremental, one-shot, and `Read` APIs. The encoder harness reports both compressed sizes and source-byte throughput for Brutli, Google Brotli, and `rust-brotli` at representative reference quality settings.

Every benchmark fixture is validated for correctness before timing. GitHub-hosted runner results are useful for finding large regressions and obvious hot paths, but they are not treated as publishable performance claims. Broader corpus measurements and controlled local profiling remain the basis for performance and ratio decisions.

On Linux with `perf` installed, the reproducible decoder profiling helper runs CPU-counter comparisons for all three decoder fixtures plus the direct Google/rust-brotli binary comparators, then records a call graph for Brutli's binary path:

```text
bash scripts/profile-decode.sh
```

It writes `perf-brutli-binary.data` and a text report at `perf-brutli-binary.txt`. `PERF_RUNS`, `SAMPLE_COUNT`, `SAMPLE_SIZE`, and `RECORD_SAMPLE_SIZE` can be overridden in the environment when a longer or shorter profile is desired.

## Initial non-goals

- API compatibility with `rust-brotli`.
- Async-runtime-specific APIs. Async adapters can drive the same streaming decoder core externally.
- `no_std` as an initial compatibility promise. The core should avoid unnecessary `std` coupling so this can be evaluated later.
- SIMD before profiling demonstrates a useful target.
- Premature encoder quality controls before the baseline is measured across representative corpora.

## Plan

1. Continue RFC 7932 compatibility and adversarial validation.
2. Broaden encoder ratio/throughput benchmarks beyond synthetic fixtures.
3. Optimize only measured decoder and encoder hot paths.
4. Add encoder quality/ratio features where corpus results justify the complexity.
5. Add RFC 9841 functionality after RFC 7932 compatibility is mature.

See [DESIGN.md](DESIGN.md) for architecture and implementation constraints.

## License

MIT
