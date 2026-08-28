use super::super::bit_writer::BitWriter;
use super::super::prefix_code::{PrefixEncoding, write_var_len_u8};

const MAX_LITERAL_HISTOGRAMS: usize = 100;
const LITERAL_BLOCK_SWITCH_COST: f64 = 28.1;
const LITERAL_STRIDE_LENGTH: usize = 70;
const SYMBOLS_PER_LITERAL_HISTOGRAM: usize = 544;
const MIN_LENGTH_FOR_BLOCK_SPLITTING: usize = 128;
const ITER_MUL_FOR_REFINING: usize = 2;
const MIN_ITERS_FOR_REFINING: usize = 100;
const FIND_BLOCKS_ITERS_Q5: usize = 3;
const LITERAL_ALPHABET_SIZE: usize = 256;
const BLOCK_LENGTH_ALPHABET_SIZE: usize = 26;

const BLOCK_LENGTH_OFFSETS: [usize; BLOCK_LENGTH_ALPHABET_SIZE] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497,
    753, 1265, 2289, 4337, 8433, 16625,
];
const BLOCK_LENGTH_EXTRA_BITS: [u8; BLOCK_LENGTH_ALPHABET_SIZE] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13,
    24,
];

type LiteralHistogram = [usize; LITERAL_ALPHABET_SIZE];

#[derive(Debug, Clone)]
pub(super) struct LiteralSplit {
    types: Vec<u8>,
    lengths: Vec<usize>,
    num_types: usize,
}

#[derive(Debug, Clone)]
pub(super) struct BlockSplitEncoding {
    split: LiteralSplit,
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

impl LiteralSplit {
    fn single(length: usize) -> Self {
        Self {
            types: vec![0],
            lengths: vec![length],
            num_types: 1,
        }
    }

    pub(super) const fn num_types(&self) -> usize {
        self.num_types
    }

    pub(super) fn histograms(&self, data: &[u8]) -> Vec<LiteralHistogram> {
        let mut histograms = vec![[0_usize; LITERAL_ALPHABET_SIZE]; self.num_types];
        let mut offset = 0_usize;
        for (&block_type, &length) in self.types.iter().zip(&self.lengths) {
            let end = offset + length;
            for &symbol in &data[offset..end] {
                histograms[usize::from(block_type)][usize::from(symbol)] += 1;
            }
            offset = end;
        }
        debug_assert_eq!(offset, data.len());
        histograms
    }
}

impl BlockSplitEncoding {
    pub(super) fn new(split: LiteralSplit) -> Option<Self> {
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
        for (index, (&block_type, &length)) in
            split.types.iter().zip(&split.lengths).enumerate()
        {
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
            u8::try_from(self.split.num_types - 1).expect("literal block type count fits in u8"),
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

pub(super) fn split_literals(data: &[u8]) -> LiteralSplit {
    if data.len() < MIN_LENGTH_FOR_BLOCK_SPLITTING {
        return LiteralSplit::single(data.len());
    }

    let mut num_histograms = (data.len() / SYMBOLS_PER_LITERAL_HISTOGRAM + 1)
        .min(MAX_LITERAL_HISTOGRAMS);
    if num_histograms == 1 {
        return LiteralSplit::single(data.len());
    }

    let mut histograms = initial_entropy_codes(data, num_histograms);
    refine_entropy_codes(data, &mut histograms);
    let mut block_ids = vec![0_u8; data.len()];

    for _ in 0..FIND_BLOCKS_ITERS_Q5 {
        find_blocks(data, LITERAL_BLOCK_SWITCH_COST, &histograms, &mut block_ids);
        num_histograms = remap_block_ids(&mut block_ids, num_histograms);
        if num_histograms == 1 {
            return LiteralSplit::single(data.len());
        }
        histograms = build_histograms(data, &block_ids, num_histograms);
    }

    split_from_assignments(&block_ids, num_histograms)
}

pub(super) fn write_trivial_context_map(
    writer: &mut BitWriter,
    num_types: usize,
    context_bits: usize,
) -> Option<()> {
    write_var_len_u8(
        writer,
        u8::try_from(num_types.checked_sub(1)?).ok()?,
    );
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

    writer.write_bits(1, 1); // use RLE for zero runs
    writer.write_bits(u64::try_from(repeat_code - 1).ok()?, 4); // RLEMAX - 1
    code.write_tree(writer, u16::try_from(alphabet_size).ok()?);
    for block_type in 0..num_types {
        let symbol = if block_type == 0 {
            0
        } else {
            block_type + context_bits - 1
        };
        code.write_symbol(writer, u16::try_from(symbol).ok()?);
        code.write_symbol(writer, u16::try_from(repeat_code).ok()?);
        writer.write_bits(u64::try_from(repeat_bits).ok()?, u8::try_from(repeat_code).ok()?);
    }
    writer.write_bits(1, 1); // inverse move-to-front
    Some(())
}

fn initial_entropy_codes(data: &[u8], num_histograms: usize) -> Vec<LiteralHistogram> {
    let mut histograms = vec![[0_usize; LITERAL_ALPHABET_SIZE]; num_histograms];
    let mut seed = 7_u32;
    let block_length = data.len() / num_histograms;
    for (index, histogram) in histograms.iter_mut().enumerate() {
        let mut position = data.len() * index / num_histograms;
        if index != 0 {
            position += my_rand(&mut seed) as usize % block_length;
        }
        if position + LITERAL_STRIDE_LENGTH >= data.len() {
            position = data.len() - LITERAL_STRIDE_LENGTH - 1;
        }
        add_vector(
            histogram,
            &data[position..position + LITERAL_STRIDE_LENGTH],
        );
    }
    histograms
}

fn refine_entropy_codes(data: &[u8], histograms: &mut [LiteralHistogram]) {
    let num_histograms = histograms.len();
    let mut iterations =
        ITER_MUL_FOR_REFINING * data.len() / LITERAL_STRIDE_LENGTH + MIN_ITERS_FOR_REFINING;
    iterations = iterations.div_ceil(num_histograms) * num_histograms;
    let mut seed = 7_u32;

    for iteration in 0..iterations {
        let mut sample = [0_usize; LITERAL_ALPHABET_SIZE];
        random_sample(&mut seed, data, LITERAL_STRIDE_LENGTH, &mut sample);
        add_histogram(&mut histograms[iteration % num_histograms], &sample);
    }
}

fn random_sample(
    seed: &mut u32,
    data: &[u8],
    mut stride: usize,
    histogram: &mut LiteralHistogram,
) {
    let position = if stride >= data.len() {
        stride = data.len();
        0
    } else {
        my_rand(seed) as usize % (data.len() - stride + 1)
    };
    add_vector(histogram, &data[position..position + stride]);
}

fn find_blocks(
    data: &[u8],
    block_switch_bitcost: f64,
    histograms: &[LiteralHistogram],
    block_ids: &mut [u8],
) -> usize {
    let num_histograms = histograms.len();
    if num_histograms <= 1 {
        block_ids.fill(0);
        return 1;
    }

    let mut insert_cost = vec![0.0_f64; LITERAL_ALPHABET_SIZE * num_histograms];
    for (histogram_index, histogram) in histograms.iter().enumerate() {
        let total = histogram.iter().sum::<usize>();
        let total_cost = (total as f64).log2();
        for (symbol, &count) in histogram.iter().enumerate() {
            let bit_cost = if count == 0 {
                -2.0
            } else {
                (count as f64).log2()
            };
            insert_cost[symbol * num_histograms + histogram_index] = total_cost - bit_cost;
        }
    }

    let bitmap_length = num_histograms.div_ceil(8);
    let mut costs = vec![0.0_f64; num_histograms];
    let mut switch_signal = vec![0_u8; data.len() * bitmap_length];

    for (position, &symbol) in data.iter().enumerate() {
        let insert_offset = usize::from(symbol) * num_histograms;
        let mut minimum_cost = f64::INFINITY;
        for histogram_index in 0..num_histograms {
            costs[histogram_index] += insert_cost[insert_offset + histogram_index];
            if costs[histogram_index] < minimum_cost {
                minimum_cost = costs[histogram_index];
                block_ids[position] = histogram_index as u8;
            }
        }

        let mut switch_cost = block_switch_bitcost;
        if position < 2000 {
            switch_cost *= 0.77 + (0.07 / 2000.0) * position as f64;
        }
        let signal_offset = position * bitmap_length;
        for (histogram_index, cost) in costs.iter_mut().enumerate() {
            *cost -= minimum_cost;
            if *cost >= switch_cost {
                *cost = switch_cost;
                switch_signal[signal_offset + histogram_index / 8] |=
                    1_u8 << (histogram_index & 7);
            }
        }
    }

    let mut num_blocks = 1_usize;
    let mut position = data.len() - 1;
    let mut current_id = block_ids[position];
    while position > 0 {
        position -= 1;
        let mask = 1_u8 << (current_id & 7);
        let signal = switch_signal[position * bitmap_length + usize::from(current_id >> 3)];
        if signal & mask != 0 && current_id != block_ids[position] {
            current_id = block_ids[position];
            num_blocks += 1;
        }
        block_ids[position] = current_id;
    }
    num_blocks
}

fn remap_block_ids(block_ids: &mut [u8], num_histograms: usize) -> usize {
    let mut new_ids = vec![u16::MAX; num_histograms];
    let mut next_id = 0_u16;
    for &block_id in block_ids.iter() {
        let slot = &mut new_ids[usize::from(block_id)];
        if *slot == u16::MAX {
            *slot = next_id;
            next_id += 1;
        }
    }
    for block_id in block_ids {
        *block_id = u8::try_from(new_ids[usize::from(*block_id)])
            .expect("literal histogram count fits in u8");
    }
    usize::from(next_id)
}

fn build_histograms(
    data: &[u8],
    block_ids: &[u8],
    num_histograms: usize,
) -> Vec<LiteralHistogram> {
    let mut histograms = vec![[0_usize; LITERAL_ALPHABET_SIZE]; num_histograms];
    for (&symbol, &block_id) in data.iter().zip(block_ids) {
        histograms[usize::from(block_id)][usize::from(symbol)] += 1;
    }
    histograms
}

fn split_from_assignments(block_ids: &[u8], num_types: usize) -> LiteralSplit {
    debug_assert!(!block_ids.is_empty());
    let mut types = Vec::new();
    let mut lengths = Vec::new();
    let mut current_type = block_ids[0];
    let mut current_length = 1_usize;
    for &block_type in &block_ids[1..] {
        if block_type == current_type {
            current_length += 1;
        } else {
            types.push(current_type);
            lengths.push(current_length);
            current_type = block_type;
            current_length = 1;
        }
    }
    types.push(current_type);
    lengths.push(current_length);
    LiteralSplit {
        types,
        lengths,
        num_types,
    }
}

fn add_vector(histogram: &mut LiteralHistogram, data: &[u8]) {
    for &symbol in data {
        histogram[usize::from(symbol)] += 1;
    }
}

fn add_histogram(destination: &mut LiteralHistogram, source: &LiteralHistogram) {
    for (destination, &source) in destination.iter_mut().zip(source) {
        *destination += source;
    }
}

fn my_rand(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(16807);
    *seed
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
        BlockTypeCodeCalculator, LiteralSplit, block_length_code, split_literals,
    };

    #[test]
    fn short_literal_stream_uses_one_type() {
        let split = split_literals(&vec![b'a'; 127]);
        assert_eq!(split.num_types(), 1);
        assert_eq!(split.lengths, [127]);
    }

    #[test]
    fn q5_splitter_separates_different_literal_regions() {
        let mut data = vec![b'a'; 4096];
        data.extend(vec![b'z'; 4096]);
        let split = split_literals(&data);
        assert!(split.num_types() >= 2);
        assert_eq!(split.lengths.iter().sum::<usize>(), data.len());
        assert_eq!(split.types.len(), split.lengths.len());
        let histograms = split.histograms(&data);
        assert_eq!(histograms.len(), split.num_types());
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
    fn split_histograms_follow_block_types() {
        let split = LiteralSplit {
            types: vec![0, 1, 0],
            lengths: vec![2, 2, 1],
            num_types: 2,
        };
        let histograms = split.histograms(b"aabbc");
        assert_eq!(histograms[0][usize::from(b'a')], 2);
        assert_eq!(histograms[0][usize::from(b'c')], 1);
        assert_eq!(histograms[1][usize::from(b'b')], 2);
    }
}
