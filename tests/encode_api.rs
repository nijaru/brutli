use brutli::{
    DecodeError, Decoder, EncodeError, EncoderOptions, compress, compress_with_options,
    compress_with_window_bits, decompress,
};

#[test]
fn default_compression_uses_window_22() {
    let encoded = compress(b"Brotli data");
    let mut decoder = Decoder::with_max_window_bits(21);
    let mut output = [0_u8; 32];

    assert_eq!(
        decoder.process(&encoded, &mut output),
        Err(DecodeError::WindowLimitExceeded {
            window_bits: 22,
            max_window_bits: 21,
        })
    );
}

#[test]
fn compression_accepts_all_rfc_window_sizes() {
    let source = b"abcd".repeat(4096);

    for window_bits in 10..=24 {
        let encoded = compress_with_window_bits(&source, window_bits).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }
}

#[test]
fn compression_accepts_explicit_options() {
    let source = b"abcd".repeat(1024);
    let encoded = compress_with_options(
        &source,
        EncoderOptions {
            quality: 1,
            window_bits: 10,
        },
    )
    .unwrap();

    assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
}

#[test]
fn compression_accepts_all_quality_levels() {
    let source = b"abcd".repeat(4096);

    for quality in 0..=11 {
        let encoded = brutli::compress_with_quality(&source, quality).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }
}

#[test]
fn compression_rejects_invalid_quality_levels() {
    for quality in [12, u8::MAX] {
        assert_eq!(
            brutli::compress_with_quality(b"Brotli data", quality),
            Err(EncodeError::InvalidQuality { quality })
        );
    }
}

#[test]
fn compression_rejects_invalid_window_sizes() {
    for window_bits in [0, 9, 25, u8::MAX] {
        assert_eq!(
            compress_with_window_bits(b"Brotli data", window_bits),
            Err(EncodeError::InvalidWindowBits { window_bits })
        );
    }
}
