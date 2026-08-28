use super::bit_reader::BitReader;
use super::compressed_header::CompressedHeader;
use super::prefix_code::PrefixCode;
use super::prefix_code_decoder::PrefixCodeDecoderError;
use super::tree_group::TreeGroupDecoder;

const LITERAL_ALPHABET_SIZE: u16 = 256;
const COMMAND_ALPHABET_SIZE: u16 = 704;
const DISTANCE_SHORT_CODES: u16 = 16;
const MAX_DISTANCE_BITS: u16 = 24;

#[derive(Debug)]
pub(super) struct CompressedTrees {
    pub(super) literal: Vec<PrefixCode>,
    pub(super) command: Vec<PrefixCode>,
    pub(super) distance: Vec<PrefixCode>,
}

#[derive(Debug)]
pub(super) struct CompressedTreesDecoder {
    state: State,
    current: TreeGroupDecoder,
    command_tree_count: u16,
    distance_tree_count: u16,
    distance_alphabet_size: u16,
    literal: Vec<PrefixCode>,
    command: Vec<PrefixCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Literal,
    Command,
    Distance,
    Done,
}

impl CompressedTreesDecoder {
    pub(super) fn new(header: &CompressedHeader) -> Self {
        Self::from_counts(
            header.num_literal_trees,
            header.command_partition.num_types,
            header.num_distance_trees,
            header.distance_postfix_bits,
            header.num_direct_distance_codes,
        )
    }

    fn from_counts(
        literal_tree_count: u16,
        command_tree_count: u16,
        distance_tree_count: u16,
        distance_postfix_bits: u8,
        num_direct_distance_codes: u16,
    ) -> Self {
        let distance_alphabet_size =
            distance_alphabet_size(distance_postfix_bits, num_direct_distance_codes);
        Self {
            state: State::Literal,
            current: TreeGroupDecoder::new(LITERAL_ALPHABET_SIZE, literal_tree_count),
            command_tree_count,
            distance_tree_count,
            distance_alphabet_size,
            literal: Vec::new(),
            command: Vec::new(),
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<CompressedTrees>, PrefixCodeDecoderError> {
        loop {
            match self.state {
                State::Literal => {
                    let Some(trees) = self.current.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.literal = trees;
                    self.current =
                        TreeGroupDecoder::new(COMMAND_ALPHABET_SIZE, self.command_tree_count);
                    self.state = State::Command;
                }
                State::Command => {
                    let Some(trees) = self.current.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.command = trees;
                    self.current = TreeGroupDecoder::new(
                        self.distance_alphabet_size,
                        self.distance_tree_count,
                    );
                    self.state = State::Distance;
                }
                State::Distance => {
                    let Some(distance) = self.current.decode(reader, input, cursor)? else {
                        return Ok(None);
                    };
                    self.state = State::Done;
                    return Ok(Some(CompressedTrees {
                        literal: core::mem::take(&mut self.literal),
                        command: core::mem::take(&mut self.command),
                        distance,
                    }));
                }
                State::Done => unreachable!("compressed tree groups decoded more than once"),
            }
        }
    }
}

fn distance_alphabet_size(distance_postfix_bits: u8, num_direct_distance_codes: u16) -> u16 {
    debug_assert!(distance_postfix_bits <= 3);
    debug_assert!(num_direct_distance_codes <= 120);
    DISTANCE_SHORT_CODES
        + num_direct_distance_codes
        + (MAX_DISTANCE_BITS << (distance_postfix_bits + 1))
}

#[cfg(test)]
mod tests {
    use super::{CompressedTreesDecoder, distance_alphabet_size};
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

        fn simple_single(&mut self, symbol: u16, symbol_bits: u8) {
            self.push(1, 2); // simple representation
            self.push(0, 2); // one symbol
            self.push(u64::from(symbol), symbol_bits);
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
    fn uses_rfc7932_distance_alphabet_size() {
        assert_eq!(distance_alphabet_size(0, 0), 64);
        assert_eq!(distance_alphabet_size(2, 12), 220);
        assert_eq!(distance_alphabet_size(3, 120), 520);
    }

    #[test]
    fn decodes_all_three_tree_groups() {
        let mut bits = Bits::default();
        bits.simple_single(65, 8);
        bits.simple_single(66, 8);
        bits.simple_single(42, 10);
        bits.simple_single(7, 6);
        let input = bits.into_bytes();

        let mut decoder = CompressedTreesDecoder::from_counts(2, 1, 1, 0, 0);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let trees = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(trees.literal.len(), 2);
        assert_eq!(trees.command.len(), 1);
        assert_eq!(trees.distance.len(), 1);

        let mut state = PrefixSymbolDecoder::default();
        let mut no_input_cursor = 0;
        assert_eq!(
            trees.literal[0].decode(&mut state, &mut reader, &[], &mut no_input_cursor),
            Some(65)
        );
        assert_eq!(
            trees.literal[1].decode(&mut state, &mut reader, &[], &mut no_input_cursor),
            Some(66)
        );
        assert_eq!(
            trees.command[0].decode(&mut state, &mut reader, &[], &mut no_input_cursor),
            Some(42)
        );
        assert_eq!(
            trees.distance[0].decode(&mut state, &mut reader, &[], &mut no_input_cursor),
            Some(7)
        );
    }

    #[test]
    fn resumes_across_tree_groups() {
        let mut bits = Bits::default();
        bits.simple_single(17, 8);
        bits.simple_single(23, 10);
        bits.simple_single(5, 6);
        let input = bits.into_bytes();

        let mut decoder = CompressedTreesDecoder::from_counts(1, 1, 1, 0, 0);
        let mut reader = BitReader::default();
        let mut first_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[..2], &mut first_cursor)
                .unwrap()
                .is_none()
        );

        let mut second_cursor = 0;
        let trees = decoder
            .decode(&mut reader, &input[2..], &mut second_cursor)
            .unwrap()
            .unwrap();
        assert_eq!(trees.literal.len(), 1);
        assert_eq!(trees.command.len(), 1);
        assert_eq!(trees.distance.len(), 1);
    }
}
