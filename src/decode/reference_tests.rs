use super::decoder::{Decoder, ProcessStatus};

fn decode_exact(input: &[u8], expected_len: usize) -> Vec<u8> {
    let mut decoder = Decoder::default();
    let mut output = vec![0; expected_len];
    let result = decoder.process(input, &mut output).unwrap();

    assert_eq!(result.status, ProcessStatus::Done);
    assert_eq!(result.consumed, input.len());
    assert_eq!(result.produced, expected_len);
    output
}

fn decode_byte_at_a_time(input: &[u8]) -> Vec<u8> {
    let mut decoder = Decoder::default();
    let mut input_offset = 0;
    let mut decoded = Vec::new();

    for _ in 0..10_000 {
        let input_end = (input_offset + 1).min(input.len());
        let mut output = [0; 1];
        let result = decoder
            .process(&input[input_offset..input_end], &mut output)
            .unwrap();
        input_offset += result.consumed;
        decoded.extend_from_slice(&output[..result.produced]);

        match result.status {
            ProcessStatus::NeedInput => {
                assert_eq!(input_offset, input_end);
                assert!(input_offset < input.len());
            }
            ProcessStatus::NeedOutput => {}
            ProcessStatus::Done => {
                assert_eq!(input_offset, input.len());
                return decoded;
            }
        }
    }

    panic!("decoder made no terminal progress");
}

#[test]
fn decodes_reference_repetitive_stream() {
    // Produced by the Brotli reference encoder at quality 5, lgwin 16.
    const COMPRESSED: &[u8] = &[
        0xe2, 0x0e, 0x00, 0x80, 0xc0, 0x0e, 0xd8, 0xdc, 0x65, 0x2e, 0x44, 0x6c, 0x71, 0x60, 0xdd,
        0x31,
    ];
    let expected = b"abc123".repeat(20);

    assert_eq!(decode_exact(COMPRESSED, expected.len()), expected);
}

#[test]
fn decodes_reference_binary_pattern_stream() {
    // Produced by the Brotli reference encoder at quality 5, lgwin 16.
    const COMPRESSED: &[u8] = &[
        0xe2, 0x0f, 0x00, 0x80, 0x78, 0x00, 0x1c, 0x4f, 0x43, 0x7c, 0x01, 0x2c, 0xc1, 0xcf, 0x0a,
        0x28, 0xa7, 0xfb, 0xfc, 0x18, 0x00, 0xc2, 0xba, 0x01,
    ];
    let expected = (0_u8..32).cycle().take(128).collect::<Vec<_>>();

    assert_eq!(decode_exact(COMPRESSED, expected.len()), expected);
}

#[test]
fn decodes_reference_binary_pattern_stream_byte_at_a_time() {
    const COMPRESSED: &[u8] = &[
        0xe2, 0x0f, 0x00, 0x80, 0x78, 0x00, 0x1c, 0x4f, 0x43, 0x7c, 0x01, 0x2c, 0xc1, 0xcf, 0x0a,
        0x28, 0xa7, 0xfb, 0xfc, 0x18, 0x00, 0xc2, 0xba, 0x01,
    ];
    let expected = (0_u8..32).cycle().take(128).collect::<Vec<_>>();

    assert_eq!(decode_byte_at_a_time(COMPRESSED), expected);
}

#[test]
fn decodes_reference_static_dictionary_identity() {
    // Produced by the Brotli reference encoder at quality 11, lgwin 16.
    const COMPRESSED: &[u8] = &[0x42, 0x01, 0x00, 0xbf, 0x04, 0x40, 0x00, 0x13, 0xb6, 0x3c];

    assert_eq!(decode_exact(COMPRESSED, 11), b"compression");
}

#[test]
fn decodes_reference_static_dictionary_uppercase_first() {
    // Produced by the Brotli reference encoder at quality 11, lgwin 16.
    const COMPRESSED: &[u8] = &[
        0x42, 0x01, 0x00, 0xbf, 0x04, 0x40, 0x00, 0x13, 0x0e, 0xfd, 0x04,
    ];

    assert_eq!(decode_exact(COMPRESSED, 11), b"Compression");
}

#[test]
fn decodes_reference_static_dictionary_suffix_space() {
    // Produced by the Brotli reference encoder at quality 11, lgwin 16.
    const COMPRESSED: &[u8] = &[
        0x62, 0x01, 0x00, 0xbf, 0x04, 0x48, 0x29, 0x02, 0x4c, 0xd8, 0xf2, 0x01,
    ];

    assert_eq!(decode_exact(COMPRESSED, 12), b"compression ");
}
