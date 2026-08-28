use super::bit_reader::BitReader;
use super::prefix_code::PrefixCode;
use super::prefix_code_decoder::{PrefixCodeDecoder, PrefixCodeDecoderError};

#[derive(Debug)]
pub(super) struct TreeGroupDecoder {
    alphabet_size: u16,
    tree_count: u16,
    current: Option<PrefixCodeDecoder>,
    trees: Vec<PrefixCode>,
    done: bool,
}

impl TreeGroupDecoder {
    pub(super) fn new(alphabet_size: u16, tree_count: u16) -> Self {
        assert!(alphabet_size != 0);
        assert!(tree_count != 0);

        Self {
            alphabet_size,
            tree_count,
            current: None,
            trees: Vec::with_capacity(usize::from(tree_count)),
            done: false,
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<Vec<PrefixCode>>, PrefixCodeDecoderError> {
        assert!(!self.done, "tree group decoded more than once");

        while self.trees.len() < usize::from(self.tree_count) {
            let decoder = self
                .current
                .get_or_insert_with(|| PrefixCodeDecoder::new(self.alphabet_size));
            let Some(tree) = decoder.decode(reader, input, cursor)? else {
                return Ok(None);
            };
            self.trees.push(tree);
            self.current = None;
        }

        self.done = true;
        Ok(Some(core::mem::take(&mut self.trees)))
    }
}

#[cfg(test)]
mod tests {
    use super::TreeGroupDecoder;
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
    fn decodes_multiple_trees() {
        let mut bits = Bits::default();
        bits.simple_single(3, 3);
        bits.simple_single(5, 3);
        let input = bits.into_bytes();

        let mut decoder = TreeGroupDecoder::new(8, 2);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let trees = decoder
            .decode(&mut reader, &input, &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(trees.len(), 2);

        let mut symbol_state = PrefixSymbolDecoder::default();
        let mut no_input_cursor = 0;
        assert_eq!(
            trees[0].decode(&mut symbol_state, &mut reader, &[], &mut no_input_cursor),
            Some(3)
        );
        assert_eq!(
            trees[1].decode(&mut symbol_state, &mut reader, &[], &mut no_input_cursor),
            Some(5)
        );
    }

    #[test]
    fn resumes_between_input_slices() {
        let mut bits = Bits::default();
        bits.simple_single(17, 8);
        bits.simple_single(42, 8);
        let input = bits.into_bytes();

        let mut decoder = TreeGroupDecoder::new(256, 2);
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
        let trees = decoder
            .decode(&mut reader, &input[1..], &mut second_cursor)
            .unwrap()
            .unwrap();
        assert_eq!(trees.len(), 2);
    }
}
