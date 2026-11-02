use std::io::{self, BufReader, Cursor, Read};

use brutli::{DecodeError, Decoder, DecoderReader};

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

#[test]
fn read_adapter_repeats_window_limit_errors() {
    let mut reader = DecoderReader::with_decoder(
        Cursor::new([0x3f]), // Empty WBITS=24 stream.
        Decoder::with_max_window_bits(20),
    );
    let mut output = [0xa5; 1];

    for _ in 0..3 {
        let error = reader.read(&mut output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.get_ref().unwrap().downcast_ref::<DecodeError>(),
            Some(&DecodeError::WindowLimitExceeded {
                window_bits: 24,
                max_window_bits: 20,
            })
        );
        assert_eq!(output, [0xa5]);
        assert_eq!(reader.get_ref().position(), 0);
    }
}

#[test]
fn read_adapter_can_retry_source_io_errors() {
    struct InterruptOnce<'a> {
        input: &'a [u8],
        interrupted: bool,
    }

    impl Read for InterruptOnce<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.input.read(output)
        }
    }

    let source = BufReader::new(InterruptOnce {
        input: COMPRESSED,
        interrupted: false,
    });
    let mut reader = DecoderReader::new(source);
    let error = reader.read(&mut [0; 1]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);

    let mut decoded = Vec::new();
    reader.read_to_end(&mut decoded).unwrap();
    assert_eq!(decoded, expected());
}

#[test]
fn read_adapter_reports_failed_decoder_before_accessing_source() {
    struct Unavailable;

    impl Read for Unavailable {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    let mut decoder = Decoder::with_output_limit(0);
    let expected = DecodeError::OutputLimitExceeded { limit: 0 };
    assert_eq!(decoder.process(COMPRESSED, &mut [0; 1]), Err(expected));
    let mut reader = DecoderReader::with_decoder(BufReader::new(Unavailable), decoder);

    for _ in 0..3 {
        let error = reader.read(&mut [0; 1]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.get_ref().unwrap().downcast_ref::<DecodeError>(),
            Some(&expected)
        );
    }
}
