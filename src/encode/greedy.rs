mod block_split;

use block_split::{
    BlockCursor, BlockSplitEncoding, split_commands, split_distances, split_literals,
    write_trivial_context_map,
};

use super::bit_writer::BitWriter;
use super::command::{ExplicitCommand, InsertCommand};
use super::distance::{DistanceCode, alphabet_size};
use super::match_finder::{MatchCommand, create_backward_references};
use super::prefix_code::PrefixEncoding;
use super::{
    COMMAND_ALPHABET_SIZE, LITERAL_ALPHABET_SIZE, MAX_META_BLOCK_SIZE,
    write_final_compressed_header, write_simple_compressed_header, write_window_bits,
};

const GREEDY_DIRECT_DISTANCE_CODES: u16 = 0;
const UTF8_LITERAL_CONTEXT_MODE: u64 = 2;

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

    let distance_alphabet = alphabet_size(GREEDY_DIRECT_DISTANCE_CODES);
    let mut literal_frequencies = vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)];
    let mut command_frequencies = vec![0_usize; usize::from(COMMAND_ALPHABET_SIZE)];
    let mut distance_frequencies = vec![0_usize; usize::from(distance_alphabet)];
    let mut commands = Vec::with_capacity(parse.commands.len());
    let mut literal_data = Vec::new();
    let mut command_data = Vec::with_capacity(parse.commands.len() + 1);
    let mut distance_data = Vec::new();

    for parsed in parse.commands {
        let command = ExplicitCommand::for_insert_and_copy_code(
            parsed.insert_length,
            parsed.copy_length_code,
            parsed.distance_code == 0,
        );
        let distance = command
            .requires_distance()
            .then(|| DistanceCode::for_code(parsed.distance_code, GREEDY_DIRECT_DISTANCE_CODES));
        command_frequencies[usize::from(command.symbol)] += 1;
        command_data.push(command.symbol);
        if let Some(distance) = distance {
            distance_frequencies[usize::from(distance.symbol)] += 1;
            distance_data.push(distance.symbol);
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
        command_data.push(command.symbol);
        count_literals(&mut literal_frequencies, tail);
        literal_data.extend_from_slice(tail);
        Some(command)
    };

    seed_empty_histogram(&mut literal_frequencies);
    seed_empty_histogram(&mut distance_frequencies);

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
    let split_tree = encode_greedy_splits(&plan, &literal_data, &command_data, &distance_data)?;
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
    write_simple_compressed_header(&mut writer, GREEDY_DIRECT_DISTANCE_CODES);
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

fn encode_greedy_splits(
    plan: &EncodingPlan<'_>,
    literals: &[u8],
    command_symbols: &[u16],
    distance_symbols: &[u16],
) -> Option<Vec<u8>> {
    let literal_result = split_literals(literals);
    let command_result = split_commands(command_symbols);
    let distance_result = split_distances(distance_symbols, usize::from(plan.distance_alphabet));

    let literal_codes = prefix_codes(literal_result.histograms, LITERAL_ALPHABET_SIZE)?;
    let command_codes = prefix_codes(command_result.histograms, COMMAND_ALPHABET_SIZE)?;
    let distance_codes = prefix_codes(distance_result.histograms, plan.distance_alphabet)?;
    let literal_split = BlockSplitEncoding::new(literal_result.split)?;
    let command_split = BlockSplitEncoding::new(command_result.split)?;
    let distance_split = BlockSplitEncoding::new(distance_result.split)?;

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, plan.input.len());
    literal_split.write_header(&mut writer);
    command_split.write_header(&mut writer);
    distance_split.write_header(&mut writer);
    writer.write_bits(0, 2); // NPOSTFIX
    writer.write_bits(0, 4); // NDIRECT
    for _ in 0..literal_split.num_types() {
        writer.write_bits(UTF8_LITERAL_CONTEXT_MODE, 2);
    }
    write_trivial_context_map(&mut writer, literal_codes.len(), 6)?;
    write_trivial_context_map(&mut writer, distance_codes.len(), 2)?;

    for code in &literal_codes {
        code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    }
    for code in &command_codes {
        code.write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    }
    for code in &distance_codes {
        code.write_tree(&mut writer, plan.distance_alphabet);
    }

    let mut literal_cursor = literal_split.cursor();
    let mut command_cursor = command_split.cursor();
    let mut distance_cursor = distance_split.cursor();
    for encoded in plan.commands {
        let command_tree = command_cursor.before_symbol(&mut writer);
        command_codes[command_tree].write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_split_literal_slice(
            &mut writer,
            &mut literal_cursor,
            &literal_codes,
            &plan.input[encoded.parsed.insert_start
                ..encoded.parsed.insert_start + encoded.parsed.insert_length],
        );
        if let Some(distance) = encoded.distance {
            let distance_tree = distance_cursor.before_symbol(&mut writer);
            distance_codes[distance_tree].write_symbol(&mut writer, distance.symbol);
            distance.write_extra(&mut writer);
        }
    }

    if let Some(command) = plan.tail_command {
        let command_tree = command_cursor.before_symbol(&mut writer);
        command_codes[command_tree].write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_split_literal_slice(&mut writer, &mut literal_cursor, &literal_codes, plan.tail);
    }

    Some(writer.finish())
}

fn prefix_codes(histograms: Vec<Vec<usize>>, alphabet_size: u16) -> Option<Vec<PrefixEncoding>> {
    histograms
        .into_iter()
        .map(|mut histogram| {
            debug_assert_eq!(histogram.len(), usize::from(alphabet_size));
            seed_empty_histogram(&mut histogram);
            PrefixEncoding::from_frequencies(&histogram)
        })
        .collect()
}

fn seed_empty_histogram(frequencies: &mut [usize]) {
    if frequencies.iter().all(|&frequency| frequency == 0) {
        frequencies[0] = 1;
    }
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
    fn q5_block_split_stream_round_trips() {
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
