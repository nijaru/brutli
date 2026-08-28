use super::bit_reader::BitReader;
use super::prefix_code::{PrefixCode, PrefixSymbolDecoder};
use super::prefix_code_decoder::{PrefixCodeDecoder, PrefixCodeDecoderError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextMapError {
    PrefixCode(PrefixCodeDecoderError),
    RepeatOverflow,
    MissingTree,
}

impl From<PrefixCodeDecoderError> for ContextMapError {
    fn from(error: PrefixCodeDecoderError) -> Self {
        Self::PrefixCode(error)
    }
}

#[derive(Debug)]
pub(super) struct ContextMapDecoder {
    size: usize,
    num_trees: u16,
    state: State,
    rle_max: u8,
    prefix_decoder: Option<PrefixCodeDecoder>,
    prefix_code: Option<PrefixCode>,
    symbol_decoder: PrefixSymbolDecoder,
    map: Vec<u8>,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    RleFlag,
    RleValue,
    PrefixCode,
    Entries,
    Repeat { bits: u8 },
    Transform,
    Done,
}

impl ContextMapDecoder {
    pub(super) fn new(size: usize, num_trees: u16) -> Self {
        assert!(size != 0);
        assert!((2..=256).contains(&num_trees));

        Self {
            size,
            num_trees,
            state: State::RleFlag,
            rle_max: 0,
            prefix_decoder: None,
            prefix_code: None,
            symbol_decoder: PrefixSymbolDecoder::default(),
            map: Vec::with_capacity(size),
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<Vec<u8>>, ContextMapError> {
        loop {
            match self.state {
                State::RleFlag => {
                    let Some(bit) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };
                    if bit == 0 {
                        self.initialize_prefix_decoder();
                        self.state = State::PrefixCode;
                    } else {
                        self.state = State::RleValue;
                    }
                }
                State::RleValue => {
                    let Some(bits) = reader.read_bits(input, cursor, 4) else {
                        return Ok(None);
                    };
                    self.rle_max = bits as u8 + 1;
                    self.initialize_prefix_decoder();
                    self.state = State::PrefixCode;
                }
                State::PrefixCode => {
                    let decoder = self
                        .prefix_decoder
                        .as_mut()
                        .expect("prefix decoder is initialized before decoding");
                    let Some(code) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.prefix_code = Some(code);
                    self.prefix_decoder = None;
                    self.state = State::Entries;
                }
                State::Entries => {
                    if self.map.len() == self.size {
                        self.state = State::Transform;
                        continue;
                    }

                    let code = self
                        .prefix_code
                        .as_ref()
                        .expect("context-map prefix code has been decoded");
                    let Some(symbol) = code.decode(
                        &mut self.symbol_decoder,
                        reader,
                        input,
                        cursor,
                    ) else {
                        return Ok(None);
                    };

                    if symbol == 0 {
                        self.map.push(0);
                    } else if symbol <= u16::from(self.rle_max) {
                        self.state = State::Repeat {
                            bits: symbol as u8,
                        };
                    } else {
                        self.map.push((symbol - u16::from(self.rle_max)) as u8);
                    }
                }
                State::Repeat { bits } => {
                    let Some(extra) = reader.read_bits(input, cursor, u32::from(bits)) else {
                        return Ok(None);
                    };
                    let count = (1_usize << bits) + extra as usize;
                    if self.map.len() + count > self.size {
                        return Err(ContextMapError::RepeatOverflow);
                    }
                    self.map.resize(self.map.len() + count, 0);
                    self.state = State::Entries;
                }
                State::Transform => {
                    let Some(bit) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };
                    if bit != 0 {
                        inverse_move_to_front(&mut self.map);
                    }
                    self.validate_tree_set()?;
                    self.state = State::Done;
                    return Ok(Some(core::mem::take(&mut self.map)));
                }
                State::Done => unreachable!("context map decoded more than once"),
            }
        }
    }

    fn initialize_prefix_decoder(&mut self) {
        let alphabet_size = self.num_trees + u16::from(self.rle_max);
        self.prefix_decoder = Some(PrefixCodeDecoder::new(alphabet_size));
    }

    fn validate_tree_set(&self) -> Result<(), ContextMapError> {
        let mut seen = [false; 256];
        for &tree in &self.map {
            let index = usize::from(tree);
            if index >= usize::from(self.num_trees) {
                return Err(ContextMapError::MissingTree);
            }
            seen[index] = true;
        }

        if seen[..usize::from(self.num_trees)].iter().all(|&value| value) {
            Ok(())
        } else {
            Err(ContextMapError::MissingTree)
        }
    }
}

fn inverse_move_to_front(map: &mut [u8]) {
    let mut mtf = [0_u8; 256];
    for (index, value) in mtf.iter_mut().enumerate() {
        *value = index as u8;
    }

    for entry in map {
        let index = usize::from(*entry);
        let value = mtf[index];
        mtf.copy_within(..index, 1);
        mtf[0] = value;
        *entry = value;
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextMapDecoder, ContextMapError, inverse_move_to_front};
    use crate::decode::bit_reader::BitReader;

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

        fn simple_code(&mut self, symbols: &[u16], symbol_bits: u8) {
            self.push(1, 2);
            self.push((symbols.len() - 1) as u64, 2);
            for &symbol in symbols {
                self.push(u64::from(symbol), symbol_bits);
            }
            if symbols.len() == 4 {
                self.push(0, 1);
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
    fn decodes_context_map_without_rle() {
        let mut bits = Bits::default();
        bits.push(0, 1); // RLEMAX = 0
        bits.simple_code(&[0, 1], 1);
        for value in [0, 1, 1, 0] {
            bits.push_prefix(value, 1);
        }
        bits.push(0, 1); // no inverse MTF
        let input = bits.into_bytes();

        let mut decoder = ContextMapDecoder::new(4, 2);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let map = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(map, [0, 1, 1, 0]);
    }

    #[test]
    fn decodes_zero_run() {
        let mut bits = Bits::default();
        bits.push(1, 1); // RLE enabled
        bits.push(0, 4); // RLEMAX = 1
        bits.simple_code(&[1, 2], 2); // repeat-2/3 and value 1
        bits.push_prefix(0, 1); // symbol 1: zero run
        bits.push(1, 1); // repeat 3 zeros
        bits.push_prefix(1, 1); // symbol 2: value 1
        bits.push(0, 1); // no inverse MTF
        let input = bits.into_bytes();

        let mut decoder = ContextMapDecoder::new(4, 2);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let map = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(map, [0, 0, 0, 1]);
    }

    #[test]
    fn rejects_zero_run_past_map_end() {
        let mut bits = Bits::default();
        bits.push(1, 1);
        bits.push(0, 4); // RLEMAX = 1
        bits.simple_code(&[0, 1, 2], 2);
        bits.push_prefix(0b10, 2); // symbol 1 in canonical 2-bit code
        bits.push(1, 1); // repeat 3 zeros into a 2-entry map
        let input = bits.into_bytes();

        let mut decoder = ContextMapDecoder::new(2, 2);
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(
            decoder.decode(&mut reader, &input, &mut cursor),
            Err(ContextMapError::RepeatOverflow)
        );
    }

    #[test]
    fn applies_inverse_move_to_front() {
        let mut map = [0, 1, 1, 0, 2];
        inverse_move_to_front(&mut map);
        assert_eq!(map, [0, 1, 0, 0, 2]);
    }

    #[test]
    fn resumes_repeat_across_input_slices() {
        let mut bits = Bits::default();
        bits.push(1, 1);
        bits.push(0, 4); // RLEMAX = 1
        bits.simple_code(&[1, 2], 2);
        bits.push_prefix(0, 1); // repeat symbol
        bits.push(1, 1); // repeat 3 zeros
        bits.push_prefix(1, 1); // value 1
        bits.push(0, 1);
        let input = bits.into_bytes();

        let mut decoder = ContextMapDecoder::new(4, 2);
        let mut reader = BitReader::default();
        let mut first_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[..1], &mut first_cursor)
                .unwrap()
                .is_none()
        );

        let mut second_cursor = 0;
        let map = decoder
            .decode(&mut reader, &input[1..], &mut second_cursor)
            .unwrap()
            .unwrap();
        assert_eq!(map, [0, 0, 0, 1]);
    }
}
