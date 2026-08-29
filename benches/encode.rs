use std::fmt::Write as _;
use std::io::Read;
use std::sync::LazyLock;

use divan::counter::BytesCount;

#[cfg(feature = "current-google-reference")]
const GOOGLE_REFERENCE: &str = "google/brotli@2ff28fb62deeb8c49720acf2c16ecc8f6f7408f1";
#[cfg(not(feature = "current-google-reference"))]
const GOOGLE_REFERENCE: &str = "brotli-sys 0.3.2 (legacy bundled C reference)";

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

static HTML: LazyLock<Case> = LazyLock::new(|| Case::new(generated_html()));
static JSON: LazyLock<Case> = LazyLock::new(|| Case::new(generated_json()));
static JAVASCRIPT: LazyLock<Case> = LazyLock::new(|| Case::new(generated_javascript()));
static STRUCTURED_BINARY: LazyLock<Case> =
    LazyLock::new(|| Case::new(generated_structured_binary()));

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
    println!("google reference: {GOOGLE_REFERENCE}");
    print_ratio("text", &TEXT);
    print_ratio("repetitive", &REPETITIVE);
    print_ratio("html", &HTML);
    print_ratio("json", &JSON);
    print_ratio("javascript", &JAVASCRIPT);
    print_ratio("structured_binary", &STRUCTURED_BINARY);
    print_ratio("binary", &BINARY);
    divan::main();
}

fn generated_html() -> Vec<u8> {
    let mut source = String::with_capacity(96 * 1024);
    source.push_str("<!doctype html><html><head><meta charset=\"utf-8\"><title>Brutli benchmark</title></head><body><main>\n");
    for section in 0..512 {
        let class = section % 7;
        let score = (section * 17) % 1000;
        writeln!(
            source,
            "<section class=\"card group-{class}\" data-index=\"{section}\"><h2>Benchmark item {section}</h2><p>This generated document contains repeated HTML structure with changing identifiers, labels, and values.</p><a href=\"/items/{section}?score={score}\">Open item</a></section>"
        )
        .unwrap();
    }
    source.push_str("</main></body></html>");
    source.into_bytes()
}

fn generated_json() -> Vec<u8> {
    let mut source = String::with_capacity(96 * 1024);
    source.push_str("{\"version\":1,\"items\":[");
    for item in 0..768 {
        if item != 0 {
            source.push(',');
        }
        let group = item % 11;
        let active = item % 3 != 0;
        let score = (item * 7919) % 100_000;
        write!(
            source,
            "{{\"id\":{item},\"group\":\"group-{group}\",\"active\":{active},\"score\":{score},\"name\":\"generated benchmark item {item}\",\"tags\":[\"brotli\",\"rust\",\"group-{group}\"]}}"
        )
        .unwrap();
    }
    source.push_str("]}");
    source.into_bytes()
}

fn generated_javascript() -> Vec<u8> {
    let mut source = String::with_capacity(96 * 1024);
    source.push_str("export const records = [];\n");
    for item in 0..640 {
        let group = item % 13;
        writeln!(
            source,
            "records.push({{id:{item}, group:'group-{group}', value:{}, label:'generated-item-{item}'}});",
            item * 31
        )
        .unwrap();
        writeln!(
            source,
            "if (records[{item}].value % 2 === 0) {{ records[{item}].label = records[{item}].label.toUpperCase(); }}"
        )
        .unwrap();
    }
    source.push_str(
        "export function total(){ return records.reduce((sum, item) => sum + item.value, 0); }\n",
    );
    source.into_bytes()
}

fn generated_structured_binary() -> Vec<u8> {
    let mut source = Vec::with_capacity(96 * 1024);
    let mut state = 0x9e37_79b9_u32;
    for record in 0_u32..2048 {
        source.extend_from_slice(b"BRUT");
        source.extend_from_slice(&record.to_le_bytes());
        source.extend_from_slice(&(record % 17).to_le_bytes());
        source.extend_from_slice(&[0, 0, 1, 0, 0, 0, 0, 1]);
        for _ in 0..24 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            source.push((state & 0xff) as u8);
        }
    }
    source
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

#[cfg(not(feature = "current-google-reference"))]
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

#[cfg(feature = "current-google-reference")]
fn google_brotli_compress(source: &[u8], quality: u32) -> Vec<u8> {
    current_google::compress(source, quality)
}

#[cfg(feature = "current-google-reference")]
mod current_google {
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

    pub(super) fn compress(source: &[u8], quality: u32) -> Vec<u8> {
        let quality = i32::try_from(quality).expect("Brotli quality fits c_int");
        let capacity = unsafe {
            // SAFETY: The function only computes an upper bound from the supplied size.
            BrotliEncoderMaxCompressedSize(source.len())
        };
        let mut compressed = vec![0_u8; capacity];
        let mut compressed_size = capacity;
        let result = unsafe {
            // SAFETY: The input pointer is valid for `source.len()` bytes. The output
            // pointer is valid for `capacity` bytes and `compressed_size` supplies
            // that capacity to the pinned Google Brotli encoder.
            BrotliEncoderCompress(
                quality,
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
encode_benches!(html, HTML);
encode_benches!(json, JSON);
encode_benches!(javascript, JAVASCRIPT);
encode_benches!(structured_binary, STRUCTURED_BINARY);
encode_benches!(binary, BINARY);
