use super::bit_reader::BitReader;

#[derive(Debug, Default)]
pub(super) struct VarLenUint8Decoder {
    state: State,
    width: u8,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    FirstBit,
    Width,
    Value,
}

impl VarLenUint8Decoder {
    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Option<u8> {
        loop {
            match self.state {
                State::FirstBit => {
                    let bit = reader.read_bits(input, cursor, 1)?;
                    if bit == 0 {
                        return Some(0);
                    }
                    self.state = State::Width;
                }
                State::Width => {
                    let width = reader.read_bits(input, cursor, 3)? as u8;
                    if width == 0 {
                        self.state = State::FirstBit;
                        return Some(1);
                    }
                    self.width = width;
                    self.state = State::Value;
                }
                State::Value => {
                    let extra = reader.read_bits(input, cursor, u32::from(self.width))? as u8;
                    let value = (1_u16 << self.width) + u16::from(extra);
                    self.width = 0;
                    self.state = State::FirstBit;
                    return Some(value as u8);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VarLenUint8Decoder;
    use crate::decode::bit_reader::BitReader;

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

    fn encode(value: u8) -> Vec<u8> {
        let mut bits = Bits::default();
        match value {
            0 => bits.push(0, 1),
            1 => {
                bits.push(1, 1);
                bits.push(0, 3);
            }
            _ => {
                let width = (u8::BITS - value.leading_zeros() - 1) as u8;
                bits.push(1, 1);
                bits.push(u64::from(width), 3);
                bits.push(u64::from(value - (1_u8 << width)), width);
            }
        }
        bits.into_bytes()
    }

    #[test]
    fn decodes_every_value() {
        for expected in u8::MIN..=u8::MAX {
            let input = encode(expected);
            let mut decoder = VarLenUint8Decoder::default();
            let mut reader = BitReader::default();
            let mut cursor = 0;

            assert_eq!(
                decoder.decode(&mut reader, &input, &mut cursor),
                Some(expected),
                "failed for {expected}"
            );
        }
    }

    #[test]
    fn resumes_across_input_slices() {
        let expected = 255;
        let input = encode(expected);
        assert_eq!(input.len(), 2);

        let mut decoder = VarLenUint8Decoder::default();
        let mut reader = BitReader::default();
        let mut first_cursor = 0;

        assert_eq!(
            decoder.decode(&mut reader, &input[..1], &mut first_cursor),
            None
        );
        assert_eq!(first_cursor, 1);

        let mut second_cursor = 0;
        assert_eq!(
            decoder.decode(&mut reader, &input[1..], &mut second_cursor),
            Some(expected)
        );
        assert_eq!(second_cursor, 1);
    }
}
