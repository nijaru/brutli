use std::sync::OnceLock;

use super::super::bit_writer::BitWriter;
use super::super::prefix_code::{PrefixEncoding, write_var_len_u8};

const MAX_BLOCK_TYPES: usize = 256;
const BLOCK_LENGTH_ALPHABET_SIZE: usize = 26;
const MAX_CONTEXT_MAP_RUN_LENGTH_PREFIX: u32 = 6;
const CONTEXT_MAP_SYMBOL_BITS: u32 = 9;
const LOG2_TABLE_SIZE: usize = 256;

static LOG2_TABLE: OnceLock<[f64; LOG2_TABLE_SIZE]> = OnceLock::new();

const BLOCK_LENGTH_OFFSETS: [usize; BLOCK_LENGTH_ALPHABET_SIZE] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497, 753, 1265,
    2289, 4337, 8433, 16625,
];
const BLOCK_LENGTH_EXTRA_BITS: [u8; BLOCK_LENGTH_ALPHABET_SIZE] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 24,
];

#[derive(Debug, Clone)]
pub(super) struct BlockSplit {
    types: Vec<u8>,
    lengths: Vec<usize>,
    num_types: usize,
}

#[derive(Debug, Clone)]
pub(super) struct SplitResult {
    pub(super) split: BlockSplit,
    pub(super) histograms: Vec<Vec<usize>>,
}

#[derive(Debug, Clone)]
pub(super) struct BlockSplitEncoding {
    split: BlockSplit,
    type_code: Option<PrefixEncoding>,
    length_code: Option<PrefixEncoding>,
}

#[derive(Debug)]
pub(super) struct BlockCursor<'a> {
    encoding: &'a BlockSplitEncoding,
    block_index: usize,
    remaining: usize,
    type_calculator: BlockTypeCodeCalculator,
}

#[derive(Debug, Clone, Copy)]
struct BlockLengthCode {
    symbol: u16,
    extra: usize,
    extra_bits: u8,
}

#[derive(Debug, Clone, Copy)]
struct BlockTypeCodeCalculator {
    last_type: usize,
    second_last_type: usize,
}

#[derive(Debug)]
struct GreedyBlockSplitter {
    symbol_alphabet_size: usize,
    context_count: usize,
    min_block_size: usize,
    split_threshold: f64,
    max_block_types: usize,
    split: BlockSplit,
    histograms: Vec<Vec<usize>>,
    target_block_size: usize,
    block_size: usize,
    current_histogram: usize,
    last_histograms: [usize; 2],
    last_entropy: [f64; 2],
    merge_last_count: usize,
}

impl BlockSplitEncoding {
    pub(super) fn new(split: BlockSplit) -> Option<Self> {
        if split.num_types == 1 {
            return Some(Self {
                split,
                type_code: None,
                length_code: None,
            });
        }

        let mut type_frequencies = vec![0_usize; split.num_types + 2];
        let mut length_frequencies = vec![0_usize; BLOCK_LENGTH_ALPHABET_SIZE];
        let mut calculator = BlockTypeCodeCalculator::new();
        for (index, (&block_type, &length)) in split.types.iter().zip(&split.lengths).enumerate() {
            let type_symbol = calculator.next(block_type);
            if index != 0 {
                type_frequencies[usize::from(type_symbol)] += 1;
            }
            length_frequencies[usize::from(block_length_code(length).symbol)] += 1;
        }

        Some(Self {
            split,
            type_code: Some(PrefixEncoding::from_frequencies(&type_frequencies)?),
            length_code: Some(PrefixEncoding::from_frequencies(&length_frequencies)?),
        })
    }

    pub(super) const fn num_types(&self) -> usize {
        self.split.num_types
    }

    pub(super) fn write_header(&self, writer: &mut BitWriter) {
        write_var_len_u8(
            writer,
            u8::try_from(self.split.num_types - 1).expect("block type count fits in u8"),
        );
        if self.split.num_types == 1 {
            return;
        }

        let type_code = self
            .type_code
            .as_ref()
            .expect("multi-type split has a block-type code");
        let length_code = self
            .length_code
            .as_ref()
            .expect("multi-type split has a block-length code");
        type_code.write_tree(
            writer,
            u16::try_from(self.split.num_types + 2).expect("block-type alphabet fits in u16"),
        );
        length_code.write_tree(writer, BLOCK_LENGTH_ALPHABET_SIZE as u16);
        write_block_length(writer, length_code, self.split.lengths[0]);
    }

    pub(super) fn cursor(&self) -> BlockCursor<'_> {
        let mut type_calculator = BlockTypeCodeCalculator::new();
        type_calculator.next(self.split.types[0]);
        BlockCursor {
            encoding: self,
            block_index: 0,
            remaining: self.split.lengths[0],
            type_calculator,
        }
    }
}

impl BlockCursor<'_> {
    pub(super) fn before_symbol(&mut self, writer: &mut BitWriter) -> usize {
        if self.encoding.split.num_types == 1 {
            return 0;
        }

        if self.remaining == 0 {
            self.block_index += 1;
            let block_type = self.encoding.split.types[self.block_index];
            let type_symbol = self.type_calculator.next(block_type);
            self.encoding
                .type_code
                .as_ref()
                .expect("multi-type split has a block-type code")
                .write_symbol(writer, type_symbol);
            let length = self.encoding.split.lengths[self.block_index];
            write_block_length(
                writer,
                self.encoding
                    .length_code
                    .as_ref()
                    .expect("multi-type split has a block-length code"),
                length,
            );
            self.remaining = length;
        }

        self.remaining -= 1;
        usize::from(self.encoding.split.types[self.block_index])
    }
}

impl BlockTypeCodeCalculator {
    const fn new() -> Self {
        Self {
            last_type: 1,
            second_last_type: 0,
        }
    }

    fn next(&mut self, block_type: u8) -> u16 {
        let block_type = usize::from(block_type);
        let symbol = if block_type == self.last_type + 1 {
            1
        } else if block_type == self.second_last_type {
            0
        } else {
            block_type + 2
        };
        self.second_last_type = self.last_type;
        self.last_type = block_type;
        u16::try_from(symbol).expect("block type symbol fits in u16")
    }
}

impl GreedyBlockSplitter {
    fn new(
        symbol_alphabet_size: usize,
        min_block_size: usize,
        split_threshold: f64,
        num_symbols: usize,
    ) -> Self {
        Self::with_contexts(
            symbol_alphabet_size,
            1,
            min_block_size,
            split_threshold,
            num_symbols,
        )
    }

    fn with_contexts(
        symbol_alphabet_size: usize,
        context_count: usize,
        min_block_size: usize,
        split_threshold: f64,
        num_symbols: usize,
    ) -> Self {
        debug_assert!(context_count > 0 && context_count <= MAX_BLOCK_TYPES);
        let max_block_types = MAX_BLOCK_TYPES / context_count;
        let max_num_blocks = num_symbols / min_block_size + 1;
        let max_num_types = max_num_blocks.min(max_block_types + 1);
        let histogram_size = symbol_alphabet_size * context_count;
        Self {
            symbol_alphabet_size,
            context_count,
            min_block_size,
            split_threshold,
            max_block_types,
            split: BlockSplit {
                types: Vec::with_capacity(max_num_blocks),
                lengths: Vec::with_capacity(max_num_blocks),
                num_types: 0,
            },
            histograms: vec![vec![0_usize; histogram_size]; max_num_types],
            target_block_size: min_block_size,
            block_size: 0,
            current_histogram: 0,
            last_histograms: [0, 0],
            last_entropy: [0.0, 0.0],
            merge_last_count: 0,
        }
    }

    fn add_symbol(&mut self, symbol: usize) {
        self.add_context_symbol(symbol, 0);
    }

    fn add_context_symbol(&mut self, symbol: usize, context: usize) {
        debug_assert!(symbol < self.symbol_alphabet_size);
        debug_assert!(context < self.context_count);
        let composite_symbol = context * self.symbol_alphabet_size + symbol;
        self.histograms[self.current_histogram][composite_symbol] += 1;
        self.block_size += 1;
        if self.block_size == self.target_block_size {
            self.finish_block(false);
        }
    }

    fn finish(mut self) -> SplitResult {
        self.finish_block(true);
        self.histograms.truncate(self.split.num_types);
        let histograms = self
            .histograms
            .into_iter()
            .flat_map(|histogram| {
                histogram
                    .chunks_exact(self.symbol_alphabet_size)
                    .map(<[usize]>::to_vec)
                    .collect::<Vec<_>>()
            })
            .collect();
        SplitResult {
            split: self.split,
            histograms,
        }
    }

    fn finish_block(&mut self, is_final: bool) {
        self.block_size = self.block_size.max(self.min_block_size);
        if self.split.types.is_empty() {
            self.split.types.push(0);
            self.split.lengths.push(self.block_size);
            let entropy = self.histogram_entropy(&self.histograms[0]);
            self.last_entropy = [entropy, entropy];
            self.split.num_types = 1;
            self.current_histogram += 1;
            self.clear_current_histogram();
            self.block_size = 0;
        } else if self.block_size > 0 {
            let current = self.current_histogram;
            let entropy = self.histogram_entropy(&self.histograms[current]);
            let combined_entropy = [
                self.combined_histogram_entropy(current, self.last_histograms[0]),
                self.combined_histogram_entropy(current, self.last_histograms[1]),
            ];
            let difference = [
                combined_entropy[0] - entropy - self.last_entropy[0],
                combined_entropy[1] - entropy - self.last_entropy[1],
            ];

            if self.split.num_types < self.max_block_types
                && difference[0] > self.split_threshold
                && difference[1] > self.split_threshold
            {
                self.split.types.push(self.split.num_types as u8);
                self.split.lengths.push(self.block_size);
                self.last_histograms[1] = self.last_histograms[0];
                self.last_histograms[0] = self.split.num_types;
                self.last_entropy[1] = self.last_entropy[0];
                self.last_entropy[0] = entropy;
                self.split.num_types += 1;
                self.current_histogram += 1;
                self.clear_current_histogram();
                self.reset_after_split();
            } else if difference[1] < difference[0] - 20.0 {
                let second_last_type = self.split.types[self.split.types.len() - 2];
                let target = self.last_histograms[1];
                self.split.types.push(second_last_type);
                self.split.lengths.push(self.block_size);
                merge_histograms(&mut self.histograms, current, target);
                self.last_histograms.swap(0, 1);
                self.last_entropy[1] = self.last_entropy[0];
                self.last_entropy[0] = combined_entropy[1];
                self.clear_current_histogram();
                self.reset_after_split();
            } else {
                *self
                    .split
                    .lengths
                    .last_mut()
                    .expect("greedy splitter has a previous block") += self.block_size;
                let target = self.last_histograms[0];
                merge_histograms(&mut self.histograms, current, target);
                self.last_entropy[0] = combined_entropy[0];
                if self.split.num_types == 1 {
                    self.last_entropy[1] = self.last_entropy[0];
                }
                self.block_size = 0;
                self.clear_current_histogram();
                self.merge_last_count += 1;
                if self.merge_last_count > 1 {
                    self.target_block_size += self.min_block_size;
                }
            }
        }

        if is_final {
            debug_assert_eq!(self.split.types.len(), self.split.lengths.len());
        }
    }

    fn histogram_entropy(&self, histogram: &[usize]) -> f64 {
        histogram
            .chunks_exact(self.symbol_alphabet_size)
            .map(bits_entropy)
            .sum()
    }

    fn combined_histogram_entropy(&self, left: usize, right: usize) -> f64 {
        self.histograms[left]
            .chunks_exact(self.symbol_alphabet_size)
            .zip(self.histograms[right].chunks_exact(self.symbol_alphabet_size))
            .map(|(left, right)| combined_bits_entropy(left, right))
            .sum()
    }

    fn clear_current_histogram(&mut self) {
        if let Some(histogram) = self.histograms.get_mut(self.current_histogram) {
            histogram.fill(0);
        }
    }

    fn reset_after_split(&mut self) {
        self.block_size = 0;
        self.merge_last_count = 0;
        self.target_block_size = self.min_block_size;
    }
}

pub(super) fn split_literals(data: &[u8]) -> SplitResult {
    let mut splitter = GreedyBlockSplitter::new(256, 512, 400.0, data.len());
    for &symbol in data {
        splitter.add_symbol(usize::from(symbol));
    }
    splitter.finish()
}

pub(super) fn split_contextual_literals(data: &[(u8, u8)], context_count: usize) -> SplitResult {
    let mut splitter =
        GreedyBlockSplitter::with_contexts(256, context_count, 512, 400.0, data.len());
    for &(symbol, context) in data {
        splitter.add_context_symbol(usize::from(symbol), usize::from(context));
    }
    splitter.finish()
}

pub(super) fn split_commands(data: &[u16]) -> SplitResult {
    let mut splitter = GreedyBlockSplitter::new(704, 1024, 500.0, data.len());
    for &symbol in data {
        splitter.add_symbol(usize::from(symbol));
    }
    splitter.finish()
}

pub(super) fn split_distances(data: &[u16], alphabet_size: usize) -> SplitResult {
    let mut splitter = GreedyBlockSplitter::new(alphabet_size, 512, 100.0, data.len());
    for &symbol in data {
        splitter.add_symbol(usize::from(symbol));
    }
    splitter.finish()
}

pub(super) fn write_context_map(
    writer: &mut BitWriter,
    context_map: &[u8],
    num_clusters: usize,
) -> Option<()> {
    write_var_len_u8(writer, u8::try_from(num_clusters.checked_sub(1)?).ok()?);
    if num_clusters == 1 {
        return Some(());
    }

    let mut transformed = move_to_front_transform(context_map);
    let max_run_length_prefix = run_length_code_zeros(&mut transformed);
    let symbol_mask = (1_u32 << CONTEXT_MAP_SYMBOL_BITS) - 1;
    let alphabet_size = num_clusters.checked_add(max_run_length_prefix as usize)?;
    let mut frequencies = vec![0_usize; alphabet_size];
    for &value in &transformed {
        frequencies[(value & symbol_mask) as usize] += 1;
    }
    let code = PrefixEncoding::from_frequencies(&frequencies)?;

    let use_rle = max_run_length_prefix > 0;
    writer.write_bits(u64::from(use_rle), 1);
    if use_rle {
        writer.write_bits(u64::from(max_run_length_prefix - 1), 4);
    }
    code.write_tree(writer, u16::try_from(alphabet_size).ok()?);
    for value in transformed {
        let symbol = value & symbol_mask;
        let extra = value >> CONTEXT_MAP_SYMBOL_BITS;
        code.write_symbol(writer, u16::try_from(symbol).ok()?);
        if symbol > 0 && symbol <= max_run_length_prefix {
            writer.write_bits(u64::from(extra), u8::try_from(symbol).ok()?);
        }
    }
    writer.write_bits(1, 1);
    Some(())
}

pub(super) fn write_trivial_context_map(
    writer: &mut BitWriter,
    num_types: usize,
    context_bits: usize,
) -> Option<()> {
    write_var_len_u8(writer, u8::try_from(num_types.checked_sub(1)?).ok()?);
    if num_types == 1 {
        return Some(());
    }

    let repeat_code = context_bits.checked_sub(1)?;
    let repeat_bits = (1_usize << repeat_code) - 1;
    let alphabet_size = num_types.checked_add(repeat_code)?;
    let mut frequencies = vec![0_usize; alphabet_size];
    frequencies[repeat_code] = num_types;
    frequencies[0] = 1;
    for frequency in &mut frequencies[context_bits..] {
        *frequency = 1;
    }
    let code = PrefixEncoding::from_frequencies(&frequencies)?;

    writer.write_bits(1, 1);
    writer.write_bits(u64::try_from(repeat_code - 1).ok()?, 4);
    code.write_tree(writer, u16::try_from(alphabet_size).ok()?);
    for block_type in 0..num_types {
        let symbol = if block_type == 0 {
            0
        } else {
            block_type + context_bits - 1
        };
        code.write_symbol(writer, u16::try_from(symbol).ok()?);
        code.write_symbol(writer, u16::try_from(repeat_code).ok()?);
        writer.write_bits(
            u64::try_from(repeat_bits).ok()?,
            u8::try_from(repeat_code).ok()?,
        );
    }
    writer.write_bits(1, 1);
    Some(())
}

fn move_to_front_transform(context_map: &[u8]) -> Vec<u32> {
    let Some(&max_value) = context_map.iter().max() else {
        return Vec::new();
    };
    let mut move_to_front = (0..=max_value).collect::<Vec<_>>();
    let mut transformed = Vec::with_capacity(context_map.len());
    for &value in context_map {
        let index = move_to_front
            .iter()
            .position(|&candidate| candidate == value)
            .expect("context map value is present in move-to-front alphabet");
        transformed.push(index as u32);
        let value = move_to_front.remove(index);
        move_to_front.insert(0, value);
    }
    transformed
}

fn run_length_code_zeros(values: &mut Vec<u32>) -> u32 {
    let mut max_repetitions = 0_u32;
    let mut index = 0_usize;
    while index < values.len() {
        while index < values.len() && values[index] != 0 {
            index += 1;
        }
        let mut repetitions = 0_u32;
        while index < values.len() && values[index] == 0 {
            repetitions += 1;
            index += 1;
        }
        max_repetitions = max_repetitions.max(repetitions);
    }

    let max_prefix = if max_repetitions == 0 {
        0
    } else {
        max_repetitions
            .ilog2()
            .min(MAX_CONTEXT_MAP_RUN_LENGTH_PREFIX)
    };
    let mut encoded = Vec::with_capacity(values.len());
    let mut index = 0_usize;
    while index < values.len() {
        if values[index] != 0 {
            encoded.push(values[index] + max_prefix);
            index += 1;
            continue;
        }

        let mut repetitions = 1_u32;
        while index + (repetitions as usize) < values.len()
            && values[index + repetitions as usize] == 0
        {
            repetitions += 1;
        }
        index += repetitions as usize;
        while repetitions != 0 {
            if repetitions < (2_u32 << max_prefix) {
                let prefix = repetitions.ilog2();
                let extra = repetitions - (1_u32 << prefix);
                encoded.push(prefix + (extra << CONTEXT_MAP_SYMBOL_BITS));
                break;
            }
            let extra = (1_u32 << max_prefix) - 1;
            encoded.push(max_prefix + (extra << CONTEXT_MAP_SYMBOL_BITS));
            repetitions -= (2_u32 << max_prefix) - 1;
        }
    }
    *values = encoded;
    max_prefix
}

fn bits_entropy(histogram: &[usize]) -> f64 {
    let total = histogram.iter().sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    let table = log2_table();
    let total_log = fast_log2(total, table);
    let mut entropy = 0.0_f64;
    for &count in histogram {
        if count != 0 {
            entropy += count as f64 * (total_log - fast_log2(count, table));
        }
    }
    entropy.max(total as f64)
}

fn combined_bits_entropy(left: &[usize], right: &[usize]) -> f64 {
    debug_assert_eq!(left.len(), right.len());
    let total = left
        .iter()
        .zip(right)
        .map(|(&left, &right)| left + right)
        .sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    let table = log2_table();
    let total_log = fast_log2(total, table);
    let mut entropy = 0.0_f64;
    for (&left, &right) in left.iter().zip(right) {
        let count = left + right;
        if count != 0 {
            entropy += count as f64 * (total_log - fast_log2(count, table));
        }
    }
    entropy.max(total as f64)
}

fn merge_histograms(histograms: &mut [Vec<usize>], source: usize, target: usize) {
    debug_assert_ne!(source, target);
    let (source_histogram, target_histogram) = if source < target {
        let (before_target, from_target) = histograms.split_at_mut(target);
        (&before_target[source], &mut from_target[0])
    } else {
        let (before_source, from_source) = histograms.split_at_mut(source);
        (&from_source[0], &mut before_source[target])
    };
    for (target, source) in target_histogram.iter_mut().zip(source_histogram) {
        *target += *source;
    }
}

fn log2_table() -> &'static [f64; LOG2_TABLE_SIZE] {
    LOG2_TABLE.get_or_init(|| {
        std::array::from_fn(|value| {
            if value == 0 {
                0.0
            } else {
                (value as f64).log2()
            }
        })
    })
}

fn fast_log2(value: usize, table: &[f64; LOG2_TABLE_SIZE]) -> f64 {
    if value < LOG2_TABLE_SIZE {
        table[value]
    } else {
        (value as f64).log2()
    }
}

fn block_length_code(length: usize) -> BlockLengthCode {
    let symbol = BLOCK_LENGTH_OFFSETS
        .iter()
        .zip(BLOCK_LENGTH_EXTRA_BITS)
        .position(|(&offset, extra_bits)| {
            length >= offset && length - offset < (1_usize << extra_bits)
        })
        .expect("Brotli block-length prefix covers metablock length");
    BlockLengthCode {
        symbol: symbol as u16,
        extra: length - BLOCK_LENGTH_OFFSETS[symbol],
        extra_bits: BLOCK_LENGTH_EXTRA_BITS[symbol],
    }
}

fn write_block_length(writer: &mut BitWriter, code: &PrefixEncoding, length: usize) {
    let length_code = block_length_code(length);
    code.write_symbol(writer, length_code.symbol);
    writer.write_bits(length_code.extra as u64, length_code.extra_bits);
}

#[cfg(test)]
mod tests {
    use super::{
        BlockSplitEncoding, BlockTypeCodeCalculator, block_length_code, fast_log2, log2_table,
        move_to_front_transform, run_length_code_zeros, split_contextual_literals, split_literals,
    };

    #[test]
    fn greedy_splitter_keeps_uniform_literals_together() {
        let result = split_literals(&[b'a'; 4096]);
        assert_eq!(result.split.num_types, 1);
        assert_eq!(result.histograms.len(), 1);
        assert_eq!(result.histograms[0][usize::from(b'a')], 4096);
    }

    #[test]
    fn greedy_splitter_separates_high_gain_literal_regions() {
        let mut data = Vec::with_capacity(1024);
        data.extend((0..512).map(|index| if index & 1 == 0 { b'a' } else { b'b' }));
        data.extend((0..512).map(|index| if index & 1 == 0 { b'y' } else { b'z' }));
        let result = split_literals(&data);
        assert!(result.split.num_types >= 2);
        assert!(result.split.lengths.iter().sum::<usize>() >= data.len());
        assert_eq!(result.split.types.len(), result.split.lengths.len());
    }

    #[test]
    fn contextual_splitter_keeps_one_histogram_per_context_and_type() {
        let data = (0..1024)
            .map(|index| (b'a' + (index & 1) as u8, (index & 1) as u8))
            .collect::<Vec<_>>();
        let result = split_contextual_literals(&data, 2);
        assert_eq!(result.histograms.len(), result.split.num_types * 2);
        assert_eq!(
            result.histograms.iter().flatten().sum::<usize>(),
            data.len()
        );
    }

    #[test]
    fn reference_block_type_codes_match_history_rules() {
        let mut calculator = BlockTypeCodeCalculator::new();
        assert_eq!(calculator.next(0), 0);
        assert_eq!(calculator.next(1), 1);
        assert_eq!(calculator.next(0), 0);
        assert_eq!(calculator.next(3), 5);
    }

    #[test]
    fn block_length_prefixes_cover_boundaries() {
        let first = block_length_code(1);
        assert_eq!(first.symbol, 0);
        assert_eq!(first.extra, 0);
        let boundary = block_length_code(16625);
        assert_eq!(boundary.symbol, 25);
        assert_eq!(boundary.extra, 0);
        let maximum = block_length_code(1 << 24);
        assert_eq!(maximum.symbol, 25);
    }

    #[test]
    fn split_encoding_accepts_single_type() {
        let result = split_literals(&[b'a'; 1024]);
        let encoding = BlockSplitEncoding::new(result.split).unwrap();
        assert_eq!(encoding.num_types(), 1);
    }

    #[test]
    fn cached_log2_matches_direct_values() {
        let table = log2_table();
        for value in 0..table.len() {
            let expected = if value == 0 {
                0.0
            } else {
                (value as f64).log2()
            };
            assert_eq!(fast_log2(value, table), expected);
        }
    }

    #[test]
    fn context_map_transform_matches_reference_shape() {
        let mut transformed = move_to_front_transform(&[0, 0, 1, 1, 0, 0, 0, 0]);
        let max_prefix = run_length_code_zeros(&mut transformed);
        assert!(max_prefix > 0);
        assert!(!transformed.is_empty());
    }
}
