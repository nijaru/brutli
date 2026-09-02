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
    for mode in [
        brutli::EncoderMode::Generic,
        brutli::EncoderMode::Text,
        brutli::EncoderMode::Font,
    ] {
        let encoded = compress_with_options(
            &source,
            EncoderOptions {
                quality: 1,
                window_bits: 10,
                mode,
            },
        )
        .unwrap();

        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }
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
fn font_mode_round_trips_with_match_distances() {
    // Font input that takes the greedy path: repeated glyphs far enough apart
    // to be encoded as backward references rather than a periodic block.
    let mut source = Vec::new();
    for _ in 0..256 {
        source.extend_from_slice(b"\x00glyph run with binary\xff payload\x01 ");
    }
    assert!(source.len() > 4096);

    for quality in [4, 5, 11] {
        let encoded = compress_with_options(
            &source,
            EncoderOptions {
                quality,
                window_bits: 22,
                mode: brutli::EncoderMode::Font,
            },
        )
        .unwrap();
        assert!(encoded.len() < source.len());
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

#[test]
fn large_input_compresses_across_multiple_metablocks() {
    // Repeating text above the 16 MiB single-metablock cap: at least two
    // metablocks, each individually compressed, round-tripping through both
    // Brutli and the reference decoder.
    let unit = b"the quick brown fox jumps over the lazy dog. ".repeat(96);
    let mut source = unit.repeat(((1_usize << 24) / unit.len()) + 2);
    source.truncate((1_usize << 24) + 4096);

    let encoded = brutli::compress(&source);
    assert!(encoded.len() < source.len() / 8);
    assert_eq!(decompress(&encoded, source.len()).unwrap(), source);

    let mut decoded = vec![0_u8; source.len() + 1];
    let info = brotli_decompressor::brotli_decode(&encoded, &mut decoded);
    assert!(matches!(
        info.result,
        brotli_decompressor::BrotliResult::ResultSuccess
    ));
    assert_eq!(info.decoded_size, source.len());
    assert_eq!(&decoded[..info.decoded_size], &source[..]);
}

#[test]
fn large_random_input_falls_back_to_stored_metablocks() {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut source = vec![0_u8; (1_usize << 24) + 64];
    for chunk in source.as_chunks_mut::<8>().0 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }

    let encoded = brutli::compress(&source);
    assert_eq!(decompress(&encoded, source.len()).unwrap(), source);

    let mut decoded = vec![0_u8; source.len() + 1];
    let info = brotli_decompressor::brotli_decode(&encoded, &mut decoded);
    assert!(matches!(
        info.result,
        brotli_decompressor::BrotliResult::ResultSuccess
    ));
    assert_eq!(info.decoded_size, source.len());
    assert_eq!(&decoded[..info.decoded_size], &source[..]);
}

#[test]
fn incremental_encoder_matches_one_shot_output() {
    let source = b"the quick brown fox jumps over the lazy dog. ".repeat(96);

    // Byte-identical output regardless of feed granularity, including the
    // trivially small buffer sizes that exercise the drain loop.
    for chunk_size in [1, 3, 17, 1024, source.len()] {
        let mut encoder = brutli::Encoder::new();
        let mut output = Vec::new();
        let mut buffer = [0_u8; 16];

        for chunk in source.chunks(chunk_size) {
            let mut consumed = 0_usize;
            loop {
                let progress = encoder.process(&chunk[consumed..], &mut buffer);
                consumed += progress.consumed;
                output.extend_from_slice(&buffer[..progress.produced]);
                match progress.status {
                    brutli::EncodeStatus::NeedOutput => continue,
                    brutli::EncodeStatus::NeedInput => break,
                    brutli::EncodeStatus::Done => panic!("process must not finish the stream"),
                }
            }
            assert_eq!(consumed, chunk.len());
        }

        loop {
            let progress = encoder.finish(&mut buffer);
            output.extend_from_slice(&buffer[..progress.produced]);
            match progress.status {
                brutli::EncodeStatus::Done => break,
                brutli::EncodeStatus::NeedOutput => continue,
                brutli::EncodeStatus::NeedInput => unreachable!("finish needs no input"),
            }
        }

        assert_eq!(output, brutli::compress(&source));
        assert_eq!(decompress(&output, source.len()).unwrap(), source);
    }
}

#[test]
fn incremental_encoder_drains_large_streams_incrementally() {
    let unit = b"the quick brown fox jumps over the lazy dog. ".repeat(96);
    let mut source = unit.repeat((16_usize << 20) / unit.len() + 2);
    source.truncate((16_usize << 20) + 4096);

    let mut encoder = brutli::Encoder::new();
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut feed = 0_usize;

    while feed < source.len() {
        let progress = encoder.process(&source[feed..], &mut buffer);
        feed += progress.consumed;
        output.extend_from_slice(&buffer[..progress.produced]);
    }
    loop {
        let progress = encoder.finish(&mut buffer);
        output.extend_from_slice(&buffer[..progress.produced]);
        if matches!(progress.status, brutli::EncodeStatus::Done) {
            break;
        }
    }

    assert_eq!(feed, source.len());
    // Multi-metablock output matches the one-shot multi-metablock encoder.
    assert_eq!(output, brutli::compress(&source));
    assert_eq!(decompress(&output, source.len()).unwrap(), source);
}

#[test]
fn incremental_encoder_rejects_input_after_done() {
    let mut encoder = brutli::Encoder::new();
    let mut buffer = [0_u8; 32];
    loop {
        let progress = encoder.finish(&mut buffer);
        if matches!(progress.status, brutli::EncodeStatus::Done) {
            break;
        }
    }

    let progress = encoder.process(b"too late", &mut buffer);
    assert_eq!(progress.consumed, 0);
    assert!(matches!(progress.status, brutli::EncodeStatus::Done));
}

#[test]
fn incremental_encoder_accepts_explicit_options() {
    let source = b"alpha beta gamma delta epsilon zeta eta theta. ".repeat(64);

    let mut encoder = brutli::Encoder::with_options(brutli::EncoderOptions {
        quality: 9,
        window_bits: 16,
        mode: brutli::EncoderMode::Font,
    })
    .unwrap();

    let mut output = Vec::new();
    let mut buffer = [0_u8; 64];
    let mut consumed = 0_usize;
    loop {
        let progress = encoder.process(&source[consumed..], &mut buffer);
        consumed += progress.consumed;
        output.extend_from_slice(&buffer[..progress.produced]);
        if matches!(progress.status, brutli::EncodeStatus::NeedInput) {
            break;
        }
    }
    loop {
        let progress = encoder.finish(&mut buffer);
        output.extend_from_slice(&buffer[..progress.produced]);
        if matches!(progress.status, brutli::EncodeStatus::Done) {
            break;
        }
    }

    assert_eq!(
        output,
        brutli::compress_with_options(
            &source,
            brutli::EncoderOptions {
                quality: 9,
                window_bits: 16,
                mode: brutli::EncoderMode::Font,
            }
        )
        .unwrap()
    );
    assert_eq!(decompress(&output, source.len()).unwrap(), source);
}
