use std::io::{self, BufRead, Read};

use crate::{DecodeError, DecodeStatus, Decoder};

/// A [`Read`] adapter that decodes one Brotli stream from a buffered reader.
///
/// `DecoderReader` consumes only bytes belonging to the first Brotli stream.
/// Any bytes already buffered after the stream remain available from the
/// underlying [`BufRead`] value returned by [`Self::into_inner`].
#[derive(Debug)]
pub struct DecoderReader<R> {
    inner: R,
    decoder: Decoder,
    done: bool,
}

impl<R> DecoderReader<R> {
    /// Creates a reader with a default decoder.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            decoder: Decoder::new(),
            done: false,
        }
    }

    /// Creates a reader using a caller-configured decoder.
    ///
    /// This is useful for applying limits such as [`Decoder::with_output_limit`].
    pub fn with_decoder(inner: R, decoder: Decoder) -> Self {
        Self {
            inner,
            decoder,
            done: false,
        }
    }

    /// Returns a shared reference to the underlying reader.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Returns a mutable reference to the underlying reader.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Returns a shared reference to the decoder.
    pub fn decoder(&self) -> &Decoder {
        &self.decoder
    }

    /// Consumes the adapter and returns the underlying reader.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: BufRead> Read for DecoderReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.done {
            return Ok(0);
        }

        loop {
            let input = self.inner.fill_buf()?;
            let end_of_input = input.is_empty();
            let progress = if end_of_input {
                self.decoder.finish(output)
            } else {
                self.decoder.process(input, output)
            }
            .map_err(decode_error_to_io)?;

            if !end_of_input {
                self.inner.consume(progress.consumed);
            }

            if progress.status == DecodeStatus::Done {
                self.done = true;
            }

            if progress.produced != 0 {
                return Ok(progress.produced);
            }

            match progress.status {
                DecodeStatus::Done => return Ok(0),
                DecodeStatus::NeedInput if end_of_input => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        DecodeError::UnexpectedEof,
                    ));
                }
                DecodeStatus::NeedInput => {
                    if progress.consumed == 0 {
                        return Err(io::Error::other(
                            "Brotli decoder made no progress while requesting input",
                        ));
                    }
                }
                DecodeStatus::NeedOutput => {
                    return Err(io::Error::other(
                        "Brotli decoder made no progress with non-empty output buffer",
                    ));
                }
            }
        }
    }
}

fn decode_error_to_io(error: DecodeError) -> io::Error {
    let kind = match error {
        DecodeError::UnexpectedEof => io::ErrorKind::UnexpectedEof,
        _ => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, error)
}
