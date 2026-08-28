use super::bit_writer::BitWriter;

const MAX_CODE_BITS: usize = 15;
const CODE_LENGTH_ORDER: [u8; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

#[derive(Debug, Clone, Copy)]
struct SymbolCode {
    code: u16,
    bits: u8,
}

#[derive(Debug, Clone, Copy)]
struct HuffmanNode {
    total_count: u128,
    children: Option<(usize, usize)>,
    symbol: Option<u16>,
}

#[derive(Debug, Clone)]
pub(super) struct PrefixEncoding {
    symbols: Vec<u16>,
    codes: Vec<Option<SymbolCode>>,
    lengths: Vec<u8>,
}

impl PrefixEncoding {
    pub(super) fn from_frequencies(frequencies: &[usize]) -> Option<Self> {
        let mut symbols: Vec<u16> = frequencies
            .iter()
            .enumerate()
            .filter(|&(_, &frequency)| frequency != 0)
            .map(|(symbol, _)| u16::try_from(symbol).expect("alphabet fits in u16"))
            .collect();
        if symbols.is_empty() {
            return None;
        }

        if symbols.len() <= 4 {
            symbols.sort_unstable();
            let mut codes = vec![None; frequencies.len()];
            for (index, &symbol) in symbols.iter().enumerate() {
                let (code, bits) = simple_symbol_code(index, symbols.len());
                codes[usize::from(symbol)] = Some(SymbolCode { code, bits });
            }
            return Some(Self {
                symbols,
                codes,
                lengths: Vec::new(),
            });
        }

        let lengths = huffman_code_lengths(frequencies);
        let codes = canonical_codes(&lengths);
        symbols.sort_unstable();
        Some(Self {
            symbols,
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
        if self.symbols.len() <= 4 {
            write_simple_prefix_code(writer, &self.symbols, alphabet_size);
        } else {
            write_complex_prefix_code(writer, &self.lengths);
        }
    }

    pub(super) fn write_symbol(&self, writer: &mut BitWriter, symbol: u16) {
        let code = self.codes[usize::from(symbol)].expect("symbol exists in prefix code");
        writer.write_prefix(code.code, code.bits);
    }
}

fn huffman_code_lengths(frequencies: &[usize]) -> Vec<u8> {
    let active_count = frequencies
        .iter()
        .filter(|&&frequency| frequency != 0)
        .count();
    debug_assert!(active_count > 4);

    let mut count_limit = 1_u128;
    loop {
        let mut nodes = Vec::with_capacity(active_count * 2 - 1);
        for (symbol, &frequency) in frequencies.iter().enumerate() {
            if frequency == 0 {
                continue;
            }
            nodes.push(HuffmanNode {
                total_count: (frequency as u128).max(count_limit),
                children: None,
                symbol: Some(u16::try_from(symbol).expect("alphabet fits in u16")),
            });
        }
        nodes.sort_unstable_by(|left, right| {
            left.total_count.cmp(&right.total_count).then_with(|| {
                right
                    .symbol
                    .expect("leaf has symbol")
                    .cmp(&left.symbol.expect("leaf has symbol"))
            })
        });

        let leaf_count = nodes.len();
        let mut next_leaf = 0_usize;
        let mut next_parent = leaf_count;
        for _ in 1..leaf_count {
            let left = take_smallest_node(&nodes, leaf_count, &mut next_leaf, &mut next_parent);
            let right = take_smallest_node(&nodes, leaf_count, &mut next_leaf, &mut next_parent);
            nodes.push(HuffmanNode {
                total_count: nodes[left].total_count + nodes[right].total_count,
                children: Some((left, right)),
                symbol: None,
            });
        }

        let mut lengths = vec![0_u8; frequencies.len()];
        let mut stack = vec![(nodes.len() - 1, 0_usize)];
        let mut fits = true;
        while let Some((node_index, depth)) = stack.pop() {
            let node = nodes[node_index];
            if let Some((left, right)) = node.children {
                let child_depth = depth + 1;
                if child_depth > MAX_CODE_BITS {
                    fits = false;
                    break;
                }
                stack.push((right, child_depth));
                stack.push((left, child_depth));
            } else {
                lengths[usize::from(node.symbol.expect("leaf has symbol"))] = depth as u8;
            }
        }
        if fits {
            return lengths;
        }
        count_limit *= 2;
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
    writer.write_bits(0, 2); // complex representation, skip zero entries

    let last_symbol = lengths
        .iter()
        .rposition(|&length| length != 0)
        .expect("complex prefix code has non-zero lengths");
    let tokens = tokenize_code_lengths(&lengths[..=last_symbol]);

    let mut token_frequencies = [0_usize; 18];
    for token in &tokens {
        token_frequencies[usize::from(token.symbol)] += 1;
    }
    let (code_length_lengths, code_length_codes) = code_length_code(&token_frequencies);

    let mut remaining_space = 32_i16;
    let active_count = code_length_lengths
        .iter()
        .filter(|&&length| length != 0)
        .count();
    for symbol in CODE_LENGTH_ORDER {
        let length = code_length_lengths[usize::from(symbol)];
        write_code_length_value(writer, length);
        if length != 0 {
            remaining_space -= 32 >> length;
        }
        if remaining_space == 0 {
            break;
        }
    }
    debug_assert!(remaining_space == 0 || active_count == 1);

    for token in tokens {
        let code = code_length_codes[usize::from(token.symbol)]
            .expect("code-length token is present in its prefix code");
        writer.write_prefix(code.code, code.bits);
        if token.symbol == 17 {
            writer.write_bits(u64::from(token.extra), 3);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CodeLengthToken {
    symbol: u8,
    extra: u8,
}

fn tokenize_code_lengths(lengths: &[u8]) -> Vec<CodeLengthToken> {
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < lengths.len() {
        if lengths[index] != 0 {
            tokens.push(CodeLengthToken {
                symbol: lengths[index],
                extra: 0,
            });
            index += 1;
            continue;
        }

        let run_start = index;
        while index < lengths.len() && lengths[index] == 0 {
            index += 1;
        }
        let mut remaining = index - run_start;

        while remaining >= 3 {
            let chunk = remaining.min(10);
            tokens.push(CodeLengthToken {
                symbol: 17,
                extra: (chunk - 3) as u8,
            });
            remaining -= chunk;

            // Consecutive repeat-17 symbols extend one chained run rather than
            // starting a new run. An explicit zero resets that state.
            if remaining >= 3 {
                tokens.push(CodeLengthToken {
                    symbol: 0,
                    extra: 0,
                });
                remaining -= 1;
            }
        }

        tokens.extend((0..remaining).map(|_| CodeLengthToken {
            symbol: 0,
            extra: 0,
        }));
    }

    tokens
}

fn code_length_code(frequencies: &[usize; 18]) -> ([u8; 18], [Option<SymbolCode>; 18]) {
    let mut active: Vec<u8> = frequencies
        .iter()
        .enumerate()
        .filter(|&(_, &frequency)| frequency != 0)
        .map(|(symbol, _)| symbol as u8)
        .collect();
    debug_assert!(!active.is_empty());

    let mut lengths = [0_u8; 18];
    if active.len() == 1 {
        lengths[usize::from(active[0])] = 1;
        let mut codes = [None; 18];
        codes[usize::from(active[0])] = Some(SymbolCode { code: 0, bits: 0 });
        return (lengths, codes);
    }

    active.sort_unstable_by(|&left, &right| {
        frequencies[usize::from(right)]
            .cmp(&frequencies[usize::from(left)])
            .then_with(|| left.cmp(&right))
    });
    let count = active.len();
    let short_bits = (usize::BITS - 1 - count.leading_zeros()) as u8;
    let long_bits = short_bits + 1;
    let short_count = (1_usize << long_bits) - count;
    for (index, &symbol) in active.iter().enumerate() {
        lengths[usize::from(symbol)] = if index < short_count {
            short_bits
        } else {
            long_bits
        };
    }

    let codes_vec = canonical_codes(&lengths);
    let mut codes = [None; 18];
    codes.copy_from_slice(&codes_vec);
    (lengths, codes)
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
        MAX_CODE_BITS, PrefixEncoding, balanced_code_lengths, tokenize_code_lengths,
        write_simple_prefix_code, write_simple_symbol, write_var_len_u8,
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
    fn zero_run_tokenization_resets_repeat_chains() {
        let lengths = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        let tokens = tokenize_code_lengths(&lengths);
        let symbols: Vec<u8> = tokens.iter().map(|token| token.symbol).collect();

        assert_eq!(symbols, [2, 17, 0, 0, 0, 2]);
        assert_eq!(tokens[1].extra, 7);
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
