use super::bit_reader::BitReader;
use super::history::History;
use super::metablock_header::{MetaBlockHeaderDecoder, MetaBlockHeaderError, MetaBlockKind};
use super::stream_header::{StreamHeaderDecoder, StreamHeaderError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessStatus {
    NeedInput,
    NeedOutput,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProcessResult {
    pub(super) consumed: usize,
    pub(super) produced: usize,
    pub(super) status: ProcessStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodeError {
    StreamHeader(StreamHeaderError),
    MetaBlockHeader(MetaBlockHeaderError),
    CompressedDataUnsupported,
}

impl From<StreamHeaderError> for DecodeError {
    fn from(error: StreamHeaderError) -> Self {
        Self::StreamHeader(error)
    }
}

impl From<MetaBlockHeaderError> for DecodeError {
    fn from(error: MetaBlockHeaderError) -> Self {
        Self::MetaBlockHeader(error)
    }
}

#[derive(Debug, Default)]
pub(super) struct Decoder {
    reader: BitReader,
    stream_header: StreamHeaderDecoder,
    metablock_header: MetaBlockHeaderDecoder,
    phase: Phase,
    window_bits: Option<u8>,
    history: Option<History>,
}

#[derive(Debug, Default)]
enum Phase {
    #[default]
    StreamHeader,
    MetaBlockHeader,
    Uncompressed {
        remaining: usize,
    },
    Metadata {
        remaining: usize,
        is_last: bool,
    },
    Done,
}

impl Decoder {
    pub(super) fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<ProcessResult, DecodeError> {
        let mut input_cursor = 0;
        let mut output_cursor = 0;

        loop {
            match self.phase {
                Phase::StreamHeader => {
                    let Some(header) =
                        self.stream_header
                            .decode(&mut self.reader, input, &mut input_cursor)?
                    else {
                        return Ok(ProcessResult {
                            consumed: input_cursor,
                            produced: output_cursor,
                            status: ProcessStatus::NeedInput,
                        });
                    };

                    let window_bits = header.window_bits();
                    self.window_bits = Some(window_bits);
                    self.history = Some(History::new(window_bits));
                    self.phase = Phase::MetaBlockHeader;
                }
                Phase::MetaBlockHeader => {
                    let Some(header) =
                        self.metablock_header
                            .decode(&mut self.reader, input, &mut input_cursor)?
                    else {
                        return Ok(ProcessResult {
                            consumed: input_cursor,
                            produced: output_cursor,
                            status: ProcessStatus::NeedInput,
                        });
                    };

                    self.phase = match header.kind {
                        MetaBlockKind::End => Phase::Done,
                        MetaBlockKind::Compressed { .. } => {
                            return Err(DecodeError::CompressedDataUnsupported);
                        }
                        MetaBlockKind::Uncompressed { length } => {
                            Phase::Uncompressed { remaining: length }
                        }
                        MetaBlockKind::Metadata { length } => Phase::Metadata {
                            remaining: length,
                            is_last: header.is_last,
                        },
                    };
                }
                Phase::Uncompressed { remaining } => {
                    if remaining == 0 {
                        self.phase = Phase::MetaBlockHeader;
                        continue;
                    }

                    if output_cursor == output.len() {
                        return Ok(ProcessResult {
                            consumed: input_cursor,
                            produced: output_cursor,
                            status: ProcessStatus::NeedOutput,
                        });
                    }

                    if input_cursor == input.len() {
                        return Ok(ProcessResult {
                            consumed: input_cursor,
                            produced: output_cursor,
                            status: ProcessStatus::NeedInput,
                        });
                    }

                    let count = remaining
                        .min(input.len() - input_cursor)
                        .min(output.len() - output_cursor);
                    let input_end = input_cursor + count;
                    let output_end = output_cursor + count;
                    let bytes = &input[input_cursor..input_end];
                    output[output_cursor..output_end].copy_from_slice(bytes);
                    self.history
                        .as_mut()
                        .expect("history is initialized after the stream header")
                        .push_slice(bytes);
                    input_cursor = input_end;
                    output_cursor = output_end;
                    self.phase = Phase::Uncompressed {
                        remaining: remaining - count,
                    };
                }
                Phase::Metadata { remaining, is_last } => {
                    if remaining == 0 {
                        self.phase = if is_last {
                            Phase::Done
                        } else {
                            Phase::MetaBlockHeader
                        };
                        continue;
                    }

                    if input_cursor == input.len() {
                        return Ok(ProcessResult {
                            consumed: input_cursor,
                            produced: output_cursor,
                            status: ProcessStatus::NeedInput,
                        });
                    }

                    let count = remaining.min(input.len() - input_cursor);
                    input_cursor += count;
                    self.phase = Phase::Metadata {
                        remaining: remaining - count,
                        is_last,
                    };
                }
                Phase::Done => {
                    return Ok(ProcessResult {
                        consumed: input_cursor,
                        produced: output_cursor,
                        status: ProcessStatus::Done,
                    });
                }
            }
        }
    }

    #[cfg(test)]
    fn window_bits(&self) -> Option<u8> {
        self.window_bits
    }

    #[cfg(test)]
    fn history_previous_bytes(&self) -> Option<(u8, u8)> {
        self.history.as_ref().map(History::previous_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeError, Decoder, ProcessStatus};

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

        fn align_zero(&mut self) {
            while !self.bits.len().is_multiple_of(8) {
                self.bits.push(false);
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

    fn empty_stream() -> Vec<u8> {
        let mut bits = Bits::default();
        bits.push(0, 1); // WBITS = 16
        bits.push(1, 1); // ISLAST
        bits.push(1, 1); // ISLASTEMPTY
        bits.align_zero();
        bits.into_bytes()
    }

    fn uncompressed_stream(payload: &[u8]) -> Vec<u8> {
        assert!(!payload.is_empty());
        assert!(payload.len() <= u16::MAX as usize + 1);

        let mut bits = Bits::default();
        bits.push(0, 1); // WBITS = 16
        bits.push(0, 1); // ISLAST
        bits.push(0, 2); // MNIBBLES = 4
        bits.push((payload.len() - 1) as u64, 16); // MLEN - 1
        bits.push(1, 1); // ISUNCOMPRESSED
        bits.align_zero();

        let mut bytes = bits.into_bytes();
        bytes.extend_from_slice(payload);
        bytes.push(0b0000_0011); // final empty metablock at a byte boundary
        bytes
    }

    fn metadata_then_end(payload: &[u8]) -> Vec<u8> {
        assert!(!payload.is_empty());
        assert!(payload.len() <= 256);

        let mut bits = Bits::default();
        bits.push(0, 1); // WBITS = 16
        bits.push(0, 1); // ISLAST
        bits.push(3, 2); // metadata marker
        bits.push(0, 1); // reserved
        bits.push(1, 2); // one metadata length byte
        bits.push((payload.len() - 1) as u64, 8);
        bits.align_zero();

        let mut bytes = bits.into_bytes();
        bytes.extend_from_slice(payload);
        bytes.push(0b0000_0011); // final empty metablock
        bytes
    }

    #[test]
    fn decodes_empty_stream() {
        let input = empty_stream();
        let mut decoder = Decoder::default();
        let mut output = [];

        let result = decoder.process(&input, &mut output).unwrap();

        assert_eq!(result.status, ProcessStatus::Done);
        assert_eq!(result.consumed, input.len());
        assert_eq!(result.produced, 0);
        assert_eq!(decoder.window_bits(), Some(16));
        assert_eq!(decoder.history_previous_bytes(), Some((0, 0)));
    }

    #[test]
    fn copies_uncompressed_metablock_end_to_end() {
        let input = uncompressed_stream(b"brutli");
        let mut decoder = Decoder::default();
        let mut output = [0; 6];

        let result = decoder.process(&input, &mut output).unwrap();

        assert_eq!(result.status, ProcessStatus::Done);
        assert_eq!(result.consumed, input.len());
        assert_eq!(result.produced, output.len());
        assert_eq!(&output, b"brutli");
        assert_eq!(decoder.history_previous_bytes(), Some((b'i', b'l')));
    }

    #[test]
    fn resumes_when_output_is_full() {
        let input = uncompressed_stream(b"abcdef");
        let mut decoder = Decoder::default();
        let mut first_output = [0; 2];

        let first = decoder.process(&input, &mut first_output).unwrap();

        assert_eq!(first.status, ProcessStatus::NeedOutput);
        assert_eq!(&first_output, b"ab");

        let mut second_output = [0; 4];
        let second = decoder
            .process(&input[first.consumed..], &mut second_output)
            .unwrap();

        assert_eq!(second.status, ProcessStatus::Done);
        assert_eq!(&second_output, b"cdef");
        assert_eq!(first.consumed + second.consumed, input.len());
        assert_eq!(decoder.history_previous_bytes(), Some((b'f', b'e')));
    }

    #[test]
    fn resumes_when_input_is_exhausted() {
        let input = uncompressed_stream(b"abcdef");
        let mut decoder = Decoder::default();
        let mut output = [0; 6];

        let first = decoder.process(&input[..2], &mut output).unwrap();

        assert_eq!(first.status, ProcessStatus::NeedInput);
        assert_eq!(first.consumed, 2);
        assert_eq!(first.produced, 0);

        let second = decoder
            .process(&input[first.consumed..], &mut output)
            .unwrap();

        assert_eq!(second.status, ProcessStatus::Done);
        assert_eq!(second.produced, output.len());
        assert_eq!(&output, b"abcdef");
        assert_eq!(decoder.history_previous_bytes(), Some((b'f', b'e')));
    }

    #[test]
    fn skips_metadata_without_producing_output_or_history() {
        let input = metadata_then_end(b"metadata");
        let mut decoder = Decoder::default();
        let mut output = [];

        let result = decoder.process(&input, &mut output).unwrap();

        assert_eq!(result.status, ProcessStatus::Done);
        assert_eq!(result.consumed, input.len());
        assert_eq!(result.produced, 0);
        assert_eq!(decoder.history_previous_bytes(), Some((0, 0)));
    }

    #[test]
    fn reports_compressed_path_as_not_yet_implemented() {
        let mut bits = Bits::default();
        bits.push(0, 1); // WBITS = 16
        bits.push(1, 1); // ISLAST
        bits.push(0, 1); // ISLASTEMPTY
        bits.push(0, 2); // MNIBBLES = 4
        bits.push(0, 16); // one-byte compressed metablock
        let input = bits.into_bytes();

        let mut decoder = Decoder::default();
        let mut output = [0; 1];

        assert_eq!(
            decoder.process(&input, &mut output),
            Err(DecodeError::CompressedDataUnsupported)
        );
    }
}
