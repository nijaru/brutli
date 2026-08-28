use std::sync::OnceLock;

use crate::dictionary::{self, MAX_WORD_LENGTH, MIN_WORD_LENGTH};

const HASH_MULTIPLIER: u32 = 0x1e35_a7bd;
const HASH_BITS: usize = 14;
const HASH_BUCKETS: usize = 1 << (HASH_BITS + 1);
const CUTOFF_TRANSFORMS_COUNT: usize = 10;
const CUTOFF_TRANSFORMS: u64 = 0x071b_520a_da2d_3200;

const FROZEN_INDEX: &[u8] = &[
    0, 0, 8, 164, 32, 56, 31, 191, 36, 4,
    128, 81, 68, 132, 145, 129, 0, 0, 0, 28, 0, 8, 1, 1, 64, 3, 1, 0, 0, 0, 0, 0, 4,
    64, 1, 2, 128, 0, 132, 49, 0, 0, 0, 0, 0, 0, 0, 0, 17, 0, 0, 0, 1, 0, 36, 152,
    0, 0, 0, 0, 128, 8, 0, 0, 128, 0, 0, 8, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8,
    0, 0, 0, 1, 0, 64, 133, 0, 32, 0, 0, 128, 1, 0, 0, 0, 0, 4, 4, 4, 32, 16, 130,
    0, 128, 8, 0, 0, 0, 0, 0, 64, 0, 64, 0, 160, 0, 148, 53, 0, 0, 0, 0, 0, 128, 0,
    130, 0, 0, 0, 8, 0, 0, 0, 0, 0, 48, 0, 0, 0, 0, 0, 0, 32, 1, 32, 129, 0, 12, 0,
    1, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 16, 32, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 8,
    0, 0, 2, 0, 0, 0, 0, 0, 32, 0, 0, 0, 2, 66, 128, 0, 0, 16, 0, 0, 0, 0, 64, 1, 6,
    128, 8, 0, 192, 24, 32, 0, 0, 8, 4, 128, 128, 2, 160, 0, 160, 0, 64, 0, 0, 2, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 32, 1, 0, 0, 64, 0, 0, 0, 0, 0, 0, 32, 0, 66, 0, 2, 0,
    4, 0, 8, 0, 2, 0, 0, 33, 8, 0, 0, 0, 8, 0, 128, 162, 4, 128, 0, 2, 33, 0, 160,
    0, 8, 0, 64, 0, 160, 0, 129, 4, 0, 0, 32, 0, 0, 32, 0, 2, 0, 0, 0, 0, 0, 0, 128,
    0, 0, 0, 0, 0, 64, 10, 0, 0, 0, 0, 32, 64, 0, 0, 0, 0, 0, 16, 0, 16, 16, 0, 0,
    80, 2, 0, 0, 0, 0, 8, 0, 0, 16, 0, 8, 0, 0, 0, 8, 64, 128, 0, 0, 0, 8, 208, 0,
    0, 0, 0, 0, 0, 0, 32, 0, 0, 0, 0, 0, 0, 32, 0, 8, 0, 128, 0, 0, 0, 1, 0, 0, 0,
    16, 8, 1, 136, 0, 0, 36, 0, 64, 9, 0, 1, 32, 8, 0, 64, 64, 131, 16, 224, 32, 4,
    0, 4, 5, 160, 0, 131, 0, 4, 96, 0, 0, 184, 192, 0, 177, 205, 96, 0, 0, 0, 0, 2,
    0, 32, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 128, 0, 0, 8, 0, 0, 0, 0, 1, 4, 0, 1,
    0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 4, 0, 0, 64, 69, 0, 0, 8, 2, 66, 32, 64, 0, 0, 0,
    0, 0, 1, 0, 128, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 12, 0, 16, 0, 0, 4, 128, 64,
    0, 0, 0, 0, 0, 0, 0, 0, 224, 0, 8, 0, 0, 130, 16, 64, 128, 2, 64, 0, 0, 0, 128,
    2, 192, 64, 0, 65, 0, 0, 0, 16, 0, 0, 0, 32, 4, 2, 2, 76, 0, 0, 0, 4, 72, 52,
    131, 44, 76, 0, 0, 0, 0, 64, 1, 16, 148, 4, 0, 16, 10, 64, 0, 2, 0, 1, 0, 128,
    64, 68, 0, 0, 0, 0, 0, 64, 144, 0, 8, 0, 2, 0, 0, 0, 0, 0, 0, 3, 64, 0, 0, 0, 0,
    1, 128, 0, 0, 32, 66, 0, 0, 0, 40, 0, 18, 0, 0, 0, 0, 0, 33, 0, 0, 32, 0, 0, 32,
    0, 128, 4, 64, 145, 140, 0, 0, 0, 128, 0, 2, 0, 0, 20, 0, 80, 38, 0, 0, 32, 0,
    32, 64, 4, 4, 0, 4, 0, 0, 0, 129, 4, 0, 0, 144, 17, 32, 130, 16, 132, 24, 134,
    0, 0, 64, 2, 5, 50, 8, 194, 33, 1, 68, 117, 1, 8, 32, 161, 54, 0, 130, 34, 0, 0,
    0, 64, 128, 0, 0, 2, 0, 0, 0, 0, 32, 1, 0, 0, 0, 3, 14, 0, 0, 0, 0, 0, 16, 4, 0,
    0, 0, 0, 0, 0, 0, 0, 96, 1, 24, 18, 0, 1, 128, 24, 0, 64, 0, 4, 0, 16, 128, 0,
    64, 0, 0, 0, 64, 0, 8, 0, 0, 0, 0, 0, 66, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 16, 0, 64, 2, 0, 0, 0, 0, 6, 0, 8, 8, 2, 0, 64, 0, 0, 0, 0, 128, 2,
    2, 12, 64, 0, 64, 0, 8, 0, 128, 32, 0, 0, 10, 0, 0, 32, 0, 128, 32, 33, 8, 136,
    0, 96, 64, 0, 0, 0, 0, 0, 64, 4, 16, 4, 8, 0, 0, 0, 16, 0, 2, 0, 0, 1, 128, 0,
    64, 16, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 2, 0, 16, 0, 4, 0, 8, 0, 0, 0, 0, 0,
    20, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 136, 0, 0, 0, 0, 0, 8, 0,
    0, 0, 0, 0, 2, 0, 0, 0, 64, 0, 0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0,
    0, 0, 4, 0, 0, 0, 0, 65, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 2, 128, 0, 0, 0, 8, 2, 0, 0, 128, 0, 16, 2, 0, 0, 4, 0, 32, 0, 0, 1,
    4, 64, 64, 0, 4, 0, 1, 0, 16, 0, 32, 68, 4, 4, 65, 10, 0, 20, 37, 18, 1, 148, 0,
    32, 128, 3, 8, 0, 64, 0, 0, 0, 0, 0, 0, 4, 0, 16, 1, 128, 0, 0, 0, 128, 16, 0,
    0, 0, 0, 1, 128, 0, 0, 128, 64, 128, 64, 0, 130, 0, 164, 8, 0, 0, 1, 64, 128, 0,
    18, 0, 2, 150, 0, 8, 0, 0, 64, 0, 81, 0, 0, 16, 128, 2, 8, 36, 32, 129, 4, 144,
    13, 0, 0, 3, 8, 1, 0, 2, 0, 0, 64, 0, 5, 0, 1, 34, 1, 32, 2, 16, 128, 128, 128,
    0, 0, 0, 2, 0, 4, 18, 8, 12, 34, 32, 192, 6, 64, 224, 33, 0, 0, 137, 72, 64, 0,
    24, 8, 128, 128, 0, 16, 0, 32, 128, 128, 132, 8, 0, 0, 16, 0, 64, 0, 0, 4, 0, 0,
    16, 0, 4, 128, 64, 0, 0, 1, 0, 4, 64, 32, 144, 130, 2, 128, 0, 192, 0, 64, 82,
    64, 1, 32, 128, 128, 2, 0, 84, 0, 32, 0, 44, 24, 72, 80, 32, 16, 0, 0, 44, 16,
    96, 64, 1, 72, 131, 0, 0, 0, 16, 0, 0, 165, 0, 129, 2, 49, 48, 64, 64, 12, 64,
    176, 64, 84, 8, 128, 20, 64, 213, 136, 104, 1, 41, 15, 83, 170, 0, 0, 41, 1, 64,
    64, 0, 193, 64, 64, 8, 0, 128, 0, 0, 64, 8, 64, 8, 1, 16, 0, 8, 0, 0, 2, 1, 128,
    28, 84, 141, 97, 0, 0, 68, 0, 0, 129, 8, 0, 16, 8, 32, 0, 64, 0, 0, 0, 24, 0, 0,
    0, 192, 0, 8, 128, 0, 0, 0, 0, 0, 64, 0, 1, 0, 0, 0, 0, 40, 1, 128, 64, 0, 4, 2,
    32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 32, 8, 0, 32, 0, 0, 0, 16, 17, 0,
    2, 4, 0, 0, 33, 128, 2, 0, 0, 0, 0, 129, 0, 2, 0, 0, 0, 36, 0, 32, 2, 0, 0, 0,
    0, 0, 0, 32, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 4, 32, 64, 0, 0, 0, 0, 0, 0,
    32, 0, 0, 32, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 16, 0, 0, 0, 0, 0, 0, 0,
    1, 0, 136, 0, 0, 24, 192, 128, 3, 0, 17, 18, 2, 0, 66, 0, 4, 24, 0, 9, 208, 167,
    0, 144, 20, 64, 0, 130, 64, 0, 2, 16, 136, 8, 74, 32, 0, 168, 0, 65, 32, 8, 12,
    1, 3, 1, 64, 180, 3, 0, 64, 0, 8, 0, 0, 32, 65, 0, 4, 16, 4, 16, 68, 32, 64, 36,
    32, 24, 33, 1, 128, 0, 0, 8, 0, 32, 64, 81, 0, 1, 10, 19, 8, 0, 0, 4, 5, 144, 0,
    0, 8, 128, 0, 0, 4, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0, 0, 0, 80, 1, 0, 0,
    33, 0, 32, 66, 4, 2, 0, 1, 43, 2, 0, 0, 4, 32, 16, 0, 64, 0, 3, 32, 0, 2, 64,
    64, 116, 0, 65, 52, 64, 0, 17, 64, 192, 96, 8, 10, 8, 2, 4, 0, 17, 64, 0, 4, 0,
    0, 4, 128, 0, 0, 9, 0, 0, 130, 2, 0, 192, 0, 48, 128, 64, 0, 96, 0, 64, 0, 1,
    16, 32, 0, 1, 32, 6, 128, 2, 32, 0, 12, 0, 0, 48, 32, 8, 0, 0, 128, 0, 18, 0,
    0, 28, 24, 41, 16, 5, 32, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 1, 0, 0, 0, 16, 0, 0, 0, 0, 64, 0, 0, 0, 0, 8, 0, 0, 0, 0, 16, 128,
    0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 33, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DictionaryMatch {
    pub(super) length: usize,
    pub(super) length_code: usize,
    pub(super) distance: usize,
    pub(super) score: usize,
}

#[derive(Debug, Default)]
pub(super) struct DictionarySearch {
    lookups: usize,
    matches: usize,
}

#[derive(Debug)]
struct DictionaryHash {
    words: Box<[u16]>,
    lengths: Box<[u8]>,
}

static DICTIONARY_HASH: OnceLock<DictionaryHash> = OnceLock::new();

impl DictionarySearch {
    pub(super) fn find(
        &mut self,
        input: &[u8],
        position: usize,
        max_length: usize,
        max_backward: usize,
        max_distance: usize,
        min_score: usize,
    ) -> Option<DictionaryMatch> {
        if self.matches < (self.lookups >> 7) || max_length < MIN_WORD_LENGTH {
            return None;
        }

        let table = DICTIONARY_HASH.get_or_init(build_dictionary_hash);
        let mut key = hash14(&input[position..]) << 1;
        let mut best = None;
        for _ in 0..2 {
            self.lookups += 1;
            let length = usize::from(table.lengths[key]);
            if length != 0
                && let Some(candidate) = test_item(
                    input,
                    position,
                    max_length,
                    max_backward,
                    max_distance,
                    min_score,
                    length,
                    usize::from(table.words[key]),
                )
            {
                self.matches += 1;
                if best.is_none_or(|current: DictionaryMatch| candidate.score >= current.score) {
                    best = Some(candidate);
                }
            }
            key += 1;
        }
        best
    }
}

fn test_item(
    input: &[u8],
    position: usize,
    max_length: usize,
    max_backward: usize,
    max_distance: usize,
    min_score: usize,
    word_length: usize,
    word_index: usize,
) -> Option<DictionaryMatch> {
    if word_length > max_length {
        return None;
    }
    let word = dictionary::word(word_length, word_index)?;
    let match_length = input[position..]
        .iter()
        .zip(word)
        .take(word_length)
        .take_while(|(left, right)| left == right)
        .count();
    if match_length == 0 || match_length + CUTOFF_TRANSFORMS_COUNT <= word_length {
        return None;
    }

    let cut = word_length - match_length;
    let transform_id = (cut << 2) + ((CUTOFF_TRANSFORMS >> (cut * 6)) & 0x3f) as usize;
    let distance = max_backward
        .checked_add(1)?
        .checked_add(word_index)?
        .checked_add(transform_id << dictionary::size_bits(word_length))?;
    if distance > max_distance {
        return None;
    }

    let score = super::match_finder::backward_reference_score(match_length, distance);
    (score >= min_score).then_some(DictionaryMatch {
        length: match_length,
        length_code: word_length,
        distance,
        score,
    })
}

fn build_dictionary_hash() -> DictionaryHash {
    assert_eq!(FROZEN_INDEX.len(), 1688, "reference dictionary hash bitset changed");
    let mut words = vec![0_u16; HASH_BUCKETS];
    let mut lengths = vec![0_u8; HASH_BUCKETS];
    let mut global_index = 0_usize;

    for length in (MIN_WORD_LENGTH..=MAX_WORD_LENGTH).rev() {
        let short_bucket = usize::from(length < 8);
        let count = 1_usize << dictionary::size_bits(length);
        for index in 0..count {
            let word_index = count - 1 - index;
            let word = dictionary::word(length, word_index).expect("RFC dictionary word exists");
            let key = hash14(word);
            let slot = (key << 1) + short_bucket;
            if lengths[slot] & 0x80 == 0 {
                let final_entry = FROZEN_INDEX[global_index / 8] & (1 << (global_index % 8)) != 0;
                words[slot] = word_index as u16;
                lengths[slot] = length as u8 | if final_entry { 0x80 } else { 0 };
            }
            global_index += 1;
        }
    }

    assert_eq!(global_index, FROZEN_INDEX.len() * 8);
    for length in &mut lengths {
        *length &= 0x7f;
    }
    DictionaryHash {
        words: words.into_boxed_slice(),
        lengths: lengths.into_boxed_slice(),
    }
}

fn hash14(input: &[u8]) -> usize {
    let value = u32::from_le_bytes(
        input[..4]
            .try_into()
            .expect("static dictionary hash input has four bytes"),
    );
    ((value.wrapping_mul(HASH_MULTIPLIER)) >> (32 - HASH_BITS)) as usize
}

#[cfg(test)]
mod tests {
    use super::{DICTIONARY_HASH, DictionarySearch, build_dictionary_hash};

    #[test]
    fn reference_hash_table_has_expected_shape() {
        let table = DICTIONARY_HASH.get_or_init(build_dictionary_hash);
        assert_eq!(table.words.len(), 32768);
        assert_eq!(table.lengths.len(), 32768);
        assert!(table.lengths.iter().any(|&length| length != 0));
    }

    #[test]
    fn finds_reference_dictionary_word() {
        let input = b"time and more";
        let mut search = DictionarySearch::default();
        let found = search.find(input, 0, input.len(), 0, (1 << 22) - 16, 0);
        assert!(found.is_some());
    }
}
