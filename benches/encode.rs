use std::io::Read;
use std::sync::LazyLock;

use divan::counter::BytesCount;

struct Case {
    source: Vec<u8>,
}

impl Case {
    fn new(source: Vec<u8>) -> Self {
        let brutli = brutli::compress(&source);
        validate(&source, &brutli, "Brutli");

        for quality in [1, 5] {
            let rust = rust_brotli_compress(&source, quality);
            validate(&source, &rust, "rust-brotli");

            let google = google_brotli_compress(&source, quality);
            validate(&source, &google, "Google Brotli");
        }

        Self { source }
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
    print_ratio("text", &TEXT);
    print_ratio("repetitive", &REPETITIVE);
    print_ratio("binary", &BINARY);
    divan::main();
}

fn validate(source: &[u8], compressed: &[u8], encoder: &str) {
    let decoded = brutli::decompress(compressed, source.len())
        .unwrap_or_else(|error| panic!("{encoder} benchmark fixture did not decode: {error}"));
    assert_eq!(decoded, source, "{encoder} benchmark fixture mismatch");
}

fn print_ratio(name: &str, case: &Case) {
    let brutli = brutli::compress(&case.source);
    let rust_q1 = rust_brotli_compress(&case.source, 1);
    let rust_q5 = rust_brotli_compress(&case.source, 5);
    let google_q1 = google_brotli_compress(&case.source, 1);
    let google_q5 = google_brotli_compress(&case.source, 5);

    println!(
        "ratio {name}: source={} brutli={} rust_q1={} rust_q5={} google_q1={} google_q5={}",
        case.source.len(),
        brutli.len(),
        rust_q1.len(),
        rust_q5.len(),
        google_q1.len(),
        google_q5.len(),
    );
}

fn rust_brotli_compress(source: &[u8], quality: u32) -> Vec<u8> {
    let mut encoder = brotli::CompressorReader::new(source, 4096, quality, 22);
    let mut compressed = Vec::new();
    encoder.read_to_end(&mut compressed).unwrap();
    compressed
}

fn google_brotli_compress(source: &[u8], quality: u32) -> Vec<u8> {
    let quality = i32::try_from(quality).expect("Brotli quality fits c_int");
    let capacity = unsafe {
        // SAFETY: The function only computes an upper bound from the supplied size.
        brotli_sys::BrotliEncoderMaxCompressedSize(source.len())
    };
    let mut compressed = vec![0_u8; capacity];
    let mut compressed_size = capacity;
    let result = unsafe {
        // SAFETY: The input pointer is valid for `source.len()` bytes. The output
        // pointer is valid for `capacity` bytes and `compressed_size` supplies
        // that capacity to the encoder.
        brotli_sys::BrotliEncoderCompress(
            quality,
            22,
            brotli_sys::BROTLI_MODE_GENERIC,
            source.len(),
            source.as_ptr(),
            &mut compressed_size,
            compressed.as_mut_ptr(),
        )
    };
    assert_ne!(result, 0, "Google Brotli encoder failed");
    compressed.truncate(compressed_size);
    compressed
}

fn bench_brutli(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| brutli::compress(divan::black_box(case.source.as_slice())));
}

fn bench_rust_brotli(bencher: divan::Bencher<'_, '_>, case: &'static Case, quality: u32) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| rust_brotli_compress(divan::black_box(case.source.as_slice()), quality));
}

fn bench_google_brotli(bencher: divan::Bencher<'_, '_>, case: &'static Case, quality: u32) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| google_brotli_compress(divan::black_box(case.source.as_slice()), quality));
}

macro_rules! encode_benches {
    ($name:ident, $case:ident) => {
        mod $name {
            use super::*;

            #[divan::bench]
            fn brutli(bencher: divan::Bencher<'_, '_>) {
                bench_brutli(bencher, &$case);
            }

            #[divan::bench]
            fn rust_brotli_q1(bencher: divan::Bencher<'_, '_>) {
                bench_rust_brotli(bencher, &$case, 1);
            }

            #[divan::bench]
            fn rust_brotli_q5(bencher: divan::Bencher<'_, '_>) {
                bench_rust_brotli(bencher, &$case, 5);
            }

            #[divan::bench]
            fn google_brotli_q1(bencher: divan::Bencher<'_, '_>) {
                bench_google_brotli(bencher, &$case, 1);
            }

            #[divan::bench]
            fn google_brotli_q5(bencher: divan::Bencher<'_, '_>) {
                bench_google_brotli(bencher, &$case, 5);
            }
        }
    };
}

encode_benches!(text, TEXT);
encode_benches!(repetitive, REPETITIVE);
encode_benches!(binary, BINARY);
