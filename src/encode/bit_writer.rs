#[derive(Debug, Default)]
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    pub(super) fn write_bits(&mut self, mut value: u64, count: u8) {
        for _ in 0..count {
            self.current |= (value as u8 & 1) << self.used;
            self.used += 1;
            value >>= 1;

            if self.used == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }
    }

    pub(super) fn write_prefix(&mut self, code: u16, count: u8) {
        for shift in (0..count).rev() {
            self.write_bits(u64::from((code >> shift) & 1), 1);
        }
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
    fn byte_alignment_uses_zero_fill() {
        let mut writer = BitWriter::default();
        writer.write_bits(0b101, 3);
        writer.align_to_byte();
        writer.write_bytes(&[0xaa, 0x55]);

        assert_eq!(writer.finish(), vec![0b0000_0101, 0xaa, 0x55]);
    }
}
