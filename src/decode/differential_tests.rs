use super::decoder::{Decoder, ProcessStatus};
use std::io::Read;

fn compress_reference(input: &[u8], quality: u32, lgwin: u32) -> Vec<u8> {
    let mut encoder = brotli::CompressorReader::new(input, 4096, quality, lgwin);
    let mut compressed = Vec::new();
    encoder.read_to_end(&mut compressed).unwrap();
    compressed
}

fn decode_stream(input: &[u8]) -> Vec<u8> {
    let mut decoder = Decoder::default();
    let mut input_offset = 0;
    let mut decoded = Vec::new();

    for _ in 0..100_000 {
        let mut output = [0; 257];
        let result = decoder
            .process(&input[input_offset..], &mut output)
            .unwrap();
        input_offset += result.consumed;
        decoded.extend_from_slice(&output[..result.produced]);

        match result.status {
            ProcessStatus::NeedInput => {
                assert!(
                    input_offset < input.len(),
                    "valid reference stream requested input after consuming all input"
                );
            }
            ProcessStatus::NeedOutput => {}
            ProcessStatus::Done => {
                assert_eq!(input_offset, input.len());
                return decoded;
            }
        }
    }

    panic!("decoder did not terminate");
}

fn corpora() -> Vec<Vec<u8>> {
    let mut binary = Vec::with_capacity(1024);
    for round in 0..4_u8 {
        binary.extend((0_u8..=255).map(|byte| byte.wrapping_add(round.wrapping_mul(17))));
    }

    let mut pseudorandom = Vec::with_capacity(1024);
    let mut state = 0x243f_6a88_u32;
    for _ in 0..1024 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        pseudorandom.push(state as u8);
    }

    vec![
        b"The quick brown fox jumps over the lazy dog. ".repeat(20),
        b"compression Compression compression compressed compressor ".repeat(16),
        b"abc123".repeat(160),
        binary,
        pseudorandom,
    ]
}

#[test]
fn decodes_reference_encoder_across_all_qualities() {
    let corpora = corpora();

    for quality in 0..=11_u32 {
        for lgwin in [16_u32, 20] {
            for (corpus_index, expected) in corpora.iter().enumerate() {
                let compressed = compress_reference(expected, quality, lgwin);
                let decoded = decode_stream(&compressed);
                assert_eq!(
                    decoded, *expected,
                    "quality={quality} lgwin={lgwin} corpus={corpus_index} compressed_len={}",
                    compressed.len()
                );
            }
        }
    }
}
