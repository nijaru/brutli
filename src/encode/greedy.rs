mod block_split;

use block_split::{
    BlockCursor, BlockSplitEncoding, GreedyBlockSplitter, SplitResult, write_context_map,
    write_trivial_context_map,
};

use crate::decode::context::LiteralContextMode;

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
const CONTEXT_SAMPLE_STRIDE: usize = 4096;
const CONTEXT_SAMPLE_LENGTH: usize = 64;
const COMPLEX_CONTEXT_MIN_SIZE: usize = 1 << 20;
const CONTEXT_SAVINGS_THRESHOLD: f64 = 0.2;

const NO_LITERAL_CONTEXT_MAP: [u8; 64] = [0; 64];
const SIMPLE_UTF8_CONTEXT_MAP: [u8; 64] = [
    0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const COMPLEX_UTF8_CONTEXT_MAP: [u8; 64] = [
    11, 11, 12, 12, 0, 0, 0, 0, 1, 1, 9, 9, 2, 2, 2, 2, 1, 1, 1, 1, 8, 3, 3, 3, 1, 1, 1, 1, 2, 2,
    2, 2, 8, 4, 4, 4, 8, 7, 4, 4, 8, 0, 0, 0, 3, 3, 3, 3, 5, 5, 10, 5, 5, 5, 10, 5, 6, 6, 6, 6, 6,
    6, 6, 6,
];

#[derive(Debug, Clone, Copy)]
struct EncodedMatch {
    parsed: MatchCommand,
    command: ExplicitCommand,
    distance: Option<DistanceCode>,
}

#[derive(Debug, Clone, Copy)]
struct LiteralContextPlan {
    count: usize,
    map: &'static [u8; 64],
}

struct EncodingPlan<'a> {
    input: &'a [u8],
    commands: &'a [EncodedMatch],
    tail: &'a [u8],
    tail_command: Option<InsertCommand>,
    command_code: &'a PrefixEncoding,
    distance_code: &'a PrefixEncoding,
    distance_alphabet: u16,
    literal_context: LiteralContextPlan,
}

struct GreedySplits {
    literal: SplitResult,
    command: SplitResult,
    distance: SplitResult,
}

pub(super) fn try_compress(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let parse = create_backward_references(input);
    if parse.commands.is_empty() {
        return None;
    }

    let literal_context = choose_q5_literal_context(input);
    let distance_alphabet = alphabet_size(GREEDY_DIRECT_DISTANCE_CODES);
    let tail_length = input.len() - parse.tail_start;
    let literal_count = parse
        .commands
        .iter()
        .map(|command| command.insert_length)
        .sum::<usize>()
        + tail_length;
    let command_count = parse.commands.len() + usize::from(tail_length != 0);
    let distance_count = parse
        .commands
        .iter()
        .filter(|parsed| {
            ExplicitCommand::for_insert_and_copy_code(
                parsed.insert_length,
                parsed.copy_length_code,
                parsed.distance_code == 0,
            )
            .requires_distance()
        })
        .count();

    let mut literal_splitter = if literal_context.count == 1 {
        GreedyBlockSplitter::literals(literal_count)
    } else {
        GreedyBlockSplitter::contextual_literals(literal_context.count, literal_count)
    };
    let mut command_splitter = GreedyBlockSplitter::commands(command_count);
    let mut distance_splitter =
        GreedyBlockSplitter::distances(usize::from(distance_alphabet), distance_count);

    let mut literal_frequencies = vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)];
    let mut command_frequencies = vec![0_usize; usize::from(COMMAND_ALPHABET_SIZE)];
    let mut distance_frequencies = vec![0_usize; usize::from(distance_alphabet)];
    let mut commands = Vec::with_capacity(parse.commands.len());

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
        command_splitter.add_symbol(usize::from(command.symbol));
        if let Some(distance) = distance {
            distance_frequencies[usize::from(distance.symbol)] += 1;
            distance_splitter.add_symbol(usize::from(distance.symbol));
        }
        let literal_start = parsed.insert_start;
        let literal_end = literal_start + parsed.insert_length;
        collect_literals(
            input,
            literal_start,
            literal_end,
            literal_context,
            &mut literal_frequencies,
            &mut literal_splitter,
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
        command_splitter.add_symbol(usize::from(command.symbol));
        collect_literals(
            input,
            parse.tail_start,
            input.len(),
            literal_context,
            &mut literal_frequencies,
            &mut literal_splitter,
        );
        Some(command)
    };
    let splits = GreedySplits {
        literal: literal_splitter.finish(),
        command: command_splitter.finish(),
        distance: distance_splitter.finish(),
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
        literal_context,
    };

    let single_tree_size = estimate_single_tree_size(
        &plan,
        &literal_code,
        &literal_frequencies,
        &command_frequencies,
        &distance_frequencies,
    );
    #[cfg(test)]
    assert_eq!(
        single_tree_size,
        encode_single_tree(&plan, &literal_code).len(),
        "single-tree bit estimate must match serialized size"
    );

    let split_tree = encode_greedy_splits(&plan, splits)?;
    Some(if split_tree.len() < single_tree_size {
        split_tree
    } else {
        encode_single_tree(&plan, &literal_code)
    })
}

fn estimate_single_tree_size(
    plan: &EncodingPlan<'_>,
    literal_code: &PrefixEncoding,
    literal_frequencies: &[usize],
    command_frequencies: &[usize],
    distance_frequencies: &[usize],
) -> usize {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, plan.input.len());
    write_simple_compressed_header(&mut writer, GREEDY_DIRECT_DISTANCE_CODES);
    literal_code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    plan.command_code
        .write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    plan.distance_code
        .write_tree(&mut writer, plan.distance_alphabet);

    let mut bits = writer.bit_len();
    bits += literal_code.data_bits(literal_frequencies);
    bits += plan.command_code.data_bits(command_frequencies);
    bits += plan.distance_code.data_bits(distance_frequencies);
    for encoded in plan.commands {
        bits += usize::from(encoded.command.extra_bit_count());
        if let Some(distance) = encoded.distance {
            bits += usize::from(distance.extra_bit_count());
        }
    }
    if let Some(command) = plan.tail_command {
        bits += usize::from(command.extra_bit_count());
    }
    bits.div_ceil(8)
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

fn encode_greedy_splits(plan: &EncodingPlan<'_>, splits: GreedySplits) -> Option<Vec<u8>> {
    let GreedySplits {
        literal: literal_result,
        command: command_result,
        distance: distance_result,
    } = splits;

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
    write_literal_context_map(
        &mut writer,
        literal_split.num_types(),
        literal_codes.len(),
        plan.literal_context,
    )?;
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
        write_split_literal_range(
            &mut writer,
            &mut literal_cursor,
            &literal_codes,
            plan.input,
            encoded.parsed.insert_start,
            encoded.parsed.insert_start + encoded.parsed.insert_length,
            plan.literal_context,
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
        write_split_literal_range(
            &mut writer,
            &mut literal_cursor,
            &literal_codes,
            plan.input,
            plan.input.len() - plan.tail.len(),
            plan.input.len(),
            plan.literal_context,
        );
    }

    Some(writer.finish())
}

fn choose_q5_literal_context(input: &[u8]) -> LiteralContextPlan {
    if input.len() < CONTEXT_SAMPLE_LENGTH {
        return LiteralContextPlan {
            count: 1,
            map: &NO_LITERAL_CONTEXT_MAP,
        };
    }
    if input.len() >= COMPLEX_CONTEXT_MIN_SIZE && should_use_complex_context(input) {
        return LiteralContextPlan {
            count: 13,
            map: &COMPLEX_UTF8_CONTEXT_MAP,
        };
    }

    const PREFIX_CLASS: [usize; 4] = [0, 0, 1, 2];
    let mut bigrams = [0_usize; 9];
    for start in (0..=input.len() - CONTEXT_SAMPLE_LENGTH).step_by(CONTEXT_SAMPLE_STRIDE) {
        let mut previous = PREFIX_CLASS[usize::from(input[start] >> 6)] * 3;
        for &literal in &input[start + 1..start + CONTEXT_SAMPLE_LENGTH] {
            let current = PREFIX_CLASS[usize::from(literal >> 6)];
            bigrams[previous + current] += 1;
            previous = current * 3;
        }
    }

    let mut monograms = [0_usize; 3];
    let mut two_prefix = [0_usize; 6];
    for (index, &count) in bigrams.iter().enumerate() {
        monograms[index % 3] += count;
        two_prefix[index % 6] += count;
    }
    let total = monograms.iter().sum::<usize>();
    debug_assert!(total != 0);
    let entropy_one = estimate_entropy(&monograms) / total as f64;
    let entropy_two =
        (estimate_entropy(&two_prefix[..3]) + estimate_entropy(&two_prefix[3..])) / total as f64;

    if entropy_one - entropy_two < CONTEXT_SAVINGS_THRESHOLD {
        LiteralContextPlan {
            count: 1,
            map: &NO_LITERAL_CONTEXT_MAP,
        }
    } else {
        LiteralContextPlan {
            count: 2,
            map: &SIMPLE_UTF8_CONTEXT_MAP,
        }
    }
}

fn should_use_complex_context(input: &[u8]) -> bool {
    let mut combined = [0_usize; 32];
    let mut contextual = [[0_usize; 32]; 13];
    let mut total = 0_usize;

    for start in (0..=input.len() - CONTEXT_SAMPLE_LENGTH).step_by(CONTEXT_SAMPLE_STRIDE) {
        let mut second_previous = input[start];
        let mut previous = input[start + 1];
        for &literal in &input[start + 2..start + CONTEXT_SAMPLE_LENGTH] {
            let context_id = LiteralContextMode::Utf8.id(previous, second_previous);
            let context = usize::from(COMPLEX_UTF8_CONTEXT_MAP[usize::from(context_id)]);
            let bucket = usize::from(literal >> 3);
            total += 1;
            combined[bucket] += 1;
            contextual[context][bucket] += 1;
            second_previous = previous;
            previous = literal;
        }
    }

    let entropy_one = estimate_entropy(&combined) / total as f64;
    let entropy_context = contextual
        .iter()
        .map(|histogram| estimate_entropy(histogram))
        .sum::<f64>()
        / total as f64;
    entropy_context <= 3.0 && entropy_one - entropy_context >= CONTEXT_SAVINGS_THRESHOLD
}

fn estimate_entropy(histogram: &[usize]) -> f64 {
    let total = histogram.iter().sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    let total_log = (total as f64).log2();
    histogram
        .iter()
        .filter(|&&count| count != 0)
        .map(|&count| count as f64 * (total_log - (count as f64).log2()))
        .sum()
}

fn collect_literals(
    input: &[u8],
    start: usize,
    end: usize,
    context_plan: LiteralContextPlan,
    frequencies: &mut [usize],
    splitter: &mut GreedyBlockSplitter,
) {
    for position in start..end {
        let literal = input[position];
        frequencies[usize::from(literal)] += 1;
        if context_plan.count == 1 {
            splitter.add_symbol(usize::from(literal));
        } else {
            let context_id = utf8_context_id(input, position);
            splitter.add_context_symbol(
                usize::from(literal),
                usize::from(context_plan.map[context_id]),
            );
        }
    }
}

fn write_literal_context_map(
    writer: &mut BitWriter,
    block_types: usize,
    literal_trees: usize,
    context_plan: LiteralContextPlan,
) -> Option<()> {
    if context_plan.count == 1 {
        return write_trivial_context_map(writer, literal_trees, 6);
    }

    let mut map = Vec::with_capacity(block_types * 64);
    for block_type in 0..block_types {
        let tree_offset = block_type * context_plan.count;
        for &context in context_plan.map {
            map.push(u8::try_from(tree_offset + usize::from(context)).ok()?);
        }
    }
    write_context_map(writer, &map, literal_trees)
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

fn utf8_context_id(input: &[u8], position: usize) -> usize {
    let previous = position.checked_sub(1).map_or(0, |index| input[index]);
    let second_previous = position.checked_sub(2).map_or(0, |index| input[index]);
    usize::from(LiteralContextMode::Utf8.id(previous, second_previous))
}

fn write_literal_slice(writer: &mut BitWriter, code: &PrefixEncoding, literals: &[u8]) {
    for &literal in literals {
        code.write_symbol(writer, u16::from(literal));
    }
}

fn write_split_literal_range(
    writer: &mut BitWriter,
    cursor: &mut BlockCursor<'_>,
    codes: &[PrefixEncoding],
    input: &[u8],
    start: usize,
    end: usize,
    context_plan: LiteralContextPlan,
) {
    for position in start..end {
        let block_type = cursor.before_symbol(writer);
        let context = if context_plan.count == 1 {
            0
        } else {
            context_plan.map[utf8_context_id(input, position)]
        };
        let tree = block_type * context_plan.count + usize::from(context);
        codes[tree].write_symbol(writer, u16::from(input[position]));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        COMPLEX_UTF8_CONTEXT_MAP, SIMPLE_UTF8_CONTEXT_MAP, choose_q5_literal_context, try_compress,
    };
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
    fn q5_context_maps_match_reference_shapes() {
        assert_eq!(&SIMPLE_UTF8_CONTEXT_MAP[..4], &[0, 0, 1, 1]);
        assert_eq!(
            SIMPLE_UTF8_CONTEXT_MAP
                .iter()
                .filter(|&&value| value == 1)
                .count(),
            2
        );
        assert_eq!(COMPLEX_UTF8_CONTEXT_MAP.iter().copied().max(), Some(12));
    }

    #[test]
    fn short_input_skips_literal_context_modeling() {
        assert_eq!(choose_q5_literal_context(&[b'a'; 63]).count, 1);
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
