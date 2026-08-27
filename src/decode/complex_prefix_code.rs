use super::bit_reader::BitReader;
use super::prefix_code::{PrefixCode, PrefixCodeError, PrefixSymbolDecoder};

const CODE_LENGTH_ORDER: [u8; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const INITIAL_REPEATED_CODE_LENGTH: u8 = 8;
const CODE_SPACE: i32 = 1 << 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComplexPrefixCodeError {
    InvalidCodeLengthAlphabet,
    InvalidTree(PrefixCodeError),
    RepeatOverflow,
    RepeatPastAlphabet,
    TooFewSymbols,
    UnusedCodeSpace,
}

impl From<PrefixCodeError> for ComplexPrefixCodeError {
    fn from(error: PrefixCodeError) -> Self {
        Self::InvalidTree(error)
    }
}

#[derive(Debug)]
pub(super) struct ComplexPrefixCodeDecoder {
    alphabet_size: u16,
    state: State,
    fixed_decoder: CodeLengthValueDecoder,
    code_length_index: u8,
    code_length_lengths: [u8; 18],
    code_length_space: i16,
    code_length_nonzero: u8,
    code_length_code: Option<PrefixCode>,
    code_length_symbol: PrefixSymbolDecoder,
    lengths: Vec<u8>,
    symbol_index: usize,
    previous_nonzero: u8,
    repeat: usize,
    repeat_code_length: u8,
    nonzero_count: usize,
    space: i32,
}

#[derive(Debug)]
enum State {
    CodeLengthAlphabet,
    CodeLengths,
    RepeatExtra { symbol: u8 },
    Done,
}

impl ComplexPrefixCodeDecoder {
    pub(super) fn new(alphabet_size: u16, skip: u8) -> Self {
        assert!(alphabet_size != 0);
        assert!(matches!(skip, 0 | 2 | 3));
        Self {
            alphabet_size,
            state: State::CodeLengthAlphabet,
            fixed_decoder: CodeLengthValueDecoder::default(),
            code_length_index: skip,
            code_length_lengths: [0; 18],
            code_length_space: 32,
            code_length_nonzero: 0,
            code_length_code: None,
            code_length_symbol: PrefixSymbolDecoder::default(),
            lengths: vec![0; usize::from(alphabet_size)],
            symbol_index: 0,
            previous_nonzero: INITIAL_REPEATED_CODE_LENGTH,
            repeat: 0,
            repeat_code_length: 0,
            nonzero_count: 0,
            space: CODE_SPACE,
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<PrefixCode>, ComplexPrefixCodeError> {
        loop {
            match self.state {
                State::CodeLengthAlphabet => {
                    while self.code_length_index < CODE_LENGTH_ORDER.len() as u8
                        && self.code_length_space > 0
                    {
                        let Some(length) = self.fixed_decoder.decode(reader, input, cursor) else {
                            return Ok(None);
                        };
                        let symbol = CODE_LENGTH_ORDER[usize::from(self.code_length_index)];
                        self.code_length_lengths[usize::from(symbol)] = length;
                        self.code_length_index += 1;

                        if length != 0 {
                            self.code_length_nonzero += 1;
                            self.code_length_space -= 32 >> length;
                            if self.code_length_space < 0 {
                                return Err(ComplexPrefixCodeError::InvalidCodeLengthAlphabet);
                            }
                        }
                    }

                    if self.code_length_space != 0 && self.code_length_nonzero != 1 {
                        return Err(ComplexPrefixCodeError::InvalidCodeLengthAlphabet);
                    }

                    self.code_length_code =
                        Some(PrefixCode::from_code_lengths(&self.code_length_lengths)?);
                    self.state = State::CodeLengths;
                }
                State::CodeLengths => {
                    if self.space == 0 {
                        return self.finish().map(Some);
                    }
                    if self.symbol_index == usize::from(self.alphabet_size) {
                        return Err(ComplexPrefixCodeError::UnusedCodeSpace);
                    }

                    let code = self
                        .code_length_code
                        .as_ref()
                        .expect("code-length prefix code is initialized before decoding lengths");
                    let Some(symbol) =
                        code.decode(&mut self.code_length_symbol, reader, input, cursor)
                    else {
                        return Ok(None);
                    };

                    match symbol {
                        0..=15 => self.push_length(symbol as u8)?,
                        16 | 17 => {
                            self.state = State::RepeatExtra {
                                symbol: symbol as u8,
                            };
                        }
                        _ => unreachable!("code-length alphabet is limited to symbols 0..=17"),
                    }
                }
                State::RepeatExtra { symbol } => {
                    let extra_bits = symbol - 14;
                    let Some(extra) = reader.read_bits(input, cursor, u32::from(extra_bits)) else {
                        return Ok(None);
                    };
                    self.push_repeat(symbol, extra as usize)?;
                    self.state = State::CodeLengths;
                }
                State::Done => unreachable!("complex prefix code decoded more than once"),
            }
        }
    }

    fn push_length(&mut self, length: u8) -> Result<(), ComplexPrefixCodeError> {
        self.repeat = 0;
        self.lengths[self.symbol_index] = length;
        self.symbol_index += 1;

        if length != 0 {
            self.previous_nonzero = length;
            self.nonzero_count += 1;
            self.consume_space(1, length)?;
        }
        Ok(())
    }

    fn push_repeat(&mut self, symbol: u8, extra: usize) -> Result<(), ComplexPrefixCodeError> {
        let extra_bits = symbol - 14;
        let code_length = if symbol == 16 {
            self.previous_nonzero
        } else {
            0
        };

        if self.repeat_code_length != code_length {
            self.repeat = 0;
            self.repeat_code_length = code_length;
        }

        let old_repeat = self.repeat;
        let base = if old_repeat == 0 {
            0
        } else {
            old_repeat
                .checked_sub(2)
                .and_then(|value| value.checked_shl(u32::from(extra_bits)))
                .ok_or(ComplexPrefixCodeError::RepeatOverflow)?
        };
        let repeat = base
            .checked_add(extra + 3)
            .ok_or(ComplexPrefixCodeError::RepeatOverflow)?;
        let delta = repeat
            .checked_sub(old_repeat)
            .ok_or(ComplexPrefixCodeError::RepeatOverflow)?;
        let end = self
            .symbol_index
            .checked_add(delta)
            .ok_or(ComplexPrefixCodeError::RepeatOverflow)?;
        if end > usize::from(self.alphabet_size) {
            return Err(ComplexPrefixCodeError::RepeatPastAlphabet);
        }

        self.lengths[self.symbol_index..end].fill(code_length);
        self.symbol_index = end;
        self.repeat = repeat;

        if code_length != 0 {
            self.nonzero_count += delta;
            self.consume_space(delta, code_length)?;
        }
        Ok(())
    }

    fn consume_space(&mut self, count: usize, length: u8) -> Result<(), ComplexPrefixCodeError> {
        let unit = CODE_SPACE >> length;
        let used = i32::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(unit))
            .ok_or(ComplexPrefixCodeError::RepeatOverflow)?;
        self.space -= used;
        if self.space < 0 {
            return Err(ComplexPrefixCodeError::InvalidTree(
                PrefixCodeError::Oversubscribed,
            ));
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<PrefixCode, ComplexPrefixCodeError> {
        if self.nonzero_count < 2 {
            return Err(ComplexPrefixCodeError::TooFewSymbols);
        }
        let code = PrefixCode::from_code_lengths(&self.lengths)?;
        self.state = State::Done;
        Ok(code)
    }
}

#[derive(Debug, Default)]
struct CodeLengthValueDecoder {
    state: CodeLengthValueState,
}

#[derive(Debug, Default)]
enum CodeLengthValueState {
    #[default]
    FirstTwo,
    Third,
    Fourth,
}

impl CodeLengthValueDecoder {
    fn decode(&mut self, reader: &mut BitReader, input: &[u8], cursor: &mut usize) -> Option<u8> {
        loop {
            match self.state {
                CodeLengthValueState::FirstTwo => match reader.read_bits(input, cursor, 2)? {
                    0 => return Some(0),
                    1 => return Some(4),
                    2 => return Some(3),
                    3 => self.state = CodeLengthValueState::Third,
                    _ => unreachable!(),
                },
                CodeLengthValueState::Third => {
                    if reader.read_bits(input, cursor, 1)? == 0 {
                        self.state = CodeLengthValueState::FirstTwo;
                        return Some(2);
                    }
                    self.state = CodeLengthValueState::Fourth;
                }
                CodeLengthValueState::Fourth => {
                    let value = if reader.read_bits(input, cursor, 1)? == 0 {
                        1
                    } else {
                        5
                    };
                    self.state = CodeLengthValueState::FirstTwo;
                    return Some(value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CODE_LENGTH_ORDER, CodeLengthValueDecoder, ComplexPrefixCodeDecoder, ComplexPrefixCodeError,
    };
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

        fn push_code_length_value(&mut self, value: u8) {
            let (bits, count) = match value {
                0 => (0b00, 2),
                1 => (0b0111, 4),
                2 => (0b011, 3),
                3 => (0b10, 2),
                4 => (0b01, 2),
                5 => (0b1111, 4),
                _ => panic!("code-length code length must be 0..=5"),
            };
            self.push(bits, count);
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
    fn fixed_code_decodes_all_code_length_values() {
        let mut bits = Bits::default();
        for value in 0..=5 {
            bits.push_code_length_value(value);
        }
        let input = bits.into_bytes();
        let mut reader = BitReader::default();
        let mut decoder = CodeLengthValueDecoder::default();
        let mut cursor = 0;

        for expected in 0..=5 {
            assert_eq!(
                decoder.decode(&mut reader, &input, &mut cursor),
                Some(expected)
            );
        }
    }

    #[test]
    fn decodes_chained_repeat_16() {
        let mut bits = Bits::default();
        for symbol in CODE_LENGTH_ORDER {
            bits.push_code_length_value(u8::from(symbol == 16));
        }
        for extra in [2, 2, 2, 1] {
            bits.push(extra, 2);
        }
        let input = bits.into_bytes();

        let mut decoder = ComplexPrefixCodeDecoder::new(256, 0);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let code = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        let mut encoded = Bits::default();
        encoded.push_prefix(0xa5, 8);
        let encoded = encoded.into_bytes();
        let mut data_reader = BitReader::default();
        let mut data_cursor = 0;
        let mut symbol_state = PrefixSymbolDecoder::default();
        assert_eq!(
            code.decode(
                &mut symbol_state,
                &mut data_reader,
                &encoded,
                &mut data_cursor,
            ),
            Some(0xa5)
        );
    }

    #[test]
    fn decodes_chained_repeat_17() {
        let mut bits = Bits::default();
        for symbol in CODE_LENGTH_ORDER.into_iter().take(10) {
            bits.push_code_length_value(u8::from(matches!(symbol, 7 | 17)));
        }

        for extra in [0, 6, 5] {
            bits.push_prefix(1, 1); // code-length symbol 17
            bits.push(extra, 3);
        }
        for _ in 0..128 {
            bits.push_prefix(0, 1); // code-length symbol 7
        }
        let input = bits.into_bytes();

        let mut decoder = ComplexPrefixCodeDecoder::new(256, 0);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let code = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        let mut encoded = Bits::default();
        encoded.push_prefix(200 - 128, 7);
        let encoded = encoded.into_bytes();
        let mut data_reader = BitReader::default();
        let mut data_cursor = 0;
        let mut symbol_state = PrefixSymbolDecoder::default();
        assert_eq!(
            code.decode(
                &mut symbol_state,
                &mut data_reader,
                &encoded,
                &mut data_cursor,
            ),
            Some(200)
        );
    }

    #[test]
    fn rejects_repeat_past_alphabet() {
        let mut bits = Bits::default();
        for symbol in CODE_LENGTH_ORDER {
            bits.push_code_length_value(u8::from(symbol == 17));
        }
        bits.push(7, 3); // repeat ten zeros into an alphabet of four
        let input = bits.into_bytes();

        let mut decoder = ComplexPrefixCodeDecoder::new(4, 0);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        assert!(matches!(
            decoder.decode(&mut reader, &input, &mut cursor),
            Err(ComplexPrefixCodeError::RepeatPastAlphabet)
        ));
    }

    #[test]
    fn resumes_across_input_slices() {
        let mut bits = Bits::default();
        for symbol in CODE_LENGTH_ORDER {
            bits.push_code_length_value(u8::from(symbol == 16));
        }
        for extra in [2, 2, 2, 1] {
            bits.push(extra, 2);
        }
        let input = bits.into_bytes();

        let split = input.len() / 2;
        let mut decoder = ComplexPrefixCodeDecoder::new(256, 0);
        let mut reader = BitReader::default();
        let mut first_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[..split], &mut first_cursor)
                .unwrap()
                .is_none()
        );
        assert_eq!(first_cursor, split);

        let mut second_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[split..], &mut second_cursor)
                .unwrap()
                .is_some()
        );
    }
}
