use super::bit_reader::BitReader;
use super::block_state::BlockState;
use super::command::CommandDecoder;
use super::compressed_header::CompressedHeader;
use super::compressed_trees::CompressedTrees;
use super::context::LiteralContextMode;
use super::dictionary::{self, DictionaryError};
use super::distance::{DistanceDecoder, DistanceError, RecentDistances};
use super::history::{History, HistoryError};
use super::prefix_code::PrefixSymbolDecoder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompressedStatus {
    NeedInput,
    NeedOutput,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompressedProgress {
    pub(super) produced: usize,
    pub(super) status: CompressedStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompressedBlockError {
    MetaBlockOverflow,
    Distance(DistanceError),
    History(HistoryError),
    Dictionary(DictionaryError),
}

impl From<DistanceError> for CompressedBlockError {
    fn from(error: DistanceError) -> Self {
        Self::Distance(error)
    }
}

impl From<HistoryError> for CompressedBlockError {
    fn from(error: HistoryError) -> Self {
        Self::History(error)
    }
}

impl From<DictionaryError> for CompressedBlockError {
    fn from(error: DictionaryError) -> Self {
        Self::Dictionary(error)
    }
}

#[derive(Debug)]
pub(super) struct CompressedBlock {
    header: CompressedHeader,
    trees: CompressedTrees,
    literal_blocks: BlockState,
    command_blocks: BlockState,
    distance_blocks: BlockState,
    literal_symbol: PrefixSymbolDecoder,
    command_symbol: PrefixSymbolDecoder,
    distance_symbol: PrefixSymbolDecoder,
    command_decoder: Option<CommandDecoder>,
    distance_decoder: Option<DistanceDecoder>,
    dictionary_output: Option<DictionaryOutput>,
    remaining: usize,
    phase: Phase,
}

#[derive(Debug)]
struct DictionaryOutput {
    bytes: Vec<u8>,
    offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Command,
    CommandExtra,
    Literals {
        remaining: usize,
        copy_length: usize,
        implicit_distance_zero: bool,
    },
    Distance {
        copy_length: usize,
    },
    DistanceExtra {
        copy_length: usize,
    },
    Copy {
        remaining: usize,
        distance: usize,
    },
    Dictionary,
    Done,
}

impl CompressedBlock {
    pub(super) fn new(header: CompressedHeader, trees: CompressedTrees, length: usize) -> Self {
        Self {
            literal_blocks: BlockState::new(&header.literal_partition),
            command_blocks: BlockState::new(&header.command_partition),
            distance_blocks: BlockState::new(&header.distance_partition),
            header,
            trees,
            literal_symbol: PrefixSymbolDecoder::default(),
            command_symbol: PrefixSymbolDecoder::default(),
            distance_symbol: PrefixSymbolDecoder::default(),
            command_decoder: None,
            distance_decoder: None,
            dictionary_output: None,
            remaining: length,
            phase: Phase::Command,
        }
    }

    pub(super) fn process(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        input_cursor: &mut usize,
        output: &mut [u8],
        history: &mut History,
        recent_distances: &mut RecentDistances,
    ) -> Result<CompressedProgress, CompressedBlockError> {
        let mut output_cursor = 0;

        loop {
            match self.phase {
                Phase::Command => {
                    if self.remaining == 0 {
                        self.phase = Phase::Done;
                        continue;
                    }

                    let Some(block_type) = self.command_blocks.current(
                        &self.header.command_partition,
                        reader,
                        input,
                        input_cursor,
                    ) else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };
                    let tree = &self.trees.command[usize::from(block_type)];
                    let Some(symbol) =
                        tree.decode(&mut self.command_symbol, reader, input, input_cursor)
                    else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };
                    self.command_blocks
                        .consume(&self.header.command_partition, 1);
                    self.command_decoder = Some(CommandDecoder::new(symbol));
                    self.phase = Phase::CommandExtra;
                }
                Phase::CommandExtra => {
                    let decoder = self
                        .command_decoder
                        .as_mut()
                        .expect("command decoder is initialized after its symbol");
                    let Some(command) = decoder.decode(reader, input, input_cursor) else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };
                    self.command_decoder = None;
                    if command.insert_length > self.remaining {
                        return Err(CompressedBlockError::MetaBlockOverflow);
                    }
                    self.phase = Phase::Literals {
                        remaining: command.insert_length,
                        copy_length: command.copy_length,
                        implicit_distance_zero: command.implicit_distance_zero,
                    };
                }
                Phase::Literals {
                    remaining,
                    copy_length,
                    implicit_distance_zero,
                } => {
                    if remaining == 0 {
                        if self.remaining == 0 {
                            self.phase = Phase::Done;
                            continue;
                        }
                        if implicit_distance_zero {
                            self.begin_copy(copy_length, recent_distances.last(), history)?;
                        } else {
                            self.phase = Phase::Distance { copy_length };
                        }
                        continue;
                    }

                    if output_cursor == output.len() {
                        return Ok(progress(output_cursor, CompressedStatus::NeedOutput));
                    }

                    let Some(block_type) = self.literal_blocks.current(
                        &self.header.literal_partition,
                        reader,
                        input,
                        input_cursor,
                    ) else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };
                    let mode = LiteralContextMode::from_bits(
                        self.header.literal_context_modes[usize::from(block_type)],
                    );
                    let (previous, second_previous) = history.previous_bytes();
                    let context = mode.id(previous, second_previous);
                    let map_index = usize::from(block_type) * 64 + usize::from(context);
                    let tree_index = usize::from(self.header.literal_context_map[map_index]);
                    let tree = &self.trees.literal[tree_index];
                    let Some(literal) =
                        tree.decode(&mut self.literal_symbol, reader, input, input_cursor)
                    else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };

                    let literal = literal as u8;
                    output[output_cursor] = literal;
                    output_cursor += 1;
                    history.push(literal);
                    self.literal_blocks
                        .consume(&self.header.literal_partition, 1);
                    self.remaining -= 1;
                    self.phase = Phase::Literals {
                        remaining: remaining - 1,
                        copy_length,
                        implicit_distance_zero,
                    };
                }
                Phase::Distance { copy_length } => {
                    let Some(block_type) = self.distance_blocks.current(
                        &self.header.distance_partition,
                        reader,
                        input,
                        input_cursor,
                    ) else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };
                    let context = distance_context(copy_length);
                    let map_index = usize::from(block_type) * 4 + usize::from(context);
                    let tree_index = usize::from(self.header.distance_context_map[map_index]);
                    let tree = &self.trees.distance[tree_index];
                    let Some(symbol) =
                        tree.decode(&mut self.distance_symbol, reader, input, input_cursor)
                    else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };
                    self.distance_blocks
                        .consume(&self.header.distance_partition, 1);
                    self.distance_decoder = Some(DistanceDecoder::new(
                        symbol,
                        self.header.distance_postfix_bits,
                        self.header.num_direct_distance_codes,
                        recent_distances,
                    )?);
                    self.phase = Phase::DistanceExtra { copy_length };
                }
                Phase::DistanceExtra { copy_length } => {
                    let decoder = self
                        .distance_decoder
                        .as_mut()
                        .expect("distance decoder is initialized after its symbol");
                    let Some(distance) = decoder.decode(reader, input, input_cursor) else {
                        return Ok(progress(output_cursor, CompressedStatus::NeedInput));
                    };
                    self.distance_decoder = None;
                    let is_lz77 = self.begin_copy(copy_length, distance.value, history)?;
                    if distance.should_push && is_lz77 {
                        recent_distances.push(distance.value);
                    }
                }
                Phase::Copy {
                    remaining,
                    distance,
                } => {
                    if remaining == 0 {
                        self.phase = Phase::Command;
                        continue;
                    }
                    if output_cursor == output.len() {
                        return Ok(progress(output_cursor, CompressedStatus::NeedOutput));
                    }

                    let produced =
                        history.copy_into(distance, remaining, &mut output[output_cursor..])?;
                    output_cursor += produced;
                    self.remaining -= produced;
                    self.phase = Phase::Copy {
                        remaining: remaining - produced,
                        distance,
                    };
                }
                Phase::Dictionary => {
                    if output_cursor == output.len() {
                        return Ok(progress(output_cursor, CompressedStatus::NeedOutput));
                    }

                    let dictionary = self
                        .dictionary_output
                        .as_ref()
                        .expect("dictionary output is initialized before dictionary phase");
                    let produced = (dictionary.bytes.len() - dictionary.offset)
                        .min(output.len() - output_cursor);
                    let end = dictionary.offset + produced;
                    let bytes = &dictionary.bytes[dictionary.offset..end];
                    output[output_cursor..output_cursor + produced].copy_from_slice(bytes);
                    history.push_slice(bytes);
                    output_cursor += produced;
                    self.remaining -= produced;

                    let dictionary = self
                        .dictionary_output
                        .as_mut()
                        .expect("dictionary output remains initialized while draining it");
                    dictionary.offset = end;
                    if dictionary.offset == dictionary.bytes.len() {
                        self.dictionary_output = None;
                        self.phase = Phase::Command;
                    }
                }
                Phase::Done => {
                    return Ok(progress(output_cursor, CompressedStatus::Done));
                }
            }
        }
    }

    fn begin_copy(
        &mut self,
        copy_length: usize,
        distance: usize,
        history: &History,
    ) -> Result<bool, CompressedBlockError> {
        let max_backward_distance = history.max_backward_distance();
        if distance > max_backward_distance {
            let bytes = dictionary::transform(distance, copy_length, max_backward_distance)?;
            if bytes.len() > self.remaining {
                return Err(CompressedBlockError::MetaBlockOverflow);
            }
            self.dictionary_output = Some(DictionaryOutput { bytes, offset: 0 });
            self.phase = Phase::Dictionary;
            return Ok(false);
        }

        if copy_length > self.remaining {
            return Err(CompressedBlockError::MetaBlockOverflow);
        }
        self.phase = Phase::Copy {
            remaining: copy_length,
            distance,
        };
        Ok(true)
    }
}

fn distance_context(copy_length: usize) -> u8 {
    match copy_length {
        2 => 0,
        3 => 1,
        4 => 2,
        _ => 3,
    }
}

fn progress(produced: usize, status: CompressedStatus) -> CompressedProgress {
    CompressedProgress { produced, status }
}

#[cfg(test)]
mod tests {
    use super::{CompressedBlock, CompressedBlockError, CompressedStatus};
    use crate::decode::bit_reader::BitReader;
    use crate::decode::block_partition::BlockPartition;
    use crate::decode::compressed_header::CompressedHeader;
    use crate::decode::compressed_trees::CompressedTrees;
    use crate::decode::distance::RecentDistances;
    use crate::decode::history::History;
    use crate::decode::prefix_code::PrefixCode;

    fn partition() -> BlockPartition {
        BlockPartition {
            num_types: 1,
            type_code: None,
            length_code: None,
            first_length: None,
        }
    }

    fn header(direct_distance_codes: u16) -> CompressedHeader {
        CompressedHeader {
            literal_partition: partition(),
            command_partition: partition(),
            distance_partition: partition(),
            distance_postfix_bits: 0,
            num_direct_distance_codes: direct_distance_codes,
            literal_context_modes: vec![0],
            num_literal_trees: 1,
            literal_context_map: vec![0; 64],
            num_distance_trees: 1,
            distance_context_map: vec![0; 4],
        }
    }

    fn trees(literal: u16, command: u16, distance: u16) -> CompressedTrees {
        CompressedTrees {
            literal: vec![PrefixCode::single(literal)],
            command: vec![PrefixCode::single(command)],
            distance: vec![PrefixCode::single(distance)],
        }
    }

    #[test]
    fn final_command_may_end_after_literals() {
        let mut block = CompressedBlock::new(header(0), trees(b'A' as u16, 8, 0), 1);
        let mut reader = BitReader::default();
        let mut input_cursor = 0;
        let mut output = [0; 1];
        let mut history = History::new(10);
        let mut recent = RecentDistances::default();

        let result = block
            .process(
                &mut reader,
                &[],
                &mut input_cursor,
                &mut output,
                &mut history,
                &mut recent,
            )
            .unwrap();

        assert_eq!(result.status, CompressedStatus::Done);
        assert_eq!(result.produced, 1);
        assert_eq!(&output, b"A");
    }

    #[test]
    fn implicit_distance_executes_overlapping_copy() {
        let mut block = CompressedBlock::new(header(0), trees(b'x' as u16, 32, 0), 6);
        let mut reader = BitReader::default();
        let mut input_cursor = 0;
        let mut output = [0; 6];
        let mut history = History::new(10);
        let mut recent = RecentDistances::default();

        let result = block
            .process(
                &mut reader,
                &[],
                &mut input_cursor,
                &mut output,
                &mut history,
                &mut recent,
            )
            .unwrap();

        assert_eq!(result.status, CompressedStatus::Done);
        assert_eq!(&output, b"xxxxxx");
    }

    #[test]
    fn explicit_direct_distance_updates_recent_ring() {
        let mut block = CompressedBlock::new(header(4), trees(0, 128, 16), 2);
        let mut reader = BitReader::default();
        let mut input_cursor = 0;
        let mut output = [0; 2];
        let mut history = History::new(10);
        history.push_slice(b"abc");
        let mut recent = RecentDistances::default();

        let result = block
            .process(
                &mut reader,
                &[],
                &mut input_cursor,
                &mut output,
                &mut history,
                &mut recent,
            )
            .unwrap();

        assert_eq!(result.status, CompressedStatus::Done);
        assert_eq!(&output, b"cc");
        assert_eq!(recent.last(), 1);
    }

    #[test]
    fn resumes_copy_when_output_fills() {
        let mut block = CompressedBlock::new(header(0), trees(b'x' as u16, 32, 0), 6);
        let mut reader = BitReader::default();
        let mut input_cursor = 0;
        let mut first = [0; 5];
        let mut history = History::new(10);
        let mut recent = RecentDistances::default();

        let first_result = block
            .process(
                &mut reader,
                &[],
                &mut input_cursor,
                &mut first,
                &mut history,
                &mut recent,
            )
            .unwrap();
        assert_eq!(first_result.status, CompressedStatus::NeedOutput);
        assert_eq!(&first, b"xxxxx");

        let mut second = [0; 1];
        let second_result = block
            .process(
                &mut reader,
                &[],
                &mut input_cursor,
                &mut second,
                &mut history,
                &mut recent,
            )
            .unwrap();
        assert_eq!(second_result.status, CompressedStatus::Done);
        assert_eq!(&second, b"x");
    }

    #[test]
    fn rejects_command_that_would_exceed_metablock_length() {
        let mut block = CompressedBlock::new(header(0), trees(b'x' as u16, 32, 0), 3);
        let mut reader = BitReader::default();
        let mut input_cursor = 0;
        let mut output = [0; 3];
        let mut history = History::new(10);
        let mut recent = RecentDistances::default();

        assert_eq!(
            block.process(
                &mut reader,
                &[],
                &mut input_cursor,
                &mut output,
                &mut history,
                &mut recent,
            ),
            Err(CompressedBlockError::MetaBlockOverflow)
        );
    }

    #[test]
    fn executes_dictionary_reference_without_caching_distance() {
        let mut block = CompressedBlock::new(header(1), trees(0, 130, 16), 4);
        let mut reader = BitReader::default();
        let mut input_cursor = 0;
        let mut output = [0; 4];
        let mut history = History::new(10);
        let mut recent = RecentDistances::default();

        let result = block
            .process(
                &mut reader,
                &[],
                &mut input_cursor,
                &mut output,
                &mut history,
                &mut recent,
            )
            .unwrap();

        assert_eq!(result.status, CompressedStatus::Done);
        assert_eq!(&output, b"time");
        assert_eq!(recent.last(), 4);
        assert_eq!(history.previous_bytes(), (b'e', b'm'));
    }
}
