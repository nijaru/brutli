use super::bit_reader::BitReader;
use super::complex_prefix_code::{ComplexPrefixCodeDecoder, ComplexPrefixCodeError};
use super::prefix_code::PrefixCode;
use super::simple_prefix_code::{SimplePrefixCodeDecoder, SimplePrefixCodeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrefixCodeDecoderError {
    Simple(SimplePrefixCodeError),
    Complex(ComplexPrefixCodeError),
}

impl From<SimplePrefixCodeError> for PrefixCodeDecoderError {
    fn from(error: SimplePrefixCodeError) -> Self {
        Self::Simple(error)
    }
}

impl From<ComplexPrefixCodeError> for PrefixCodeDecoderError {
    fn from(error: ComplexPrefixCodeError) -> Self {
        Self::Complex(error)
    }
}

#[derive(Debug)]
pub(super) struct PrefixCodeDecoder {
    alphabet_size: u16,
    state: State,
}

#[derive(Debug, Default)]
#[expect(
    clippy::large_enum_variant,
    reason = "keeping complex prefix-code construction inline avoids a heap allocation per tree"
)]
enum State {
    #[default]
    Representation,
    Simple(SimplePrefixCodeDecoder),
    Complex(ComplexPrefixCodeDecoder),
    Done,
}

impl PrefixCodeDecoder {
    pub(super) fn new(alphabet_size: u16) -> Self {
        assert!(alphabet_size != 0);
        Self {
            alphabet_size,
            state: State::Representation,
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<PrefixCode>, PrefixCodeDecoderError> {
        loop {
            match &mut self.state {
                State::Representation => {
                    let Some(selector) = reader.read_bits(input, cursor, 2) else {
                        return Ok(None);
                    };
                    self.state = if selector == 1 {
                        State::Simple(SimplePrefixCodeDecoder::new(self.alphabet_size))
                    } else {
                        State::Complex(ComplexPrefixCodeDecoder::new(
                            self.alphabet_size,
                            selector as u8,
                        ))
                    };
                }
                State::Simple(decoder) => {
                    let Some(code) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.state = State::Done;
                    return Ok(Some(code));
                }
                State::Complex(decoder) => {
                    let Some(code) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.state = State::Done;
                    return Ok(Some(code));
                }
                State::Done => unreachable!("prefix-code representation decoded more than once"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PrefixCodeDecoder;
    use crate::decode::bit_reader::BitReader;
    use crate::decode::prefix_code::PrefixSymbolDecoder;

    const CODE_LENGTH_ORDER: [u8; 18] =
        [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

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
                _ => panic!("test only needs code-length values 0 and 1"),
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
    fn routes_simple_representation() {
        let mut bits = Bits::default();
        bits.push(1, 2); // simple representation
        bits.push(1, 2); // two symbols
        bits.push(2, 3);
        bits.push(5, 3);
        let input = bits.into_bytes();

        let mut decoder = PrefixCodeDecoder::new(8);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let code = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        let mut encoded = Bits::default();
        encoded.push_prefix(0, 1);
        encoded.push_prefix(1, 1);
        let encoded = encoded.into_bytes();
        let mut symbol_reader = BitReader::default();
        let mut symbol_state = PrefixSymbolDecoder::default();
        let mut symbol_cursor = 0;

        assert_eq!(
            code.decode(
                &mut symbol_state,
                &mut symbol_reader,
                &encoded,
                &mut symbol_cursor,
            ),
            Some(2)
        );
        assert_eq!(
            code.decode(
                &mut symbol_state,
                &mut symbol_reader,
                &encoded,
                &mut symbol_cursor,
            ),
            Some(5)
        );
    }

    #[test]
    fn routes_complex_representation() {
        let mut bits = Bits::default();
        bits.push(0, 2); // complex representation, HSKIP = 0
        for symbol in CODE_LENGTH_ORDER {
            bits.push_code_length_value(u8::from(symbol == 16));
        }
        for extra in [2, 2, 2, 1] {
            bits.push(extra, 2); // 256 repeats of the initial length 8
        }
        let input = bits.into_bytes();

        let mut decoder = PrefixCodeDecoder::new(256);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let code = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        let mut encoded = Bits::default();
        encoded.push_prefix(0x7b, 8);
        let encoded = encoded.into_bytes();
        let mut symbol_reader = BitReader::default();
        let mut symbol_state = PrefixSymbolDecoder::default();
        let mut symbol_cursor = 0;

        assert_eq!(
            code.decode(
                &mut symbol_state,
                &mut symbol_reader,
                &encoded,
                &mut symbol_cursor,
            ),
            Some(0x7b)
        );
    }

    #[test]
    fn preserves_selector_across_input_exhaustion() {
        let mut bits = Bits::default();
        bits.push(1, 2); // simple representation
        bits.push(3, 2); // four symbols
        for symbol in [1, 2, 3, 4] {
            bits.push(symbol, 8);
        }
        bits.push(0, 1);
        let input = bits.into_bytes();

        let mut decoder = PrefixCodeDecoder::new(256);
        let mut reader = BitReader::default();
        let mut first_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[..1], &mut first_cursor)
                .unwrap()
                .is_none()
        );
        assert_eq!(first_cursor, 1);

        let mut second_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[1..], &mut second_cursor)
                .unwrap()
                .is_some()
        );
    }
}
