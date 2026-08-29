#[derive(Debug, Default)]
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    pending: u64,
    used: u8,
}

impl BitWriter {
    pub(super) fn write_bits(&mut self, mut value: u64, mut count: u8) {
        while count != 0 {
            let available = u64::BITS as u8 - self.used;
            let take = count.min(available);
            self.pending |= (value & low_mask(take)) << self.used;
            self.used += take;
            count -= take;
            if take == u64::BITS as u8 {
                value = 0;
            } else {
                value >>= take;
            }

            if self.used >= 48 {
                self.flush_complete_bytes();
            }
        }
    }

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
        self.bytes
            .extend_from_slice(&self.pending.to_le_bytes()[..byte_count]);
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

    fn flush_complete_bytes(&mut self) {
        let byte_count = usize::from(self.used / 8);
        debug_assert!(byte_count != 0);
        self.bytes
            .extend_from_slice(&self.pending.to_le_bytes()[..byte_count]);
        let flushed_bits = (byte_count * 8) as u8;
        if flushed_bits == u64::BITS as u8 {
            self.pending = 0;
        } else {
            self.pending >>= flushed_bits;
        }
        self.used -= flushed_bits;
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
    fn packs_full_width_values() {
        let mut writer = BitWriter::default();
        writer.write_bits(u64::MAX, 64);
        writer.write_bits(0b101, 3);

        assert_eq!(
            writer.finish(),
            vec![0xff; 8]
                .into_iter()
                .chain([0x05])
                .collect::<Vec<_>>()
        );
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
    fn byte_alignment_uses_zero_fill() {
        let mut writer = BitWriter::default();
        writer.write_bits(0b101, 3);
        writer.align_to_byte();
        writer.write_bytes(&[0xaa, 0x55]);

        assert_eq!(writer.finish(), vec![0b0000_0101, 0xaa, 0x55]);
    }
}
