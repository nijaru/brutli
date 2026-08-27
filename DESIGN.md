# Brutli design

This document records the initial architecture and constraints for Brutli. The implementation should change when measurements or correctness requirements justify it; these are guardrails, not an attempt to freeze internal structure before profiling.

## Baseline

- Rust 1.98 stable.
- Rust 2024 Edition.
- RFC 7932 is the first compatibility target.
- RFC 9841 support is deferred, but the decoder must not bake in assumptions that make its larger windows, shared dictionaries, or framing difficult to add later.
- The reference implementations are compatibility and performance oracles, not source architectures to translate.

## Design principles

### Spec-driven implementation

Implement the Brotli bitstream from its normative specification. Existing implementations may be used to validate behavior, generate test data, compare performance, and understand edge cases, but Brutli should own its data model, state machine, and algorithms.

### Streaming core first

The fundamental decoder interface should consume an input slice and fill a caller-provided output slice. Decoder state carries across calls.

Conceptually:

```rust
let progress = decoder.process(input, output)?;
```

`progress` should report at least:

- bytes consumed,
- bytes produced,
- whether more input is needed,
- whether more output space is needed,
- whether the stream is complete.

This keeps the codec independent of `std::io`, async runtimes, and allocation policy. One-shot helpers and `std::io::{Read, Write}` adapters can be thin layers over the same state machine.

Do not store caller input or output slice references in the decoder across calls. Persist only the decoder state needed to continue processing, including partially consumed bit state.

### Explicit state, not callback architecture

Represent parsing and decoding phases as explicit state. Avoid callback-heavy control flow, trait objects, and virtual dispatch in hot paths. The state machine must be resumable at any point where input or output is exhausted.

Likely decoder responsibilities include:

- bit input,
- stream/window header parsing,
- metablock parsing,
- block types and context maps,
- prefix/Huffman table construction and decoding,
- literal decoding,
- command and distance decoding,
- backward-copy execution,
- static dictionary lookup and transforms,
- sliding-window history.

These responsibilities do not need one source file each. Split modules when the ownership boundary or optimization boundary is useful, not merely to mirror the specification's section structure.

### Allocation policy

- Reuse decoder-owned buffers and tables across metablocks where practical.
- Keep hot data structures dense and contiguous.
- Avoid per-symbol and per-command allocation.
- Validate attacker-controlled sizes before allocation or indexing.
- Do not add custom allocator abstractions unless a demonstrated use case requires them.
- Do not promise zero allocation where the format fundamentally requires persistent history or dynamic tables; optimize allocation count and reuse instead.

### Safety policy

Start with safe Rust.

`unsafe` is permitted only when all of the following are true:

1. profiling identifies a meaningful hot path,
2. safe code cannot achieve the required result reasonably,
3. the unsafe region is small and isolated,
4. its invariants are documented with `SAFETY` comments,
5. a portable safe implementation remains available where appropriate,
6. differential tests and fuzzing exercise the optimized path.

The public API must remain safe.

### SIMD policy

Do not design around unstable portable SIMD APIs. Start with a scalar implementation that the compiler can optimize well. Add architecture-specific or other stable SIMD implementations only after profiles identify suitable kernels and benchmarks demonstrate a worthwhile gain.

Dispatch should stay internal so callers do not need architecture-specific APIs.

### Standard library and dependencies

The first release targets normal stable Rust with `std`. Keep the algorithmic core free of unnecessary OS and I/O dependencies so `no_std + alloc` can be evaluated later, but do not impose a premature `no_std` compatibility contract.

Prefer no mandatory third-party runtime dependencies initially. Add dependencies only when they provide clear correctness, maintenance, or performance value that outweighs the additional surface area.

## Decoder architecture

The decoder should be incremental and bounded by the input and output buffers supplied to each call. A call must either make observable progress, return a terminal result, or report exactly what resource it needs next.

### Bit reader

The bit reader should:

- follow Brotli's specified bit ordering exactly,
- support incremental input without reading beyond the supplied slice,
- retain only the minimal partial-bit state needed across calls,
- use a refill strategy that can later be optimized without changing higher-level parsing code,
- distinguish incomplete input from malformed format data.

Avoid exposing the bit reader publicly.

### Prefix decoding

Keep table construction separate from symbol decoding. Begin with a simple, auditable scalar representation. Optimize table layout only from profiles and corpus measurements.

Reject oversubscribed, incomplete where forbidden, or otherwise invalid trees according to the format rules before unsafe indexing could become possible.

### History window

Treat the effective window size as an explicit value rather than assuming the RFC 7932 maximum throughout the implementation. This is important for future RFC 9841 large-window support.

The history representation should support overlapping backward copies correctly and efficiently. Optimize common short-distance copies after correctness is established.

### Static dictionary

Keep dictionary addressing and transforms behind an internal boundary. The decoder should validate dictionary references before lookup. The representation can later be tuned for binary size, cache behavior, or compile time without affecting the public API.

## Error model

Expose structured decode errors rather than strings as the primary error contract. Preserve enough distinction for callers and tests to identify malformed input versus incomplete streaming input without exposing internal state-machine details.

Public error enums should be `#[non_exhaustive]` until the error taxonomy is mature.

Running out of the current input buffer is normally streaming control flow, not a malformed-stream error. Truncated input becomes an error only when the caller declares end-of-input and the decoder still requires more bits or bytes.

## Encoder architecture

Do not let the eventual encoder dictate decoder structure unnecessarily. Decoder completion comes first.

The encoder will be developed in stages:

1. valid Brotli output with a simple strategy,
2. baseline match finding and command generation,
3. entropy modeling and context selection,
4. progressively better parsing and quality levels,
5. profile-guided performance work and SIMD where justified.

Encoder output does not need to match another implementation byte-for-byte. Compatibility means standards-compliant streams that decode to the original data; quality and performance are benchmark dimensions.

## Correctness strategy

Correctness precedes optimization.

Use:

- specification-derived unit tests,
- official or established Brotli test corpora where licensing permits redistribution,
- streams generated across reference encoder quality/window settings,
- differential decoding against `google/brotli`,
- differential decoding against `rust-brotli`,
- malformed and truncated input tests,
- property tests where useful,
- coverage-guided fuzzing.

Fuzz properties should include:

- no panics for arbitrary input,
- no out-of-bounds behavior,
- deterministic results,
- agreement with a reference decoder on acceptance/rejection where semantics are comparable,
- successful round trips once the encoder exists.

Keep externally sourced corpus material outside the crate package unless its redistribution terms are explicit.

## Performance strategy

Benchmark before optimizing and preserve representative corpora rather than relying on one synthetic input.

Track at least:

- decompression throughput,
- compression throughput once available,
- compression ratio by quality level,
- small-input latency,
- allocation count and allocated bytes,
- peak decoder memory,
- compile time and binary-size impact when optimization techniques materially affect them.

Compare against:

- `google/brotli`,
- `rust-brotli`.

Where useful, retain both warm-cache microbenchmarks for kernels and end-to-end benchmarks for realistic data.

## API evolution

Do not stabilize a broad public API during early implementation. Keep internals private and expose the smallest surface needed for integration tests. Add convenience APIs only after the streaming state machine is proven.

Before removing `publish = false`, define and test:

- incremental decoder API,
- one-shot decompression API,
- `std::io` adapters,
- error semantics,
- resource limits / defensive decoding controls,
- MSRV policy.

## Security

Treat all compressed input as attacker-controlled.

The decoder must defend against:

- integer overflow,
- invalid shifts and bit counts,
- malformed prefix trees,
- invalid distances,
- invalid dictionary references,
- unbounded allocation requests,
- decompression bombs where caller-configurable limits can mitigate them,
- state-machine loops that consume neither input nor produce output.

Fuzzing and adversarial corpus tests are release requirements, not optional cleanup.
