use std::io::Read;
use std::sync::LazyLock;

use divan::counter::BytesCount;

struct Case {
    source: Vec<u8>,
    compressed: Vec<u8>,
}

impl Case {
    fn new(source: Vec<u8>) -> Self {
        let mut encoder = brotli::CompressorReader::new(source.as_slice(), 4096, 5, 22);
        let mut compressed = Vec::new();
        encoder.read_to_end(&mut compressed).unwrap();

        let decoded = brutli::decompress(&compressed, source.len()).unwrap();
        assert_eq!(decoded, source, "Brutli benchmark fixture failed validation");

        let mut reader = brotli::Decompressor::new(compressed.as_slice(), 4096);
        let mut decoded = Vec::with_capacity(source.len());
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, source, "rust-brotli reader fixture failed validation");

        let mut input = compressed.as_slice();
        let mut decoded = Vec::with_capacity(source.len());
        brotli::BrotliDecompress(&mut input, &mut decoded).unwrap();
        assert_eq!(
            decoded, source,
            "rust-brotli stream-copy fixture failed validation"
        );

        Self { source, compressed }
    }
}

static TEXT: LazyLock<Case> = LazyLock::new(|| {
    Case::new(
        b"Brotli combines a modern LZ77 variant with Huffman coding and a static dictionary. "
            .repeat(1024),
    )
});

static REPETITIVE: LazyLock<Case> =
    LazyLock::new(|| Case::new(b"abc123abc123abc123abc123".repeat(4096)));

static BINARY: LazyLock<Case> = LazyLock::new(|| {
    let mut source = Vec::with_capacity(64 * 1024);
    let mut state = 0x243f_6a88_u32;
    for _ in 0..source.capacity() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        source.push(state as u8);
    }
    Case::new(source)
});

fn main() {
    divan::main();
}

fn bench_brutli_one_shot(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| {
            brutli::decompress(divan::black_box(&case.compressed), case.source.len()).unwrap()
        });
}

fn bench_brutli_reader(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| {
            let mut decoder = brutli::DecoderReader::new(divan::black_box(case.compressed.as_slice()));
            let mut output = Vec::with_capacity(case.source.len());
            decoder.read_to_end(&mut output).unwrap();
            output
        });
}

fn bench_rust_brotli_reader(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| {
            let mut decoder =
                brotli::Decompressor::new(divan::black_box(case.compressed.as_slice()), 4096);
            let mut output = Vec::with_capacity(case.source.len());
            decoder.read_to_end(&mut output).unwrap();
            output
        });
}

fn bench_rust_brotli_stream_copy(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| {
            let mut input = divan::black_box(case.compressed.as_slice());
            let mut output = Vec::with_capacity(case.source.len());
            brotli::BrotliDecompress(&mut input, &mut output).unwrap();
            output
        });
}

macro_rules! decode_benches {
    ($name:ident, $case:ident) => {
        mod $name {
            use super::*;

            #[divan::bench]
            fn brutli_one_shot(bencher: divan::Bencher<'_, '_>) {
                bench_brutli_one_shot(bencher, &$case);
            }

            #[divan::bench]
            fn brutli_reader(bencher: divan::Bencher<'_, '_>) {
                bench_brutli_reader(bencher, &$case);
            }

            #[divan::bench]
            fn rust_brotli_reader(bencher: divan::Bencher<'_, '_>) {
                bench_rust_brotli_reader(bencher, &$case);
            }

            #[divan::bench]
            fn rust_brotli_stream_copy(bencher: divan::Bencher<'_, '_>) {
                bench_rust_brotli_stream_copy(bencher, &$case);
            }
        }
    };
}

decode_benches!(text, TEXT);
decode_benches!(repetitive, REPETITIVE);
decode_benches!(binary, BINARY);
