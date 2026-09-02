use super::bit_writer::BitWriter;
use super::greedy::emit_chunk;
use super::match_finder::MatchFinder;
use super::{
    EncoderConfig, MAX_META_BLOCK_SIZE, compress_with_config, write_final_empty_metablock,
    write_uncompressed_metablock, write_window_bits,
};
use crate::{EncodeProgress, EncodeStatus};
/// Incremental greedy encoder core.
///
/// Accumulates caller input in a bounded window and emits one greedy
/// metablock each time `MAX_META_BLOCK_SIZE` unparsed bytes accumulate. The
/// match finder's hash index and recent-distance ring persist across
/// metablocks, so matches reference earlier output exactly as the decoder
/// replays them. History beyond `max_backward_distance` bytes is compacted
/// away, remapping the hash index; compaction only happens once the parse
/// frontier has passed the window cap, so window-relative positions produce
/// byte-identical output to the one-shot multi-metablock path.
///
/// When `finish` arrives before any metablock has been emitted, the buffered
/// input is encoded with the full one-shot candidate ladder, so small streams
/// keep their exact one-shot form.
#[derive(Debug)]
pub(crate) struct StreamEncoder {
    config: EncoderConfig,
    finder: MatchFinder,
    writer: BitWriter,
    window: Vec<u8>,
    /// Window-relative frontier of emitted input; equals the parse frontier.
    flushed: usize,
    /// Whether the stream header has been written.
    started: bool,
    /// Whether the terminating metablock has been emitted.
    finished: bool,
}

impl StreamEncoder {
    pub(crate) fn new(config: EncoderConfig) -> Self {
        let finder = MatchFinder::new(
            config.max_backward_distance(),
            config.max_distance(),
            config.search_depth(),
            config.max_lazy_delays(),
        );
        Self {
            config,
            finder,
            writer: BitWriter::default(),
            window: Vec::new(),
            flushed: 0,
            started: false,
            finished: false,
        }
    }

    /// Consumes input, emitting one metablock per `MAX_META_BLOCK_SIZE` bytes,
    /// and drains completed output bytes into `output`. All supplied input is
    /// consumed in one call; output is the bounded resource.
    ///
    /// Returns `consumed = 0, status = Done` when the stream has already been
    /// terminated: input supplied after completion is not consumed.
    pub(crate) fn process(&mut self, input: &[u8], output: &mut [u8]) -> EncodeProgress {
        if self.finished {
            let mut progress = self.drain(output);
            progress.status = EncodeStatus::Done;
            return progress;
        }
        let mut remaining = input;

        while !remaining.is_empty() {
            let pending = self.window.len() - self.flushed;
            // Fill to one byte past the metablock cap before emitting. A
            // stream that ends at or below the cap therefore reaches `finish`
            // with no metablock emitted and keeps its exact one-shot form
            // (including a single last metablock instead of a non-last one
            // plus a terminating empty metablock).
            let fill = (MAX_META_BLOCK_SIZE + 1 - pending).min(remaining.len());
            self.window.extend_from_slice(&remaining[..fill]);
            remaining = &remaining[fill..];

            if self.window.len() - self.flushed > MAX_META_BLOCK_SIZE {
                self.emit(false);
            }
        }

        let progress = self.drain(output);
        EncodeProgress {
            consumed: input.len(),
            ..progress
        }
    }

    /// Emits the remaining input as final metablocks, terminates the stream,
    /// and drains completed output bytes into `output`. Idempotent: further
    /// calls only drain.
    pub(crate) fn finish(&mut self, output: &mut [u8]) -> EncodeProgress {
        if !self.finished {
            if !self.started {
                // No metablock filled, so the whole input is buffered below
                // one metablock: delegate to the one-shot ladder so small
                // streams keep their exact one-shot form.
                let stream = compress_with_config(&self.window, self.config);
                self.writer.write_bytes(&stream);
                self.window.clear();
                self.flushed = 0;
            } else {
                let mut terminated = false;
                while self.window.len() > self.flushed {
                    let is_last = self.window.len() - self.flushed <= MAX_META_BLOCK_SIZE;
                    self.emit(is_last);
                    terminated |= is_last;
                }
                if !terminated {
                    // Input ended exactly at a mid-stream chunk boundary; the
                    // stream still needs a terminating last metablock.
                    write_final_empty_metablock(&mut self.writer);
                }
                self.writer.align_to_byte();
            }
            self.finished = true;
        }

        self.drain(output)
    }

    /// Emits the next metablock starting at the flush frontier. Quality 0
    /// skips the greedy parse (mirroring the one-shot candidate ladder) and
    /// emits a stored metablock; stored metablocks cannot be last, so a last
    /// stored chunk is followed by the terminating empty metablock.
    fn emit(&mut self, is_last: bool) {
        debug_assert!(self.window.len() > self.flushed);
        if !self.started {
            write_window_bits(&mut self.writer, self.config.window_bits());
            self.started = true;
        }

        let chunk_start = self.flushed;
        let chunk_end = if self.config.quality() == 0 {
            let chunk_end = (chunk_start + MAX_META_BLOCK_SIZE).min(self.window.len());
            write_uncompressed_metablock(&mut self.writer, &self.window[chunk_start..chunk_end]);
            if is_last {
                write_final_empty_metablock(&mut self.writer);
            }
            self.finder.set_position(chunk_end);
            chunk_end
        } else {
            emit_chunk(
                &self.window,
                chunk_start,
                &mut self.finder,
                &mut self.writer,
                self.config,
                is_last,
            )
        };
        debug_assert_eq!(chunk_end, self.finder.position());
        self.flushed = chunk_end;
        self.compact();
    }

    /// Drops history before the flush frontier that no future match can
    /// reference, remapping the finder's positions so window-relative
    /// distances stay correct. Compaction keeps exactly `max_backward_distance`
    /// history bytes, which is provably sufficient: the finder clamps every
    /// backward distance to `min(position, max_backward_distance)`, and
    /// compaction only runs once the stream has passed the window cap.
    fn compact(&mut self) {
        let max_backward = self.config.max_backward_distance();
        if self.flushed > max_backward {
            let dropped = self.flushed - max_backward;
            self.window.drain(..dropped);
            self.flushed -= dropped;
            self.finder.compact(dropped);
        }
    }

    /// Copies queued output bytes into `output` and reports what the caller
    /// must supply next.
    fn drain(&mut self, output: &mut [u8]) -> EncodeProgress {
        let produced = self.writer.drain_bytes(output);
        let status = if self.finished && self.writer.queued_bytes() == 0 {
            EncodeStatus::Done
        } else if self.writer.queued_bytes() > 0 {
            EncodeStatus::NeedOutput
        } else {
            EncodeStatus::NeedInput
        };
        EncodeProgress {
            consumed: 0,
            produced,
            status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DEFAULT_WINDOW_BITS, EncoderConfig, MAX_META_BLOCK_SIZE};
    use super::StreamEncoder;
    use crate::{EncodeStatus, EncoderOptions, decompress};

    fn default_config() -> EncoderConfig {
        EncoderConfig::new(DEFAULT_WINDOW_BITS, 5, crate::EncoderMode::Generic).unwrap()
    }

    fn drive_to_completion(encoder: &mut StreamEncoder, output: &mut Vec<u8>) -> EncodeStatus {
        let mut chunk = [0_u8; 4096];
        loop {
            let progress = encoder.finish(&mut chunk);
            output.extend_from_slice(&chunk[..progress.produced]);
            match progress.status {
                EncodeStatus::Done => return EncodeStatus::Done,
                EncodeStatus::NeedOutput => continue,
                EncodeStatus::NeedInput => unreachable!("finish never requests input"),
            }
        }
    }

    fn encode_incrementally(input: &[u8], chunk_size: usize, config: EncoderConfig) -> Vec<u8> {
        let mut encoder = StreamEncoder::new(config);
        let mut output = Vec::new();
        let mut buffer = [0_u8; 8192];

        for chunk in input.chunks(chunk_size) {
            let mut consumed = 0_usize;
            loop {
                let progress = encoder.process(&chunk[consumed..], &mut buffer);
                consumed += progress.consumed;
                output.extend_from_slice(&buffer[..progress.produced]);
                match progress.status {
                    EncodeStatus::NeedOutput => continue,
                    EncodeStatus::NeedInput => break,
                    EncodeStatus::Done => unreachable!("process never terminates the stream"),
                }
            }
            debug_assert_eq!(consumed, chunk.len());
        }

        drive_to_completion(&mut encoder, &mut output);
        output
    }

    #[test]
    fn small_input_matches_one_shot_exactly() {
        let source = b"the quick brown fox jumps over the lazy dog. ".repeat(96);
        for chunk_size in [1, 7, 1024, 4096, source.len()] {
            let streamed = encode_incrementally(&source, chunk_size, default_config());
            assert_eq!(streamed, crate::encode::compress(&source));
        }
    }

    #[test]
    fn multi_metablock_stream_matches_one_shot_exactly() {
        let unit = b"the quick brown fox jumps over the lazy dog. ".repeat(96);
        let mut source = unit.repeat((MAX_META_BLOCK_SIZE / unit.len()) + 2);
        source.truncate(MAX_META_BLOCK_SIZE + 4096);

        let streamed = encode_incrementally(&source, 1 << 20, default_config());
        let one_shot = crate::encode::compress(&source);
        assert_eq!(streamed, one_shot);
        assert_eq!(decompress(&streamed, source.len()).unwrap(), source);
    }

    #[test]
    fn compaction_crossing_stream_round_trips() {
        // Twice the window cap (WBITS 16 -> 65520 bytes) with matches that
        // reference data beyond the compaction frontier: the pattern repeats
        // at a stride longer than the window so distance codes stay valid.
        let mut source = Vec::new();
        let pattern: Vec<u8> = (0..200u32).map(|index| (index % 251) as u8).collect();
        while source.len() < (1 << 17) {
            source.extend_from_slice(&pattern);
        }
        source.truncate(1 << 17);

        let streamed = encode_incrementally(&source, 4096, default_config());
        assert_eq!(decompress(&streamed, source.len()).unwrap(), source);
    }

    #[test]
    fn small_window_compaction_round_trips() {
        // WBITS 10 gives a 1008-byte window: several compactions occur even
        // for a moderate stream.
        let config = EncoderConfig::new(10, 5, crate::EncoderMode::Generic).unwrap();
        let source = b"alpha beta gamma delta epsilon zeta eta theta. ".repeat(200);

        let streamed = encode_incrementally(&source, 333, config);
        assert_eq!(decompress(&streamed, source.len()).unwrap(), source);

        let mut decoded = vec![0_u8; source.len() + 1];
        let info = brotli_decompressor::brotli_decode(&streamed, &mut decoded);
        assert!(matches!(
            info.result,
            brotli_decompressor::BrotliResult::ResultSuccess
        ));
        assert_eq!(info.decoded_size, source.len());
        assert_eq!(&decoded[..info.decoded_size], &source[..]);
    }

    #[test]
    fn quality_zero_stream_matches_one_shot() {
        let mut source = vec![0_u8; MAX_META_BLOCK_SIZE + 64];
        for (index, chunk) in source.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            chunk.copy_from_slice(&(index as u64).to_le_bytes());
        }

        let one_shot = crate::encode::compress_with_options(
            &source,
            EncoderOptions {
                quality: 0,
                ..EncoderOptions::default()
            },
        )
        .unwrap();
        // Quality 0 one-shot output above the cap is all-stored, matching the
        // streaming stored chunks plus terminating empty metablock.
        let config =
            EncoderConfig::new(DEFAULT_WINDOW_BITS, 0, crate::EncoderMode::Generic).unwrap();
        let streamed_q0 = encode_incrementally(&source, 1 << 20, config);
        assert_eq!(streamed_q0, one_shot);
        assert_eq!(decompress(&streamed_q0, source.len()).unwrap(), source);
    }

    #[test]
    fn empty_stream_matches_one_shot() {
        let mut encoder = StreamEncoder::new(default_config());
        let mut output = Vec::new();
        drive_to_completion(&mut encoder, &mut output);
        assert_eq!(output, crate::encode::compress(&[]));
    }

    #[test]
    fn finish_is_idempotent_and_rejects_late_input() {
        let mut encoder = StreamEncoder::new(default_config());
        let mut output = Vec::new();
        let status = drive_to_completion(&mut encoder, &mut output);
        assert_eq!(status, EncodeStatus::Done);

        // Late input is not consumed; the encoder keeps reporting Done.
        let mut buffer = [0_u8; 16];
        let progress = encoder.process(b"late bytes", &mut buffer);
        assert_eq!(progress.consumed, 0);
        assert_eq!(progress.status, EncodeStatus::Done);
        let again = encoder.finish(&mut buffer);
        assert_eq!(again.status, EncodeStatus::Done);
    }

    #[test]
    fn random_input_still_round_trips() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut source = vec![0_u8; 1 << 16];
        for chunk in source.as_chunks_mut::<8>().0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes());
        }

        let streamed = encode_incrementally(&source, 1000, default_config());
        assert_eq!(decompress(&streamed, source.len()).unwrap(), source);
    }
}
