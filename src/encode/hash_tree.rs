//! The H10 binary-tree hash chain used by the Zopfli qualities (10 and 11):
//! an idiomatic port of upstream `hash_to_binary_tree_inc.h`.
//!
//! Each hash bucket holds a binary tree of window positions whose first 4
//! bytes share a hash: the trees are sorted lexicographically by the
//! sequence at each position and re-rooted at every stored position. The
//! structure is also a max-heap on position (newer positions sit closer to
//! the root), which bounds tree depth by the window and makes in-order match
//! retrieval cheap: `find_all_matches` returns matches sorted by strictly
//! increasing length and non-strictly increasing distance, exactly as
//! upstream requires.
//!
//! Positions are absolute stream positions within one chunk; children are
//! stored at `2 * (position & window_mask)` in `forest`, so the structure
//! forgets entries older than the window automatically (upstream's
//! "forgetful" chain). Streaming uses a fresh tree per 16 MiB chunk with
//! chunk-relative positions (upstream stitches across blocks; brutli's
//! uniform chunking keeps streaming byte-identical to one-shot instead).

// Landed and tested ahead of its only consumer — the Zopfli shortest-path
// parse, which wires this module to the quality 10/11 paths in the next
// slice. This allowance goes away with that wiring.
#![allow(dead_code)]

use super::static_dictionary::DictionarySearch;
use crate::dictionary::MAX_WORD_LENGTH;

/// Upstream H10 parameters: BUCKET_BITS 17, MAX_TREE_SEARCH_DEPTH 64,
/// MAX_TREE_COMP_LENGTH 128.
const BUCKET_BITS: u32 = 17;
const BUCKET_COUNT: usize = 1 << BUCKET_BITS;
const MAX_TREE_SEARCH_DEPTH: usize = 64;
const MAX_TREE_COMP_LENGTH: usize = 128;

/// The longest sequence the tree compares (upstream `StoreLookahead`).
const STORE_LOOKAHEAD: usize = MAX_TREE_COMP_LENGTH;

/// Upstream `kHashMul32`.
const HASH_MULTIPLIER: u32 = 0x1e35_a7bd;

/// Upstream `BROTLI_MAX_STATIC_DICTIONARY_MATCH_LEN`.
const MAX_DICTIONARY_MATCH_LENGTH: usize = 37;

/// Sentinel for a nonexistent tree node: `0 - window_mask` in upstream's
/// position space, chosen so that `position - sentinel` always exceeds the
/// backward limit for any in-window position.
const INVALID_POSITION: u32 = u32::MAX;

/// A found backward reference (upstream `BackwardMatch`: distance and
/// length; dictionary matches carry a distinct length code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TreeMatch {
    pub(super) distance: usize,
    pub(super) length: usize,
    /// Zero for LZ77 matches; dictionary matches carry their word-length
    /// code (upstream `length_and_code & 31`).
    pub(super) length_code: usize,
}

impl TreeMatch {
    const fn lz77(distance: usize, length: usize) -> Self {
        Self {
            distance,
            length,
            length_code: 0,
        }
    }

    const fn dictionary(distance: usize, length: usize, length_code: usize) -> Self {
        Self {
            distance,
            length,
            length_code,
        }
    }

    /// Upstream `BackwardMatchLengthCode`: the dictionary length code when
    /// present, otherwise the match length itself.
    pub(super) const fn effective_length_code(self) -> usize {
        if self.length_code != 0 {
            self.length_code
        } else {
            self.length
        }
    }
}

/// The binary-tree hasher (upstream `HashToBinaryTree`).
#[derive(Debug)]
pub(super) struct HashTree {
    /// Bucket roots: the most recent position with each 4-byte hash.
    buckets: Vec<u32>,
    /// Left/right children of each position, indexed `2 * (pos & mask)`.
    forest: Box<[u32]>,
    window_mask: usize,
    dictionary: DictionarySearch,
    /// A valid-but-unused root marker for freshly initialized buckets (see
    /// [`INVALID_POSITION`]).
    invalid_root: u32,
}

impl HashTree {
    pub(super) fn new(window_bits: u8) -> Self {
        let window_mask = (1_usize << window_bits) - 1;
        // Roots start invalid: no position hashed yet. The root sentinel is
        // distance `window_mask + 1` "behind zero" so any real position
        // reads as out-of-window; store it as a wrapped u32.
        let invalid_root = (0_u32).wrapping_sub(window_mask as u32);
        Self {
            buckets: vec![invalid_root; BUCKET_COUNT],
            forest: vec![0_u32; 2 * window_mask + 2].into_boxed_slice(),
            window_mask,
            dictionary: DictionarySearch::default(),
            invalid_root,
        }
    }

    fn hash4(input: &[u8], position: usize) -> usize {
        let value = u32::from_le_bytes(
            input[position..position + 4]
                .try_into()
                .expect("tree hash reads four bytes"),
        );
        value.wrapping_mul(HASH_MULTIPLIER) as usize >> (32 - BUCKET_BITS)
    }

    fn left_index(&self, position: usize) -> usize {
        2 * (position & self.window_mask)
    }

    fn right_index(&self, position: usize) -> usize {
        2 * (position & self.window_mask) + 1
    }

    /// Stores the hash of the 4 bytes at `position` and finds every distinct
    /// match length in the bucket's tree, re-rooting the tree at `position`
    /// (upstream `StoreAndFindMatches`). Must be called with increasing
    /// positions. `max_length` is the bytes available from `position`; when
    /// fewer than a full comparison window remain, the bucket is searched
    /// without re-rooting (the final sort order of an incomplete sequence
    /// is unknowable).
    ///
    /// Returns the appended matches; `best_len` starts at the caller's
    /// current best so only strictly longer matches are recorded.
    fn store_and_find(
        &mut self,
        input: &[u8],
        position: usize,
        max_length: usize,
        max_backward: usize,
        best_len: &mut usize,
        matches: &mut Vec<TreeMatch>,
    ) {
        let key = Self::hash4(input, position);
        let should_reroot = max_length >= MAX_TREE_COMP_LENGTH;
        let mut prev = self.buckets[key] as usize;
        let mut node_left = self.left_index(position);
        let mut node_right = self.right_index(position);
        let mut best_len_left = 0_usize;
        let mut best_len_right = 0_usize;
        let invalid = self.invalid_root;

        if should_reroot {
            self.buckets[key] = position as u32;
        }

        let mut depth_remaining = MAX_TREE_SEARCH_DEPTH;
        loop {
            // A tree entry always points strictly backwards; anything else
            // is the invalid sentinel (or a wrapped position outside the
            // window), which ends the walk exactly as upstream's backward
            // check does.
            let Some(backward) = position.checked_sub(prev) else {
                if should_reroot {
                    self.forest[node_left] = invalid;
                    self.forest[node_right] = invalid;
                }
                break;
            };
            if depth_remaining == 0 || backward == 0 || backward > max_backward {
                if should_reroot {
                    self.forest[node_left] = invalid;
                    self.forest[node_right] = invalid;
                }
                break;
            }

            let current = best_len_left.min(best_len_right);
            let max_comp_len = max_length.min(MAX_TREE_COMP_LENGTH);
            let len = current
                + match_length(
                    input,
                    prev + current,
                    position + current,
                    max_length - current,
                );
            if len > *best_len {
                *best_len = len;
                matches.push(TreeMatch::lz77(backward, len));
            }
            if len >= max_comp_len {
                if should_reroot {
                    self.forest[node_left] = self.forest[self.left_index(prev)];
                    self.forest[node_right] = self.forest[self.right_index(prev)];
                }
                break;
            }
            if input[position + len] > input[prev + len] {
                best_len_left = len;
                if should_reroot {
                    self.forest[node_left] = prev as u32;
                }
                node_left = self.right_index(prev);
                prev = self.forest[node_left] as usize;
            } else {
                best_len_right = len;
                if should_reroot {
                    self.forest[node_right] = prev as u32;
                }
                node_right = self.left_index(prev);
                prev = self.forest[node_right] as usize;
            }
            depth_remaining -= 1;
        }
    }

    /// Finds all backward matches at `position` (upstream `FindAllMatches`
    /// for H10): the short-match scan for lengths 2..=3 within 16 (64 for
    /// the HQ quality) bytes back, the tree matches, and the static
    /// dictionary through the full transform space. Returns matches sorted
    /// by strictly increasing length and non-strictly increasing distance.
    pub(super) fn find_all_matches(
        &mut self,
        input: &[u8],
        position: usize,
        max_length: usize,
        max_backward: usize,
        max_distance: usize,
        short_match_max_backward: usize,
    ) -> Vec<TreeMatch> {
        let mut matches = Vec::new();
        let mut best_len = 1_usize;

        // Short matches: scan backwards for 2..=3-byte matches close to the
        // current position (upstream's best_len <= 2 loop).
        let stop = position.saturating_sub(short_match_max_backward);
        let mut scan = position;
        while scan > stop && best_len <= 2 {
            scan -= 1;
            let backward = position - scan;
            if backward > max_backward {
                break;
            }
            if input[position] != input[scan] || input[position + 1] != input[scan + 1] {
                continue;
            }
            let len = match_length(input, scan, position, max_length);
            if len > best_len {
                best_len = len;
                matches.push(TreeMatch::lz77(backward, len));
            }
        }

        // Tree matches, if any comparison window remains.
        if best_len < max_length && position + 4 <= input.len() {
            self.store_and_find(
                input,
                position,
                max_length,
                max_backward,
                &mut best_len,
                &mut matches,
            );
        }

        // Static dictionary through the full transform space. Upstream
        // passes minlen = max(4, best_len + 1): both as the omit-last ladder
        // bound and as the collection filter, so dictionary matches never
        // shadow the tree's longer matches or duplicate its lengths.
        if best_len < max_length {
            let minlen = (best_len + 1).max(4);
            let found = self.dictionary.find_all(
                input,
                position,
                minlen,
                max_length,
                max_backward,
                max_distance,
            );
            matches.extend(
                found
                    .into_iter()
                    .filter(|m| m.length >= minlen)
                    .map(|m| TreeMatch::dictionary(m.distance, m.length, m.length_code)),
            );
        }

        matches
    }

    /// Stores the hash at `position` without returning matches (upstream
    /// `Store`): used for copy tails and skipped spans. The caller must
    /// guarantee `MAX_TREE_COMP_LENGTH` readable bytes at `position`
    /// (upstream's `store_end` discipline), since the tree must compare the
    /// full sequence window to know its final sort position.
    pub(super) fn store(&mut self, input: &[u8], position: usize, max_backward: usize) {
        debug_assert!(
            position + MAX_TREE_COMP_LENGTH <= input.len(),
            "tree stores need a full comparison window"
        );
        let mut best_len = usize::MAX;
        self.store_and_find(
            input,
            position,
            MAX_TREE_COMP_LENGTH,
            max_backward,
            &mut best_len,
            &mut Vec::new(),
        );
    }

    /// Stores positions `[start, end)` (upstream `StoreRange`, with its
    /// stride-8 fast path for long ranges).
    pub(super) fn store_range(
        &mut self,
        input: &[u8],
        start: usize,
        end: usize,
        max_backward: usize,
    ) {
        if start + 63 <= end {
            let mut cursor = start;
            let fast_end = end - 63;
            while cursor < fast_end {
                self.store(input, cursor, max_backward);
                cursor += 8;
            }
            for position in (end - 63).max(start)..end {
                self.store(input, position, max_backward);
            }
        } else {
            for position in start..end {
                self.store(input, position, max_backward);
            }
        }
    }
}

/// Matching bytes at two positions, capped at `limit`, 8-byte steps with a
/// byte tail (upstream `FindMatchLengthWithLimit`; `match_finder` keeps a
/// private copy for the q5 path).
fn match_length(input: &[u8], a: usize, b: usize, limit: usize) -> usize {
    let mut length = 0_usize;
    while length + 8 <= limit && a + length + 8 <= input.len() && b + length + 8 <= input.len() {
        let difference = u64::from_le_bytes(input[a + length..a + length + 8].try_into().unwrap())
            ^ u64::from_le_bytes(input[b + length..b + length + 8].try_into().unwrap());
        if difference != 0 {
            return length + (difference.trailing_zeros() >> 3) as usize;
        }
        length += 8;
    }
    while length < limit && a + length < input.len() && b + length < input.len() {
        if input[a + length] != input[b + length] {
            return length;
        }
        length += 1;
    }
    length
}

/// Longest word length usable by the dictionary ladder (module-private
/// parity check with upstream `BROTLI_MAX_STATIC_DICTIONARY_MATCH_LEN`).
const _: () = assert!(MAX_DICTIONARY_MATCH_LENGTH == 37);
const _: () = assert!(MAX_WORD_LENGTH == 24);

#[cfg(test)]
mod tests {
    use super::{HashTree, MAX_TREE_COMP_LENGTH, TreeMatch};

    fn window_bits() -> u8 {
        16
    }

    /// A test input padded so `store_range` has a full comparison window at
    /// every stored position (the upstream `store_end` discipline).
    fn padded(body: &[u8]) -> Vec<u8> {
        let mut input = body.to_vec();
        input.resize(body.len() + MAX_TREE_COMP_LENGTH - 1, 0);
        input
    }

    #[test]
    fn tree_finds_matches_in_increasing_length_order() {
        // "alpha beta " repeats: the tree must report the 11-byte match and
        // any shorter prefixes in strictly increasing length order.
        let input = padded(b"alpha beta gamma alpha beta delta");
        let mut tree = HashTree::new(window_bits());
        tree.store_range(&input, 0, 17, (1 << 16) - 16);

        let matches = tree.find_all_matches(
            &input,
            17,
            input.len() - 17,
            (1 << 16) - 16, // the WBITS 16 backward limit
            usize::MAX,
            16,
        );
        assert!(!matches.is_empty(), "expected a match at the repeat");
        for pair in matches.windows(2) {
            assert!(
                pair[0].length < pair[1].length,
                "matches must be strictly increasing in length: {matches:?}"
            );
        }
        assert!(matches.iter().any(|m| m.length == 11 && m.distance == 17));
    }

    #[test]
    fn tree_matches_respect_backward_limit() {
        let input = padded(b"alpha beta gamma alpha beta delta");
        let mut tree = HashTree::new(window_bits());
        tree.store_range(&input, 0, 17, (1 << 16) - 16);

        // The 11-byte match sits at distance 17: within the WBITS 16
        // window it must appear; beyond a 4-byte window it must not.
        // Dictionary matches are suppressed to isolate the LZ77 check.
        let far = tree.find_all_matches(&input, 17, input.len() - 17, (1 << 16) - 16, 0, 0);
        assert!(
            far.iter().any(|m| m.distance == 17),
            "the distance-17 match must appear within the window"
        );

        let near = tree.find_all_matches(
            &input,
            17,
            input.len() - 17,
            4, // window smaller than the distance to the first "alpha"
            0,
            0,
        );
        assert!(
            near.iter().all(|m| m.distance <= 4),
            "distances must respect the backward limit: {near:?}"
        );
        assert!(!near.iter().any(|m| m.length == 11));
    }

    #[test]
    fn tree_finds_dictionary_matches() {
        // "time and more" starts with the dictionary word "time"; the
        // dictionary ladder must surface a length-4 match with a wire
        // distance beyond the LZ77 window.
        let max_backward = (1_usize << 22) - 16;
        let input = padded(b"time and more of it. ");
        let mut tree = HashTree::new(window_bits());
        let matches = tree.find_all_matches(
            &input,
            0,
            input.len(),
            max_backward, // no LZ77 history: only dictionary matches appear
            0x3ff_fffc,
            0,
        );
        let dict: Vec<&TreeMatch> = matches
            .iter()
            .filter(|m| m.distance > max_backward)
            .collect();
        assert!(
            dict.iter().any(|m| m.length == 4),
            "expected the dictionary word 'time': {matches:?}"
        );
        for m in dict {
            assert!(m.length_code != 0, "dictionary matches carry a length code");
            assert!(m.distance <= 0x3ff_fffc);
        }
    }
}
