use std::io::Read;
use std::sync::LazyLock;

use brutli::DecodeStatus;
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
        assert_eq!(
            decoded, source,
            "Brutli benchmark fixture failed validation"
        );

        let mut decoded = vec![0_u8; source.len() + 1];
        let decoded_size = brutli_decode_into(&compressed, &mut decoded);
        assert_eq!(decoded_size, source.len());
        assert_eq!(
            &decoded[..decoded_size],
            source,
            "Brutli direct-slice fixture failed validation"
        );

        let mut decoded = vec![0_u8; source.len() + 1];
        let decoded_size = brutli_decode_into_chunked(&compressed, &mut decoded, 8192);
        assert_eq!(decoded_size, source.len());
        assert_eq!(
            &decoded[..decoded_size],
            source,
            "Brutli 8 KiB direct-slice fixture failed validation"
        );

        let mut reader = brotli::Decompressor::new(compressed.as_slice(), 4096);
        let mut decoded = Vec::with_capacity(source.len());
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(
            decoded, source,
            "rust-brotli reader fixture failed validation"
        );

        let mut input = compressed.as_slice();
        let mut decoded = Vec::with_capacity(source.len());
        brotli::BrotliDecompress(&mut input, &mut decoded).unwrap();
        assert_eq!(
            decoded, source,
            "rust-brotli stream-copy fixture failed validation"
        );

        let mut decoded = vec![0_u8; source.len() + 1];
        let info = brotli_decompressor::brotli_decode(&compressed, &mut decoded);
        let succeeded = matches!(
            info.result,
            brotli_decompressor::BrotliResult::ResultSuccess
        );
        assert!(
            succeeded,
            "rust-brotli direct-slice fixture failed: {:?}",
            info.error_code
        );
        assert_eq!(info.decoded_size, source.len());
        assert_eq!(
            &decoded[..info.decoded_size],
            source,
            "rust-brotli direct-slice fixture failed validation"
        );

        let mut decoded = vec![0_u8; source.len() + 1];
        let decoded_size = google_brotli_decode_into(&compressed, &mut decoded);
        assert_eq!(decoded_size, source.len());
        assert_eq!(
            &decoded[..decoded_size],
            source,
            "Google Brotli direct-slice fixture failed validation"
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

fn brutli_decode_into(input: &[u8], output: &mut [u8]) -> usize {
    brutli_decode_into_chunked(input, output, usize::MAX)
}

fn brutli_decode_into_chunked(input: &[u8], output: &mut [u8], max_chunk: usize) -> usize {
    assert!(max_chunk != 0);

    let mut decoder = brutli::Decoder::new();
    let mut input_offset = 0;
    let mut output_offset = 0;
    let mut finishing = false;

    loop {
        let output_end = output_offset.saturating_add(max_chunk).min(output.len());
        let output_chunk = &mut output[output_offset..output_end];
        let progress = if finishing {
            decoder.finish(output_chunk).unwrap()
        } else {
            decoder
                .process(&input[input_offset..], output_chunk)
                .unwrap()
        };

        if !finishing {
            input_offset += progress.consumed;
        }
        output_offset += progress.produced;

        match progress.status {
            DecodeStatus::Done => {
                assert_eq!(input_offset, input.len());
                return output_offset;
            }
            DecodeStatus::NeedInput => {
                assert_eq!(input_offset, input.len());
                finishing = true;
            }
            DecodeStatus::NeedOutput => {
                assert!(
                    output_offset < output.len(),
                    "direct-slice output buffer exhausted"
                );
            }
        }
    }
}

fn bench_brutli_one_shot(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| {
            brutli::decompress(divan::black_box(&case.compressed), case.source.len()).unwrap()
        });
}

fn bench_brutli_direct(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    let mut output = vec![0_u8; case.source.len() + 1];
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench_local(move || {
            let decoded_size = brutli_decode_into(
                divan::black_box(&case.compressed),
                divan::black_box(&mut output),
            );
            divan::black_box(decoded_size)
        });
}

fn bench_brutli_direct_8k(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    let mut output = vec![0_u8; case.source.len() + 1];
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench_local(move || {
            let decoded_size = brutli_decode_into_chunked(
                divan::black_box(&case.compressed),
                divan::black_box(&mut output),
                8192,
            );
            divan::black_box(decoded_size)
        });
}

fn bench_brutli_reader(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench(|| {
            let mut decoder =
                brutli::DecoderReader::new(divan::black_box(case.compressed.as_slice()));
            let mut output = Vec::with_capacity(case.source.len());
            decoder.read_to_end(&mut output).unwrap();
            output
        });
}

fn bench_rust_brotli_direct(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    let mut output = vec![0_u8; case.source.len() + 1];
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench_local(move || {
            let info = brotli_decompressor::brotli_decode(
                divan::black_box(&case.compressed),
                divan::black_box(&mut output),
            );
            divan::black_box(info.decoded_size)
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

fn google_brotli_decode_into(input: &[u8], output: &mut [u8]) -> usize {
    let mut decoded_size = output.len();
    let result = unsafe {
        // SAFETY: The input and output pointers are valid for their respective lengths,
        // and the decoder writes at most the capacity supplied through `decoded_size`.
        brotli_sys::BrotliDecoderDecompress(
            input.len(),
            input.as_ptr(),
            &mut decoded_size,
            output.as_mut_ptr(),
        )
    };
    assert_ne!(
        result, 0,
        "Google Brotli decoder rejected benchmark fixture"
    );
    decoded_size
}

fn bench_google_brotli_direct(bencher: divan::Bencher<'_, '_>, case: &'static Case) {
    let mut output = vec![0_u8; case.source.len() + 1];
    bencher
        .counter(BytesCount::new(case.source.len()))
        .bench_local(move || {
            let decoded_size = google_brotli_decode_into(
                divan::black_box(&case.compressed),
                divan::black_box(&mut output),
            );
            divan::black_box(decoded_size)
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
            fn brutli_direct(bencher: divan::Bencher<'_, '_>) {
                bench_brutli_direct(bencher, &$case);
            }

            #[divan::bench]
            fn brutli_direct_8k(bencher: divan::Bencher<'_, '_>) {
                bench_brutli_direct_8k(bencher, &$case);
            }

            #[divan::bench]
            fn brutli_reader(bencher: divan::Bencher<'_, '_>) {
                bench_brutli_reader(bencher, &$case);
            }

            #[divan::bench]
            fn rust_brotli_direct(bencher: divan::Bencher<'_, '_>) {
                bench_rust_brotli_direct(bencher, &$case);
            }

            #[divan::bench]
            fn rust_brotli_reader(bencher: divan::Bencher<'_, '_>) {
                bench_rust_brotli_reader(bencher, &$case);
            }

            #[divan::bench]
            fn google_brotli_direct(bencher: divan::Bencher<'_, '_>) {
                bench_google_brotli_direct(bencher, &$case);
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
