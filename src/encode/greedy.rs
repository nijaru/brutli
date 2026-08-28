use super::bit_writer::BitWriter;
use super::command::{ExplicitCommand, InsertCommand};
use super::distance::{DistanceCode, RecentDistances, alphabet_size};
use super::match_finder::{MatchCommand, greedy_parse};
use super::prefix_code::PrefixEncoding;
use super::{
    COMMAND_ALPHABET_SIZE, DIRECT_DISTANCE_CODES, LITERAL_ALPHABET_SIZE, MAX_META_BLOCK_SIZE,
    write_final_compressed_header, write_simple_compressed_header, write_window_bits,
};

#[derive(Debug, Clone, Copy)]
struct EncodedMatch {
    parsed: MatchCommand,
    command: ExplicitCommand,
    distance: DistanceCode,
}

pub(super) fn try_compress(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let parse = greedy_parse(input);
    if parse.commands.is_empty() {
        return None;
    }

    let distance_alphabet = alphabet_size(DIRECT_DISTANCE_CODES);
    let mut literal_frequencies = vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)];
    let mut command_frequencies = vec![0_usize; usize::from(COMMAND_ALPHABET_SIZE)];
    let mut distance_frequencies = vec![0_usize; usize::from(distance_alphabet)];
    let mut commands = Vec::with_capacity(parse.commands.len());
    let mut recent_distances = RecentDistances::default();

    for parsed in parse.commands {
        let command = ExplicitCommand::for_lengths(parsed.insert_length, parsed.copy_length);
        let distance = if parsed.is_dictionary {
            DistanceCode::for_distance(parsed.distance, DIRECT_DISTANCE_CODES)
        } else {
            recent_distances.encode(parsed.distance, DIRECT_DISTANCE_CODES)
        };
        command_frequencies[usize::from(command.symbol)] += 1;
        distance_frequencies[usize::from(distance.symbol)] += 1;
        count_literals(
            &mut literal_frequencies,
            &input[parsed.insert_start..parsed.insert_start + parsed.insert_length],
        );
        commands.push(EncodedMatch {
            parsed,
            command,
            distance,
        });
    }

    let tail = &input[parse.tail_start..];
    let tail_command = if tail.is_empty() {
        None
    } else {
        let command = InsertCommand::for_length(tail.len());
        command_frequencies[usize::from(command.symbol)] += 1;
        count_literals(&mut literal_frequencies, tail);
        Some(command)
    };

    let literal_code = PrefixEncoding::from_frequencies(&literal_frequencies)?;
    let command_code = PrefixEncoding::from_frequencies(&command_frequencies)?;
    let distance_code = PrefixEncoding::from_frequencies(&distance_frequencies)?;

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, input.len());
    write_simple_compressed_header(&mut writer, DIRECT_DISTANCE_CODES);
    literal_code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    command_code.write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    distance_code.write_tree(&mut writer, distance_alphabet);

    for encoded in commands {
        command_code.write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_literal_slice(
            &mut writer,
            &literal_code,
            &input[encoded.parsed.insert_start
                ..encoded.parsed.insert_start + encoded.parsed.insert_length],
        );
        distance_code.write_symbol(&mut writer, encoded.distance.symbol);
        encoded.distance.write_extra(&mut writer);
    }

    if let Some(command) = tail_command {
        command_code.write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_literal_slice(&mut writer, &literal_code, tail);
    }

    Some(writer.finish())
}

fn count_literals(frequencies: &mut [usize], literals: &[u8]) {
    for &literal in literals {
        frequencies[usize::from(literal)] += 1;
    }
}

fn write_literal_slice(writer: &mut BitWriter, code: &PrefixEncoding, literals: &[u8]) {
    for &literal in literals {
        code.write_symbol(writer, u16::from(literal));
    }
}

#[cfg(test)]
mod tests {
    use super::try_compress;
    use crate::decompress;

    #[test]
    fn greedy_stream_round_trips_text() {
        let source = b"the quick brown fox jumps over the lazy dog; the quick brown fox jumps over the lazy dog.";
        let encoded = try_compress(source).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn greedy_stream_round_trips_mixed_distances() {
        let source =
            b"alpha beta gamma alpha beta delta alpha beta gamma alpha beta delta".repeat(64);
        let encoded = try_compress(&source).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn identity_dictionary_stream_round_trips() {
        let source = b"timeXYZQ";
        let encoded = try_compress(source).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn reference_decoder_accepts_greedy_output() {
        let source = b"general greedy LZ77 should interoperate with the Brotli reference decoder. "
            .repeat(128);
        let encoded = try_compress(&source).unwrap();
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
    fn reference_decoder_accepts_identity_dictionary_output() {
        let source = b"timeXYZQ";
        let encoded = try_compress(source).unwrap();
        let mut decoded = vec![0_u8; source.len() + 1];
        let info = brotli_decompressor::brotli_decode(&encoded, &mut decoded);

        assert!(matches!(
            info.result,
            brotli_decompressor::BrotliResult::ResultSuccess
        ));
        assert_eq!(info.decoded_size, source.len());
        assert_eq!(&decoded[..info.decoded_size], source);
    }
}
