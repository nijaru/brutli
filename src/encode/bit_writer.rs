#[derive(Debug, Default)]
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    pending: u64,
    used: u8,
}

impl BitWriter {
    #[inline(always)]
    pub(super) fn write_bits(&mut self, mut value: u64, mut count: u8) {
        if count == 0 {
            return;
        }
        debug_assert!(count <= u64::BITS as u8);

        let total = u16::from(self.used) + u16::from(count);
        if total <= u64::BITS as u16 {
            self.pending |= (value & low_mask(count)) << self.used;
            self.used = total as u8;
            if self.used == u64::BITS as u8 {
                self.flush_word();
            }
            return;
        }

        let take = u64::BITS as u8 - self.used;
        self.pending |= (value & low_mask(take)) << self.used;
        value >>= take;
        count -= take;
        self.flush_word();

        self.pending = value & low_mask(count);
        self.used = count;
    }

    #[inline(always)]
    pub(super) fn write_prefix(&mut self, code: u16, count: u8) {
        if count == 0 {
            return;
        }
        debug_assert!(count <= u16::BITS as u8);
        let reversed = code.reverse_bits() >> (u16::BITS as u8 - count);
        self.write_bits(u64::from(reversed), count);
    }

    pub(super) fn bit_len(&self) -> usize {
        self.bytes.len() * 8 + usize::from(self.used)
    }

    pub(super) fn align_to_byte(&mut self) {
        if self.used == 0 {
            return;
        }

        let byte_count = usize::from(self.used.div_ceil(8));
        let pending = self.pending.to_le_bytes();
        self.bytes.extend_from_slice(&pending[..byte_count]);
        self.pending = 0;
        self.used = 0;
    }

    pub(super) fn write_bytes(&mut self, bytes: &[u8]) {
        assert_eq!(self.used, 0, "byte writes require byte alignment");
        self.bytes.extend_from_slice(bytes);
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        self.align_to_byte();
        self.bytes
    }

    #[inline(always)]
    fn flush_word(&mut self) {
        self.bytes.extend_from_slice(&self.pending.to_le_bytes());
        self.pending = 0;
        self.used = 0;
    }
}

const fn low_mask(bits: u8) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1_u64 << bits) - 1
    }
}

#[cfg(test)]
mod tests {
    use super::BitWriter;

    #[test]
    fn packs_integer_bits_lsb_first() {
        let mut writer = BitWriter::default();
        writer.write_bits(0b110, 3);
        writer.write_bits(0b0010, 4);
        writer.write_bits(0b110, 3);

        assert_eq!(writer.finish(), vec![0b0001_0110, 0b0000_0011]);
    }

    #[test]
    fn writes_prefix_bits_most_significant_first() {
        let mut writer = BitWriter::default();
        writer.write_prefix(0b110, 3);
        writer.write_prefix(0b01, 2);

        assert_eq!(writer.finish(), vec![0b0001_0011]);
    }

    #[test]
    fn packs_across_byte_boundaries() {
        let mut writer = BitWriter::default();
        writer.write_bits(0b101, 3);
        writer.write_bits(0xabcd, 16);
        writer.write_bits(0b11, 2);

        assert_eq!(writer.finish(), vec![0x6d, 0x5e, 0x1d]);
    }

    #[test]
    fn packs_across_word_boundaries() {
        let mut writer = BitWriter::default();
        writer.write_bits(u64::MAX >> 3, 61);
        writer.write_bits(0b101_0110, 7);

        let bytes = writer.finish();
        assert_eq!(bytes.len(), 9);
        assert_eq!(&bytes[..7], &[0xff; 7]);
        assert_eq!(bytes[7], 0xdf);
        assert_eq!(bytes[8], 0x0a);
    }

    #[test]
    fn reports_unaligned_bit_length() {
        let mut writer = BitWriter::default();
        writer.write_bits(0b101, 3);
        assert_eq!(writer.bit_len(), 3);
        writer.write_bits(0xff, 8);
        assert_eq!(writer.bit_len(), 11);
    }

    #[test]
    fn reports_length_after_full_word_flush() {
        let mut writer = BitWriter::default();
        writer.write_bits(u64::MAX, 64);
        assert_eq!(writer.bit_len(), 64);
        writer.write_bits(0b101, 3);
        assert_eq!(writer.bit_len(), 67);
    }

    #[test]
    fn byte_alignment_uses_zero_fill() {
        let mut writer = BitWriter::default();
        writer.write_bits(0b101, 3);
        writer.align_to_byte();
        writer.write_bytes(&[0xaa, 0x55]);

        assert_eq!(writer.finish(), vec![0b0000_0101, 0xaa, 0x55]);
    }
}
