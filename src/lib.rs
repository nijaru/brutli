//! Brutli is an idiomatic Brotli implementation in Rust.
//!
//! The RFC 7932 decoder provides incremental buffer-to-buffer decoding. The
//! encoder provides a one-shot standards-compliant baseline with compressed
//! and stored meta-blocks.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use std::fmt;

mod decode;
mod dictionary;
mod encode;
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
    /// The stream requests a window larger than the configured limit.
    WindowLimitExceeded {
        /// Window size requested by the stream, expressed as `WBITS`.
        window_bits: u8,
        /// Maximum accepted `WBITS` value.
        max_window_bits: u8,
    },
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
            Self::WindowLimitExceeded {
                window_bits,
                max_window_bits,
            } => write!(
                formatter,
                "Brotli stream requests WBITS={window_bits}, exceeding configured maximum {max_window_bits}"
            ),
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
    /// Creates a decoder with no decoded-output limit and the full RFC 7932 window range.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a decoder that rejects streams producing more than `limit` bytes.
    #[must_use]
    pub fn with_output_limit(limit: usize) -> Self {
        Self::with_limits(Some(limit), 24)
    }

    /// Creates a decoder that rejects streams whose `WBITS` exceeds `max_window_bits`.
    ///
    /// RFC 7932 defines `WBITS` values from 10 through 24. Values at or above
    /// 24 therefore accept every RFC 7932 window size; values below 10 reject
    /// every valid RFC 7932 stream.
    #[must_use]
    pub fn with_max_window_bits(max_window_bits: u8) -> Self {
        Self::with_limits(None, max_window_bits)
    }

    /// Creates a decoder with explicit output and window limits.
    ///
    /// `max_output_size = None` leaves decoded output unbounded. RFC 7932 uses
    /// `WBITS` values from 10 through 24.
    #[must_use]
    pub fn with_limits(max_output_size: Option<usize>, max_window_bits: u8) -> Self {
        Self {
            core: decode::CoreDecoder::with_max_window_bits(max_window_bits),
            output_limit: max_output_size,
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

/// Input modes supported by Brotli encoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderMode {
    /// General-purpose binary or mixed input.
    Generic,
    /// Natural-language or UTF-8-heavy input.
    Text,
    /// Font data, which uses Brotli's font distance parameters.
    Font,
}

/// Options controlling one-shot Brotli encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderOptions {
    /// Quality level from `0` (fastest) through `11` (slowest).
    pub quality: u8,
    /// RFC 7932 window bits from `10` through `24`.
    pub window_bits: u8,
    /// Heuristic mode used by the encoder.
    pub mode: EncoderMode,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            quality: 5,
            window_bits: 22,
            mode: EncoderMode::Generic,
        }
    }
}

/// Errors reported while encoding a Brotli stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeError {
    /// The requested quality is outside the Brotli range `0..=11`.
    InvalidQuality {
        /// The rejected quality value.
        quality: u8,
    },
    /// The requested `WBITS` value is outside the RFC 7932 range `10..=24`.
    InvalidWindowBits {
        /// The rejected `WBITS` value.
        window_bits: u8,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQuality { quality } => {
                write!(formatter, "Brotli quality must be in 0..=11, got {quality}")
            }
            Self::InvalidWindowBits { window_bits } => write!(
                formatter,
                "RFC 7932 window bits must be in 10..=24, got {window_bits}"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encodes one complete RFC 7932 Brotli stream with the default `WBITS=22`.
///
/// The current one-shot encoder uses a fast greedy LZ77 parse, canonical prefix
/// codes, specialized short-period handling, and stored-block fallback for
/// incompressible data. Use [`compress_with_window_bits`] to select another
/// RFC 7932 window size.
#[must_use]
pub fn compress(input: &[u8]) -> Vec<u8> {
    encode::compress(input)
}

/// Encodes one complete Brotli stream with explicit encoder options.
///
/// Quality values `0..=11` and RFC 7932 window values `10..=24` are accepted.
/// The current implementation remains a partial upstream-compatible encoder;
/// streaming controls are not part of [`EncoderOptions`] yet.
pub fn compress_with_options(
    input: &[u8],
    options: EncoderOptions,
) -> Result<Vec<u8>, EncodeError> {
    encode::compress_with_options(input, options)
}

/// Encodes one complete RFC 7932 Brotli stream with a selected window size.
///
/// `window_bits` must be in the RFC 7932 range `10..=24`. The window size is
/// `(1 << window_bits) - 16` bytes. This remains a one-shot encoder;
/// streaming controls are not exposed yet.
pub fn compress_with_window_bits(input: &[u8], window_bits: u8) -> Result<Vec<u8>, EncodeError> {
    compress_with_options(
        input,
        EncoderOptions {
            window_bits,
            ..EncoderOptions::default()
        },
    )
}

/// Encodes one complete Brotli stream using a selected quality level and the
/// default `WBITS=22` window.
///
/// Quality values from `0` through `11` are accepted. Higher values spend more
/// work searching for matches. This one-shot encoder is still a partial
/// upstream-compatible implementation; streaming controls are not exposed yet.
pub fn compress_with_quality(input: &[u8], quality: u8) -> Result<Vec<u8>, EncodeError> {
    compress_with_options(
        input,
        EncoderOptions {
            quality,
            ..EncoderOptions::default()
        },
    )
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
        decode::CoreError::WindowLimitExceeded {
            window_bits,
            max_window_bits,
        } => DecodeError::WindowLimitExceeded {
            window_bits,
            max_window_bits,
        },
    }
}

/// Drives the internal decoder with bounded resources for coverage-guided fuzzing.
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn fuzz_decode(input: &[u8]) {
    decode::fuzz_decode(input);
}
