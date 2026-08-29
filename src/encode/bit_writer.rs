const REVERSED_BYTES: [u8; 256] = reversed_bytes();

#[derive(Debug, Default)]
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    pub(super) fn write_bits(&mut self, mut value: u64, mut count: u8) {
        if count == 0 {
            return;
        }

        if self.used != 0 {
            let available = 8 - self.used;
            let take = count.min(available);
            let mask = low_mask(take);
            self.current |= ((value & mask) as u8) << self.used;
            self.used += take;
            value >>= take;
            count -= take;

            if self.used == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }

        while count >= 8 {
            self.bytes.push(value as u8);
            value >>= 8;
            count -= 8;
        }

        if count != 0 {
            self.current = (value & low_mask(count)) as u8;
            self.used = count;
        }
    }

    pub(super) fn write_prefix(&mut self, code: u16, count: u8) {
        if count == 0 {
            return;
        }
        debug_assert!(count <= u16::BITS as u8);
        let reversed = (u16::from(REVERSED_BYTES[usize::from(code as u8)]) << 8)
            | u16::from(REVERSED_BYTES[usize::from((code >> 8) as u8)]);
        self.write_bits(u64::from(reversed >> (u16::BITS as u8 - count)), count);
    }

    pub(super) fn bit_len(&self) -> usize {
        self.bytes.len() * 8 + usize::from(self.used)
    }

    pub(super) fn align_to_byte(&mut self) {
        if self.used != 0 {
            self.bytes.push(self.current);
            self.current = 0;
            self.used = 0;
        }
    }

    pub(super) fn write_bytes(&mut self, bytes: &[u8]) {
        assert_eq!(self.used, 0, "byte writes require byte alignment");
        self.bytes.extend_from_slice(bytes);
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        self.align_to_byte();
        self.bytes
    }
}

const fn low_mask(bits: u8) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1_u64 << bits) - 1
    }
}

const fn reversed_bytes() -> [u8; 256] {
    let mut table = [0_u8; 256];
    let mut value = 0_usize;
    while value < table.len() {
        let mut source = value as u8;
        let mut reversed = 0_u8;
        let mut bit = 0;
        while bit < 8 {
            reversed = (reversed << 1) | (source & 1);
            source >>= 1;
            bit += 1;
        }
        table[value] = reversed;
        value += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::{BitWriter, REVERSED_BYTES};

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
    fn reverse_table_matches_builtin() {
        for byte in 0..=u8::MAX {
            assert_eq!(REVERSED_BYTES[usize::from(byte)], byte.reverse_bits());
        }
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
