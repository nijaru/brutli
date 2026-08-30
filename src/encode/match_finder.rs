use std::mem::MaybeUninit;

use super::distance::RecentDistances;
use super::static_dictionary::DictionarySearch;

const HASH_MULTIPLIER: u32 = 0x1e35_a7bd;
const BUCKET_BITS: usize = 15;
const BUCKET_COUNT: usize = 1 << BUCKET_BITS;
const BLOCK_BITS: usize = 3;
const BLOCK_SIZE: usize = 1 << BLOCK_BITS;
const BLOCK_MASK: usize = BLOCK_SIZE - 1;
const HASH_TYPE_LENGTH: usize = 4;
const STORE_LOOKAHEAD: usize = 4;
const MAX_BACKWARD_DISTANCE: usize = (1 << 22) - 16;
const NUM_LAST_DISTANCES_TO_CHECK: usize = 4;
const LITERAL_BYTE_SCORE: usize = 135;
const DISTANCE_BIT_PENALTY: usize = 30;
const SCORE_BASE: usize = DISTANCE_BIT_PENALTY * usize::BITS as usize;
const MIN_SCORE: usize = SCORE_BASE + 100;
const LAZY_SCORE_DELTA: usize = 175;
const MAX_LAZY_DELAYS: usize = 4;
const RANDOM_HEURISTICS_WINDOW: usize = 64;
const MATCH_WORD_BYTES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MatchCommand {
    pub(super) insert_start: usize,
    pub(super) insert_length: usize,
    pub(super) copy_length: usize,
    pub(super) copy_length_code: usize,
    pub(super) distance: usize,
    pub(super) distance_code: usize,
}

#[derive(Debug, Default)]
pub(super) struct Parse {
    pub(super) commands: Vec<MatchCommand>,
    pub(super) tail_start: usize,
}

#[derive(Debug, Clone, Copy)]
struct SearchResult {
    length: usize,
    length_code: usize,
    distance: usize,
    score: usize,
}

impl SearchResult {
    const fn new() -> Self {
        Self {
            length: 0,
            length_code: 0,
            distance: 0,
            score: MIN_SCORE,
        }
    }
}

#[derive(Debug)]
struct QualityFiveHasher {
    counts: Vec<u16>,
    buckets: Box<[MaybeUninit<u32>]>,
    dictionary: DictionarySearch,
}

impl QualityFiveHasher {
    fn new() -> Self {
        Self {
            counts: vec![0; BUCKET_COUNT],
            buckets: Box::<[u32]>::new_uninit_slice(BUCKET_COUNT * BLOCK_SIZE),
            dictionary: DictionarySearch::default(),
        }
    }

    fn store(&mut self, input: &[u8], position: usize) {
        let key = hash4(input, position);
        let count = usize::from(self.counts[key]);
        let offset = (key << BLOCK_BITS) + (count & BLOCK_MASK);
        self.buckets[offset].write(position as u32);
        self.counts[key] = self.counts[key].wrapping_add(1);
    }

    fn store_range(&mut self, input: &[u8], start: usize, end: usize) {
        for position in start..end {
            self.store(input, position);
        }
    }

    fn bucket_position(&self, offset: usize) -> usize {
        // SAFETY: a bucket slot is read only for indices below `counts[key]`.
        // Each count increment follows a write to the corresponding ring slot,
        // so every reachable slot has been initialized. If the u16 count wraps,
        // all ring slots have necessarily been written many times already.
        unsafe { *self.buckets[offset].assume_init_ref() as usize }
    }

    fn find_longest_match(
        &mut self,
        input: &[u8],
        recent_distances: [usize; 4],
        position: usize,
        max_length: usize,
        max_backward: usize,
    ) -> SearchResult {
        let key = hash4(input, position);
        let count = usize::from(self.counts[key]);
        let bucket_start = key << BLOCK_BITS;
        let mut result = SearchResult::new();
        let mut best_length = 0_usize;
        let mut best_score = MIN_SCORE;

        for (index, &backward) in recent_distances
            .iter()
            .take(NUM_LAST_DISTANCES_TO_CHECK)
            .enumerate()
        {
            if backward == 0 || backward > position || backward > max_backward {
                continue;
            }
            let previous = position - backward;
            if best_length < max_length
                && input[position + best_length] != input[previous + best_length]
            {
                continue;
            }

            let length = match_length(input, previous, position, max_length);
            if length >= 3 || (length == 2 && index < 2) {
                let mut score = score_using_last_distance(length);
                if best_score < score {
                    if index != 0 {
                        score = score.saturating_sub(last_distance_penalty(index));
                    }
                    if best_score < score {
                        best_score = score;
                        best_length = length;
                        result = SearchResult {
                            length,
                            length_code: length,
                            distance: backward,
                            score,
                        };
                    }
                }
            }
        }

        let oldest = count.saturating_sub(BLOCK_SIZE);
        for index in (oldest..count).rev() {
            let previous = self.bucket_position(bucket_start + (index & BLOCK_MASK));
            debug_assert!(previous < position);
            let backward = position - previous;
            if backward > max_backward {
                break;
            }

            let comparison_length = best_length.max(3);
            if comparison_length < max_length {
                let compare_at = comparison_length - 3;
                if read_u32(input, position + compare_at) != read_u32(input, previous + compare_at)
                {
                    continue;
                }
            }

            let length = match_length(input, previous, position, max_length);
            if length >= 4 {
                let score = backward_reference_score(length, backward);
                if best_score < score {
                    best_score = score;
                    best_length = length;
                    result = SearchResult {
                        length,
                        length_code: length,
                        distance: backward,
                        score,
                    };
                }
            }
        }

        let offset = bucket_start + (count & BLOCK_MASK);
        self.buckets[offset].write(position as u32);
        self.counts[key] = self.counts[key].wrapping_add(1);

        if result.score == MIN_SCORE
            && let Some(found) = self.dictionary.find(
                input,
                position,
                max_length,
                max_backward,
                MAX_BACKWARD_DISTANCE,
                MIN_SCORE,
            )
        {
            result = SearchResult {
                length: found.length,
                length_code: found.length_code,
                distance: found.distance,
                score: found.score,
            };
        }
        result
    }
}

pub(super) fn create_backward_references(input: &[u8]) -> Parse {
    assert!(
        u32::try_from(input.len()).is_ok(),
        "match input exceeds u32 position range"
    );

    if input.len() <= HASH_TYPE_LENGTH {
        return Parse {
            commands: Vec::new(),
            tail_start: 0,
        };
    }

    let end = input.len();
    let store_end = if end >= STORE_LOOKAHEAD {
        end - STORE_LOOKAHEAD + 1
    } else {
        0
    };
    let mut hasher = QualityFiveHasher::new();
    let mut recent_distances = RecentDistances::default();
    let mut commands = Vec::new();
    let mut position = 0_usize;
    let mut insert_start = 0_usize;
    let mut insert_length = 0_usize;
    let mut apply_random_heuristics = RANDOM_HEURISTICS_WINDOW;

    while position + HASH_TYPE_LENGTH < end {
        let max_backward = position.min(MAX_BACKWARD_DISTANCE);
        let mut result = hasher.find_longest_match(
            input,
            recent_distances.values(),
            position,
            end - position,
            max_backward,
        );

        if result.score > MIN_SCORE {
            let mut delayed = 0_usize;
            loop {
                let next_position = position + 1;
                let next_max_backward = next_position.min(MAX_BACKWARD_DISTANCE);
                let next = hasher.find_longest_match(
                    input,
                    recent_distances.values(),
                    next_position,
                    end - next_position,
                    next_max_backward,
                );
                if next.score >= result.score + LAZY_SCORE_DELTA {
                    position = next_position;
                    insert_length += 1;
                    result = next;
                    delayed += 1;
                    if delayed < MAX_LAZY_DELAYS && position + HASH_TYPE_LENGTH < end {
                        continue;
                    }
                }
                break;
            }

            apply_random_heuristics = position
                .saturating_add(2 * result.length)
                .saturating_add(RANDOM_HEURISTICS_WINDOW);
            let max_backward = position.min(MAX_BACKWARD_DISTANCE);
            let distance_code = recent_distances.compute_code(result.distance, max_backward);
            if result.distance <= max_backward && distance_code != 0 {
                recent_distances.push(result.distance);
            }

            commands.push(MatchCommand {
                insert_start,
                insert_length,
                copy_length: result.length,
                copy_length_code: result.length_code,
                distance: result.distance,
                distance_code,
            });

            let mut range_start = position + 2;
            let range_end = (position + result.length).min(store_end);
            if result.distance < (result.length >> 2) {
                range_start = range_start
                    .max(position + result.length - (result.distance << 2))
                    .min(range_end);
            }
            hasher.store_range(input, range_start, range_end);

            position += result.length;
            insert_start = position;
            insert_length = 0;
        } else {
            insert_length += 1;
            position += 1;

            if position > apply_random_heuristics {
                if position > apply_random_heuristics + 4 * RANDOM_HEURISTICS_WINDOW {
                    let margin = (STORE_LOOKAHEAD - 1).max(4);
                    let jump_end = (position + 16).min(end.saturating_sub(margin));
                    while position < jump_end {
                        hasher.store(input, position);
                        position += 4;
                        insert_length += 4;
                    }
                } else {
                    let margin = (STORE_LOOKAHEAD - 1).max(2);
                    let jump_end = (position + 8).min(end.saturating_sub(margin));
                    while position < jump_end {
                        hasher.store(input, position);
                        position += 2;
                        insert_length += 2;
                    }
                }
            }
        }
    }

    Parse {
        commands,
        tail_start: insert_start,
    }
}

fn hash4(input: &[u8], position: usize) -> usize {
    let value = read_u32(input, position);
    ((value.wrapping_mul(HASH_MULTIPLIER)) >> (32 - BUCKET_BITS)) as usize
}

#[inline(always)]
fn read_u32(input: &[u8], position: usize) -> u32 {
    debug_assert!(
        position
            .checked_add(4)
            .is_some_and(|end| end <= input.len())
    );
    // SAFETY: all callers ensure that four bytes starting at `position` are
    // within `input`; `read_unaligned` does not require pointer alignment.
    unsafe { std::ptr::read_unaligned(input.as_ptr().add(position).cast::<u32>()).to_le() }
}

#[inline(always)]
fn read_u64(input: &[u8], position: usize) -> u64 {
    debug_assert!(
        position
            .checked_add(MATCH_WORD_BYTES)
            .is_some_and(|end| end <= input.len())
    );
    // SAFETY: `match_length` only reaches this helper when a full eight-byte
    // word remains inside both compared ranges; unaligned reads are permitted.
    unsafe { std::ptr::read_unaligned(input.as_ptr().add(position).cast::<u64>()).to_le() }
}

pub(super) fn backward_reference_score(copy_length: usize, distance: usize) -> usize {
    SCORE_BASE + LITERAL_BYTE_SCORE * copy_length
        - DISTANCE_BIT_PENALTY * log2_floor_nonzero(distance)
}

fn score_using_last_distance(copy_length: usize) -> usize {
    SCORE_BASE + LITERAL_BYTE_SCORE * copy_length + 15
}

fn last_distance_penalty(short_code: usize) -> usize {
    39 + ((0x1ca10_usize >> (short_code & 0xe)) & 0xe)
}

fn log2_floor_nonzero(value: usize) -> usize {
    debug_assert!(value != 0);
    usize::BITS as usize - 1 - value.leading_zeros() as usize
}

fn match_length(input: &[u8], previous: usize, current: usize, limit: usize) -> usize {
    let mut length = 0_usize;
    while length + MATCH_WORD_BYTES <= limit {
        let difference = read_u64(input, previous + length) ^ read_u64(input, current + length);
        if difference != 0 {
            return length + ((difference.trailing_zeros() as usize) >> 3);
        }
        length += MATCH_WORD_BYTES;
    }
    while length < limit && input[previous + length] == input[current + length] {
        length += 1;
    }
    length
}

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_BITS, BLOCK_SIZE, QualityFiveHasher, backward_reference_score,
        create_backward_references, hash4, match_length, score_using_last_distance,
    };

    #[test]
    fn q5_hasher_keeps_most_recent_positions() {
        let input = vec![b'a'; 64];
        let mut hasher = QualityFiveHasher::new();
        for position in 0..20 {
            hasher.store(&input, position);
        }

        let key = hash4(&input, 0);
        let start = key << BLOCK_BITS;
        let mut positions = (0..BLOCK_SIZE)
            .map(|offset| hasher.bucket_position(start + offset) as u32)
            .collect::<Vec<_>>();
        positions.sort_unstable();
        let oldest = 20_u32 - BLOCK_SIZE as u32;
        assert_eq!(positions, (oldest..20).collect::<Vec<_>>());
    }

    #[test]
    fn last_distance_gets_reference_score_bonus() {
        assert!(score_using_last_distance(8) > backward_reference_score(8, 4));
    }

    #[test]
    fn match_length_finds_difference_inside_word() {
        let input = b"abcdefghabcxefgh";
        assert_eq!(match_length(input, 0, 8, 8), 3);
    }

    #[test]
    fn initial_last_distance_is_encoded_implicitly() {
        let parse = create_backward_references(b"abcdabcdabcd");
        let first = parse.commands[0];
        assert_eq!(first.insert_length, 4);
        assert_eq!(first.distance, 4);
        assert_eq!(first.distance_code, 0);
        assert_eq!(first.copy_length, 8);
        assert_eq!(parse.tail_start, 12);
    }

    #[test]
    fn dictionary_match_keeps_reference_length_code() {
        let parse = create_backward_references(b"time and more");
        let first = parse.commands[0];
        assert_eq!(first.insert_length, 0);
        assert_eq!(first.copy_length, 4);
        assert_eq!(first.copy_length_code, 4);
        assert!(first.distance > 0);
    }

    #[test]
    fn incompressible_input_remains_literal_tail() {
        let parse = create_backward_references(b"abcdefghijklmno");
        assert!(parse.commands.is_empty());
        assert_eq!(parse.tail_start, 0);
    }
}
