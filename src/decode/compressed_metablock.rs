use super::bit_reader::BitReader;
use super::compressed_block::{CompressedBlock, CompressedBlockError, CompressedProgress, CompressedStatus};
use super::compressed_header::{CompressedHeader, CompressedHeaderDecoder, CompressedHeaderError};
use super::compressed_trees::CompressedTreesDecoder;
use super::distance::RecentDistances;
use super::history::History;
use super::prefix_code_decoder::PrefixCodeDecoderError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompressedMetaBlockError {
    Header(CompressedHeaderError),
    Trees(PrefixCodeDecoderError),
    Data(CompressedBlockError),
}

impl From<CompressedHeaderError> for CompressedMetaBlockError {
    fn from(error: CompressedHeaderError) -> Self {
        Self::Header(error)
    }
}

impl From<CompressedBlockError> for CompressedMetaBlockError {
    fn from(error: CompressedBlockError) -> Self {
        Self::Data(error)
    }
}

#[derive(Debug)]
pub(super) struct CompressedMetaBlockDecoder {
    length: usize,
    state: State,
    header_decoder: CompressedHeaderDecoder,
    header: Option<CompressedHeader>,
    trees_decoder: Option<CompressedTreesDecoder>,
    block: Option<CompressedBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Header,
    Trees,
    Data,
    Done,
}

impl CompressedMetaBlockDecoder {
    pub(super) fn new(length: usize) -> Self {
        assert!(length != 0);
        Self {
            length,
            state: State::Header,
            header_decoder: CompressedHeaderDecoder::default(),
            header: None,
            trees_decoder: None,
            block: None,
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
    ) -> Result<CompressedProgress, CompressedMetaBlockError> {
        loop {
            match self.state {
                State::Header => {
                    let Some(header) = self.header_decoder.decode(reader, input, input_cursor)?
                    else {
                        return Ok(CompressedProgress {
                            produced: 0,
                            status: CompressedStatus::NeedInput,
                        });
                    };
                    self.trees_decoder = Some(CompressedTreesDecoder::new(&header));
                    self.header = Some(header);
                    self.state = State::Trees;
                }
                State::Trees => {
                    let decoder = self
                        .trees_decoder
                        .as_mut()
                        .expect("tree decoder is initialized after compressed header");
                    let Some(trees) = decoder
                        .decode(reader, input, input_cursor)
                        .map_err(CompressedMetaBlockError::Trees)?
                    else {
                        return Ok(CompressedProgress {
                            produced: 0,
                            status: CompressedStatus::NeedInput,
                        });
                    };
                    let header = self
                        .header
                        .take()
                        .expect("compressed header is retained until trees are decoded");
                    self.trees_decoder = None;
                    self.block = Some(CompressedBlock::new(header, trees, self.length));
                    self.state = State::Data;
                }
                State::Data => {
                    let block = self
                        .block
                        .as_mut()
                        .expect("compressed block is initialized after its trees");
                    let progress = block.process(
                        reader,
                        input,
                        input_cursor,
                        output,
                        history,
                        recent_distances,
                    )?;
                    if progress.status == CompressedStatus::Done {
                        self.state = State::Done;
                    }
                    return Ok(progress);
                }
                State::Done => {
                    return Ok(CompressedProgress {
                        produced: 0,
                        status: CompressedStatus::Done,
                    });
                }
            }
        }
    }
}
