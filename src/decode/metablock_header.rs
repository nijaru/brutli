use super::bit_reader::BitReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MetaBlockHeader {
    pub(super) is_last: bool,
    pub(super) kind: MetaBlockKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetaBlockKind {
    End,
    Compressed { length: usize },
    Uncompressed { length: usize },
    Metadata { length: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetaBlockHeaderError {
    NonZeroPadding,
    NonZeroReservedBit,
    NonMinimalLength,
}

#[derive(Debug, Default)]
pub(super) struct MetaBlockHeaderDecoder {
    state: State,
    is_last: bool,
    width: u8,
    index: u8,
    value: usize,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    IsLast,
    IsLastEmpty,
    Nibbles,
    DataLength,
    IsUncompressed,
    UncompressedAlignment,
    MetadataReserved,
    MetadataLengthWidth,
    MetadataLength,
    MetadataAlignment,
    EndAlignment,
    Done,
}

impl MetaBlockHeaderDecoder {
    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<MetaBlockHeader>, MetaBlockHeaderError> {
        loop {
            match self.state {
                State::IsLast => {
                    let Some(bit) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };
                    self.is_last = bit != 0;
                    self.state = if self.is_last {
                        State::IsLastEmpty
                    } else {
                        State::Nibbles
                    };
                }
                State::IsLastEmpty => {
                    let Some(bit) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };
                    self.state = if bit != 0 {
                        State::EndAlignment
                    } else {
                        State::Nibbles
                    };
                }
                State::Nibbles => {
                    let Some(bits) = reader.read_bits(input, cursor, 2) else {
                        return Ok(None);
                    };

                    self.index = 0;
                    self.value = 0;
                    self.width = match bits {
                        0 => 4,
                        1 => 5,
                        2 => 6,
                        3 => {
                            self.state = State::MetadataReserved;
                            continue;
                        }
                        _ => unreachable!(),
                    };
                    self.state = State::DataLength;
                }
                State::DataLength => {
                    while self.index < self.width {
                        let Some(nibble) = reader.read_bits(input, cursor, 4) else {
                            return Ok(None);
                        };

                        if self.index + 1 == self.width && self.width > 4 && nibble == 0 {
                            return Err(MetaBlockHeaderError::NonMinimalLength);
                        }

                        self.value |= (nibble as usize) << (4 * self.index);
                        self.index += 1;
                    }

                    if self.is_last {
                        let length = self.value + 1;
                        return Ok(Some(self.finish(MetaBlockKind::Compressed { length })));
                    }
                    self.state = State::IsUncompressed;
                }
                State::IsUncompressed => {
                    let Some(bit) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };
                    if bit == 0 {
                        let length = self.value + 1;
                        return Ok(Some(self.finish(MetaBlockKind::Compressed { length })));
                    }
                    self.state = State::UncompressedAlignment;
                }
                State::UncompressedAlignment => {
                    if !reader.align_to_byte_with_zero_fill() {
                        return Err(MetaBlockHeaderError::NonZeroPadding);
                    }
                    let length = self.value + 1;
                    return Ok(Some(self.finish(MetaBlockKind::Uncompressed { length })));
                }
                State::MetadataReserved => {
                    let Some(bit) = reader.read_bits(input, cursor, 1) else {
                        return Ok(None);
                    };
                    if bit != 0 {
                        return Err(MetaBlockHeaderError::NonZeroReservedBit);
                    }
                    self.state = State::MetadataLengthWidth;
                }
                State::MetadataLengthWidth => {
                    let Some(bits) = reader.read_bits(input, cursor, 2) else {
                        return Ok(None);
                    };

                    self.width = bits as u8;
                    self.index = 0;
                    self.value = 0;
                    self.state = if self.width == 0 {
                        State::MetadataAlignment
                    } else {
                        State::MetadataLength
                    };
                }
                State::MetadataLength => {
                    while self.index < self.width {
                        let Some(byte) = reader.read_bits(input, cursor, 8) else {
                            return Ok(None);
                        };

                        if self.index + 1 == self.width && self.width > 1 && byte == 0 {
                            return Err(MetaBlockHeaderError::NonMinimalLength);
                        }

                        self.value |= (byte as usize) << (8 * self.index);
                        self.index += 1;
                    }
                    self.value += 1;
                    self.state = State::MetadataAlignment;
                }
                State::MetadataAlignment => {
                    if !reader.align_to_byte_with_zero_fill() {
                        return Err(MetaBlockHeaderError::NonZeroPadding);
                    }
                    let length = self.value;
                    return Ok(Some(self.finish(MetaBlockKind::Metadata { length })));
                }
                State::EndAlignment => {
                    if !reader.align_to_byte_with_zero_fill() {
                        return Err(MetaBlockHeaderError::NonZeroPadding);
                    }
                    self.state = State::Done;
                    return Ok(Some(MetaBlockHeader {
                        is_last: true,
                        kind: MetaBlockKind::End,
                    }));
                }
                State::Done => unreachable!("metablock header decoded after stream end"),
            }
        }
    }

    fn finish(&mut self, kind: MetaBlockKind) -> MetaBlockHeader {
        let header = MetaBlockHeader {
            is_last: self.is_last,
            kind,
        };
        self.state = State::IsLast;
        self.is_last = false;
        self.width = 0;
        self.index = 0;
        self.value = 0;
        header
    }
}

#[cfg(test)]
mod tests {
    use super::{MetaBlockHeaderDecoder, MetaBlockHeaderError, MetaBlockKind};
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

    fn decode(bytes: &[u8]) -> Result<super::MetaBlockHeader, MetaBlockHeaderError> {
        let mut decoder = MetaBlockHeaderDecoder::default();
        let mut reader = BitReader::default();
        let mut cursor = 0;
        decoder
            .decode(&mut reader, bytes, &mut cursor)
            .map(|header| header.expect("test header should contain enough input"))
    }

    #[test]
    fn decodes_empty_final_metablock() {
        let mut bits = Bits::default();
        bits.push(1, 1); // ISLAST
        bits.push(1, 1); // ISLASTEMPTY

        let header = decode(&bits.into_bytes()).unwrap();

        assert!(header.is_last);
        assert_eq!(header.kind, MetaBlockKind::End);
    }

    #[test]
    fn decodes_last_compressed_metablock() {
        let mut bits = Bits::default();
        bits.push(1, 1); // ISLAST
        bits.push(0, 1); // ISLASTEMPTY
        bits.push(0, 2); // 4 length nibbles
        bits.push(1, 16); // MLEN - 1

        let header = decode(&bits.into_bytes()).unwrap();

        assert!(header.is_last);
        assert_eq!(header.kind, MetaBlockKind::Compressed { length: 2 });
    }

    #[test]
    fn decodes_non_final_uncompressed_metablock() {
        let mut bits = Bits::default();
        bits.push(0, 1); // ISLAST
        bits.push(0, 2); // 4 length nibbles
        bits.push(0, 16); // MLEN - 1
        bits.push(1, 1); // ISUNCOMPRESSED

        let header = decode(&bits.into_bytes()).unwrap();

        assert!(!header.is_last);
        assert_eq!(header.kind, MetaBlockKind::Uncompressed { length: 1 });
    }

    #[test]
    fn decodes_empty_metadata_metablock() {
        let mut bits = Bits::default();
        bits.push(0, 1); // ISLAST
        bits.push(3, 2); // metadata marker
        bits.push(0, 1); // reserved
        bits.push(0, 2); // zero metadata length bytes

        let header = decode(&bits.into_bytes()).unwrap();

        assert!(!header.is_last);
        assert_eq!(header.kind, MetaBlockKind::Metadata { length: 0 });
    }

    #[test]
    fn rejects_reserved_metadata_bit() {
        let mut bits = Bits::default();
        bits.push(0, 1);
        bits.push(3, 2);
        bits.push(1, 1);

        assert_eq!(
            decode(&bits.into_bytes()),
            Err(MetaBlockHeaderError::NonZeroReservedBit)
        );
    }

    #[test]
    fn rejects_non_minimal_data_length() {
        let mut bits = Bits::default();
        bits.push(0, 1);
        bits.push(1, 2); // 5 length nibbles
        bits.push(0, 20); // top nibble is zero

        assert_eq!(
            decode(&bits.into_bytes()),
            Err(MetaBlockHeaderError::NonMinimalLength)
        );
    }

    #[test]
    fn rejects_non_minimal_metadata_length() {
        let mut bits = Bits::default();
        bits.push(0, 1);
        bits.push(3, 2);
        bits.push(0, 1);
        bits.push(2, 2); // two metadata length bytes
        bits.push(1, 8);
        bits.push(0, 8); // high byte must not be zero

        assert_eq!(
            decode(&bits.into_bytes()),
            Err(MetaBlockHeaderError::NonMinimalLength)
        );
    }

    #[test]
    fn resumes_across_input_slices() {
        let mut bits = Bits::default();
        bits.push(0, 1);
        bits.push(0, 2);
        bits.push(0x1234, 16);
        bits.push(0, 1);
        let bytes = bits.into_bytes();

        let mut decoder = MetaBlockHeaderDecoder::default();
        let mut reader = BitReader::default();
        let mut first_cursor = 0;

        assert_eq!(
            decoder
                .decode(&mut reader, &bytes[..1], &mut first_cursor)
                .unwrap(),
            None
        );
        assert_eq!(first_cursor, 1);

        let mut second_cursor = 0;
        let header = decoder
            .decode(&mut reader, &bytes[1..], &mut second_cursor)
            .unwrap()
            .unwrap();

        assert_eq!(header.kind, MetaBlockKind::Compressed { length: 0x1235 });
    }

    #[test]
    fn rejects_non_zero_alignment_padding() {
        let mut bits = Bits::default();
        bits.push(0, 1);
        bits.push(3, 2);
        bits.push(0, 1);
        bits.push(0, 2);
        bits.push(1, 1); // metadata padding must be zero

        assert_eq!(
            decode(&bits.into_bytes()),
            Err(MetaBlockHeaderError::NonZeroPadding)
        );
    }
}
