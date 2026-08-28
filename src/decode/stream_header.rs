use super::bit_reader::BitReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StreamHeader {
    window_bits: u8,
}

impl StreamHeader {
    pub(super) fn window_bits(self) -> u8 {
        self.window_bits
    }

    #[cfg(test)]
    pub(super) fn window_size(self) -> usize {
        (1_usize << self.window_bits) - 16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamHeaderError {
    InvalidWindowBits,
}

#[derive(Debug, Default)]
pub(super) struct StreamHeaderDecoder {
    state: State,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    FirstBit,
    UpperRange,
    LowerRange,
    Done,
}

impl StreamHeaderDecoder {
    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<StreamHeader>, StreamHeaderError> {
        loop {
            match self.state {
                State::FirstBit => {
                    let Some(first) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };

                    if first == 0 {
                        self.state = State::Done;
                        return Ok(Some(StreamHeader { window_bits: 16 }));
                    }

                    self.state = State::UpperRange;
                }
                State::UpperRange => {
                    let Some(bits) = reader.read_bits(input, cursor, 3) else {
                        return Ok(None);
                    };

                    if bits != 0 {
                        self.state = State::Done;
                        return Ok(Some(StreamHeader {
                            window_bits: 17 + bits as u8,
                        }));
                    }

                    self.state = State::LowerRange;
                }
                State::LowerRange => {
                    let Some(bits) = reader.read_bits(input, cursor, 3) else {
                        return Ok(None);
                    };

                    let window_bits = match bits {
                        0 => 17,
                        1 => return Err(StreamHeaderError::InvalidWindowBits),
                        2..=7 => 8 + bits as u8,
                        _ => unreachable!(),
                    };

                    self.state = State::Done;
                    return Ok(Some(StreamHeader { window_bits }));
                }
                State::Done => unreachable!("stream header decoded more than once"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamHeaderDecoder, StreamHeaderError};
    use crate::decode::bit_reader::BitReader;

    const WINDOW_CODES: &[(u8, u8)] = &[
        (10, 0b0100001),
        (11, 0b0110001),
        (12, 0b1000001),
        (13, 0b1010001),
        (14, 0b1100001),
        (15, 0b1110001),
        (16, 0b0000000),
        (17, 0b0000001),
        (18, 0b0000011),
        (19, 0b0000101),
        (20, 0b0000111),
        (21, 0b0001001),
        (22, 0b0001011),
        (23, 0b0001101),
        (24, 0b0001111),
    ];

    #[test]
    fn decodes_every_rfc_7932_window_size() {
        for &(expected, encoded) in WINDOW_CODES {
            let mut decoder = StreamHeaderDecoder::default();
            let mut reader = BitReader::default();
            let mut cursor = 0;

            let header = decoder
                .decode(&mut reader, &[encoded], &mut cursor)
                .unwrap()
                .unwrap();

            assert_eq!(header.window_bits(), expected);
            assert_eq!(header.window_size(), (1_usize << expected) - 16);
            assert_eq!(cursor, 1);
        }
    }

    #[test]
    fn rejects_reserved_window_code() {
        let mut decoder = StreamHeaderDecoder::default();
        let mut reader = BitReader::default();
        let mut cursor = 0;

        let result = decoder.decode(&mut reader, &[0b0010001], &mut cursor);

        assert_eq!(result, Err(StreamHeaderError::InvalidWindowBits));
    }

    #[test]
    fn resumes_after_input_exhaustion() {
        let mut decoder = StreamHeaderDecoder::default();
        let mut reader = BitReader::default();
        let mut empty_cursor = 0;

        assert_eq!(
            decoder.decode(&mut reader, &[], &mut empty_cursor),
            Ok(None)
        );
        assert_eq!(empty_cursor, 0);

        let mut cursor = 0;
        let header = decoder
            .decode(&mut reader, &[0b0001111], &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(header.window_bits(), 24);
    }

    #[test]
    fn leaves_following_bits_buffered() {
        let mut decoder = StreamHeaderDecoder::default();
        let mut reader = BitReader::default();
        let mut cursor = 0;

        let header = decoder
            .decode(&mut reader, &[0b1010_0000], &mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(header.window_bits(), 16);
        assert_eq!(reader.read_bits(&[], &mut cursor, 3), Some(0));
    }
}
