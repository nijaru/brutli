use super::bit_writer::BitWriter;

const MAX_CODE_BITS: usize = 15;
const CODE_LENGTH_CODE_BITS: usize = 5;
const INITIAL_REPEATED_CODE_LENGTH: u8 = 8;
const CODE_LENGTH_ORDER: [u8; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const LEAF_SENTINEL: u16 = u16::MAX;
const HUFFMAN_SHELL_GAPS: [usize; 6] = [132, 57, 23, 10, 4, 1];

#[derive(Debug, Clone, Copy)]
struct SymbolCode {
    code: u16,
    bits: u8,
}

#[derive(Debug, Clone, Copy)]
struct HuffmanNode {
    total_count: u32,
    left: u16,
    right_or_symbol: u16,
}

impl HuffmanNode {
    fn leaf(total_count: u32, symbol: usize) -> Self {
        Self {
            total_count,
            left: LEAF_SENTINEL,
            right_or_symbol: u16::try_from(symbol).expect("alphabet fits in u16"),
        }
    }

    fn parent(total_count: u32, left: usize, right: usize) -> Self {
        Self {
            total_count,
            left: u16::try_from(left).expect("Huffman tree index fits in u16"),
            right_or_symbol: u16::try_from(right).expect("Huffman tree index fits in u16"),
        }
    }

    const fn is_leaf(self) -> bool {
        self.left == LEAF_SENTINEL
    }
}

#[derive(Debug, Clone)]
pub(super) struct PrefixEncoding {
    simple_symbols: [u16; 4],
    simple_symbol_count: u8,
    codes: Vec<Option<SymbolCode>>,
    lengths: Vec<u8>,
}

impl PrefixEncoding {
    pub(super) fn from_frequencies(frequencies: &[usize]) -> Option<Self> {
        let mut simple_symbols = [0_u16; 4];
        let mut symbol_count = 0_usize;
        for (symbol, &frequency) in frequencies.iter().enumerate() {
            if frequency == 0 {
                continue;
            }
            if symbol_count < simple_symbols.len() {
                simple_symbols[symbol_count] = u16::try_from(symbol).expect("alphabet fits in u16");
            }
            symbol_count += 1;
        }
        if symbol_count == 0 {
            return None;
        }

        if symbol_count <= simple_symbols.len() {
            simple_symbols[..symbol_count].sort_unstable();
            let mut codes = vec![None; frequencies.len()];
            for (index, &symbol) in simple_symbols[..symbol_count].iter().enumerate() {
                let (code, bits) = simple_symbol_code(index, symbol_count);
                codes[usize::from(symbol)] = Some(SymbolCode { code, bits });
            }
            return Some(Self {
                simple_symbols,
                simple_symbol_count: symbol_count as u8,
                codes,
                lengths: Vec::new(),
            });
        }

        let lengths =
            huffman_code_lengths_with_active_count(frequencies, MAX_CODE_BITS, symbol_count);
        let codes = canonical_codes(&lengths);
        Some(Self {
            simple_symbols,
            simple_symbol_count: 0,
            codes,
            lengths,
        })
    }

    pub(super) fn data_bits(&self, frequencies: &[usize]) -> usize {
        debug_assert_eq!(self.codes.len(), frequencies.len());
        frequencies
            .iter()
            .zip(&self.codes)
            .map(|(&frequency, code)| {
                if frequency == 0 {
                    0
                } else {
                    frequency * usize::from(code.expect("used symbol exists in prefix code").bits)
                }
            })
            .sum()
    }

    pub(super) fn write_tree(&self, writer: &mut BitWriter, alphabet_size: u16) {
        debug_assert_eq!(self.codes.len(), usize::from(alphabet_size));
        let simple_symbol_count = usize::from(self.simple_symbol_count);
        if simple_symbol_count != 0 {
            write_simple_prefix_code(
                writer,
                &self.simple_symbols[..simple_symbol_count],
                alphabet_size,
            );
        } else {
            write_complex_prefix_code(writer, &self.lengths);
        }
    }

    pub(super) fn write_symbol(&self, writer: &mut BitWriter, symbol: u16) {
        let code = self.codes[usize::from(symbol)].expect("symbol exists in prefix code");
        writer.write_prefix(code.code, code.bits);
    }
}

fn huffman_code_lengths_with_limit(frequencies: &[usize], max_code_bits: usize) -> Vec<u8> {
    let active_count = frequencies
        .iter()
        .filter(|&&frequency| frequency != 0)
        .count();
    huffman_code_lengths_with_active_count(frequencies, max_code_bits, active_count)
}

fn huffman_code_lengths_with_active_count(
    frequencies: &[usize],
    max_code_bits: usize,
    active_count: usize,
) -> Vec<u8> {
    debug_assert_eq!(
        active_count,
        frequencies
            .iter()
            .filter(|&&frequency| frequency != 0)
            .count()
    );
    debug_assert!(active_count != 0);
    debug_assert!(active_count <= 1_usize << max_code_bits);
    debug_assert!(active_count * 2 - 1 < usize::from(LEAF_SENTINEL));

    if active_count == 1 {
        let mut lengths = vec![0_u8; frequencies.len()];
        let symbol = frequencies
            .iter()
            .position(|&frequency| frequency != 0)
            .expect("active symbol exists");
        lengths[symbol] = 1;
        return lengths;
    }

    let mut count_limit = 1_u32;
    loop {
        let mut nodes = Vec::with_capacity(active_count * 2 - 1);
        for (symbol, &frequency) in frequencies.iter().enumerate() {
            if frequency == 0 {
                continue;
            }
            let frequency = u32::try_from(frequency).expect("Brotli histogram count fits in u32");
            nodes.push(HuffmanNode::leaf(frequency.max(count_limit), symbol));
        }
        sort_huffman_nodes(&mut nodes);

        let leaf_count = nodes.len();
        let mut next_leaf = 0_usize;
        let mut next_parent = leaf_count;
        for _ in 1..leaf_count {
            let left = take_smallest_node(&nodes, leaf_count, &mut next_leaf, &mut next_parent);
            let right = take_smallest_node(&nodes, leaf_count, &mut next_leaf, &mut next_parent);
            let total_count = nodes[left]
                .total_count
                .checked_add(nodes[right].total_count)
                .expect("Brotli Huffman population fits in u32");
            nodes.push(HuffmanNode::parent(total_count, left, right));
        }

        let mut lengths = vec![0_u8; frequencies.len()];
        if set_huffman_depths(&nodes, nodes.len() - 1, &mut lengths, max_code_bits) {
            return lengths;
        }
        count_limit *= 2;
    }
}

fn set_huffman_depths(
    nodes: &[HuffmanNode],
    root: usize,
    lengths: &mut [u8],
    max_code_bits: usize,
) -> bool {
    debug_assert!(max_code_bits <= MAX_CODE_BITS);
    let mut stack = [LEAF_SENTINEL; MAX_CODE_BITS + 1];
    let mut level = 0_usize;
    let mut node_index = u16::try_from(root).expect("Huffman tree index fits in u16");

    loop {
        let node = nodes[usize::from(node_index)];
        if !node.is_leaf() {
            level += 1;
            if level > max_code_bits {
                return false;
            }
            stack[level] = node.right_or_symbol;
            node_index = node.left;
            continue;
        }

        lengths[usize::from(node.right_or_symbol)] = level as u8;
        while stack[level] == LEAF_SENTINEL {
            if level == 0 {
                return true;
            }
            level -= 1;
        }
        node_index = stack[level];
        stack[level] = LEAF_SENTINEL;
    }
}

fn take_smallest_node(
    nodes: &[HuffmanNode],
    leaf_count: usize,
    next_leaf: &mut usize,
    next_parent: &mut usize,
) -> usize {
    if *next_leaf < leaf_count
        && (*next_parent >= nodes.len()
            || nodes[*next_leaf].total_count <= nodes[*next_parent].total_count)
    {
        let result = *next_leaf;
        *next_leaf += 1;
        result
    } else {
        let result = *next_parent;
        *next_parent += 1;
        result
    }
}

fn huffman_node_before(left: HuffmanNode, right: HuffmanNode) -> bool {
    left.total_count < right.total_count
        || (left.total_count == right.total_count && left.right_or_symbol > right.right_or_symbol)
}

fn sort_huffman_nodes(nodes: &mut [HuffmanNode]) {
    let len = nodes.len();
    if len < 13 {
        for index in 1..len {
            let value = nodes[index];
            let mut slot = index;
            while slot != 0 && huffman_node_before(value, nodes[slot - 1]) {
                nodes[slot] = nodes[slot - 1];
                slot -= 1;
            }
            nodes[slot] = value;
        }
        return;
    }

    let first_gap = if len < 57 { 2 } else { 0 };
    for &gap in &HUFFMAN_SHELL_GAPS[first_gap..] {
        for index in gap..len {
            let value = nodes[index];
            let mut slot = index;
            while slot >= gap && huffman_node_before(value, nodes[slot - gap]) {
                nodes[slot] = nodes[slot - gap];
                slot -= gap;
            }
            nodes[slot] = value;
        }
    }
}

#[cfg(test)]
fn balanced_code_lengths(frequencies: &[usize]) -> Vec<u8> {
    let mut symbols: Vec<u16> = frequencies
        .iter()
        .enumerate()
        .filter(|&(_, &frequency)| frequency != 0)
        .map(|(symbol, _)| u16::try_from(symbol).expect("alphabet fits in u16"))
        .collect();
    debug_assert!(symbols.len() > 4);

    symbols.sort_unstable_by(|&left, &right| {
        frequencies[usize::from(right)]
            .cmp(&frequencies[usize::from(left)])
            .then_with(|| left.cmp(&right))
    });

    let count = symbols.len();
    let short_bits = (usize::BITS - 1 - count.leading_zeros()) as u8;
    let long_bits = short_bits + 1;
    debug_assert!(usize::from(long_bits) <= MAX_CODE_BITS);
    let short_count = (1_usize << long_bits) - count;

    let mut lengths = vec![0_u8; frequencies.len()];
    for (index, symbol) in symbols.into_iter().enumerate() {
        lengths[usize::from(symbol)] = if index < short_count {
            short_bits
        } else {
            long_bits
        };
    }
    lengths
}

pub(super) fn write_var_len_u8(writer: &mut BitWriter, value: u8) {
    match value {
        0 => writer.write_bits(0, 1),
        1 => {
            writer.write_bits(1, 1);
            writer.write_bits(0, 3);
        }
        _ => {
            let width = (u8::BITS - value.leading_zeros() - 1) as u8;
            writer.write_bits(1, 1);
            writer.write_bits(u64::from(width), 3);
            writer.write_bits(u64::from(value - (1_u8 << width)), width);
        }
    }
}

pub(super) fn write_simple_prefix_code(
    writer: &mut BitWriter,
    symbols: &[u16],
    alphabet_size: u16,
) {
    assert!((1..=4).contains(&symbols.len()));
    assert!(alphabet_size != 0);
    assert!(symbols.iter().all(|&symbol| symbol < alphabet_size));

    writer.write_bits(1, 2); // simple representation
    writer.write_bits((symbols.len() - 1) as u64, 2);

    let alphabet_bits = (u16::BITS - (alphabet_size - 1).leading_zeros()) as u8;
    for &symbol in symbols {
        writer.write_bits(u64::from(symbol), alphabet_bits);
    }

    if symbols.len() == 4 {
        writer.write_bits(0, 1); // four codes of length 2
    }
}

pub(super) fn write_simple_symbol(
    writer: &mut BitWriter,
    symbol_index: usize,
    symbol_count: usize,
) {
    let (code, bits) = simple_symbol_code(symbol_index, symbol_count);
    writer.write_prefix(code, bits);
}

fn simple_symbol_code(symbol_index: usize, symbol_count: usize) -> (u16, u8) {
    match symbol_count {
        1 => (0, 0),
        2 => (symbol_index as u16, 1),
        3 => match symbol_index {
            0 => (0, 1),
            1 => (0b10, 2),
            2 => (0b11, 2),
            _ => unreachable!(),
        },
        4 => (symbol_index as u16, 2),
        _ => unreachable!(),
    }
}

fn write_complex_prefix_code(writer: &mut BitWriter, lengths: &[u8]) {
    debug_assert!(lengths.iter().filter(|&&length| length != 0).count() > 4);
    let tokens = tokenize_code_lengths(lengths);

    let mut token_frequencies = [0_usize; 18];
    for token in &tokens {
        token_frequencies[usize::from(token.symbol)] += 1;
    }
    let (code_length_lengths, code_length_codes) = code_length_code(&token_frequencies);
    write_code_length_code(writer, &code_length_lengths, &token_frequencies);

    for token in tokens {
        let code = code_length_codes[usize::from(token.symbol)]
            .expect("code-length token is present in its prefix code");
        writer.write_prefix(code.code, code.bits);
        match token.symbol {
            16 => writer.write_bits(u64::from(token.extra), 2),
            17 => writer.write_bits(u64::from(token.extra), 3),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeLengthToken {
    symbol: u8,
    extra: u8,
}

fn tokenize_code_lengths(lengths: &[u8]) -> Vec<CodeLengthToken> {
    let new_length = lengths
        .iter()
        .rposition(|&length| length != 0)
        .map_or(0, |index| index + 1);
    debug_assert!(new_length != 0);

    let (use_rle_for_non_zero, use_rle_for_zero) = if lengths.len() > 50 {
        decide_over_rle_use(&lengths[..new_length])
    } else {
        (false, false)
    };

    let mut previous_value = INITIAL_REPEATED_CODE_LENGTH;
    let mut tokens = Vec::with_capacity(new_length);
    let mut index = 0_usize;
    while index < new_length {
        let value = lengths[index];
        let mut repetitions = 1_usize;
        if (value != 0 && use_rle_for_non_zero) || (value == 0 && use_rle_for_zero) {
            while index + repetitions < new_length && lengths[index + repetitions] == value {
                repetitions += 1;
            }
        }

        if value == 0 {
            write_zero_repetitions(repetitions, &mut tokens);
        } else {
            write_repetitions(previous_value, value, repetitions, &mut tokens);
            previous_value = value;
        }
        index += repetitions;
    }
    tokens
}

fn decide_over_rle_use(lengths: &[u8]) -> (bool, bool) {
    let mut total_reps_zero = 0_usize;
    let mut total_reps_non_zero = 0_usize;
    let mut count_reps_zero = 1_usize;
    let mut count_reps_non_zero = 1_usize;
    let mut index = 0_usize;

    while index < lengths.len() {
        let value = lengths[index];
        let mut repetitions = 1_usize;
        while index + repetitions < lengths.len() && lengths[index + repetitions] == value {
            repetitions += 1;
        }
        if repetitions >= 3 && value == 0 {
            total_reps_zero += repetitions;
            count_reps_zero += 1;
        }
        if repetitions >= 4 && value != 0 {
            total_reps_non_zero += repetitions;
            count_reps_non_zero += 1;
        }
        index += repetitions;
    }

    (
        total_reps_non_zero > count_reps_non_zero * 2,
        total_reps_zero > count_reps_zero * 2,
    )
}

fn write_repetitions(
    previous_value: u8,
    value: u8,
    mut repetitions: usize,
    tokens: &mut Vec<CodeLengthToken>,
) {
    debug_assert!(repetitions > 0);
    if previous_value != value {
        tokens.push(CodeLengthToken {
            symbol: value,
            extra: 0,
        });
        repetitions -= 1;
    }
    if repetitions == 7 {
        tokens.push(CodeLengthToken {
            symbol: value,
            extra: 0,
        });
        repetitions -= 1;
    }
    if repetitions < 3 {
        tokens.extend((0..repetitions).map(|_| CodeLengthToken {
            symbol: value,
            extra: 0,
        }));
        return;
    }

    repetitions -= 3;
    let start = tokens.len();
    loop {
        tokens.push(CodeLengthToken {
            symbol: 16,
            extra: (repetitions & 0x3) as u8,
        });
        repetitions >>= 2;
        if repetitions == 0 {
            break;
        }
        repetitions -= 1;
    }
    tokens[start..].reverse();
}

fn write_zero_repetitions(mut repetitions: usize, tokens: &mut Vec<CodeLengthToken>) {
    if repetitions == 11 {
        tokens.push(CodeLengthToken {
            symbol: 0,
            extra: 0,
        });
        repetitions -= 1;
    }
    if repetitions < 3 {
        tokens.extend((0..repetitions).map(|_| CodeLengthToken {
            symbol: 0,
            extra: 0,
        }));
        return;
    }

    repetitions -= 3;
    let start = tokens.len();
    loop {
        tokens.push(CodeLengthToken {
            symbol: 17,
            extra: (repetitions & 0x7) as u8,
        });
        repetitions >>= 3;
        if repetitions == 0 {
            break;
        }
        repetitions -= 1;
    }
    tokens[start..].reverse();
}

fn code_length_code(frequencies: &[usize; 18]) -> ([u8; 18], [Option<SymbolCode>; 18]) {
    let lengths_vec = huffman_code_lengths_with_limit(frequencies, CODE_LENGTH_CODE_BITS);
    let mut lengths = [0_u8; 18];
    lengths.copy_from_slice(&lengths_vec);

    let codes_vec = canonical_codes(&lengths);
    let mut codes = [None; 18];
    codes.copy_from_slice(&codes_vec);
    if frequencies
        .iter()
        .filter(|&&frequency| frequency != 0)
        .count()
        == 1
    {
        let symbol = frequencies
            .iter()
            .position(|&frequency| frequency != 0)
            .expect("code-length token exists");
        codes[symbol] = Some(SymbolCode { code: 0, bits: 0 });
    }
    (lengths, codes)
}

fn write_code_length_code(
    writer: &mut BitWriter,
    code_length_lengths: &[u8; 18],
    token_frequencies: &[usize; 18],
) {
    let num_codes = token_frequencies
        .iter()
        .filter(|&&frequency| frequency != 0)
        .count();
    let mut codes_to_store = CODE_LENGTH_ORDER.len();
    if num_codes > 1 {
        while codes_to_store > 0
            && code_length_lengths[usize::from(CODE_LENGTH_ORDER[codes_to_store - 1])] == 0
        {
            codes_to_store -= 1;
        }
    }

    let mut skip = 0_usize;
    if code_length_lengths[usize::from(CODE_LENGTH_ORDER[0])] == 0
        && code_length_lengths[usize::from(CODE_LENGTH_ORDER[1])] == 0
    {
        skip = 2;
        if code_length_lengths[usize::from(CODE_LENGTH_ORDER[2])] == 0 {
            skip = 3;
        }
    }
    writer.write_bits(skip as u64, 2);
    for &symbol in &CODE_LENGTH_ORDER[skip..codes_to_store] {
        write_code_length_value(writer, code_length_lengths[usize::from(symbol)]);
    }
}

fn canonical_codes(lengths: &[u8]) -> Vec<Option<SymbolCode>> {
    let max_bits = usize::from(*lengths.iter().max().unwrap_or(&0));
    let mut counts = vec![0_u16; max_bits + 1];
    for &length in lengths {
        if length != 0 {
            counts[usize::from(length)] += 1;
        }
    }

    let mut next_code = vec![0_u16; max_bits + 1];
    let mut code = 0_u16;
    for bits in 1..=max_bits {
        code = (code + counts[bits - 1]) << 1;
        next_code[bits] = code;
    }

    lengths
        .iter()
        .map(|&length| {
            if length == 0 {
                return None;
            }
            let bits = usize::from(length);
            let code = next_code[bits];
            next_code[bits] += 1;
            Some(SymbolCode { code, bits: length })
        })
        .collect()
}

fn write_code_length_value(writer: &mut BitWriter, value: u8) {
    let (bits, count) = match value {
        0 => (0b00, 2),
        1 => (0b0111, 4),
        2 => (0b011, 3),
        3 => (0b10, 2),
        4 => (0b01, 2),
        5 => (0b1111, 4),
        _ => unreachable!("code-length code length must be 0..=5"),
    };
    writer.write_bits(bits, count);
}

#[cfg(test)]
mod tests {
    use super::{
        CodeLengthToken, MAX_CODE_BITS, PrefixEncoding, balanced_code_lengths,
        tokenize_code_lengths, write_simple_prefix_code, write_simple_symbol, write_var_len_u8,
    };
    use crate::encode::bit_writer::BitWriter;

    #[test]
    fn variable_length_zero_is_one_zero_bit() {
        let mut writer = BitWriter::default();
        write_var_len_u8(&mut writer, 0);
        assert_eq!(writer.finish(), [0]);
    }

    #[test]
    fn simple_two_symbol_code_has_expected_layout() {
        let mut writer = BitWriter::default();
        write_simple_prefix_code(&mut writer, &[2, 5], 8);
        assert_eq!(writer.finish(), [0b1010_0101, 0b0000_0010]);
    }

    #[test]
    fn emits_canonical_simple_symbols() {
        let mut writer = BitWriter::default();
        for index in 0..3 {
            write_simple_symbol(&mut writer, index, 3);
        }
        assert_eq!(writer.finish(), [0b0001_1010]);
    }

    #[test]
    fn huffman_code_prefers_frequent_symbols() {
        let frequencies = [100, 10, 10, 10, 10];
        let code = PrefixEncoding::from_frequencies(&frequencies).unwrap();

        assert_eq!(code.lengths[0], 1);
        assert!(code.lengths[1..].iter().all(|&length| length == 3));
        assert_eq!(code.data_bits(&frequencies), 220);
        assert_complete_tree(&code.lengths);
    }

    #[test]
    fn huffman_cost_is_optimal_for_small_histograms() {
        for frequencies in [
            vec![1, 1, 1, 1, 1],
            vec![100, 10, 10, 10, 10],
            vec![9, 7, 5, 3, 1],
            vec![20, 11, 10, 9, 8, 7],
        ] {
            let code = PrefixEncoding::from_frequencies(&frequencies).unwrap();
            assert_eq!(
                code.data_bits(&frequencies),
                exhaustive_best_cost(&frequencies)
            );
            assert_complete_tree(&code.lengths);
        }
    }

    #[test]
    fn huffman_cost_never_exceeds_balanced_baseline() {
        for frequencies in [
            vec![100, 10, 10, 10, 10],
            vec![30, 20, 10, 5, 3, 2, 1],
            vec![1, 1, 1, 1, 1, 1, 1, 1, 1],
        ] {
            let code = PrefixEncoding::from_frequencies(&frequencies).unwrap();
            let balanced = balanced_code_lengths(&frequencies);
            let balanced_cost = weighted_cost(&frequencies, &balanced);
            assert!(code.data_bits(&frequencies) <= balanced_cost);
        }
    }

    #[test]
    fn deep_huffman_tree_is_length_limited_without_balancing() {
        let mut frequencies = vec![1_usize, 1];
        while frequencies.len() < 32 {
            let next = frequencies[frequencies.len() - 1] + frequencies[frequencies.len() - 2];
            frequencies.push(next);
        }

        let code = PrefixEncoding::from_frequencies(&frequencies).unwrap();
        assert!(
            code.lengths
                .iter()
                .all(|&length| usize::from(length) <= MAX_CODE_BITS)
        );
        assert!(
            code.data_bits(&frequencies)
                <= weighted_cost(&frequencies, &balanced_code_lengths(&frequencies))
        );
        assert_complete_tree(&code.lengths);
    }

    #[test]
    fn nonzero_repetition_tokens_match_reference_chaining() {
        let mut lengths = vec![5_u8; 12];
        lengths.push(4);
        lengths.resize(64, 0);
        let tokens = tokenize_code_lengths(&lengths);
        assert_eq!(
            &tokens[..3],
            &[
                CodeLengthToken {
                    symbol: 5,
                    extra: 0,
                },
                CodeLengthToken {
                    symbol: 16,
                    extra: 1,
                },
                CodeLengthToken {
                    symbol: 16,
                    extra: 0,
                },
            ]
        );
    }

    #[test]
    fn eleven_zero_repetitions_use_reference_special_case() {
        let mut lengths = vec![3_u8; 8];
        lengths.extend([0_u8; 11]);
        lengths.push(3);
        lengths.resize(64, 0);
        let tokens = tokenize_code_lengths(&lengths);
        assert!(tokens.windows(2).any(|pair| {
            pair == [
                CodeLengthToken {
                    symbol: 0,
                    extra: 0,
                },
                CodeLengthToken {
                    symbol: 17,
                    extra: 7,
                },
            ]
        }));
    }

    fn assert_complete_tree(lengths: &[u8]) {
        let used_lengths: Vec<u8> = lengths
            .iter()
            .copied()
            .filter(|&length| length != 0)
            .collect();
        assert!(
            used_lengths
                .iter()
                .all(|&length| usize::from(length) <= MAX_CODE_BITS)
        );
        let kraft_units: usize = used_lengths
            .iter()
            .map(|&length| 1_usize << (MAX_CODE_BITS - usize::from(length)))
            .sum();
        assert_eq!(kraft_units, 1_usize << MAX_CODE_BITS);
    }

    fn weighted_cost(frequencies: &[usize], lengths: &[u8]) -> usize {
        frequencies
            .iter()
            .zip(lengths)
            .map(|(&frequency, &length)| frequency * usize::from(length))
            .sum()
    }

    fn exhaustive_best_cost(frequencies: &[usize]) -> usize {
        assert!(frequencies.len() > 1);
        let max_length = frequencies.len() - 1;
        let target_space = 1_usize << max_length;
        let mut best = usize::MAX;

        fn search(
            frequencies: &[usize],
            max_length: usize,
            target_space: usize,
            index: usize,
            used_space: usize,
            cost: usize,
            best: &mut usize,
        ) {
            if index == frequencies.len() {
                if used_space == target_space {
                    *best = (*best).min(cost);
                }
                return;
            }

            for length in 1..=max_length {
                let space = 1_usize << (max_length - length);
                let next_space = used_space + space;
                if next_space > target_space {
                    continue;
                }
                search(
                    frequencies,
                    max_length,
                    target_space,
                    index + 1,
                    next_space,
                    cost + frequencies[index] * length,
                    best,
                );
            }
        }

        search(frequencies, max_length, target_space, 0, 0, 0, &mut best);
        assert_ne!(best, usize::MAX);
        best
    }
}
