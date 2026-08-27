use super::bit_reader::BitReader;
use super::prefix_code::{PrefixCode, PrefixSymbolDecoder};
use super::prefix_code_decoder::{PrefixCodeDecoder, PrefixCodeDecoderError};

const BLOCK_LENGTH_OFFSETS: [usize; 26] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497, 753, 1265,
    2289, 4337, 8433, 16625,
];
const BLOCK_LENGTH_EXTRA_BITS: [u8; 26] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 24,
];

#[derive(Debug)]
pub(super) struct BlockPartition {
    pub(super) num_types: u16,
    pub(super) type_code: Option<PrefixCode>,
    pub(super) length_code: Option<PrefixCode>,
    pub(super) first_length: Option<usize>,
}

impl BlockPartition {
    fn single() -> Self {
        Self {
            num_types: 1,
            type_code: None,
            length_code: None,
            first_length: None,
        }
    }
}

#[derive(Debug)]
pub(super) struct BlockPartitionDecoder {
    num_types: u16,
    state: State,
    type_decoder: Option<PrefixCodeDecoder>,
    length_decoder: Option<PrefixCodeDecoder>,
    type_code: Option<PrefixCode>,
    length_code: Option<PrefixCode>,
    block_length: BlockLengthDecoder,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    TypeCode,
    LengthCode,
    FirstLength,
    Done,
}

impl BlockPartitionDecoder {
    pub(super) fn new(num_types: u16) -> Self {
        assert!(num_types != 0);
        let multiple = num_types > 1;
        Self {
            num_types,
            state: if multiple {
                State::TypeCode
            } else {
                State::Done
            },
            type_decoder: multiple.then(|| PrefixCodeDecoder::new(num_types + 2)),
            length_decoder: multiple.then(|| PrefixCodeDecoder::new(26)),
            type_code: None,
            length_code: None,
            block_length: BlockLengthDecoder::default(),
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<BlockPartition>, PrefixCodeDecoderError> {
        if self.num_types == 1 {
            return Ok(Some(BlockPartition::single()));
        }

        loop {
            match self.state {
                State::TypeCode => {
                    let decoder = self
                        .type_decoder
                        .as_mut()
                        .expect("multi-type partition has a type-code decoder");
                    let Some(code) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.type_code = Some(code);
                    self.state = State::LengthCode;
                }
                State::LengthCode => {
                    let decoder = self
                        .length_decoder
                        .as_mut()
                        .expect("multi-type partition has a length-code decoder");
                    let Some(code) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.length_code = Some(code);
                    self.state = State::FirstLength;
                }
                State::FirstLength => {
                    let code = self
                        .length_code
                        .as_ref()
                        .expect("length prefix code is decoded before its first value");
                    let Some(first_length) = self.block_length.decode(code, reader, input, cursor)
                    else {
                        return Ok(None);
                    };

                    self.state = State::Done;
                    return Ok(Some(BlockPartition {
                        num_types: self.num_types,
                        type_code: self.type_code.take(),
                        length_code: self.length_code.take(),
                        first_length: Some(first_length),
                    }));
                }
                State::Done => unreachable!("block partition decoded more than once"),
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct BlockLengthDecoder {
    symbol_decoder: PrefixSymbolDecoder,
    pending_symbol: Option<u8>,
}

impl BlockLengthDecoder {
    pub(super) fn decode(
        &mut self,
        code: &PrefixCode,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Option<usize> {
        let symbol = match self.pending_symbol {
            Some(symbol) => symbol,
            None => {
                let symbol = code.decode(&mut self.symbol_decoder, reader, input, cursor)? as u8;
                debug_assert!(usize::from(symbol) < BLOCK_LENGTH_OFFSETS.len());
                self.pending_symbol = Some(symbol);
                symbol
            }
        };

        let extra_bits = BLOCK_LENGTH_EXTRA_BITS[usize::from(symbol)];
        let extra = reader.read_bits(input, cursor, u32::from(extra_bits))? as usize;
        self.pending_symbol = None;
        Some(BLOCK_LENGTH_OFFSETS[usize::from(symbol)] + extra)
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockLengthDecoder, BlockPartitionDecoder};
    use crate::decode::bit_reader::BitReader;
    use crate::decode::prefix_code::PrefixCode;

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

    fn two_type_partition() -> Vec<u8> {
        let mut bits = Bits::default();

        bits.push(1, 2); // simple block-type prefix code
        bits.push(1, 2); // two symbols
        bits.push(0, 2); // symbol 0 in alphabet of four
        bits.push(1, 2); // symbol 1

        bits.push(1, 2); // simple block-length prefix code
        bits.push(0, 2); // one symbol
        bits.push(0, 5); // block-length symbol 0
        bits.push(3, 2); // offset 1 + 3 = 4

        bits.into_bytes()
    }

    #[test]
    fn single_type_partition_consumes_no_input() {
        let mut decoder = BlockPartitionDecoder::new(1);
        let mut reader = BitReader::default();
        let mut cursor = 0;

        let partition = decoder
            .decode(&mut reader, &[0xff], &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(partition.num_types, 1);
        assert!(partition.type_code.is_none());
        assert!(partition.length_code.is_none());
        assert!(partition.first_length.is_none());
        assert_eq!(cursor, 0);
    }

    #[test]
    fn decodes_multi_type_partition_and_first_length() {
        let input = two_type_partition();
        let mut decoder = BlockPartitionDecoder::new(2);
        let mut reader = BitReader::default();
        let mut cursor = 0;

        let partition = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(partition.num_types, 2);
        assert!(partition.type_code.is_some());
        assert!(partition.length_code.is_some());
        assert_eq!(partition.first_length, Some(4));
    }

    #[test]
    fn block_length_retains_symbol_until_extra_bits_arrive() {
        let code = PrefixCode::single(25);
        let mut reader = BitReader::default();
        let mut decoder = BlockLengthDecoder::default();
        let mut first_cursor = 0;

        assert_eq!(
            decoder.decode(&code, &mut reader, &[0x34], &mut first_cursor),
            None
        );
        assert_eq!(first_cursor, 1);

        let mut second_cursor = 0;
        assert_eq!(
            decoder.decode(&code, &mut reader, &[0x12, 0, 0], &mut second_cursor,),
            Some(16625 + 0x1234)
        );
    }

    #[test]
    fn partition_resumes_across_input_slices() {
        let input = two_type_partition();
        let split = 1;
        let mut decoder = BlockPartitionDecoder::new(2);
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
