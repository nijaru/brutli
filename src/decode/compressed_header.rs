use super::bit_reader::BitReader;
use super::block_partition::{BlockPartition, BlockPartitionDecoder};
use super::prefix_code_decoder::PrefixCodeDecoderError;
use super::var_len_uint8::VarLenUint8Decoder;

#[derive(Debug)]
pub(super) struct CompressedHeader {
    pub(super) literal_partition: BlockPartition,
    pub(super) command_partition: BlockPartition,
    pub(super) distance_partition: BlockPartition,
    pub(super) distance_postfix_bits: u8,
    pub(super) num_direct_distance_codes: u16,
    pub(super) literal_context_modes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct CompressedHeaderDecoder {
    state: State,
    count_decoder: VarLenUint8Decoder,
    partition_decoder: Option<BlockPartitionDecoder>,
    literal_partition: Option<BlockPartition>,
    command_partition: Option<BlockPartition>,
    distance_partition: Option<BlockPartition>,
    distance_postfix_bits: u8,
    literal_context_modes: Vec<u8>,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    LiteralTypeCount,
    LiteralPartition,
    CommandTypeCount,
    CommandPartition,
    DistanceTypeCount,
    DistancePartition,
    DistancePostfix,
    DirectDistance,
    ContextModes,
    Done,
}

impl Default for CompressedHeaderDecoder {
    fn default() -> Self {
        Self {
            state: State::LiteralTypeCount,
            count_decoder: VarLenUint8Decoder::default(),
            partition_decoder: None,
            literal_partition: None,
            command_partition: None,
            distance_partition: None,
            distance_postfix_bits: 0,
            literal_context_modes: Vec::new(),
        }
    }
}

impl CompressedHeaderDecoder {
    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<CompressedHeader>, PrefixCodeDecoderError> {
        loop {
            match self.state {
                State::LiteralTypeCount => {
                    let Some(encoded) = self.count_decoder.decode(reader, input, cursor) else {
                        return Ok(None);
                    };
                    let count = u16::from(encoded) + 1;
                    self.partition_decoder = Some(BlockPartitionDecoder::new(count));
                    self.state = State::LiteralPartition;
                }
                State::LiteralPartition => {
                    let decoder = self
                        .partition_decoder
                        .as_mut()
                        .expect("literal partition decoder is initialized");
                    let Some(partition) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.literal_context_modes
                        .reserve_exact(usize::from(partition.num_types));
                    self.literal_partition = Some(partition);
                    self.partition_decoder = None;
                    self.state = State::CommandTypeCount;
                }
                State::CommandTypeCount => {
                    let Some(encoded) = self.count_decoder.decode(reader, input, cursor) else {
                        return Ok(None);
                    };
                    self.partition_decoder =
                        Some(BlockPartitionDecoder::new(u16::from(encoded) + 1));
                    self.state = State::CommandPartition;
                }
                State::CommandPartition => {
                    let decoder = self
                        .partition_decoder
                        .as_mut()
                        .expect("command partition decoder is initialized");
                    let Some(partition) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.command_partition = Some(partition);
                    self.partition_decoder = None;
                    self.state = State::DistanceTypeCount;
                }
                State::DistanceTypeCount => {
                    let Some(encoded) = self.count_decoder.decode(reader, input, cursor) else {
                        return Ok(None);
                    };
                    self.partition_decoder =
                        Some(BlockPartitionDecoder::new(u16::from(encoded) + 1));
                    self.state = State::DistancePartition;
                }
                State::DistancePartition => {
                    let decoder = self
                        .partition_decoder
                        .as_mut()
                        .expect("distance partition decoder is initialized");
                    let Some(partition) = decoder.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.distance_partition = Some(partition);
                    self.partition_decoder = None;
                    self.state = State::DistancePostfix;
                }
                State::DistancePostfix => {
                    let Some(bits) = reader.read_bits(input, cursor, 2) else {
                        return Ok(None);
                    };
                    self.distance_postfix_bits = bits as u8;
                    self.state = State::DirectDistance;
                }
                State::DirectDistance => {
                    let Some(value) = reader.read_bits(input, cursor, 4) else {
                        return Ok(None);
                    };
                    let num_direct_distance_codes =
                        (value as u16) << self.distance_postfix_bits;
                    self.state = State::ContextModes;

                    while self.literal_context_modes.len()
                        < usize::from(
                            self.literal_partition
                                .as_ref()
                                .expect("literal partition is decoded before context modes")
                                .num_types,
                        )
                    {
                        let Some(mode) = reader.read_bits(input, cursor, 2) else {
                            return Ok(None);
                        };
                        self.literal_context_modes.push(mode as u8);
                    }

                    self.state = State::Done;
                    return Ok(Some(CompressedHeader {
                        literal_partition: self
                            .literal_partition
                            .take()
                            .expect("literal partition is present at completion"),
                        command_partition: self
                            .command_partition
                            .take()
                            .expect("command partition is present at completion"),
                        distance_partition: self
                            .distance_partition
                            .take()
                            .expect("distance partition is present at completion"),
                        distance_postfix_bits: self.distance_postfix_bits,
                        num_direct_distance_codes,
                        literal_context_modes: core::mem::take(&mut self.literal_context_modes),
                    }));
                }
                State::ContextModes => {
                    unreachable!("context modes are consumed with direct-distance parameters")
                }
                State::Done => unreachable!("compressed header decoded more than once"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CompressedHeaderDecoder;
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

        fn var_len_u8(&mut self, value: u8) {
            match value {
                0 => self.push(0, 1),
                1 => {
                    self.push(1, 1);
                    self.push(0, 3);
                }
                _ => {
                    let width = (u8::BITS - value.leading_zeros() - 1) as u8;
                    self.push(1, 1);
                    self.push(u64::from(width), 3);
                    self.push(u64::from(value - (1_u8 << width)), width);
                }
            }
        }

        fn two_type_partition(&mut self) {
            self.push(1, 2); // simple block-type code
            self.push(1, 2); // two symbols
            self.push(0, 2);
            self.push(1, 2);

            self.push(1, 2); // simple block-length code
            self.push(0, 2); // one symbol
            self.push(0, 5); // block-length symbol 0
            self.push(3, 2); // first block length 4
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
    fn decodes_single_type_prelude() {
        let mut bits = Bits::default();
        bits.var_len_u8(0); // one literal block type
        bits.var_len_u8(0); // one command block type
        bits.var_len_u8(0); // one distance block type
        bits.push(2, 2); // NPOSTFIX
        bits.push(3, 4); // NDIRECT >> NPOSTFIX
        bits.push(3, 2); // context mode
        let input = bits.into_bytes();

        let mut decoder = CompressedHeaderDecoder::default();
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let header = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(header.literal_partition.num_types, 1);
        assert_eq!(header.command_partition.num_types, 1);
        assert_eq!(header.distance_partition.num_types, 1);
        assert_eq!(header.distance_postfix_bits, 2);
        assert_eq!(header.num_direct_distance_codes, 12);
        assert_eq!(header.literal_context_modes, [3]);
    }

    #[test]
    fn decodes_multiple_literal_block_types() {
        let mut bits = Bits::default();
        bits.var_len_u8(1); // two literal block types
        bits.two_type_partition();
        bits.var_len_u8(0); // one command block type
        bits.var_len_u8(0); // one distance block type
        bits.push(0, 2); // NPOSTFIX
        bits.push(0, 4); // NDIRECT
        bits.push(1, 2);
        bits.push(2, 2);
        let input = bits.into_bytes();

        let mut decoder = CompressedHeaderDecoder::default();
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let header = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(header.literal_partition.num_types, 2);
        assert_eq!(header.literal_partition.first_length, Some(4));
        assert_eq!(header.literal_context_modes, [1, 2]);
    }

    #[test]
    fn resumes_across_input_slices() {
        let mut bits = Bits::default();
        bits.var_len_u8(0);
        bits.var_len_u8(0);
        bits.var_len_u8(0);
        bits.push(3, 2);
        bits.push(15, 4);
        bits.push(2, 2);
        let input = bits.into_bytes();

        let mut decoder = CompressedHeaderDecoder::default();
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
        let header = decoder
            .decode(&mut reader, &input[1..], &mut second_cursor)
            .unwrap()
            .unwrap();
        assert_eq!(header.distance_postfix_bits, 3);
        assert_eq!(header.num_direct_distance_codes, 120);
        assert_eq!(header.literal_context_modes, [2]);
    }
}
