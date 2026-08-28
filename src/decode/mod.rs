//! Internal incremental decoder implementation.

mod bit_reader;
mod block_partition;
mod block_state;
mod command;
mod complex_prefix_code;
mod compressed_block;
mod compressed_header;
mod compressed_metablock;
mod compressed_trees;
mod context;
mod context_map;
mod decoder;
mod dictionary;
#[cfg(test)]
mod differential_tests;
mod distance;
mod history;
#[cfg(test)]
mod malformed_tests;
mod metablock_header;
mod prefix_code;
mod prefix_code_decoder;
#[cfg(test)]
mod reference_tests;
mod simple_prefix_code;
mod stream_header;
mod tree_group;
mod var_len_uint8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreStatus {
    NeedInput,
    NeedOutput,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoreProgress {
    pub(crate) consumed: usize,
    pub(crate) produced: usize,
    pub(crate) status: CoreStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreError {
    InvalidStreamHeader,
    InvalidMetaBlock,
    InvalidCompressedData,
    NonZeroPadding,
}

#[derive(Debug, Default)]
pub(crate) struct CoreDecoder {
    inner: decoder::Decoder,
}

impl CoreDecoder {
    pub(crate) fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<CoreProgress, CoreError> {
        let result = self
            .inner
            .process(input, output)
            .map_err(|error| match error {
                decoder::DecodeError::StreamHeader(_) => CoreError::InvalidStreamHeader,
                decoder::DecodeError::MetaBlockHeader(_) => CoreError::InvalidMetaBlock,
                decoder::DecodeError::Compressed(_) => CoreError::InvalidCompressedData,
                decoder::DecodeError::NonZeroFinalPadding => CoreError::NonZeroPadding,
            })?;

        let status = match result.status {
            decoder::ProcessStatus::NeedInput => CoreStatus::NeedInput,
            decoder::ProcessStatus::NeedOutput => CoreStatus::NeedOutput,
            decoder::ProcessStatus::Done => CoreStatus::Done,
        };

        Ok(CoreProgress {
            consumed: result.consumed,
            produced: result.produced,
            status,
        })
    }
}

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_decode(input: &[u8]) {
    const MAX_OUTPUT: usize = 1 << 20;
    const MAX_STEPS: usize = 4096;

    let mut decoder = CoreDecoder::default();
    let mut input_offset = 0;
    let mut total_output = 0;

    for _ in 0..MAX_STEPS {
        let mut output = [0_u8; 4096];
        let result = match decoder.process(&input[input_offset..], &mut output) {
            Ok(result) => result,
            Err(_) => return,
        };

        input_offset += result.consumed;
        total_output += result.produced;
        assert!(input_offset <= input.len());

        if total_output >= MAX_OUTPUT {
            return;
        }

        match result.status {
            CoreStatus::Done => return,
            CoreStatus::NeedInput => {
                assert_eq!(input_offset, input.len());
                return;
            }
            CoreStatus::NeedOutput => {
                assert!(
                    result.consumed != 0 || result.produced != 0,
                    "decoder stalled while requesting output"
                );
            }
        }
    }

    panic!("decoder exceeded bounded fuzz work");
}
