mod block_split;

use block_split::{
    BlockCursor, BlockSplitEncoding, GreedyBlockSplitter, SplitResult, write_context_map,
    write_trivial_context_map,
};

use crate::decode::context::LiteralContextMode;

use super::bit_writer::BitWriter;
use super::command::{ExplicitCommand, InsertCommand};
use super::distance::{DistanceCode, alphabet_size};
use super::match_finder::{MatchCommand, MatchFinder, create_backward_references};
use super::prefix_code::PrefixEncoding;
use super::{
    COMMAND_ALPHABET_SIZE, EncoderConfig, LITERAL_ALPHABET_SIZE, MAX_META_BLOCK_SIZE,
    nibbles_for_length, write_compressed_metablock_header, write_final_empty_metablock,
    write_simple_compressed_header, write_uncompressed_metablock, write_window_bits,
};

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

struct GreedySplits {
    literal: SplitResult,
    command: SplitResult,
    distance: SplitResult,
}

/// One metablock of a greedy parse over `[chunk_start, chunk_end)`: the parsed
/// commands with stream-absolute positions, plus everything needed to serialize
/// the chunk as a single-tree or block-split metablock.
struct EncodingBundle {
    config: EncoderConfig,
    literal_context: LiteralContextPlan,
    commands: Vec<EncodedMatch>,
    tail_start: usize,
    tail_command: Option<InsertCommand>,
    distance_alphabet: u16,
    literal_code: PrefixEncoding,
    command_code: PrefixEncoding,
    distance_code: PrefixEncoding,
    literal_frequencies: Vec<usize>,
    command_frequencies: Vec<usize>,
    distance_frequencies: Vec<usize>,
    splits: GreedySplits,
}

/// Builds the histograms, block splits, and prefix codes for the chunk
/// `[chunk_start, input.len())` of the stream `input`. `commands` holds the
/// parse results for that range; the pending literal run `[tail_start,
/// input.len())` becomes the trailing insert-only command.
fn build_bundle(
    input: &[u8],
    chunk_start: usize,
    config: EncoderConfig,
    commands: &[MatchCommand],
    tail_start: usize,
) -> Option<EncodingBundle> {
    debug_assert!(!commands.is_empty());
    let chunk_end = input.len();
    debug_assert!(chunk_start < chunk_end);
    debug_assert!(chunk_start <= tail_start && tail_start <= chunk_end);

    let literal_context = choose_q5_literal_context(&input[chunk_start..]);
    let direct_distance_codes = config.direct_distance_codes();
    let distance_postfix_bits = config.distance_postfix_bits();
    let distance_alphabet = alphabet_size(direct_distance_codes, distance_postfix_bits);
    let tail_length = chunk_end - tail_start;
    let literal_count = commands
        .iter()
        .map(|command| command.insert_length)
        .sum::<usize>()
        + tail_length;
    let command_count = commands.len() + usize::from(tail_length != 0);
    let distance_count = commands
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
    let mut encoded = Vec::with_capacity(commands.len());

    for parsed in commands {
        let command = ExplicitCommand::for_insert_and_copy_code(
            parsed.insert_length,
            parsed.copy_length_code,
            parsed.distance_code == 0,
        );
        let distance = command.requires_distance().then(|| {
            DistanceCode::for_code(
                parsed.distance_code,
                direct_distance_codes,
                distance_postfix_bits,
            )
        });
        command_frequencies[usize::from(command.symbol)] += 1;
        command_splitter.add_symbol(usize::from(command.symbol));
        if let Some(distance) = distance {
            distance_frequencies[usize::from(distance.symbol)] += 1;
            distance_splitter.add_symbol(usize::from(distance.symbol));
        }
        collect_literals(
            input,
            parsed.insert_start,
            parsed.insert_start + parsed.insert_length,
            literal_context,
            &mut literal_frequencies,
            &mut literal_splitter,
        );
        encoded.push(EncodedMatch {
            parsed: *parsed,
            command,
            distance,
        });
    }

    let tail_command = if tail_length == 0 {
        None
    } else {
        let command = InsertCommand::for_length(tail_length);
        command_frequencies[usize::from(command.symbol)] += 1;
        command_splitter.add_symbol(usize::from(command.symbol));
        collect_literals(
            input,
            tail_start,
            chunk_end,
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

    Some(EncodingBundle {
        config,
        literal_context,
        commands: encoded,
        tail_start,
        tail_command,
        distance_alphabet,
        literal_code,
        command_code,
        distance_code,
        literal_frequencies,
        command_frequencies,
        distance_frequencies,
        splits,
    })
}

impl EncodingBundle {
    /// Exact bit cost of the single-tree metablock: header, trees (measured by
    /// serializing them), and data (measured from the frequency tables).
    fn single_body_bits(&self, is_last: bool, length: usize) -> usize {
        let mut writer = BitWriter::default();
        write_compressed_metablock_header(&mut writer, length, is_last);
        write_simple_compressed_header(
            &mut writer,
            self.config.direct_distance_codes(),
            self.config.distance_postfix_bits(),
        );
        self.literal_code
            .write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
        self.command_code
            .write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
        self.distance_code
            .write_tree(&mut writer, self.distance_alphabet);

        let mut bits = writer.bit_len();
        bits += self.literal_code.data_bits(&self.literal_frequencies);
        bits += self.command_code.data_bits(&self.command_frequencies);
        bits += self.distance_code.data_bits(&self.distance_frequencies);
        for encoded in &self.commands {
            bits += usize::from(encoded.command.extra_bit_count());
            if let Some(distance) = encoded.distance {
                bits += usize::from(distance.extra_bit_count());
            }
        }
        if let Some(command) = self.tail_command {
            bits += usize::from(command.extra_bit_count());
        }
        bits
    }

    fn write_single(&self, writer: &mut BitWriter, input: &[u8], is_last: bool, length: usize) {
        write_compressed_metablock_header(writer, length, is_last);
        write_simple_compressed_header(
            writer,
            self.config.direct_distance_codes(),
            self.config.distance_postfix_bits(),
        );
        self.literal_code.write_tree(writer, LITERAL_ALPHABET_SIZE);
        self.command_code.write_tree(writer, COMMAND_ALPHABET_SIZE);
        self.distance_code
            .write_tree(writer, self.distance_alphabet);

        for encoded in &self.commands {
            self.command_code
                .write_symbol(writer, encoded.command.symbol);
            encoded.command.write_extra(writer);
            write_literal_slice(
                writer,
                &self.literal_code,
                &input[encoded.parsed.insert_start
                    ..encoded.parsed.insert_start + encoded.parsed.insert_length],
            );
            if let Some(distance) = encoded.distance {
                self.distance_code.write_symbol(writer, distance.symbol);
                distance.write_extra(writer);
            }
        }

        if let Some(command) = self.tail_command {
            self.command_code.write_symbol(writer, command.symbol);
            command.write_extra(writer);
            write_literal_slice(writer, &self.literal_code, &input[self.tail_start..]);
        }
    }

    fn write_split(
        &self,
        writer: &mut BitWriter,
        input: &[u8],
        is_last: bool,
        length: usize,
    ) -> Option<()> {
        let GreedySplits {
            literal: literal_result,
            command: command_result,
            distance: distance_result,
        } = &self.splits;

        let literal_codes = prefix_codes(&literal_result.histograms, LITERAL_ALPHABET_SIZE)?;
        let command_codes = prefix_codes(&command_result.histograms, COMMAND_ALPHABET_SIZE)?;
        let distance_codes = prefix_codes(&distance_result.histograms, self.distance_alphabet)?;
        let literal_split = BlockSplitEncoding::new(literal_result.split.clone())?;
        let command_split = BlockSplitEncoding::new(command_result.split.clone())?;
        let distance_split = BlockSplitEncoding::new(distance_result.split.clone())?;

        write_compressed_metablock_header(writer, length, is_last);
        literal_split.write_header(writer);
        command_split.write_header(writer);
        distance_split.write_header(writer);
        writer.write_bits(u64::from(self.config.distance_postfix_bits()), 2); // NPOSTFIX
        // The wire field stores NDIRECT >> NPOSTFIX.
        writer.write_bits(
            u64::from(self.config.direct_distance_codes() >> self.config.distance_postfix_bits()),
            4,
        ); // NDIRECT
        for _ in 0..literal_split.num_types() {
            writer.write_bits(UTF8_LITERAL_CONTEXT_MODE, 2);
        }
        write_literal_context_map(
            writer,
            literal_split.num_types(),
            literal_codes.len(),
            self.literal_context,
        )?;
        write_trivial_context_map(writer, distance_codes.len(), 2)?;

        for code in &literal_codes {
            code.write_tree(writer, LITERAL_ALPHABET_SIZE);
        }
        for code in &command_codes {
            code.write_tree(writer, COMMAND_ALPHABET_SIZE);
        }
        for code in &distance_codes {
            code.write_tree(writer, self.distance_alphabet);
        }

        let mut literal_cursor = literal_split.cursor();
        let mut command_cursor = command_split.cursor();
        let mut distance_cursor = distance_split.cursor();
        for encoded in &self.commands {
            let command_tree = command_cursor.before_symbol(writer);
            command_codes[command_tree].write_symbol(writer, encoded.command.symbol);
            encoded.command.write_extra(writer);
            write_split_literal_range(
                writer,
                &mut literal_cursor,
                &literal_codes,
                input,
                encoded.parsed.insert_start,
                encoded.parsed.insert_start + encoded.parsed.insert_length,
                self.literal_context,
            );
            if let Some(distance) = encoded.distance {
                let distance_tree = distance_cursor.before_symbol(writer);
                distance_codes[distance_tree].write_symbol(writer, distance.symbol);
                distance.write_extra(writer);
            }
        }

        if let Some(command) = self.tail_command {
            let command_tree = command_cursor.before_symbol(writer);
            command_codes[command_tree].write_symbol(writer, command.symbol);
            command.write_extra(writer);
            write_split_literal_range(
                writer,
                &mut literal_cursor,
                &literal_codes,
                input,
                self.tail_start,
                input.len(),
                self.literal_context,
            );
        }

        Some(())
    }

    /// Serializes the smaller of the two compressed forms into `writer`.
    /// `prefix_bits` is the bit length of the stream header the caller already
    /// wrote; both candidates are compared as whole-stream byte sizes so the
    /// single-metablock choice matches the legacy standalone-encoder choice.
    fn write_best(
        &self,
        writer: &mut BitWriter,
        input: &[u8],
        is_last: bool,
        length: usize,
        prefix_bits: usize,
    ) {
        let mut scratch = BitWriter::default();
        let split_serialized = self
            .write_split(&mut scratch, input, is_last, length)
            .is_some();
        let split_bytes = (prefix_bits + scratch.bit_len()).div_ceil(8);
        let single_bytes = (prefix_bits + self.single_body_bits(is_last, length)).div_ceil(8);

        if split_serialized && split_bytes < single_bytes {
            writer.append_writer(scratch);
        } else {
            self.write_single(writer, input, is_last, length);
        }
    }
}

pub(super) fn try_compress(input: &[u8], config: EncoderConfig) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let parse = create_backward_references(
        input,
        config.max_backward_distance(),
        config.max_distance(),
        config.search_depth(),
        config.max_lazy_delays(),
    );
    if parse.commands.is_empty() {
        return None;
    }

    let bundle = build_bundle(input, 0, config, &parse.commands, parse.tail_start)?;

    #[cfg(test)]
    {
        let mut probe = BitWriter::default();
        bundle.write_single(&mut probe, input, true, input.len());
        assert_eq!(
            bundle.single_body_bits(true, input.len()),
            probe.bit_len(),
            "single-tree bit estimate must match serialized size"
        );
    }

    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, config.window_bits());
    bundle.write_best(
        &mut writer,
        input,
        true,
        input.len(),
        window_header_bits(config.window_bits()),
    );
    Some(writer.finish())
}

/// Compresses an input larger than one metablock as a stream of greedy
/// metablocks, each at most `MAX_META_BLOCK_SIZE` bytes. The match finder
/// state (hash index and recent distances) persists across metablocks so
/// matches reference earlier output; per chunk the smaller of the compressed
/// and stored forms is emitted, restoring the distance-cache snapshot when a
/// chunk is stored.
/// Emits one metablock covering the next chunk of `window` (starting at
/// `chunk_start`, capped at `MAX_META_BLOCK_SIZE`), appending the chosen form
/// to `writer` using `finder`'s persistent hash index and distance ring.
/// Returns the window-relative end of the emitted range. When the stored form
/// wins, the distance-cache snapshot is restored, mirroring upstream
/// `saved_dist_cache`.
pub(super) fn emit_chunk(
    window: &[u8],
    chunk_start: usize,
    finder: &mut MatchFinder,
    writer: &mut BitWriter,
    config: EncoderConfig,
    is_last: bool,
) -> usize {
    let chunk_end = (chunk_start + MAX_META_BLOCK_SIZE).min(window.len());
    let snapshot = finder.snapshot_distances();
    let parse = finder.parse_chunk(&window[..chunk_end], chunk_start);

    let compressed = if parse.commands.is_empty() {
        None
    } else {
        build_bundle(
            &window[..chunk_end],
            chunk_start,
            config,
            &parse.commands,
            parse.tail_start,
        )
        .map(|bundle| {
            let mut scratch = BitWriter::default();
            bundle.write_best(
                &mut scratch,
                &window[..chunk_end],
                is_last,
                chunk_end - chunk_start,
                0,
            );
            scratch
        })
    };

    match compressed {
        Some(scratch) if scratch.bit_len() < stored_chunk_bits(chunk_end - chunk_start) => {
            writer.append_writer(scratch);
        }
        _ => {
            finder.restore_distances(snapshot);
            write_uncompressed_metablock(writer, &window[chunk_start..chunk_end]);
            if is_last {
                write_final_empty_metablock(writer);
            }
        }
    }
    chunk_end
}

/// Compresses an input larger than one metablock as a stream of greedy
/// metablocks, each at most `MAX_META_BLOCK_SIZE` bytes. The match finder
/// state (hash index and recent distances) persists across metablocks so
/// matches reference earlier output; per chunk the smaller of the compressed
/// and stored forms is emitted, restoring the distance-cache snapshot when a
/// chunk is stored.
pub(super) fn try_compress_stream(input: &[u8], config: EncoderConfig) -> Option<Vec<u8>> {
    if input.len() <= MAX_META_BLOCK_SIZE {
        return None;
    }

    let mut finder = MatchFinder::new(
        config.max_backward_distance(),
        config.max_distance(),
        config.search_depth(),
        config.max_lazy_delays(),
    );
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, config.window_bits());
    let mut chunk_start = 0_usize;

    while chunk_start < input.len() {
        chunk_start = emit_chunk(
            input,
            chunk_start,
            &mut finder,
            &mut writer,
            config,
            chunk_start + MAX_META_BLOCK_SIZE >= input.len(),
        );
    }

    Some(writer.finish())
}

/// Bit cost of a stored metabock: ISLAST + MNIBBLES + MLEN + ISUNCOMPRESSED,
/// zero-padded to a byte boundary, plus the raw data.
fn stored_chunk_bits(length: usize) -> usize {
    let header = 4 + 4 * usize::from(nibbles_for_length(length));
    let padding = (8 - header % 8) % 8;
    header + padding + 8 * length
}

fn window_header_bits(window_bits: u8) -> usize {
    match window_bits {
        16 => 1,
        18..=24 => 4,
        17 => 7,
        10..=15 => 7,
        _ => panic!("RFC 7932 window bits must be in 10..=24"),
    }
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

fn prefix_codes(histograms: &[Vec<usize>], alphabet_size: u16) -> Option<Vec<PrefixEncoding>> {
    histograms
        .iter()
        .map(|histogram| {
            debug_assert_eq!(histogram.len(), usize::from(alphabet_size));
            let mut histogram = histogram.clone();
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
    use super::super::{DEFAULT_WINDOW_BITS, EncoderConfig, MAX_META_BLOCK_SIZE, compress_stored};
    use super::{
        COMPLEX_UTF8_CONTEXT_MAP, SIMPLE_UTF8_CONTEXT_MAP, choose_q5_literal_context,
        stored_chunk_bits, try_compress, try_compress_stream, window_header_bits,
    };
    use crate::decompress;

    fn default_config() -> EncoderConfig {
        EncoderConfig::new(DEFAULT_WINDOW_BITS, 5, crate::EncoderMode::Generic).unwrap()
    }

    #[test]
    fn greedy_stream_round_trips_text() {
        let source = b"the quick brown fox jumps over the lazy dog; the quick brown fox jumps over the lazy dog.";
        let encoded = try_compress(source, default_config()).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn greedy_stream_round_trips_mixed_distances() {
        let source =
            b"alpha beta gamma alpha beta delta alpha beta gamma alpha beta delta".repeat(64);
        let encoded = try_compress(&source, default_config()).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn implicit_last_distance_round_trips() {
        let source = b"abcdabcdabcd";
        let encoded = try_compress(source, default_config()).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn q5_block_split_stream_round_trips() {
        let mut source = b"alpha beta gamma delta epsilon zeta eta theta ".repeat(256);
        source.extend(b"function(x){return x*x+17;} const value = 123456789; ".repeat(256));
        let encoded = try_compress(&source, default_config()).unwrap();
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
        let encoded = try_compress(&source, default_config()).unwrap();
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
    fn window_header_bit_counts_match_serialized_headers() {
        for window_bits in 10..=24 {
            let mut writer = super::super::bit_writer::BitWriter::default();
            super::write_window_bits(&mut writer, window_bits);
            assert_eq!(writer.bit_len(), window_header_bits(window_bits));
        }
    }

    #[test]
    fn stored_chunk_cost_matches_serialized_metablock() {
        for length in [
            1,
            2,
            0x1_0000,
            0x1_0001,
            0x10_0000,
            0x10_0001,
            MAX_META_BLOCK_SIZE,
        ] {
            let mut writer = super::super::bit_writer::BitWriter::default();
            super::super::write_uncompressed_metablock(&mut writer, &vec![0_u8; length]);
            assert_eq!(writer.bit_len(), stored_chunk_bits(length));
        }
    }

    #[test]
    fn multi_metablock_stream_round_trips() {
        let unit = b"the quick brown fox jumps over the lazy dog. ".repeat(96);
        let mut source = unit.repeat(MAX_META_BLOCK_SIZE / unit.len() + 2);
        source.truncate(MAX_META_BLOCK_SIZE + 4096);

        let encoded = try_compress_stream(&source, default_config()).unwrap();
        assert!(encoded.len() < source.len() / 10);
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);

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
    fn multi_metablock_stream_falls_back_to_stored_chunks() {
        // Pseudo-random bytes defeat literal coding (compressed exceeds stored
        // once tree overhead is counted), so every metablock is emitted stored
        // and the distance-cache snapshots are restored per chunk.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut source = vec![0_u8; MAX_META_BLOCK_SIZE + 64];
        for chunk in source.as_chunks_mut::<8>().0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes());
        }

        let encoded = try_compress_stream(&source, default_config()).unwrap();
        assert_eq!(encoded, compress_stored(&source, default_config()));
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);

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
