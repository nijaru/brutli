/// Incremental least-significant-bit-first reader for the Brotli bitstream.
///
/// Input slices are borrowed only for the duration of bit-reader calls. Any
/// whole bytes consumed from an input slice are retained in `buffer` until
/// their bits are consumed, allowing decoding to resume with a different input
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
        if !self.ensure_bits(input, cursor, count) {
            return None;
        }

        let value = self.peek_bits(count);
        self.consume_bits(count);
        Some(value)
    }

    /// Refills the staging buffer until at least `count` bits are available.
    ///
    /// Returning `false` does not discard already-buffered bits. Bytes that
    /// were available in `input` are retained in the staging buffer so a
    /// smaller read or a later call can still consume them.
    pub(super) fn ensure_bits(
        &mut self,
        input: &[u8],
        cursor: &mut usize,
        count: u32,
    ) -> bool {
        assert!(count <= Self::MAX_READ_BITS);

        while self.buffered < count {
            let Some(&byte) = input.get(*cursor) else {
                return false;
            };
            self.buffer |= u64::from(byte) << self.buffered;
            self.buffered += 8;
            *cursor += 1;
        }

        true
    }

    /// Returns the next `count` buffered bits without consuming them.
    pub(super) fn peek_bits(&self, count: u32) -> u64 {
        debug_assert!(count <= self.buffered);
        debug_assert!(count <= Self::MAX_READ_BITS);
        if count == 0 {
            0
        } else {
            self.buffer & ((1_u64 << count) - 1)
        }
    }

    /// Discards `count` bits from the front of the staging buffer.
    pub(super) fn consume_bits(&mut self, count: u32) {
        debug_assert!(count <= self.buffered);
        self.buffer >>= count;
        self.buffered -= count;
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
    fn peeks_and_consumes_without_reloading() {
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert!(reader.ensure_bits(&[0b1101_0010], &mut cursor, 8));
        assert_eq!(cursor, 1);
        assert_eq!(reader.peek_bits(4), 0b0010);
        assert_eq!(reader.peek_bits(8), 0b1101_0010);
        assert_eq!(reader.buffered_bits(), 8);

        reader.consume_bits(3);
        assert_eq!(reader.peek_bits(5), 0b11010);
        assert_eq!(reader.buffered_bits(), 5);
    }

    #[test]
    fn failed_refill_keeps_bits_for_smaller_reads() {
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert!(!reader.ensure_bits(&[0b1010_0101], &mut cursor, 12));
        assert_eq!(cursor, 1);
        assert_eq!(reader.read_bits(&[], &mut cursor, 3), Some(0b101));
        assert_eq!(reader.buffered_bits(), 5);
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
