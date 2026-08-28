use super::bit_reader::BitReader;
use super::block_partition::{BlockLengthDecoder, BlockPartition};
use super::prefix_code::PrefixSymbolDecoder;

#[derive(Debug)]
pub(super) struct BlockState {
    current_type: u16,
    previous_types: [u16; 2],
    remaining: Option<usize>,
    type_decoder: PrefixSymbolDecoder,
    length_decoder: BlockLengthDecoder,
    switch_pending: bool,
}

impl BlockState {
    pub(super) fn new(partition: &BlockPartition) -> Self {
        Self {
            current_type: 0,
            previous_types: [1, 0],
            remaining: partition.first_length,
            type_decoder: PrefixSymbolDecoder::default(),
            length_decoder: BlockLengthDecoder::default(),
            switch_pending: false,
        }
    }

    pub(super) fn current(
        &mut self,
        partition: &BlockPartition,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Option<u16> {
        if partition.num_types == 1 {
            return Some(0);
        }

        if self.remaining.is_some_and(|remaining| remaining != 0) {
            return Some(self.current_type);
        }

        if !self.switch_pending {
            let type_code = partition
                .type_code
                .as_ref()
                .expect("multi-type partition has a block-type code");
            let symbol = type_code.decode(&mut self.type_decoder, reader, input, cursor)?;
            self.current_type = resolve_type(symbol, partition.num_types, self.previous_types);
            self.previous_types = [self.previous_types[1], self.current_type];
            self.switch_pending = true;
        }

        let length_code = partition
            .length_code
            .as_ref()
            .expect("multi-type partition has a block-length code");
        let length = self
            .length_decoder
            .decode(length_code, reader, input, cursor)?;
        self.remaining = Some(length);
        self.switch_pending = false;
        Some(self.current_type)
    }

    pub(super) fn consume(&mut self, partition: &BlockPartition, count: usize) {
        if partition.num_types == 1 {
            return;
        }

        let remaining = self
            .remaining
            .as_mut()
            .expect("multi-type partition has a finite current block");
        assert!(count <= *remaining, "consumed past the current block");
        *remaining -= count;
    }

    #[cfg(test)]
    fn remaining(&self) -> Option<usize> {
        self.remaining
    }
}

fn resolve_type(symbol: u16, num_types: u16, previous: [u16; 2]) -> u16 {
    debug_assert!(num_types > 1);
    debug_assert!(symbol < num_types + 2);

    let block_type = match symbol {
        0 => previous[0],
        1 => previous[1] + 1,
        _ => symbol - 2,
    };
    if block_type >= num_types {
        block_type - num_types
    } else {
        block_type
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockState, resolve_type};
    use crate::decode::bit_reader::BitReader;
    use crate::decode::block_partition::BlockPartition;
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

    fn partition() -> BlockPartition {
        BlockPartition {
            num_types: 3,
            type_code: Some(PrefixCode::from_code_lengths(&[2, 2, 2, 2]).unwrap()),
            length_code: Some(PrefixCode::single(0)),
            first_length: Some(2),
        }
    }

    #[test]
    fn resolves_special_and_explicit_type_symbols() {
        assert_eq!(resolve_type(0, 3, [1, 0]), 1);
        assert_eq!(resolve_type(1, 3, [1, 0]), 1);
        assert_eq!(resolve_type(1, 3, [0, 2]), 0);
        assert_eq!(resolve_type(2, 3, [1, 0]), 0);
        assert_eq!(resolve_type(3, 3, [1, 0]), 1);
        assert_eq!(resolve_type(4, 3, [1, 0]), 2);
    }

    #[test]
    fn starts_with_type_zero_and_first_length() {
        let partition = partition();
        let mut state = BlockState::new(&partition);
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(
            state.current(&partition, &mut reader, &[], &mut cursor),
            Some(0)
        );
        assert_eq!(state.remaining(), Some(2));
        state.consume(&partition, 1);
        assert_eq!(state.remaining(), Some(1));
    }

    #[test]
    fn switches_type_and_decodes_next_length() {
        let partition = partition();
        let mut state = BlockState::new(&partition);
        state.consume(&partition, 2);

        let mut bits = Bits::default();
        bits.push(0b10, 2); // canonical symbol 1: most recent + 1
        bits.push(2, 2); // block-length code 0 => 1 + extra = 3
        let input = bits.into_bytes();
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(
            state.current(&partition, &mut reader, &input, &mut cursor),
            Some(1)
        );
        assert_eq!(state.remaining(), Some(3));
    }

    #[test]
    fn retains_type_switch_while_waiting_for_length_bits() {
        let partition = BlockPartition {
            num_types: 2,
            type_code: Some(PrefixCode::single(1)),
            length_code: Some(PrefixCode::single(25)),
            first_length: Some(1),
        };
        let mut state = BlockState::new(&partition);
        state.consume(&partition, 1);

        let mut reader = BitReader::default();
        let mut first_cursor = 0;
        assert_eq!(
            state.current(&partition, &mut reader, &[0x34], &mut first_cursor),
            None
        );
        assert_eq!(first_cursor, 1);

        let mut second_cursor = 0;
        assert_eq!(
            state.current(&partition, &mut reader, &[0x12, 0, 0], &mut second_cursor,),
            Some(1)
        );
        assert_eq!(state.remaining(), Some(16625 + 0x1234));
    }

    #[test]
    fn single_type_partition_never_consumes_switch_bits() {
        let partition = BlockPartition {
            num_types: 1,
            type_code: None,
            length_code: None,
            first_length: None,
        };
        let mut state = BlockState::new(&partition);
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(
            state.current(&partition, &mut reader, &[0xff], &mut cursor),
            Some(0)
        );
        state.consume(&partition, 1_000_000);
        assert_eq!(cursor, 0);
    }
}
