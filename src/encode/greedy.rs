use super::bit_writer::BitWriter;
use super::block_split::TwoBlockPartition;
use super::command::{ExplicitCommand, InsertCommand};
use super::distance::{DistanceCode, RecentDistances, alphabet_size};
use super::match_finder::{MatchCommand, greedy_parse};
use super::prefix_code::{
    PrefixEncoding, write_simple_prefix_code, write_simple_symbol, write_var_len_u8,
};
use super::{
    COMMAND_ALPHABET_SIZE, DIRECT_DISTANCE_CODES, LITERAL_ALPHABET_SIZE, MAX_META_BLOCK_SIZE,
    write_final_compressed_header, write_simple_compressed_header, write_window_bits,
};

const SPLIT_GRANULARITY: usize = 8;
const MIN_SPLIT_SAVINGS_BITS: usize = 256;
const LITERAL_CONTEXT_COUNT: usize = 64;

#[derive(Debug, Clone, Copy)]
struct EncodedMatch {
    parsed: MatchCommand,
    command: ExplicitCommand,
    distance: DistanceCode,
}

#[derive(Debug)]
struct PreparedStream {
    commands: Vec<EncodedMatch>,
    tail_start: usize,
    tail_command: Option<InsertCommand>,
    literal_frequencies: Vec<usize>,
    command_frequencies: Vec<usize>,
    distance_frequencies: Vec<usize>,
    distance_alphabet: u16,
}

#[derive(Debug)]
struct StreamCodes {
    literal: PrefixEncoding,
    command: PrefixEncoding,
    distance: PrefixEncoding,
}

#[derive(Debug)]
struct SplitModel {
    boundary: usize,
    literal_codes: [PrefixEncoding; 2],
    command_codes: [PrefixEncoding; 2],
    literal_partition: TwoBlockPartition,
    command_partition: TwoBlockPartition,
}

pub(super) fn try_compress(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let prepared = prepare_stream(input)?;
    let codes = StreamCodes {
        literal: PrefixEncoding::from_frequencies(&prepared.literal_frequencies)?,
        command: PrefixEncoding::from_frequencies(&prepared.command_frequencies)?,
        distance: PrefixEncoding::from_frequencies(&prepared.distance_frequencies)?,
    };

    let baseline = encode_baseline(input, &prepared, &codes);
    let Some(model) = build_split_model(input, &prepared, &codes) else {
        return Some(baseline);
    };
    let split = encode_split(input, &prepared, &codes.distance, &model);
    if split.len() < baseline.len() {
        Some(split)
    } else {
        Some(baseline)
    }
}

fn prepare_stream(input: &[u8]) -> Option<PreparedStream> {
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
        let distance = recent_distances.encode(parsed.distance, DIRECT_DISTANCE_CODES);
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

    let tail_start = parse.tail_start;
    let tail = &input[tail_start..];
    let tail_command = if tail.is_empty() {
        None
    } else {
        let command = InsertCommand::for_length(tail.len());
        command_frequencies[usize::from(command.symbol)] += 1;
        count_literals(&mut literal_frequencies, tail);
        Some(command)
    };

    Some(PreparedStream {
        commands,
        tail_start,
        tail_command,
        literal_frequencies,
        command_frequencies,
        distance_frequencies,
        distance_alphabet,
    })
}

fn encode_baseline(input: &[u8], prepared: &PreparedStream, codes: &StreamCodes) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, input.len());
    write_simple_compressed_header(&mut writer, DIRECT_DISTANCE_CODES);
    codes.literal.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    codes.command.write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    codes
        .distance
        .write_tree(&mut writer, prepared.distance_alphabet);

    for encoded in &prepared.commands {
        codes.command.write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_literal_slice(
            &mut writer,
            &codes.literal,
            &input[encoded.parsed.insert_start
                ..encoded.parsed.insert_start + encoded.parsed.insert_length],
        );
        codes
            .distance
            .write_symbol(&mut writer, encoded.distance.symbol);
        encoded.distance.write_extra(&mut writer);
    }

    if let Some(command) = prepared.tail_command {
        codes.command.write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_literal_slice(&mut writer, &codes.literal, &input[prepared.tail_start..]);
    }

    writer.finish()
}

fn build_split_model(
    input: &[u8],
    prepared: &PreparedStream,
    baseline: &StreamCodes,
) -> Option<SplitModel> {
    let event_count = prepared.commands.len() + usize::from(prepared.tail_command.is_some());
    if event_count < 2 {
        return None;
    }

    let candidates = split_candidates(event_count);
    if candidates.is_empty() {
        return None;
    }

    let baseline_bits = baseline.literal.data_bits(&prepared.literal_frequencies)
        + baseline.command.data_bits(&prepared.command_frequencies);
    let total_literals: usize = prepared.literal_frequencies.iter().sum();
    let mut left_literals = vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)];
    let mut right_literals = prepared.literal_frequencies.clone();
    let mut left_commands = vec![0_usize; usize::from(COMMAND_ALPHABET_SIZE)];
    let mut right_commands = prepared.command_frequencies.clone();
    let mut left_literal_count = 0_usize;
    let mut best = None;

    for event_index in 0..event_count {
        let (command_symbol, literal_start, literal_end) = event(input, prepared, event_index);
        left_commands[usize::from(command_symbol)] += 1;
        right_commands[usize::from(command_symbol)] -= 1;
        for &literal in &input[literal_start..literal_end] {
            let symbol = usize::from(literal);
            left_literals[symbol] += 1;
            right_literals[symbol] -= 1;
            left_literal_count += 1;
        }

        let boundary = event_index + 1;
        if candidates.binary_search(&boundary).is_err() {
            continue;
        }
        let right_literal_count = total_literals - left_literal_count;
        if left_literal_count == 0 || right_literal_count == 0 {
            continue;
        }

        let literal_codes = [
            PrefixEncoding::from_frequencies(&left_literals)?,
            PrefixEncoding::from_frequencies(&right_literals)?,
        ];
        let command_codes = [
            PrefixEncoding::from_frequencies(&left_commands)?,
            PrefixEncoding::from_frequencies(&right_commands)?,
        ];
        let split_bits = literal_codes[0].data_bits(&left_literals)
            + literal_codes[1].data_bits(&right_literals)
            + command_codes[0].data_bits(&left_commands)
            + command_codes[1].data_bits(&right_commands);

        if baseline_bits < split_bits.saturating_add(MIN_SPLIT_SAVINGS_BITS) {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|choice: &SplitChoice| split_bits < choice.data_bits)
        {
            best = Some(SplitChoice {
                boundary,
                left_literal_count,
                data_bits: split_bits,
                literal_codes,
                command_codes,
            });
        }
    }

    let best = best?;
    let right_literal_count = total_literals - best.left_literal_count;
    Some(SplitModel {
        boundary: best.boundary,
        literal_codes: best.literal_codes,
        command_codes: best.command_codes,
        literal_partition: TwoBlockPartition::new(best.left_literal_count, right_literal_count)?,
        command_partition: TwoBlockPartition::new(best.boundary, event_count - best.boundary)?,
    })
}

#[derive(Debug)]
struct SplitChoice {
    boundary: usize,
    left_literal_count: usize,
    data_bits: usize,
    literal_codes: [PrefixEncoding; 2],
    command_codes: [PrefixEncoding; 2],
}

fn split_candidates(event_count: usize) -> Vec<usize> {
    let mut candidates = Vec::with_capacity(SPLIT_GRANULARITY - 1);
    for part in 1..SPLIT_GRANULARITY {
        let boundary = event_count * part / SPLIT_GRANULARITY;
        if boundary != 0
            && boundary != event_count
            && candidates.last().copied() != Some(boundary)
        {
            candidates.push(boundary);
        }
    }
    candidates
}

fn event(
    input: &[u8],
    prepared: &PreparedStream,
    index: usize,
) -> (u16, usize, usize) {
    if let Some(encoded) = prepared.commands.get(index) {
        let start = encoded.parsed.insert_start;
        return (
            encoded.command.symbol,
            start,
            start + encoded.parsed.insert_length,
        );
    }

    debug_assert_eq!(index, prepared.commands.len());
    let command = prepared
        .tail_command
        .expect("event beyond match commands is the literal tail");
    (command.symbol, prepared.tail_start, input.len())
}

fn encode_split(
    input: &[u8],
    prepared: &PreparedStream,
    distance_code: &PrefixEncoding,
    model: &SplitModel,
) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, input.len());
    write_split_header(&mut writer, &model.literal_partition, &model.command_partition);
    for code in &model.literal_codes {
        code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    }
    for code in &model.command_codes {
        code.write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    }
    distance_code.write_tree(&mut writer, prepared.distance_alphabet);

    let mut literal_switched = false;
    for (index, encoded) in prepared.commands.iter().enumerate() {
        let region = usize::from(index >= model.boundary);
        if index == model.boundary {
            model.command_partition.write_switch(&mut writer);
        }
        model.command_codes[region].write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_split_literals(
            &mut writer,
            input,
            encoded.parsed.insert_start,
            encoded.parsed.insert_start + encoded.parsed.insert_length,
            region,
            model,
            &mut literal_switched,
        );
        distance_code.write_symbol(&mut writer, encoded.distance.symbol);
        encoded.distance.write_extra(&mut writer);
    }

    if let Some(command) = prepared.tail_command {
        let index = prepared.commands.len();
        let region = usize::from(index >= model.boundary);
        if index == model.boundary {
            model.command_partition.write_switch(&mut writer);
        }
        model.command_codes[region].write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_split_literals(
            &mut writer,
            input,
            prepared.tail_start,
            input.len(),
            region,
            model,
            &mut literal_switched,
        );
    }

    writer.finish()
}

fn write_split_header(
    writer: &mut BitWriter,
    literal_partition: &TwoBlockPartition,
    command_partition: &TwoBlockPartition,
) {
    write_var_len_u8(writer, 1);
    literal_partition.write_header(writer);
    write_var_len_u8(writer, 1);
    command_partition.write_header(writer);
    write_var_len_u8(writer, 0);
    writer.write_bits(0, 2);
    writer.write_bits(u64::from(DIRECT_DISTANCE_CODES), 4);
    writer.write_bits(0, 4);
    write_var_len_u8(writer, 1);
    write_literal_block_context_map(writer);
    write_var_len_u8(writer, 0);
}

fn write_literal_block_context_map(writer: &mut BitWriter) {
    writer.write_bits(0, 1);
    write_simple_prefix_code(writer, &[0, 1], 2);
    for tree in 0..2 {
        for _ in 0..LITERAL_CONTEXT_COUNT {
            write_simple_symbol(writer, tree, 2);
        }
    }
    writer.write_bits(0, 1);
}

fn write_split_literals(
    writer: &mut BitWriter,
    input: &[u8],
    start: usize,
    end: usize,
    region: usize,
    model: &SplitModel,
    literal_switched: &mut bool,
) {
    if start == end {
        return;
    }
    if region == 1 && !*literal_switched {
        model.literal_partition.write_switch(writer);
        *literal_switched = true;
    }
    for &literal in &input[start..end] {
        model.literal_codes[region].write_symbol(writer, u16::from(literal));
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

#[cfg(test)]
mod tests {
    use super::{split_candidates, try_compress};
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
    fn split_points_are_bounded_and_unique() {
        assert_eq!(split_candidates(2), [1]);
        assert_eq!(split_candidates(4), [1, 2, 3]);
        assert!(split_candidates(64).iter().all(|&point| point > 0 && point < 64));
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
