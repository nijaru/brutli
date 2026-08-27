use super::bit_reader::BitReader;
use super::prefix_code::{PrefixCode, PrefixCodeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SimplePrefixCodeError {
    InvalidSymbol,
    DuplicateSymbol,
    InvalidTree(PrefixCodeError),
}

impl From<PrefixCodeError> for SimplePrefixCodeError {
    fn from(error: PrefixCodeError) -> Self {
        Self::InvalidTree(error)
    }
}

#[derive(Debug)]
pub(super) struct SimplePrefixCodeDecoder {
    alphabet_size: u16,
    alphabet_bits: u8,
    state: State,
    symbol_count: u8,
    symbols_read: u8,
    symbols: [u16; 4],
}

#[derive(Debug, Default)]
enum State {
    #[default]
    SymbolCount,
    Symbols,
    TreeSelect,
}

impl SimplePrefixCodeDecoder {
    pub(super) fn new(alphabet_size: u16) -> Self {
        assert!(alphabet_size != 0);
        let alphabet_bits = u16::BITS - (alphabet_size - 1).leading_zeros();
        Self {
            alphabet_size,
            alphabet_bits: alphabet_bits as u8,
            state: State::SymbolCount,
            symbol_count: 0,
            symbols_read: 0,
            symbols: [0; 4],
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<PrefixCode>, SimplePrefixCodeError> {
        loop {
            match self.state {
                State::SymbolCount => {
                    let Some(bits) = reader.read_bits(input, cursor, 2) else {
                        return Ok(None);
                    };
                    self.symbol_count = bits as u8 + 1;
                    self.symbols_read = 0;
                    self.state = State::Symbols;
                }
                State::Symbols => {
                    while self.symbols_read < self.symbol_count {
                        let Some(symbol) =
                            reader.read_bits(input, cursor, u32::from(self.alphabet_bits))
                        else {
                            return Ok(None);
                        };
                        let symbol = symbol as u16;
                        if symbol >= self.alphabet_size {
                            return Err(SimplePrefixCodeError::InvalidSymbol);
                        }
                        if self.symbols[..usize::from(self.symbols_read)].contains(&symbol) {
                            return Err(SimplePrefixCodeError::DuplicateSymbol);
                        }
                        self.symbols[usize::from(self.symbols_read)] = symbol;
                        self.symbols_read += 1;
                    }

                    if self.symbol_count == 4 {
                        self.state = State::TreeSelect;
                        continue;
                    }
                    return self.finish(false).map(Some);
                }
                State::TreeSelect => {
                    let Some(bit) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };
                    return self.finish(bit != 0).map(Some);
                }
            }
        }
    }

    fn finish(&mut self, tree_select: bool) -> Result<PrefixCode, SimplePrefixCodeError> {
        let count = usize::from(self.symbol_count);
        let code = if count == 1 {
            PrefixCode::single(self.symbols[0])
        } else {
            let code_lengths = match (count, tree_select) {
                (2, _) => [1, 1, 0, 0],
                (3, _) => [1, 2, 2, 0],
                (4, false) => [2, 2, 2, 2],
                (4, true) => [1, 2, 3, 3],
                _ => unreachable!(),
            };
            let mut lengths = vec![0_u8; usize::from(self.alphabet_size)];
            for index in 0..count {
                lengths[usize::from(self.symbols[index])] = code_lengths[index];
            }
            PrefixCode::from_code_lengths(&lengths)?
        };

        self.state = State::SymbolCount;
        self.symbol_count = 0;
        self.symbols_read = 0;
        self.symbols = [0; 4];
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::{SimplePrefixCodeDecoder, SimplePrefixCodeError};
    use crate::decode::bit_reader::BitReader;
    use crate::decode::prefix_code::PrefixSymbolDecoder;

    #[derive(Default)]
    struct Bits {
        bits: Vec<bool>,
    }

    impl Bits {
        fn push(&mut self, value: u64, count: u8) {
            for bit in 0..count {
                self.bits.push((value >> bit) & 1 != 0);
            }
        }

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
    fn decodes_single_symbol_code() {
        let mut bits = Bits::default();
        bits.push(0, 2); // NSYM - 1
        bits.push(5, 3);
        let input = bits.into_bytes();

        let mut decoder = SimplePrefixCodeDecoder::new(8);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let code = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        let mut symbol_state = PrefixSymbolDecoder::default();
        assert_eq!(
            code.decode(&mut symbol_state, &mut reader, &[], &mut cursor,),
            Some(5)
        );
    }

    #[test]
    fn preserves_simple_tree_symbol_order_rules() {
        let mut bits = Bits::default();
        bits.push(3, 2); // four symbols
        bits.push(3, 3); // length 1
        bits.push(0, 3); // length 2
        bits.push(2, 3); // length 3
        bits.push(1, 3); // length 3
        bits.push(1, 1); // tree-select
        let input = bits.into_bytes();

        let mut decoder = SimplePrefixCodeDecoder::new(8);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let code = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        let mut encoded = Bits::default();
        encoded.push_prefix(0, 1); // symbol 3
        encoded.push_prefix(0b10, 2); // symbol 0
        encoded.push_prefix(0b110, 3); // symbol 1
        encoded.push_prefix(0b111, 3); // symbol 2
        let encoded = encoded.into_bytes();
        let mut data_reader = BitReader::default();
        let mut data_cursor = 0;
        let mut symbol_state = PrefixSymbolDecoder::default();

        for expected in [3, 0, 1, 2] {
            assert_eq!(
                code.decode(
                    &mut symbol_state,
                    &mut data_reader,
                    &encoded,
                    &mut data_cursor,
                ),
                Some(expected)
            );
        }
    }

    #[test]
    fn rejects_duplicate_symbols() {
        let mut bits = Bits::default();
        bits.push(1, 2); // two symbols
        bits.push(2, 3);
        bits.push(2, 3);
        let input = bits.into_bytes();

        let mut decoder = SimplePrefixCodeDecoder::new(8);
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert!(matches!(
            decoder.decode(&mut reader, &input, &mut cursor),
            Err(SimplePrefixCodeError::DuplicateSymbol)
        ));
    }

    #[test]
    fn rejects_symbol_outside_alphabet() {
        let mut bits = Bits::default();
        bits.push(0, 2);
        bits.push(7, 3);
        let input = bits.into_bytes();

        let mut decoder = SimplePrefixCodeDecoder::new(5);
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert!(matches!(
            decoder.decode(&mut reader, &input, &mut cursor),
            Err(SimplePrefixCodeError::InvalidSymbol)
        ));
    }

    #[test]
    fn resumes_across_input_slices() {
        let mut bits = Bits::default();
        bits.push(3, 2);
        bits.push(0, 8);
        bits.push(1, 8);
        bits.push(2, 8);
        bits.push(3, 8);
        bits.push(0, 1);
        let input = bits.into_bytes();

        let mut decoder = SimplePrefixCodeDecoder::new(256);
        let mut reader = BitReader::default();
        let mut first_cursor = 0;

        assert!(
            decoder
                .decode(&mut reader, &input[..2], &mut first_cursor)
                .unwrap()
                .is_none()
        );
        assert_eq!(first_cursor, 2);

        let mut second_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[2..], &mut second_cursor)
                .unwrap()
                .is_some()
        );
    }
}
