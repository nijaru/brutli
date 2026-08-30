use std::fs;
use std::sync::LazyLock;

use divan::counter::BytesCount;

const GOOGLE_REFERENCE: &str = "google/brotli@2ff28fb62deeb8c49720acf2c16ecc8f6f7408f1";
const GOOGLE_TESTDATA: &str = "google-brotli/tests/testdata";

struct Case {
    source: Vec<u8>,
}

impl Case {
    fn from_file(name: &str) -> Self {
        let source = fs::read(format!("{GOOGLE_TESTDATA}/{name}"))
            .unwrap_or_else(|error| panic!("failed to read {name}: {error}"));
        Self::new(source)
    }

    fn from_file_prefix(name: &str, bytes: usize) -> Self {
        let mut source = fs::read(format!("{GOOGLE_TESTDATA}/{name}"))
            .unwrap_or_else(|error| panic!("failed to read {name}: {error}"));
        source.truncate(bytes);
        Self::new(source)
    }

    fn new(source: Vec<u8>) -> Self {
        let brutli = brutli::compress(&source);
        validate(&source, &brutli, "Brutli");
        let google = google_brotli_compress(&source);
        validate(&source, &google, "Google Brotli");
        Self { source }
    }
}

static ALICE: LazyLock<Case> = LazyLock::new(|| Case::from_file("alice29.txt"));
static AS_YOU_LIKE_IT: LazyLock<Case> = LazyLock::new(|| Case::from_file("asyoulik.txt"));
static LCET10: LazyLock<Case> = LazyLock::new(|| Case::from_file("lcet10.txt"));
static PARADISE_LOST: LazyLock<Case> = LazyLock::new(|| Case::from_file("plrabn12.txt"));
static BINAST: LazyLock<Case> =
    LazyLock::new(|| Case::from_file_prefix("bb.binast", 256 * 1024));

fn main() {
    println!("google reference: {GOOGLE_REFERENCE}");
    print_ratio("alice29", &ALICE);
    print_ratio("asyoulik", &AS_YOU_LIKE_IT);
    print_ratio("lcet10", &LCET10);
    print_ratio("plrabn12", &PARADISE_LOST);
    print_ratio("binast-256k", &BINAST);
    divan::main();
}

fn validate(source: &[u8], compressed: &[u8], encoder: &str) {
    let decoded = brutli::decompress(compressed, source.len())
        .unwrap_or_else(|error| panic!("{encoder} corpus fixture did not decode: {error}"));
    assert_eq!(decoded, source, "{encoder} corpus fixture mismatch");
}

fn print_ratio(name: &str, case: &Case) {
    let brutli = brutli::compress(&case.source);
    let google = google_brotli_compress(&case.source);
    println!(
        "corpus {name}: source={} brutli={} google_q5={}",
        case.source.len(),
        brutli.len(),
        google.len(),
    );
}

fn google_brotli_compress(source: &[u8]) -> Vec<u8> {
    let capacity = unsafe {
        // SAFETY: The function only computes an upper bound from the supplied size.
        BrotliEncoderMaxCompressedSize(source.len())
    };
    let mut compressed = vec![0_u8; capacity];
    let mut compressed_size = capacity;
    let result = unsafe {
        // SAFETY: The input pointer is valid for `source.len()` bytes. The output
        // pointer is valid for `capacity` bytes and `compressed_size` supplies
        // that capacity to the encoder.
        BrotliEncoderCompress(
            5,
            22,
            0,
            source.len(),
            source.as_ptr(),
            &mut compressed_size,
            compressed.as_mut_ptr(),
        )
    };
    assert_ne!(result, 0, "pinned Google Brotli encoder failed");
    compressed.truncate(compressed_size);
    compressed
}

unsafe extern "C" {
    fn BrotliEncoderMaxCompressedSize(input_size: usize) -> usize;
    fn BrotliEncoderCompress(
        quality: i32,
        lgwin: i32,
        mode: i32,
        input_size: usize,
        input_buffer: *const u8,
        encoded_size: *mut usize,
        encoded_buffer: *mut u8,
    ) -> i32;
}

fn bench_brutli(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| brutli::compress(divan::black_box(case.source.as_slice())));
}

fn bench_google(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| google_brotli_compress(divan::black_box(case.source.as_slice())));
}

macro_rules! corpus_benches {
    ($name:ident, $case:ident) => {
        mod $name {
            use super::*;

            #[divan::bench]
            fn brutli(bencher: divan::Bencher<'_, '_>) {
                bench_brutli(bencher, &$case);
            }

            #[divan::bench]
            fn google_brotli_q5(bencher: divan::Bencher<'_, '_>) {
                bench_google(bencher, &$case);
            }
        }
    };
}

corpus_benches!(alice29, ALICE);
corpus_benches!(asyoulik, AS_YOU_LIKE_IT);
corpus_benches!(lcet10, LCET10);
corpus_benches!(plrabn12, PARADISE_LOST);
corpus_benches!(binast_256k, BINAST);
