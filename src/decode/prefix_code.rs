use super::bit_reader::BitReader;

const MAX_CODE_BITS: usize = 15;
const FAST_BITS: u8 = 8;
const FAST_SIZE: usize = 1 << FAST_BITS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrefixCodeError {
    Empty,
    InvalidCodeLength,
    Incomplete,
    Oversubscribed,
}

#[derive(Debug, Clone)]
pub(super) enum PrefixCode {
    Single(u16),
    Canonical(CanonicalCode),
}

impl PrefixCode {
    pub(super) fn single(symbol: u16) -> Self {
        Self::Single(symbol)
    }

    pub(super) fn from_code_lengths(lengths: &[u8]) -> Result<Self, PrefixCodeError> {
        let non_zero = lengths.iter().filter(|&&length| length != 0).count();
        if non_zero == 0 {
            return Err(PrefixCodeError::Empty);
        }
        if non_zero == 1 {
            let symbol = lengths
                .iter()
                .position(|&length| length != 0)
                .expect("non-zero code length was counted");
            return Ok(Self::Single(symbol as u16));
        }

        CanonicalCode::from_code_lengths(lengths).map(Self::Canonical)
    }

    pub(super) fn decode(
        &self,
        state: &mut PrefixSymbolDecoder,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Option<u16> {
        match self {
            Self::Single(symbol) => Some(*symbol),
            Self::Canonical(code) => code.decode(state, reader, input, cursor),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FastEntry {
    symbol: u16,
    bits: u8,
}

impl FastEntry {
    const EMPTY: Self = Self { symbol: 0, bits: 0 };
}

#[derive(Debug, Clone)]
pub(super) struct CanonicalCode {
    counts: [u16; MAX_CODE_BITS + 1],
    first_code: [u16; MAX_CODE_BITS + 1],
    first_symbol: [u16; MAX_CODE_BITS + 1],
    symbols: Vec<u16>,
    fast: Box<[FastEntry; FAST_SIZE]>,
    max_len: u8,
}

impl CanonicalCode {
    fn from_code_lengths(lengths: &[u8]) -> Result<Self, PrefixCodeError> {
        let mut counts = [0_u16; MAX_CODE_BITS + 1];
        let mut non_zero = 0_usize;
        let mut max_len = 0_u8;

        for &length in lengths {
            if usize::from(length) > MAX_CODE_BITS {
                return Err(PrefixCodeError::InvalidCodeLength);
            }
            if length != 0 {
                counts[usize::from(length)] += 1;
                non_zero += 1;
                max_len = max_len.max(length);
            }
        }

        if non_zero == 0 {
            return Err(PrefixCodeError::Empty);
        }

        let full = 1_usize << MAX_CODE_BITS;
        let used = counts
            .iter()
            .enumerate()
            .skip(1)
            .map(|(length, &count)| usize::from(count) << (MAX_CODE_BITS - length))
            .sum::<usize>();
        if used < full {
            return Err(PrefixCodeError::Incomplete);
        }
        if used > full {
            return Err(PrefixCodeError::Oversubscribed);
        }

        let mut first_code = [0_u16; MAX_CODE_BITS + 1];
        let mut next = 0_u16;
        for length in 1..=MAX_CODE_BITS {
            next = (next + counts[length - 1]) << 1;
            first_code[length] = next;
        }

        let mut symbols = Vec::with_capacity(non_zero);
        let mut first_symbol = [0_u16; MAX_CODE_BITS + 1];
        for length in 1..=MAX_CODE_BITS {
            first_symbol[length] = symbols.len() as u16;
            for (symbol, &symbol_length) in lengths.iter().enumerate() {
                if usize::from(symbol_length) == length {
                    symbols.push(symbol as u16);
                }
            }
        }

        let fast = build_fast_table(lengths, &first_code);

        Ok(Self {
            counts,
            first_code,
            first_symbol,
            symbols,
            fast,
            max_len,
        })
    }

    fn decode(
        &self,
        state: &mut PrefixSymbolDecoder,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Option<u16> {
        if state.length == 0 && reader.ensure_bits(input, cursor, u32::from(FAST_BITS)) {
            let entry = self.fast[reader.peek_bits(u32::from(FAST_BITS)) as usize];
            if entry.bits != 0 {
                reader.consume_bits(u32::from(entry.bits));
                return Some(entry.symbol);
            }
        }

        loop {
            let bit = reader.read_bits(input, cursor, 1)? as u16;
            state.code = (state.code << 1) | bit;
            state.length += 1;

            let length = usize::from(state.length);
            let count = self.counts[length];
            let first = self.first_code[length];
            if count != 0 && state.code >= first && state.code - first < count {
                let index = self.first_symbol[length] + (state.code - first);
                let symbol = self.symbols[usize::from(index)];
                state.reset();
                return Some(symbol);
            }

            debug_assert!(state.length < self.max_len);
        }
    }
}

fn build_fast_table(
    lengths: &[u8],
    first_code: &[u16; MAX_CODE_BITS + 1],
) -> Box<[FastEntry; FAST_SIZE]> {
    let mut table = Box::new([FastEntry::EMPTY; FAST_SIZE]);
    let mut next_code = *first_code;

    for (symbol, &length) in lengths.iter().enumerate() {
        if length == 0 {
            continue;
        }

        let length_index = usize::from(length);
        let code = next_code[length_index];
        next_code[length_index] += 1;

        if length > FAST_BITS {
            continue;
        }

        let reversed = reverse_low_bits(code, length);
        let repetitions = 1_usize << (FAST_BITS - length);
        for suffix in 0..repetitions {
            let index = usize::from(reversed) | (suffix << length);
            table[index] = FastEntry {
                symbol: symbol as u16,
                bits: length,
            };
        }
    }

    table
}

fn reverse_low_bits(value: u16, count: u8) -> u16 {
    value.reverse_bits() >> (u16::BITS as u8 - count)
}

#[derive(Debug, Default)]
pub(super) struct PrefixSymbolDecoder {
    code: u16,
    length: u8,
}

impl PrefixSymbolDecoder {
    fn reset(&mut self) {
        self.code = 0;
        self.length = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{PrefixCode, PrefixCodeError, PrefixSymbolDecoder};
    use crate::decode::bit_reader::BitReader;

    #[derive(Default)]
    struct Bits {
        bits: Vec<bool>,
    }

    impl Bits {
        fn push_prefix(&mut self, code: u16, length: u8) {
            for shift in (0..length).rev() {
                self.bits.push((code >> shift) & 1 != 0);
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            let mut bytes = vec![0; self.bits.len().div_ceil(8)];
            for (index, bit) in self.bits.into_iter().enumerate() {
                if bit {
                    bytes[index / 8] |= 1 << (index % 8);
                }
            }
            bytes
        }
    }

    #[test]
    fn decodes_rfc_canonical_example() {
        let code = PrefixCode::from_code_lengths(&[3, 3, 3, 3, 3, 2, 4, 4]).unwrap();
        let expected_codes = [
            (5, 0b00, 2),
            (0, 0b010, 3),
            (1, 0b011, 3),
            (2, 0b100, 3),
            (3, 0b101, 3),
            (4, 0b110, 3),
            (6, 0b1110, 4),
            (7, 0b1111, 4),
        ];

        let mut bits = Bits::default();
        for &(_, encoded, length) in &expected_codes {
            bits.push_prefix(encoded, length);
        }
        let input = bits.into_bytes();

        let mut reader = BitReader::default();
        let mut state = PrefixSymbolDecoder::default();
        let mut cursor = 0;
        for &(expected, _, _) in &expected_codes {
            assert_eq!(
                code.decode(&mut state, &mut reader, &input, &mut cursor),
                Some(expected)
            );
        }
    }

    #[test]
    fn long_code_uses_canonical_fallback() {
        let code = PrefixCode::from_code_lengths(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 9]).unwrap();
        let mut bits = Bits::default();
        bits.push_prefix(0b1_1111_1111, 9);
        let input = bits.into_bytes();
        let mut reader = BitReader::default();
        let mut state = PrefixSymbolDecoder::default();
        let mut cursor = 0;

        assert_eq!(
            code.decode(&mut state, &mut reader, &input, &mut cursor),
            Some(9)
        );
    }

    #[test]
    fn single_symbol_consumes_no_bits() {
        let code = PrefixCode::single(42);
        let mut reader = BitReader::default();
        let mut state = PrefixSymbolDecoder::default();
        let mut cursor = 0;

        assert_eq!(
            code.decode(&mut state, &mut reader, &[0xff], &mut cursor),
            Some(42)
        );
        assert_eq!(cursor, 0);
    }

    #[test]
    fn resumes_symbol_across_input_slices() {
        let code = PrefixCode::from_code_lengths(&[1, 2, 3, 3]).unwrap();
        let mut bits = Bits::default();
        for _ in 0..3 {
            bits.push_prefix(0b111, 3);
        }
        let input = bits.into_bytes();

        let mut reader = BitReader::default();
        let mut state = PrefixSymbolDecoder::default();
        let mut first_cursor = 0;

        assert_eq!(
            code.decode(&mut state, &mut reader, &input[..1], &mut first_cursor),
            Some(3)
        );
        assert_eq!(
            code.decode(&mut state, &mut reader, &input[..1], &mut first_cursor),
            Some(3)
        );
        assert_eq!(
            code.decode(&mut state, &mut reader, &input[..1], &mut first_cursor),
            None
        );
        assert_eq!(first_cursor, 1);

        let mut second_cursor = 0;
        assert_eq!(
            code.decode(&mut state, &mut reader, &input[1..], &mut second_cursor),
            Some(3)
        );
    }

    #[test]
    fn short_code_decodes_when_fast_refill_cannot_reach_eight_bits() {
        let code = PrefixCode::from_code_lengths(&[1, 2, 3, 3]).unwrap();
        let mut reader = BitReader::default();
        let mut state = PrefixSymbolDecoder::default();
        let mut cursor = 0;

        assert_eq!(
            code.decode(&mut state, &mut reader, &[0], &mut cursor),
            Some(0)
        );
    }

    #[test]
    fn rejects_incomplete_tree() {
        assert_eq!(
            PrefixCode::from_code_lengths(&[2, 2]).unwrap_err(),
            PrefixCodeError::Incomplete
        );
    }

    #[test]
    fn rejects_oversubscribed_tree() {
        assert_eq!(
            PrefixCode::from_code_lengths(&[1, 1, 1]).unwrap_err(),
            PrefixCodeError::Oversubscribed
        );
    }

    #[test]
    fn rejects_too_long_code() {
        assert_eq!(
            PrefixCode::from_code_lengths(&[1, 16]).unwrap_err(),
            PrefixCodeError::InvalidCodeLength
        );
    }
}
