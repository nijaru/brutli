//! Brutli is an idiomatic, high-performance Brotli implementation in Rust.
//!
//! The initial implementation target is an incremental RFC 7932 decoder.
//! The primary API consumes input slices and fills caller-provided output
//! slices without taking ownership of either buffer.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use std::fmt;

mod decode;
mod reader;

pub use reader::DecoderReader;

/// The resource the decoder needs next, or completion of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStatus {
    /// More compressed input is required.
    NeedInput,
    /// More output space is required.
    NeedOutput,
    /// The Brotli stream is complete.
    Done,
}

/// Progress made by one incremental decoder call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeProgress {
    /// Number of bytes consumed from the supplied input slice.
    pub consumed: usize,
    /// Number of bytes written to the supplied output slice.
    pub produced: usize,
    /// What the decoder needs next.
    pub status: DecodeStatus,
}

/// Errors reported while decoding a Brotli stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The stream/window header is invalid.
    InvalidStreamHeader,
    /// A metablock header or metablock structure is invalid.
    InvalidMetaBlock,
    /// Compressed data within a metablock is invalid.
    InvalidCompressedData,
    /// Final alignment bits that must be zero were non-zero.
    NonZeroPadding,
    /// The caller declared end-of-input before the stream completed.
    UnexpectedEof,
    /// Decoded output would exceed the configured limit.
    OutputLimitExceeded {
        /// Maximum number of decoded bytes allowed.
        limit: usize,
    },
    /// Decoded byte accounting overflowed the platform address space.
    OutputSizeOverflow,
    /// A one-shot decode completed before consuming the full input slice.
    TrailingData {
        /// Number of bytes remaining after the Brotli stream.
        remaining: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStreamHeader => formatter.write_str("invalid Brotli stream header"),
            Self::InvalidMetaBlock => formatter.write_str("invalid Brotli metablock"),
            Self::InvalidCompressedData => formatter.write_str("invalid Brotli compressed data"),
            Self::NonZeroPadding => formatter.write_str("non-zero final Brotli padding"),
            Self::UnexpectedEof => formatter.write_str("unexpected end of Brotli input"),
            Self::OutputLimitExceeded { limit } => {
                write!(formatter, "decoded output exceeds limit of {limit} bytes")
            }
            Self::OutputSizeOverflow => formatter.write_str("decoded output size overflow"),
            Self::TrailingData { remaining } => {
                write!(formatter, "{remaining} trailing bytes after Brotli stream")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Incremental RFC 7932 Brotli decoder.
///
/// `Decoder` retains only decoding state between calls. It never stores
/// references to caller-provided input or output buffers.
#[derive(Debug, Default)]
pub struct Decoder {
    core: decode::CoreDecoder,
    output_limit: Option<usize>,
    total_output: usize,
}

impl Decoder {
    /// Creates a decoder with no decoded-output limit.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a decoder that rejects streams producing more than `limit` bytes.
    #[must_use]
    pub fn with_output_limit(limit: usize) -> Self {
        Self {
            core: decode::CoreDecoder::default(),
            output_limit: Some(limit),
            total_output: 0,
        }
    }

    /// Returns the number of decoded bytes produced so far.
    #[must_use]
    pub fn total_output(&self) -> usize {
        self.total_output
    }

    /// Consumes compressed input and writes decoded bytes into `output`.
    ///
    /// A `NeedInput` result means the supplied input slice was exhausted. A
    /// `NeedOutput` result means the output slice was exhausted. Advance the
    /// corresponding slice by `consumed` or `produced` before calling again.
    pub fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<DecodeProgress, DecodeError> {
        self.process_inner(input, output, false)
    }

    /// Declares that no more compressed input is available and drains output.
    ///
    /// Call this only after all input returned as unconsumed by [`Self::process`]
    /// has been supplied. If the decoder still requires compressed input,
    /// `UnexpectedEof` is returned. If output fills first, call `finish` again
    /// with additional output space.
    pub fn finish(&mut self, output: &mut [u8]) -> Result<DecodeProgress, DecodeError> {
        self.process_inner(&[], output, true)
    }

    fn process_inner(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        end_of_input: bool,
    ) -> Result<DecodeProgress, DecodeError> {
        let allowed_output = match self.output_limit {
            Some(limit) => output.len().min(limit.saturating_sub(self.total_output)),
            None => output.len(),
        };

        let progress = self
            .core
            .process(input, &mut output[..allowed_output])
            .map_err(map_core_error)?;

        self.total_output = self
            .total_output
            .checked_add(progress.produced)
            .ok_or(DecodeError::OutputSizeOverflow)?;

        let status = match progress.status {
            decode::CoreStatus::NeedInput if end_of_input => {
                return Err(DecodeError::UnexpectedEof);
            }
            decode::CoreStatus::NeedInput => DecodeStatus::NeedInput,
            decode::CoreStatus::NeedOutput => {
                if let Some(limit) = self.output_limit
                    && self.total_output >= limit
                {
                    return Err(DecodeError::OutputLimitExceeded { limit });
                }
                DecodeStatus::NeedOutput
            }
            decode::CoreStatus::Done => DecodeStatus::Done,
        };

        Ok(DecodeProgress {
            consumed: progress.consumed,
            produced: progress.produced,
            status,
        })
    }
}

/// Decompresses one complete Brotli stream into a `Vec<u8>`.
///
/// The helper is strict: trailing bytes after the first Brotli stream are
/// rejected, and decoded output is capped at `max_output_size`.
pub fn decompress(input: &[u8], max_output_size: usize) -> Result<Vec<u8>, DecodeError> {
    let mut decoder = Decoder::with_output_limit(max_output_size);
    let mut input_offset = 0;
    let mut output = Vec::new();
    let mut finishing = false;

    loop {
        let mut buffer = [0_u8; 8192];
        let progress = if finishing {
            decoder.finish(&mut buffer)?
        } else {
            decoder.process(&input[input_offset..], &mut buffer)?
        };

        if !finishing {
            input_offset = input_offset
                .checked_add(progress.consumed)
                .ok_or(DecodeError::OutputSizeOverflow)?;
            if input_offset > input.len() {
                return Err(DecodeError::InvalidCompressedData);
            }
        }
        output.extend_from_slice(&buffer[..progress.produced]);

        match progress.status {
            DecodeStatus::NeedOutput => {}
            DecodeStatus::NeedInput => {
                if input_offset != input.len() {
                    return Err(DecodeError::InvalidCompressedData);
                }
                finishing = true;
            }
            DecodeStatus::Done => {
                let remaining = input.len() - input_offset;
                if remaining != 0 {
                    return Err(DecodeError::TrailingData { remaining });
                }
                return Ok(output);
            }
        }
    }
}

fn map_core_error(error: decode::CoreError) -> DecodeError {
    match error {
        decode::CoreError::InvalidStreamHeader => DecodeError::InvalidStreamHeader,
        decode::CoreError::InvalidMetaBlock => DecodeError::InvalidMetaBlock,
        decode::CoreError::InvalidCompressedData => DecodeError::InvalidCompressedData,
        decode::CoreError::NonZeroPadding => DecodeError::NonZeroPadding,
    }
}

/// Drives the internal decoder with bounded resources for coverage-guided fuzzing.
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn fuzz_decode(input: &[u8]) {
    decode::fuzz_decode(input);
}
