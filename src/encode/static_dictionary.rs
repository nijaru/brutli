#![expect(
    clippy::explicit_counter_loop,
    clippy::too_many_arguments,
    reason = "reference-derived static dictionary search mirrors upstream structure"
)]

// The static dictionary search is reference-derived and contains a frozen generated table.
// Keep that token stream outside rustfmt while still compiling and linting it normally.
include!("static_dictionary.in");

#[derive(Debug)]
pub(super) struct CachedDictionarySearch {
    inner: DictionarySearch,
    table: &'static DictionaryHash,
}

impl Default for CachedDictionarySearch {
    fn default() -> Self {
        Self {
            inner: DictionarySearch::default(),
            table: DICTIONARY_HASH.get_or_init(build_dictionary_hash),
        }
    }
}

impl CachedDictionarySearch {
    pub(super) fn find(
        &mut self,
        input: &[u8],
        position: usize,
        max_length: usize,
        max_backward: usize,
        max_distance: usize,
        min_score: usize,
    ) -> Option<DictionaryMatch> {
        if self.inner.matches < (self.inner.lookups >> 7) || max_length < MIN_WORD_LENGTH {
            return None;
        }

        let mut key = hash14(&input[position..]) << 1;
        let mut best = None;
        for _ in 0..2 {
            self.inner.lookups += 1;
            let length = usize::from(self.table.lengths[key]);
            if length != 0
                && let Some(candidate) = test_item(
                    input,
                    position,
                    max_length,
                    max_backward,
                    max_distance,
                    min_score,
                    length,
                    usize::from(self.table.words[key]),
                )
            {
                self.inner.matches += 1;
                if best.is_none_or(|current: DictionaryMatch| candidate.score >= current.score) {
                    best = Some(candidate);
                }
            }
            key += 1;
        }
        best
    }
}
