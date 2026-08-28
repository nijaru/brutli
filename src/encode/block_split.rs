use super::{bit_writer::BitWriter, prefix_code::PrefixEncoding};

const BLOCK_LENGTH_OFFSETS: [usize; 26] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497, 753, 1265,
    2289, 4337, 8433, 16625,
];
const BLOCK_LENGTH_EXTRA_BITS: [u8; 26] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 24,
];
const BLOCK_TYPE_ALPHABET_SIZE: u16 = 4;
const BLOCK_LENGTH_ALPHABET_SIZE: u16 = 26;

#[derive(Debug)]
pub(super) struct TwoBlockPartition {
    type_code: PrefixEncoding,
    length_code: PrefixEncoding,
    first: BlockLength,
    second: BlockLength,
}

impl TwoBlockPartition {
    pub(super) fn new(first: usize, second: usize) -> Option<Self> {
        let first = BlockLength::for_count(first)?;
        let second = BlockLength::for_count(second)?;

        let mut type_frequencies = [0_usize; BLOCK_TYPE_ALPHABET_SIZE as usize];
        type_frequencies[1] = 1;
        let type_code = PrefixEncoding::from_frequencies(&type_frequencies)?;

        let mut length_frequencies = [0_usize; BLOCK_LENGTH_ALPHABET_SIZE as usize];
        length_frequencies[usize::from(first.symbol)] += 1;
        length_frequencies[usize::from(second.symbol)] += 1;
        let length_code = PrefixEncoding::from_frequencies(&length_frequencies)?;

        Some(Self {
            type_code,
            length_code,
            first,
            second,
        })
    }

    pub(super) fn write_header(&self, writer: &mut BitWriter) {
        self.type_code
            .write_tree(writer, BLOCK_TYPE_ALPHABET_SIZE);
        self.length_code
            .write_tree(writer, BLOCK_LENGTH_ALPHABET_SIZE);
        self.first.write(writer, &self.length_code);
    }

    pub(super) fn write_switch(&self, writer: &mut BitWriter) {
        self.type_code.write_symbol(writer, 1);
        self.second.write(writer, &self.length_code);
    }
}

#[derive(Debug, Clone, Copy)]
struct BlockLength {
    symbol: u16,
    extra: usize,
    extra_bits: u8,
}

impl BlockLength {
    fn for_count(count: usize) -> Option<Self> {
        if count == 0 {
            return None;
        }

        BLOCK_LENGTH_OFFSETS
            .iter()
            .zip(BLOCK_LENGTH_EXTRA_BITS)
            .enumerate()
            .find_map(|(symbol, (&offset, extra_bits))| {
                let maximum = offset + (1_usize << extra_bits) - 1;
                (count <= maximum).then_some(Self {
                    symbol: symbol as u16,
                    extra: count - offset,
                    extra_bits,
                })
            })
    }

    fn write(self, writer: &mut BitWriter, code: &PrefixEncoding) {
        code.write_symbol(writer, self.symbol);
        writer.write_bits(self.extra as u64, self.extra_bits);
    }
}

#[cfg(test)]
mod tests {
    use super::BlockLength;

    #[test]
    fn block_length_boundaries_map_to_rfc_ranges() {
        for count in [1, 4, 5, 16, 17, 64, 65, 496, 497, 16_625, 16_793_840] {
            let encoded = BlockLength::for_count(count).unwrap();
            let offset = super::BLOCK_LENGTH_OFFSETS[usize::from(encoded.symbol)];
            assert_eq!(offset + encoded.extra, count);
            assert!(encoded.extra < (1_usize << encoded.extra_bits));
        }
    }

    #[test]
    fn zero_block_length_is_rejected() {
        assert!(BlockLength::for_count(0).is_none());
    }
}
