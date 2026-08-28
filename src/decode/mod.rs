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
