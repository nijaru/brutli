/// Incremental least-significant-bit-first reader for the Brotli bitstream.
///
/// Input slices are borrowed only for the duration of `read_bits`. Any whole
/// bytes consumed from an input slice are retained in `buffer` until their
/// bits are consumed, allowing decoding to resume with a different input
/// slice on the next call.
#[derive(Debug, Default)]
pub(super) struct BitReader {
    buffer: u64,
    buffered: u32,
}

impl BitReader {
    /// Keep enough headroom that refilling by one byte can never overflow the
    /// 64-bit staging buffer.
    const MAX_READ_BITS: u32 = 56;

    /// Reads `count` bits in Brotli's least-significant-bit-first order.
    ///
    /// `cursor` is advanced for each input byte transferred into the internal
    /// staging buffer. If the input ends before `count` bits are available,
    /// the consumed bytes remain buffered and `None` is returned.
    pub(super) fn read_bits(
        &mut self,
        input: &[u8],
        cursor: &mut usize,
        count: u32,
    ) -> Option<u64> {
        assert!(count <= Self::MAX_READ_BITS);

        while self.buffered < count {
            let byte = *input.get(*cursor)?;
            self.buffer |= u64::from(byte) << self.buffered;
            self.buffered += 8;
            *cursor += 1;
        }

        let mask = if count == 0 { 0 } else { (1_u64 << count) - 1 };
        let value = self.buffer & mask;
        self.buffer >>= count;
        self.buffered -= count;
        Some(value)
    }

    /// Consumes the remainder of the current byte and reports whether every
    /// discarded fill bit was zero.
    ///
    /// If the reader is not byte-aligned, those bits have already been pulled
    /// into the staging buffer with the current byte, so alignment never needs
    /// additional input.
    pub(super) fn align_to_byte_with_zero_fill(&mut self) -> bool {
        let count = self.buffered % 8;
        if count == 0 {
            return true;
        }

        let mask = (1_u64 << count) - 1;
        let is_zero = self.buffer & mask == 0;
        self.buffer >>= count;
        self.buffered -= count;
        is_zero
    }

    #[cfg(test)]
    fn buffered_bits(&self) -> u32 {
        self.buffered
    }
}

#[cfg(test)]
mod tests {
    use super::BitReader;

    #[test]
    fn reads_lsb_first() {
        let mut reader = BitReader::default();
        let input = [0b1011_0010];
        let mut cursor = 0;

        assert_eq!(reader.read_bits(&input, &mut cursor, 3), Some(0b010));
        assert_eq!(reader.read_bits(&input, &mut cursor, 3), Some(0b110));
        assert_eq!(reader.read_bits(&input, &mut cursor, 2), Some(0b10));
        assert_eq!(cursor, 1);
        assert_eq!(reader.buffered_bits(), 0);
    }

    #[test]
    fn reads_across_byte_boundaries() {
        let mut reader = BitReader::default();
        let input = [0b1111_0000, 0b0000_1111];
        let mut cursor = 0;

        assert_eq!(reader.read_bits(&input, &mut cursor, 12), Some(0x0ff0));
        assert_eq!(cursor, 2);
        assert_eq!(reader.buffered_bits(), 4);
    }

    #[test]
    fn retains_partial_input_across_calls() {
        let mut reader = BitReader::default();
        let mut first_cursor = 0;

        assert_eq!(reader.read_bits(&[0xaa], &mut first_cursor, 12), None);
        assert_eq!(first_cursor, 1);
        assert_eq!(reader.buffered_bits(), 8);

        let mut second_cursor = 0;
        assert_eq!(
            reader.read_bits(&[0x0f], &mut second_cursor, 12),
            Some(0x0faa)
        );
        assert_eq!(second_cursor, 1);
        assert_eq!(reader.buffered_bits(), 4);
    }

    #[test]
    fn zero_bit_read_does_not_consume_input() {
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(reader.read_bits(&[0xff], &mut cursor, 0), Some(0));
        assert_eq!(cursor, 0);
    }

    #[test]
    fn byte_alignment_validates_fill_bits() {
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(reader.read_bits(&[0b0000_0101], &mut cursor, 3), Some(5));
        assert!(reader.align_to_byte_with_zero_fill());
        assert_eq!(reader.buffered_bits(), 0);

        let mut cursor = 0;
        assert_eq!(reader.read_bits(&[0b1000_0101], &mut cursor, 3), Some(5));
        assert!(!reader.align_to_byte_with_zero_fill());
        assert_eq!(reader.buffered_bits(), 0);
    }
}
