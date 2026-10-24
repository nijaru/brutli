# brutli

A from-scratch Brotli implementation in Rust, targeting RFC 7932.

**Status:** the decoder is functional and the encoder supports qualities
`0..=11` with compressed and stored streams. The encoder is still under
compatibility and corpus validation. Brutli is not ready for production use,
does not implement RFC 9841, and does not aim for API compatibility with
`rust-brotli`.

## Install

The crate is not published. Depend on the Git repository (or a local path):

```toml
[dependencies]
brutli = { git = "https://github.com/nijaru/brutli" }
```

## Decoding

The one-shot API requires a maximum decoded size and rejects trailing bytes
after the Brotli stream:

```rust
let compressed: &[u8] = /* Brotli bytes */;
let decoded = brutli::decompress(compressed, 16 * 1024 * 1024)?;
# Ok::<(), brutli::DecodeError>(())
```

For incremental decoding, `Decoder` exposes output/window limits and explicit
`process` / `finish` calls:

```rust
use brutli::{DecodeStatus, Decoder};

let mut decoder = Decoder::with_limits(Some(16 * 1024 * 1024), 22);
let mut output = [0_u8; 8192];
let progress = decoder.process(compressed, &mut output)?;
# let _ = (progress, DecodeStatus::NeedInput);
# Ok::<(), brutli::DecodeError>(())
```

`Decoder::with_output_limit` and `Decoder::with_max_window_bits` configure one
limit at a time. The window limit is checked before history growth.

`DecoderReader<R>` implements `std::io::Read` for `R: BufRead`. It stops at the
end of the first Brotli stream and preserves buffered trailing bytes:

```rust
use std::io::{Cursor, Read};

let mut reader = brutli::DecoderReader::new(Cursor::new(compressed));
let mut decoded = Vec::new();
reader.read_to_end(&mut decoded)?;
# Ok::<(), std::io::Error>(())
```

## Encoding

The default one-shot encoder uses quality 5 and `WBITS=22`:

```rust
let compressed = brutli::compress(b"Brotli data");
let compressed = brutli::compress_with_quality(b"Brotli data", 9)?;
# Ok::<(), brutli::EncodeError>(())
```

`compress_with_options` accepts `EncoderOptions`:

```rust
use brutli::{EncoderMode, EncoderOptions, compress_with_options};

let compressed = compress_with_options(
    b"Brotli data",
    EncoderOptions {
        quality: 9,
        window_bits: 22,
        mode: EncoderMode::Generic,
    },
)?;
# Ok::<(), brutli::EncodeError>(())
```

For streaming, `Encoder` provides `process` and `finish` with the same
bounded-output model as `Decoder`. `EncoderOptions` validates quality and
window bits (`10..=24`).

Quality levels `10` and `11` do not yet use Zopfli-style optimal parsing.
`EncoderMode::Font` selects Brotli's font distance parameters; other modes
currently share the generic encoder path.

## Validation

```sh
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

The test suite includes reference differential tests, malformed and truncated
input cases, history-window wrap tests, and encoder interoperability checks. A
C CLI round-trip test runs when the `brotli` executable is installed; CI
installs it. Fuzz targets for decoding and encoder round trips are in `fuzz/`.

## Benchmarks

Decoder and encoder benchmarks compare Brutli with `rust-brotli` by default:

```sh
cargo bench --bench decode
cargo bench --bench encode
```

Enable `current-google-reference` to add the pinned Google Brotli C
implementation. That feature requires linking a local Google Brotli build; the
benchmark workflow builds the pinned commit automatically. On Linux,
`scripts/profile-decode.sh` profiles the decoder, with an optional Google
comparator via `GOOGLE_BROTLI_BUILD`.

## License

MIT
