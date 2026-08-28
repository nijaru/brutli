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

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_decode(input: &[u8]) {
    use decoder::{Decoder, ProcessStatus};

    const MAX_OUTPUT: usize = 1 << 20;
    const MAX_STEPS: usize = 4096;

    let mut decoder = Decoder::default();
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
            ProcessStatus::Done => return,
            ProcessStatus::NeedInput => {
                assert_eq!(input_offset, input.len());
                return;
            }
            ProcessStatus::NeedOutput => {
                assert!(
                    result.consumed != 0 || result.produced != 0,
                    "decoder stalled while requesting output"
                );
            }
        }
    }

    panic!("decoder exceeded bounded fuzz work");
}
