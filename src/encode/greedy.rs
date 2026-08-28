mod block_split;

use block_split::{BlockCursor, BlockSplitEncoding, split_literals, write_trivial_context_map};

use super::bit_writer::BitWriter;
use super::command::{ExplicitCommand, InsertCommand};
use super::distance::{DistanceCode, alphabet_size};
use super::match_finder::{MatchCommand, create_backward_references};
use super::prefix_code::{PrefixEncoding, write_var_len_u8};
use super::{
    COMMAND_ALPHABET_SIZE, DIRECT_DISTANCE_CODES, LITERAL_ALPHABET_SIZE, MAX_META_BLOCK_SIZE,
    write_final_compressed_header, write_simple_compressed_header, write_window_bits,
};

#[derive(Debug, Clone, Copy)]
struct EncodedMatch {
    parsed: MatchCommand,
    command: ExplicitCommand,
    distance: Option<DistanceCode>,
}

struct EncodingPlan<'a> {
    input: &'a [u8],
    commands: &'a [EncodedMatch],
    tail: &'a [u8],
    tail_command: Option<InsertCommand>,
    command_code: &'a PrefixEncoding,
    distance_code: &'a PrefixEncoding,
    distance_alphabet: u16,
}

pub(super) fn try_compress(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let parse = create_backward_references(input);
    if parse.commands.is_empty() {
        return None;
    }

    let distance_alphabet = alphabet_size(DIRECT_DISTANCE_CODES);
    let mut literal_frequencies = vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)];
    let mut command_frequencies = vec![0_usize; usize::from(COMMAND_ALPHABET_SIZE)];
    let mut distance_frequencies = vec![0_usize; usize::from(distance_alphabet)];
    let mut commands = Vec::with_capacity(parse.commands.len());
    let mut literal_data = Vec::new();

    for parsed in parse.commands {
        let command = ExplicitCommand::for_insert_and_copy_code(
            parsed.insert_length,
            parsed.copy_length_code,
            parsed.distance_code == 0,
        );
        let distance = command
            .requires_distance()
            .then(|| DistanceCode::for_code(parsed.distance_code, DIRECT_DISTANCE_CODES));
        command_frequencies[usize::from(command.symbol)] += 1;
        if let Some(distance) = distance {
            distance_frequencies[usize::from(distance.symbol)] += 1;
        }
        let literals = &input[parsed.insert_start..parsed.insert_start + parsed.insert_length];
        count_literals(&mut literal_frequencies, literals);
        literal_data.extend_from_slice(literals);
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
        literal_data.extend_from_slice(tail);
        Some(command)
    };

    if literal_frequencies.iter().all(|&frequency| frequency == 0) {
        literal_frequencies[0] = 1;
    }
    if distance_frequencies.iter().all(|&frequency| frequency == 0) {
        distance_frequencies[0] = 1;
    }

    let literal_code = PrefixEncoding::from_frequencies(&literal_frequencies)?;
    let command_code = PrefixEncoding::from_frequencies(&command_frequencies)?;
    let distance_code = PrefixEncoding::from_frequencies(&distance_frequencies)?;
    let plan = EncodingPlan {
        input,
        commands: &commands,
        tail,
        tail_command,
        command_code: &command_code,
        distance_code: &distance_code,
        distance_alphabet,
    };

    let single_tree = encode_single_tree(&plan, &literal_code);
    let Some(split_tree) = encode_literal_split(&plan, &literal_data) else {
        return Some(single_tree);
    };
    Some(if split_tree.len() < single_tree.len() {
        split_tree
    } else {
        single_tree
    })
}

fn encode_single_tree(plan: &EncodingPlan<'_>, literal_code: &PrefixEncoding) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, plan.input.len());
    write_simple_compressed_header(&mut writer, DIRECT_DISTANCE_CODES);
    literal_code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    plan.command_code
        .write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    plan.distance_code
        .write_tree(&mut writer, plan.distance_alphabet);

    for encoded in plan.commands {
        plan.command_code
            .write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_literal_slice(
            &mut writer,
            literal_code,
            &plan.input[encoded.parsed.insert_start
                ..encoded.parsed.insert_start + encoded.parsed.insert_length],
        );
        if let Some(distance) = encoded.distance {
            plan.distance_code
                .write_symbol(&mut writer, distance.symbol);
            distance.write_extra(&mut writer);
        }
    }

    if let Some(command) = plan.tail_command {
        plan.command_code.write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_literal_slice(&mut writer, literal_code, plan.tail);
    }

    writer.finish()
}

fn encode_literal_split(plan: &EncodingPlan<'_>, literals: &[u8]) -> Option<Vec<u8>> {
    let split = split_literals(literals);
    if split.num_types() == 1 {
        return None;
    }

    let literal_codes = split
        .histograms(literals)
        .into_iter()
        .map(|histogram| PrefixEncoding::from_frequencies(&histogram))
        .collect::<Option<Vec<_>>>()?;
    let split_encoding = BlockSplitEncoding::new(split)?;

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, plan.input.len());
    split_encoding.write_header(&mut writer);
    write_var_len_u8(&mut writer, 0); // one command block type
    write_var_len_u8(&mut writer, 0); // one distance block type
    writer.write_bits(0, 2); // NPOSTFIX
    writer.write_bits(u64::from(DIRECT_DISTANCE_CODES), 4); // NDIRECT
    for _ in 0..split_encoding.num_types() {
        writer.write_bits(0, 2); // LSB6 literal context mode
    }
    write_trivial_context_map(&mut writer, literal_codes.len(), 6)?;
    write_var_len_u8(&mut writer, 0); // one distance tree

    for literal_code in &literal_codes {
        literal_code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    }
    plan.command_code
        .write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    plan.distance_code
        .write_tree(&mut writer, plan.distance_alphabet);

    let mut literal_cursor = split_encoding.cursor();
    for encoded in plan.commands {
        plan.command_code
            .write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_split_literal_slice(
            &mut writer,
            &mut literal_cursor,
            &literal_codes,
            &plan.input[encoded.parsed.insert_start
                ..encoded.parsed.insert_start + encoded.parsed.insert_length],
        );
        if let Some(distance) = encoded.distance {
            plan.distance_code
                .write_symbol(&mut writer, distance.symbol);
            distance.write_extra(&mut writer);
        }
    }

    if let Some(command) = plan.tail_command {
        plan.command_code.write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_split_literal_slice(&mut writer, &mut literal_cursor, &literal_codes, plan.tail);
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

fn write_split_literal_slice(
    writer: &mut BitWriter,
    cursor: &mut BlockCursor<'_>,
    codes: &[PrefixEncoding],
    literals: &[u8],
) {
    for &literal in literals {
        let tree = cursor.before_symbol(writer);
        codes[tree].write_symbol(writer, u16::from(literal));
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
    fn implicit_last_distance_round_trips() {
        let source = b"abcdabcdabcd";
        let encoded = try_compress(source).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn literal_block_split_stream_round_trips() {
        let mut source = b"alpha beta gamma delta epsilon zeta eta theta ".repeat(256);
        source.extend(b"function(x){return x*x+17;} const value = 123456789; ".repeat(256));
        let encoded = try_compress(&source).unwrap();
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
}
