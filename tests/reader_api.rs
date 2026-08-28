use std::io::{Cursor, Read};

use brutli::{Decoder, DecoderReader};

const COMPRESSED: &[u8] = &[
    0xe2, 0x0e, 0x00, 0x80, 0xc0, 0x0e, 0xd8, 0xdc, 0x65, 0x2e, 0x44, 0x6c, 0x71, 0x60, 0xdd, 0x31,
];

fn expected() -> Vec<u8> {
    b"abc123".repeat(20)
}

#[test]
fn read_adapter_decodes_stream() {
    let source = Cursor::new(COMPRESSED);
    let mut reader = DecoderReader::new(source);
    let mut decoded = Vec::new();

    reader.read_to_end(&mut decoded).unwrap();

    assert_eq!(decoded, expected());
    assert_eq!(reader.decoder().total_output(), expected().len());
}

#[test]
fn read_adapter_preserves_trailing_buffered_bytes() {
    let mut source = COMPRESSED.to_vec();
    source.extend_from_slice(b"tail");
    let cursor = Cursor::new(source);
    let mut reader = DecoderReader::new(cursor);
    let mut decoded = Vec::new();

    reader.read_to_end(&mut decoded).unwrap();
    let mut cursor = reader.into_inner();
    let mut trailing = Vec::new();
    cursor.read_to_end(&mut trailing).unwrap();

    assert_eq!(decoded, expected());
    assert_eq!(trailing, b"tail");
}

#[test]
fn read_adapter_reports_truncated_stream() {
    let source = Cursor::new(&COMPRESSED[..COMPRESSED.len() - 1]);
    let mut reader = DecoderReader::new(source);
    let mut decoded = Vec::new();

    let error = reader.read_to_end(&mut decoded).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_adapter_honors_configured_decoder_limit() {
    let source = Cursor::new(COMPRESSED);
    let decoder = Decoder::with_output_limit(expected().len() - 1);
    let mut reader = DecoderReader::with_decoder(source, decoder);
    let mut decoded = Vec::new();

    let error = reader.read_to_end(&mut decoded).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
