mod bit_writer;
mod command;
mod prefix_code;

use bit_writer::BitWriter;
use command::{ExplicitCommand, InsertCommand};
use prefix_code::{write_simple_prefix_code, write_simple_symbol, write_var_len_u8};

const DEFAULT_WINDOW_BITS: u8 = 22;
const MAX_META_BLOCK_SIZE: usize = 1 << 24;
const LITERAL_ALPHABET_SIZE: u16 = 256;
const COMMAND_ALPHABET_SIZE: u16 = 704;
const BASE_DISTANCE_ALPHABET_SIZE: u16 = 64;
const DIRECT_DISTANCE_CODES: u16 = 4;
const DIRECT_DISTANCE_ALPHABET_SIZE: u16 = BASE_DISTANCE_ALPHABET_SIZE + DIRECT_DISTANCE_CODES;

pub(super) fn compress(input: &[u8]) -> Vec<u8> {
    let mut best = compress_stored(input);

    for candidate in [try_simple_compressed(input), try_periodic_compressed(input)]
        .into_iter()
        .flatten()
    {
        if candidate.len() < best.len() {
            best = candidate;
        }
    }

    best
}

fn compress_stored(input: &[u8]) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, DEFAULT_WINDOW_BITS);

    for chunk in input.chunks(MAX_META_BLOCK_SIZE) {
        write_uncompressed_metablock(&mut writer, chunk);
    }

    write_final_empty_metablock(&mut writer);
    writer.finish()
}

fn try_simple_compressed(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let symbols = simple_literal_alphabet(input)?;
    let command = InsertCommand::for_length(input.len());

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, input.len());
    write_simple_compressed_header(&mut writer, 0);

    write_simple_prefix_code(&mut writer, &symbols, LITERAL_ALPHABET_SIZE);
    write_simple_prefix_code(&mut writer, &[command.symbol], COMMAND_ALPHABET_SIZE);
    write_simple_prefix_code(&mut writer, &[0], BASE_DISTANCE_ALPHABET_SIZE);

    command.write_extra(&mut writer);
    write_literals(&mut writer, input, &symbols);

    Some(writer.finish())
}

fn try_periodic_compressed(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let period = periodic_prefix_length(input)?;
    let copy_length = input.len() - period;
    let symbols = simple_literal_alphabet(&input[..period])?;
    let command = ExplicitCommand::for_lengths(period, copy_length);
    let distance_symbol = 15 + period as u16;

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, DEFAULT_WINDOW_BITS);
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
        MAX_META_BLOCK_SIZE, compress, compress_stored, nibbles_for_length, periodic_prefix_length,
        simple_literal_alphabet, try_periodic_compressed, try_simple_compressed,
    };
    use crate::{DecodeError, Decoder, decompress};

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
            let encoded = try_simple_compressed(source).unwrap();
            assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
            assert!(encoded.len() < compress_stored(source).len());
        }
    }

    #[test]
    fn periodic_copy_round_trips_for_direct_distances() {
        for source in [
            b"aaaaaaaaaaaaaaaa".as_slice(),
            b"abababababababab".as_slice(),
            b"abcabcabcabcabca".as_slice(),
            b"abcdabcdabcdabcd".as_slice(),
        ] {
            let encoded = try_periodic_compressed(source).unwrap();
            assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
        }
    }

    #[test]
    fn periodic_copy_beats_literal_only_on_long_patterns() {
        let source = b"abcd".repeat(4096);
        let periodic = try_periodic_compressed(&source).unwrap();
        let literals = try_simple_compressed(&source).unwrap();
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
        let encoded = compress_stored(&source);
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
