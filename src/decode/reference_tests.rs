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

#[test]
fn decodes_reference_repetitive_stream() {
    // Produced by the Brotli reference encoder at quality 5, lgwin 16.
    const COMPRESSED: &[u8] = &[
        0xe2, 0x0e, 0x00, 0x80, 0xc0, 0x0e, 0xd8, 0xdc, 0x65, 0x2e, 0x44, 0x6c, 0x71, 0x60,
        0xdd, 0x31,
    ];
    let expected = b"abc123".repeat(20);

    assert_eq!(decode_exact(COMPRESSED, expected.len()), expected);
}

#[test]
fn decodes_reference_binary_pattern_stream() {
    // Produced by the Brotli reference encoder at quality 5, lgwin 16.
    const COMPRESSED: &[u8] = &[
        0xe2, 0x0f, 0x00, 0x80, 0x78, 0x00, 0x1c, 0x4f, 0x43, 0x7c, 0x01, 0x2c, 0xc1, 0xcf,
        0x0a, 0x28, 0xa7, 0xfb, 0xfc, 0x18, 0x00, 0xc2, 0xba, 0x01,
    ];
    let expected = (0_u8..32).cycle().take(128).collect::<Vec<_>>();

    assert_eq!(decode_exact(COMPRESSED, expected.len()), expected);
}
