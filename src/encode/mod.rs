mod bit_writer;
mod command;
mod distance;
mod greedy;
mod match_finder;
mod prefix_code;
mod static_dictionary;

use bit_writer::BitWriter;
use command::{ExplicitCommand, InsertCommand};
use prefix_code::{
    PrefixEncoding, write_simple_prefix_code, write_simple_symbol, write_var_len_u8,
};

use crate::EncodeError;

pub(super) const DEFAULT_WINDOW_BITS: u8 = 22;
const MIN_WINDOW_BITS: u8 = 10;
const MAX_WINDOW_BITS: u8 = 24;
const MAX_META_BLOCK_SIZE: usize = 1 << 24;
const MAX_DISTANCE: usize = 0x3ff_fffc;

#[derive(Debug, Clone, Copy)]
pub(super) struct EncoderConfig {
    window_bits: u8,
}

impl EncoderConfig {
    fn new(window_bits: u8) -> Result<Self, EncodeError> {
        if !(MIN_WINDOW_BITS..=MAX_WINDOW_BITS).contains(&window_bits) {
            return Err(EncodeError::InvalidWindowBits { window_bits });
        }
        Ok(Self { window_bits })
    }

    pub(super) const fn window_bits(self) -> u8 {
        self.window_bits
    }

    pub(super) const fn max_backward_distance(self) -> usize {
        (1 << self.window_bits) - 16
    }

    pub(super) const fn max_distance(self) -> usize {
        MAX_DISTANCE
    }
}
const LITERAL_ALPHABET_SIZE: u16 = 256;
const COMMAND_ALPHABET_SIZE: u16 = 704;
const BASE_DISTANCE_ALPHABET_SIZE: u16 = 64;
const DIRECT_DISTANCE_CODES: u16 = 4;
const DIRECT_DISTANCE_ALPHABET_SIZE: u16 = BASE_DISTANCE_ALPHABET_SIZE + DIRECT_DISTANCE_CODES;

pub(super) fn compress(input: &[u8]) -> Vec<u8> {
    compress_with_window_bits(input, DEFAULT_WINDOW_BITS)
        .expect("the default RFC 7932 window bits are valid")
}

pub(super) fn compress_with_window_bits(
    input: &[u8],
    window_bits: u8,
) -> Result<Vec<u8>, EncodeError> {
    let config = EncoderConfig::new(window_bits)?;
    Ok(compress_with_config(input, config))
}

fn compress_with_config(input: &[u8], config: EncoderConfig) -> Vec<u8> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return compress_stored(input, config);
    }

    if let Some(candidate) = try_periodic_compressed(input, config) {
        return choose_against_stored(input, candidate, config);
    }

    if let Some(candidate) = greedy::try_compress(input, config) {
        if candidate.len() <= input.len() {
            return candidate;
        }

        let mut best = candidate;
        if let Some(literal) = try_simple_compressed(input, config)
            && literal.len() < best.len()
        {
            best = literal;
        }
        if let Some(literal) = try_general_literal_compressed(input, config)
            && literal.len() < best.len()
        {
            best = literal;
        }
        return choose_against_stored(input, best, config);
    }

    if let Some(candidate) = try_simple_compressed(input, config) {
        return choose_against_stored(input, candidate, config);
    }

    if let Some(candidate) = try_general_literal_compressed(input, config) {
        return choose_against_stored(input, candidate, config);
    }

    compress_stored(input, config)
}

fn choose_against_stored(input: &[u8], candidate: Vec<u8>, config: EncoderConfig) -> Vec<u8> {
    if candidate.len() <= input.len() {
        return candidate;
    }

    let stored = compress_stored(input, config);
    if candidate.len() < stored.len() {
        candidate
    } else {
        stored
    }
}

fn compress_stored(input: &[u8], config: EncoderConfig) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, config.window_bits());

    for chunk in input.chunks(MAX_META_BLOCK_SIZE) {
        write_uncompressed_metablock(&mut writer, chunk);
    }

    write_final_empty_metablock(&mut writer);
    writer.finish()
}

fn try_simple_compressed(input: &[u8], config: EncoderConfig) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let symbols = simple_literal_alphabet(input)?;
    let command = InsertCommand::for_length(input.len());

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, config.window_bits());
    write_final_compressed_header(&mut writer, input.len());
    write_simple_compressed_header(&mut writer, 0);

    write_simple_prefix_code(&mut writer, &symbols, LITERAL_ALPHABET_SIZE);
    write_simple_prefix_code(&mut writer, &[command.symbol], COMMAND_ALPHABET_SIZE);
    write_simple_prefix_code(&mut writer, &[0], BASE_DISTANCE_ALPHABET_SIZE);

    command.write_extra(&mut writer);
    write_literals(&mut writer, input, &symbols);

    Some(writer.finish())
}

fn try_general_literal_compressed(input: &[u8], config: EncoderConfig) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let mut frequencies = [0_usize; LITERAL_ALPHABET_SIZE as usize];
    for &byte in input {
        frequencies[usize::from(byte)] += 1;
    }
    if frequencies
        .iter()
        .filter(|&&frequency| frequency != 0)
        .count()
        <= 4
    {
        return None;
    }

    let literal_code = PrefixEncoding::from_frequencies(&frequencies)?;
    if literal_code.data_bits(&frequencies) >= input.len().saturating_mul(8) {
        return None;
    }
    let command = InsertCommand::for_length(input.len());

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, config.window_bits());
    write_final_compressed_header(&mut writer, input.len());
    write_simple_compressed_header(&mut writer, 0);

    literal_code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    write_simple_prefix_code(&mut writer, &[command.symbol], COMMAND_ALPHABET_SIZE);
    write_simple_prefix_code(&mut writer, &[0], BASE_DISTANCE_ALPHABET_SIZE);

    command.write_extra(&mut writer);
    for &byte in input {
        literal_code.write_symbol(&mut writer, u16::from(byte));
    }

    Some(writer.finish())
}

fn try_periodic_compressed(input: &[u8], config: EncoderConfig) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let period = periodic_prefix_length(input)?;
    let copy_length = input.len() - period;
    let symbols = simple_literal_alphabet(&input[..period])?;
    let command = ExplicitCommand::for_lengths(period, copy_length);
    let distance_symbol = 15 + period as u16;

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, config.window_bits());
    write_final_compressed_header(&mut writer, input.len());
    write_simple_compressed_header(&mut writer, DIRECT_DISTANCE_CODES);

    write_simple_prefix_code(&mut writer, &symbols, LITERAL_ALPHABET_SIZE);
    write_simple_prefix_code(&mut writer, &[command.symbol], COMMAND_ALPHABET_SIZE);
    write_simple_prefix_code(
        &mut writer,
        &[distance_symbol],
        DIRECT_DISTANCE_ALPHABET_SIZE,
    );

    command.write_extra(&mut writer);
    write_literals(&mut writer, &input[..period], &symbols);
    // The one distance tree contains only this direct-distance symbol, so it
    // emits no data bits and direct distance codes require no extra bits.

    Some(writer.finish())
}

fn periodic_prefix_length(input: &[u8]) -> Option<usize> {
    (1..=4).find(|&period| {
        input.len() >= period + 2
            && input[period..]
                .iter()
                .enumerate()
                .all(|(index, &byte)| byte == input[index % period])
    })
}

fn write_literals(writer: &mut BitWriter, input: &[u8], symbols: &[u16]) {
    for &byte in input {
        let index = symbols
            .binary_search(&u16::from(byte))
            .expect("literal alphabet was built from the input");
        write_simple_symbol(writer, index, symbols.len());
    }
}

fn simple_literal_alphabet(input: &[u8]) -> Option<Vec<u16>> {
    let mut used = [false; 256];
    let mut count = 0_usize;

    for &byte in input {
        let slot = &mut used[usize::from(byte)];
        if !*slot {
            *slot = true;
            count += 1;
            if count > 4 {
                return None;
            }
        }
    }

    Some(
        used.into_iter()
            .enumerate()
            .filter_map(|(symbol, present)| present.then_some(symbol as u16))
            .collect(),
    )
}

fn write_window_bits(writer: &mut BitWriter, window_bits: u8) {
    match window_bits {
        16 => writer.write_bits(0, 1),
        18..=24 => {
            writer.write_bits(1, 1);
            writer.write_bits(u64::from(window_bits - 17), 3);
        }
        17 => {
            writer.write_bits(1, 1);
            writer.write_bits(0, 3);
            writer.write_bits(0, 3);
        }
        10..=15 => {
            writer.write_bits(1, 1);
            writer.write_bits(0, 3);
            writer.write_bits(u64::from(window_bits - 8), 3);
        }
        _ => panic!("RFC 7932 window bits must be in 10..=24"),
    }
}

fn write_final_compressed_header(writer: &mut BitWriter, length: usize) {
    assert!((1..=MAX_META_BLOCK_SIZE).contains(&length));

    writer.write_bits(1, 1); // ISLAST
    writer.write_bits(0, 1); // ISLASTEMPTY
    let nibbles = nibbles_for_length(length);
    writer.write_bits(u64::from(nibbles - 4), 2); // MNIBBLES
    writer.write_bits((length - 1) as u64, nibbles * 4); // MLEN - 1
}

fn write_simple_compressed_header(writer: &mut BitWriter, direct_distance_codes: u16) {
    debug_assert!(direct_distance_codes <= 15);

    write_var_len_u8(writer, 0); // one literal block type
    write_var_len_u8(writer, 0); // one insert-and-copy block type
    write_var_len_u8(writer, 0); // one distance block type
    writer.write_bits(0, 2); // NPOSTFIX
    writer.write_bits(u64::from(direct_distance_codes), 4); // NDIRECT
    writer.write_bits(0, 2); // literal context mode
    write_var_len_u8(writer, 0); // one literal tree
    write_var_len_u8(writer, 0); // one distance tree
}

fn write_uncompressed_metablock(writer: &mut BitWriter, input: &[u8]) {
    assert!(!input.is_empty());
    assert!(input.len() <= MAX_META_BLOCK_SIZE);

    writer.write_bits(0, 1); // ISLAST

    let nibbles = nibbles_for_length(input.len());
    writer.write_bits(u64::from(nibbles - 4), 2); // MNIBBLES
    writer.write_bits((input.len() - 1) as u64, nibbles * 4); // MLEN - 1
    writer.write_bits(1, 1); // ISUNCOMPRESSED
    writer.align_to_byte();
    writer.write_bytes(input);
}

fn write_final_empty_metablock(writer: &mut BitWriter) {
    writer.write_bits(1, 1); // ISLAST
    writer.write_bits(1, 1); // ISLASTEMPTY
}

fn nibbles_for_length(length: usize) -> u8 {
    debug_assert!((1..=MAX_META_BLOCK_SIZE).contains(&length));
    match length {
        1..=0x1_0000 => 4,
        0x1_0001..=0x10_0000 => 5,
        _ => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_WINDOW_BITS, EncoderConfig, MAX_META_BLOCK_SIZE, compress, compress_stored,
        nibbles_for_length, periodic_prefix_length, simple_literal_alphabet,
        try_general_literal_compressed, try_periodic_compressed, try_simple_compressed,
    };
    use crate::{DecodeError, Decoder, compress_with_window_bits, decompress};

    fn default_config() -> EncoderConfig {
        EncoderConfig::new(DEFAULT_WINDOW_BITS).unwrap()
    }

    #[test]
    fn empty_stream_round_trips() {
        let encoded = compress(&[]);
        assert_eq!(decompress(&encoded, 0).unwrap(), b"");
    }

    #[test]
    fn stored_stream_round_trips() {
        let source = b"brutli encoder baseline";
        let encoded = compress(source);
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn simple_compressed_stream_round_trips() {
        for source in [
            b"aaaaaaaaaaaaaaaa".as_slice(),
            b"abababababababab".as_slice(),
            b"abcabcabcabcabca".as_slice(),
            b"abcdabcdabcdabcd".as_slice(),
        ] {
            let encoded = try_simple_compressed(source, default_config()).unwrap();
            assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
            assert!(encoded.len() < compress_stored(source, default_config()).len());
        }
    }

    #[test]
    fn complex_literal_stream_round_trips() {
        let source = b"the quick brown fox jumps over the lazy dog. ".repeat(256);
        let encoded = try_general_literal_compressed(&source, default_config()).unwrap();

        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
        assert!(encoded.len() < compress_stored(&source, default_config()).len());
    }

    #[test]
    fn reference_decoder_accepts_complex_literal_output() {
        let source = b"complex canonical literal trees interoperate with Brotli. ".repeat(256);
        let encoded = try_general_literal_compressed(&source, default_config()).unwrap();
        let mut decoded = vec![0_u8; source.len() + 1];
        let info = brotli_decompressor::brotli_decode(&encoded, &mut decoded);

        assert!(matches!(
            info.result,
            brotli_decompressor::BrotliResult::ResultSuccess
        ));
        assert_eq!(info.decoded_size, source.len());
        assert_eq!(&decoded[..info.decoded_size], source);
    }

    #[test]
    fn periodic_copy_round_trips_for_direct_distances() {
        for source in [
            b"aaaaaaaaaaaaaaaa".as_slice(),
            b"abababababababab".as_slice(),
            b"abcabcabcabcabca".as_slice(),
            b"abcdabcdabcdabcd".as_slice(),
        ] {
            let encoded = try_periodic_compressed(source, default_config()).unwrap();
            assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
        }
    }

    #[test]
    fn periodic_copy_beats_literal_only_on_long_patterns() {
        let source = b"abcd".repeat(4096);
        let periodic = try_periodic_compressed(&source, default_config()).unwrap();
        let literals = try_simple_compressed(&source, default_config()).unwrap();
        assert!(periodic.len() < literals.len());
    }

    #[test]
    fn reference_decoder_accepts_stored_output() {
        let source = b"standards-compliant Brotli output from Brutli".repeat(1024);
        let encoded = compress(&source);
        let mut decoded = vec![0_u8; source.len() + 1];
        let info = brotli_decompressor::brotli_decode(&encoded, &mut decoded);

        assert!(matches!(
            info.result,
            brotli_decompressor::BrotliResult::ResultSuccess
        ));
        assert_eq!(info.decoded_size, source.len());
        assert_eq!(&decoded[..info.decoded_size], source);
    }

    #[test]
    fn reference_decoder_accepts_compressed_output() {
        let source = b"abcd".repeat(4096);
        let encoded = compress(&source);
        assert!(encoded.len() < source.len());

        let mut decoded = vec![0_u8; source.len() + 1];
        let info = brotli_decompressor::brotli_decode(&encoded, &mut decoded);
        assert!(matches!(
            info.result,
            brotli_decompressor::BrotliResult::ResultSuccess
        ));
        assert_eq!(info.decoded_size, source.len());
        assert_eq!(&decoded[..info.decoded_size], source);
    }

    #[test]
    fn stored_encoder_crosses_four_nibble_length_boundary() {
        let source = vec![0xa5; 0x1_0001];
        let encoded = compress_stored(&source, default_config());
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn compressed_encoder_crosses_four_nibble_length_boundary() {
        let source = vec![0xa5; 0x1_0001];
        let encoded = compress(&source);
        assert!(encoded.len() < source.len());
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn default_stream_uses_window_22() {
        let encoded = compress(b"x");
        let mut decoder = Decoder::with_max_window_bits(21);
        let mut output = [0_u8; 1];

        assert_eq!(
            decoder.process(&encoded, &mut output),
            Err(DecodeError::WindowLimitExceeded {
                window_bits: 22,
                max_window_bits: 21,
            })
        );
    }

    #[test]
    fn configurable_stream_uses_every_rfc_window_size() {
        let source = b"abcd".repeat(4096);
        for window_bits in 10..=24 {
            let encoded = compress_with_window_bits(&source, window_bits).unwrap();
            assert_eq!(decompress(&encoded, source.len()).unwrap(), source);

            let mut decoder = Decoder::with_max_window_bits(window_bits - 1);
            let mut output = vec![0_u8; source.len()];
            assert_eq!(
                decoder.process(&encoded, &mut output),
                Err(DecodeError::WindowLimitExceeded {
                    window_bits,
                    max_window_bits: window_bits - 1,
                })
            );
        }

        for window_bits in 10..=24 {
            let encoded = compress_with_window_bits(b"time and more", window_bits).unwrap();
            assert_eq!(decompress(&encoded, 13).unwrap(), b"time and more");
        }
    }

    #[test]
    fn configurable_stream_rejects_invalid_window_sizes() {
        for window_bits in [0, 9, 25, u8::MAX] {
            assert_eq!(
                compress_with_window_bits(b"input", window_bits),
                Err(crate::EncodeError::InvalidWindowBits { window_bits })
            );
        }
    }

    #[test]
    fn detects_periods_up_to_four_bytes() {
        assert_eq!(periodic_prefix_length(b"aaaaaaaa"), Some(1));
        assert_eq!(periodic_prefix_length(b"abababab"), Some(2));
        assert_eq!(periodic_prefix_length(b"abcabcabc"), Some(3));
        assert_eq!(periodic_prefix_length(b"abcdabcd"), Some(4));
        assert_eq!(periodic_prefix_length(b"abcdeabcde"), None);
    }

    #[test]
    fn simple_alphabet_rejects_five_symbols() {
        assert_eq!(simple_literal_alphabet(b"abcde"), None);
        assert_eq!(
            simple_literal_alphabet(b"dcba"),
            Some(vec![97, 98, 99, 100])
        );
    }

    #[test]
    fn chooses_minimal_length_width() {
        assert_eq!(nibbles_for_length(1), 4);
        assert_eq!(nibbles_for_length(0x1_0000), 4);
        assert_eq!(nibbles_for_length(0x1_0001), 5);
        assert_eq!(nibbles_for_length(0x10_0000), 5);
        assert_eq!(nibbles_for_length(0x10_0001), 6);
        assert_eq!(nibbles_for_length(MAX_META_BLOCK_SIZE), 6);
    }
}
