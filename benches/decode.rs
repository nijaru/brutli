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

fn bench_brutli(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| {
            brutli::decompress(divan::black_box(&case.compressed), case.source.len()).unwrap()
        });
}

fn bench_rust_brotli(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
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

#[divan::bench]
fn brutli_text(bencher: divan::Bencher<'_, '_>) {
    bench_brutli(bencher, &TEXT);
}

#[divan::bench]
fn rust_brotli_text(bencher: divan::Bencher<'_, '_>) {
    bench_rust_brotli(bencher, &TEXT);
}

#[divan::bench]
fn brutli_repetitive(bencher: divan::Bencher<'_, '_>) {
    bench_brutli(bencher, &REPETITIVE);
}

#[divan::bench]
fn rust_brotli_repetitive(bencher: divan::Bencher<'_, '_>) {
    bench_rust_brotli(bencher, &REPETITIVE);
}

#[divan::bench]
fn brutli_binary(bencher: divan::Bencher<'_, '_>) {
    bench_brutli(bencher, &BINARY);
}

#[divan::bench]
fn rust_brotli_binary(bencher: divan::Bencher<'_, '_>) {
    bench_rust_brotli(bencher, &BINARY);
}
