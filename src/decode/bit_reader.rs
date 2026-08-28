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

        let value = self.peek_buffered_bits(count);
        self.consume_bits(count);
        Some(value)
    }

    /// Peeks at `count` bits using both buffered bits and caller input without
    /// advancing `cursor` or transferring input bytes into decoder state.
    ///
    /// This is intended for speculative lookup-table decoding: callers can
    /// inspect a wider prefix and then commit only the bits actually belonging
    /// to the decoded symbol with [`Self::read_bits`].
    pub(super) fn peek_bits_from_input(
        &self,
        input: &[u8],
        cursor: usize,
        count: u32,
    ) -> Option<u64> {
        assert!(count <= Self::MAX_READ_BITS);

        if self.buffered >= count {
            return Some(self.peek_buffered_bits(count));
        }

        let mut value = self.buffer;
        let mut available = self.buffered;
        let mut input_cursor = cursor;
        while available < count {
            let byte = *input.get(input_cursor)?;
            value |= u64::from(byte) << available;
            available += 8;
            input_cursor += 1;
        }

        let mask = if count == 0 { 0 } else { (1_u64 << count) - 1 };
        Some(value & mask)
    }

    /// Refills the staging buffer until at least `count` bits are available.
    ///
    /// Returning `false` does not discard already-buffered bits. Bytes that
    /// were available in `input` are retained in the staging buffer so a
    /// smaller read or a later call can still consume them.
    fn ensure_bits(&mut self, input: &[u8], cursor: &mut usize, count: u32) -> bool {
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
    fn peek_buffered_bits(&self, count: u32) -> u64 {
        debug_assert!(count <= self.buffered);
        debug_assert!(count <= Self::MAX_READ_BITS);
        if count == 0 {
            0
        } else {
            self.buffer & ((1_u64 << count) - 1)
        }
    }

    /// Discards `count` bits from the front of the staging buffer.
    fn consume_bits(&mut self, count: u32) {
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
    fn speculative_peek_does_not_consume_input() {
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(reader.read_bits(&[0b1010_0101], &mut cursor, 3), Some(0b101));
        assert_eq!(cursor, 1);
        assert_eq!(reader.buffered_bits(), 5);

        let following = [0b1100_0011];
        let mut following_cursor = 0;
        assert_eq!(
            reader.peek_bits_from_input(&following, following_cursor, 8),
            Some(0b0111_0100)
        );
        assert_eq!(following_cursor, 0);
        assert_eq!(reader.buffered_bits(), 5);
        assert_eq!(
            reader.read_bits(&following, &mut following_cursor, 5),
            Some(0b10100)
        );
        assert_eq!(following_cursor, 0);
    }

    #[test]
    fn failed_refill_keeps_bits_for_smaller_reads() {
        let mut reader = BitReader::default();
        let mut cursor = 0;

        assert_eq!(reader.read_bits(&[0b1010_0101], &mut cursor, 12), None);
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
