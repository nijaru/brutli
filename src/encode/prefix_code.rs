use super::bit_writer::BitWriter;

const MAX_CODE_BITS: usize = 15;
const CODE_LENGTH_ORDER: [u8; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

#[derive(Debug, Clone, Copy)]
struct SymbolCode {
    code: u16,
    bits: u8,
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
            .filter_map(|(symbol, &frequency)| {
                (frequency != 0).then(|| u16::try_from(symbol).expect("alphabet fits in u16"))
            })
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
        for (index, &symbol) in symbols.iter().enumerate() {
            lengths[usize::from(symbol)] = if index < short_count {
                short_bits
            } else {
                long_bits
            };
        }

        let codes = canonical_codes(&lengths);
        symbols.sort_unstable();
        Some(Self {
            symbols,
            codes,
            lengths,
        })
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
        .filter_map(|(symbol, &frequency)| (frequency != 0).then_some(symbol as u8))
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
        PrefixEncoding, tokenize_code_lengths, write_simple_prefix_code, write_simple_symbol,
        write_var_len_u8,
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
    fn balanced_code_prefers_frequent_symbols() {
        let mut frequencies = vec![0; 16];
        for (symbol, frequency) in [10, 9, 8, 7, 6].into_iter().enumerate() {
            frequencies[symbol] = frequency;
        }
        let code = PrefixEncoding::from_frequencies(&frequencies).unwrap();

        assert_eq!(code.lengths[0], 2);
        assert_eq!(code.lengths[1], 2);
        assert_eq!(code.lengths[2], 2);
        assert_eq!(code.lengths[3], 3);
        assert_eq!(code.lengths[4], 3);
    }

    #[test]
    fn zero_run_tokenization_resets_repeat_chains() {
        let lengths = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        let tokens = tokenize_code_lengths(&lengths);
        assert_eq!(tokens[0].symbol, 2);
        assert_eq!(tokens[1].symbol, 17);
        assert_eq!(tokens[1].extra, 7);
        assert_eq!(tokens[2].symbol, 0);
        assert_eq!(tokens[3].symbol, 17);
        assert_eq!(tokens.last().unwrap().symbol, 2);
    }
}
