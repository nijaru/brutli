#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryError {
    InvalidDistance,
}

#[derive(Debug)]
pub(super) struct History {
    buffer: Vec<u8>,
    ring_size: usize,
    ring_mask: usize,
    window_size: usize,
    write: usize,
    available: usize,
    previous: u8,
    second_previous: u8,
}

impl History {
    pub(super) fn new(window_bits: u8) -> Self {
        debug_assert!((10..=24).contains(&window_bits));
        let ring_size = 1_usize << window_bits;
        Self {
            buffer: Vec::new(),
            ring_size,
            ring_mask: ring_size - 1,
            window_size: ring_size - 16,
            write: 0,
            available: 0,
            previous: 0,
            second_previous: 0,
        }
    }

    pub(super) fn previous_bytes(&self) -> (u8, u8) {
        (self.previous, self.second_previous)
    }

    pub(super) fn max_backward_distance(&self) -> usize {
        self.available.min(self.window_size)
    }

    pub(super) fn push(&mut self, byte: u8) {
        if self.buffer.len() < self.ring_size {
            self.buffer.push(byte);
            self.write = self.buffer.len() & self.ring_mask;
        } else {
            self.buffer[self.write] = byte;
            self.write = (self.write + 1) & self.ring_mask;
        }

        self.available = self.available.saturating_add(1).min(self.window_size);
        self.second_previous = self.previous;
        self.previous = byte;
    }

    pub(super) fn push_slice(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        self.update_previous(bytes);
        self.available = self
            .available
            .saturating_add(bytes.len())
            .min(self.window_size);

        let mut remaining = bytes;
        if self.buffer.len() < self.ring_size {
            let grow = remaining.len().min(self.ring_size - self.buffer.len());
            self.buffer.extend_from_slice(&remaining[..grow]);
            self.write = self.buffer.len() & self.ring_mask;
            remaining = &remaining[grow..];
        }

        while !remaining.is_empty() {
            let contiguous = remaining.len().min(self.ring_size - self.write);
            let end = self.write + contiguous;
            self.buffer[self.write..end].copy_from_slice(&remaining[..contiguous]);
            self.write = end & self.ring_mask;
            remaining = &remaining[contiguous..];
        }
    }

    pub(super) fn copy_into(
        &mut self,
        distance: usize,
        count: usize,
        output: &mut [u8],
    ) -> Result<usize, HistoryError> {
        if distance == 0 || distance > self.max_backward_distance() {
            return Err(HistoryError::InvalidDistance);
        }

        let produced = count.min(output.len());
        if produced == 0 {
            return Ok(0);
        }

        let seed_len = produced.min(distance);
        self.copy_history_prefix(distance, &mut output[..seed_len]);

        let mut filled = seed_len;
        while filled < produced {
            let chunk = (produced - filled).min(filled);
            output.copy_within(..chunk, filled);
            filled += chunk;
        }

        self.push_slice(&output[..produced]);
        Ok(produced)
    }

    fn copy_history_prefix(&self, distance: usize, output: &mut [u8]) {
        debug_assert!(!output.is_empty());
        debug_assert!(output.len() <= distance);
        debug_assert!(distance <= self.max_backward_distance());

        if self.buffer.len() < self.ring_size {
            let start = self.buffer.len() - distance;
            output.copy_from_slice(&self.buffer[start..start + output.len()]);
            return;
        }

        let start = (self.write + self.ring_size - distance) & self.ring_mask;
        let first = output.len().min(self.ring_size - start);
        output[..first].copy_from_slice(&self.buffer[start..start + first]);
        if first < output.len() {
            output[first..].copy_from_slice(&self.buffer[..output.len() - first]);
        }
    }

    fn update_previous(&mut self, bytes: &[u8]) {
        if bytes.len() == 1 {
            self.second_previous = self.previous;
            self.previous = bytes[0];
        } else {
            self.second_previous = bytes[bytes.len() - 2];
            self.previous = bytes[bytes.len() - 1];
        }
    }

    #[cfg(test)]
    fn allocated_len(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{History, HistoryError};

    #[test]
    fn starts_without_allocating_the_full_window() {
        let history = History::new(24);
        assert_eq!(history.allocated_len(), 0);
        assert_eq!(history.max_backward_distance(), 0);
    }

    #[test]
    fn tracks_literal_context_bytes() {
        let mut history = History::new(10);
        assert_eq!(history.previous_bytes(), (0, 0));

        history.push(b'a');
        assert_eq!(history.previous_bytes(), (b'a', 0));

        history.push(b'b');
        assert_eq!(history.previous_bytes(), (b'b', b'a'));
    }

    #[test]
    fn bulk_push_tracks_context_bytes() {
        let mut history = History::new(10);
        history.push(b'x');
        history.push_slice(b"a");
        assert_eq!(history.previous_bytes(), (b'a', b'x'));

        history.push_slice(b"bcdef");
        assert_eq!(history.previous_bytes(), (b'f', b'e'));
    }

    #[test]
    fn copies_non_overlapping_history() {
        let mut history = History::new(10);
        history.push_slice(b"abcd");
        let mut output = [0; 4];

        assert_eq!(history.copy_into(4, 4, &mut output), Ok(4));
        assert_eq!(&output, b"abcd");
        assert_eq!(history.previous_bytes(), (b'd', b'c'));
    }

    #[test]
    fn overlapping_copy_observes_bytes_it_just_produced() {
        let mut history = History::new(10);
        history.push_slice(b"abc");
        let mut output = [0; 8];

        assert_eq!(history.copy_into(3, 8, &mut output), Ok(8));
        assert_eq!(&output, b"abcabcab");
    }

    #[test]
    fn distance_one_copy_scales_without_per_byte_history_lookup() {
        let mut history = History::new(10);
        history.push(b'z');
        let mut output = [0; 257];

        assert_eq!(history.copy_into(1, output.len(), &mut output), Ok(257));
        assert!(output.iter().all(|&byte| byte == b'z'));
        assert_eq!(history.previous_bytes(), (b'z', b'z'));
    }

    #[test]
    fn rejects_distances_before_the_start_or_past_the_window() {
        let mut history = History::new(10);
        history.push_slice(b"abc");
        let mut output = [0; 1];

        assert_eq!(
            history.copy_into(4, 1, &mut output),
            Err(HistoryError::InvalidDistance)
        );

        for byte in 0..=u8::MAX {
            history.push(byte);
        }
        while history.max_backward_distance() < 1008 {
            history.push(0);
        }
        assert_eq!(history.max_backward_distance(), 1008);
        assert_eq!(
            history.copy_into(1009, 1, &mut output),
            Err(HistoryError::InvalidDistance)
        );
    }

    #[test]
    fn wraps_without_changing_distance_semantics() {
        let mut history = History::new(10);
        let mut model = Vec::new();
        for index in 0..1300_usize {
            let byte = index.wrapping_mul(37) as u8;
            history.push(byte);
            model.push(byte);
        }

        for distance in [1, 17, 511, 1008] {
            let mut output = [0; 1];
            history.copy_into(distance, 1, &mut output).unwrap();
            let expected = model[model.len() - distance];
            assert_eq!(output[0], expected, "distance {distance}");
            model.push(expected);
        }
    }

    #[test]
    fn bulk_push_wraps_without_changing_distance_semantics() {
        let bytes = (0..2500_usize)
            .map(|index| index.wrapping_mul(53) as u8)
            .collect::<Vec<_>>();
        let mut history = History::new(10);
        history.push_slice(&bytes);

        for distance in [1, 17, 511, 1008] {
            let mut output = [0; 1];
            history.copy_into(distance, 1, &mut output).unwrap();
            assert_eq!(output[0], bytes[bytes.len() - distance]);
        }
    }

    #[test]
    fn overlapping_copy_reads_seed_across_ring_boundary() {
        let bytes = (0..1100_usize)
            .map(|index| index as u8)
            .collect::<Vec<_>>();
        let mut history = History::new(10);
        history.push_slice(&bytes);
        let mut output = [0; 64];
        let distance = 100;

        history
            .copy_into(distance, output.len(), &mut output)
            .unwrap();
        assert_eq!(
            &output[..],
            &bytes[bytes.len() - distance..bytes.len() - distance + 64]
        );
    }

    #[test]
    fn partial_output_preserves_remaining_copy_for_the_caller() {
        let mut history = History::new(10);
        history.push_slice(b"xyz");
        let mut output = [0; 2];

        assert_eq!(history.copy_into(3, 5, &mut output), Ok(2));
        assert_eq!(&output, b"xy");

        let mut rest = [0; 3];
        assert_eq!(history.copy_into(3, 3, &mut rest), Ok(3));
        assert_eq!(&rest, b"zxy");
    }
}
