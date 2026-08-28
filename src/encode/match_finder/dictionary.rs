use std::sync::OnceLock;

const DICTIONARY: &[u8; 122_784] = include_bytes!("../../decode/dictionary.bin");
const SIZE_BITS_BY_LENGTH: [u8; 32] = [
    0, 0, 0, 0, 10, 10, 11, 11, 10, 10, 10, 10, 10, 9, 9, 8, 7, 7, 8, 7, 7, 6, 6, 5, 5, 0, 0, 0, 0,
    0, 0, 0,
];
const OFFSETS_BY_LENGTH: [usize; 32] = [
    0, 0, 0, 0, 0, 4096, 9216, 21504, 35840, 44032, 53248, 63488, 74752, 87040, 93696, 100864,
    104704, 106752, 108928, 113536, 115968, 118528, 119872, 121280, 122016, 122784, 122784, 122784,
    122784, 122784, 122784, 122784,
];
const HASH_BITS: usize = 16;
const HASH_SIZE: usize = 1 << HASH_BITS;
const NONE: u16 = u16::MAX;

static INDEX: OnceLock<DictionaryIndex> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DictionaryMatch {
    pub(super) length: usize,
    pub(super) distance: usize,
}

#[derive(Debug)]
struct DictionaryIndex {
    heads: Vec<u16>,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    word_index: u16,
    next: u16,
    length: u8,
}

pub(super) fn best_identity_match(
    input: &[u8],
    position: usize,
    max_backward_distance: usize,
) -> Option<DictionaryMatch> {
    if position + 4 > input.len() {
        return None;
    }

    let index = INDEX.get_or_init(build_index);
    let remaining = &input[position..];
    let mut current = index.heads[hash4(remaining)];
    let mut best = None;

    while current != NONE {
        let entry = index.entries[usize::from(current)];
        let length = usize::from(entry.length);
        let word_index = usize::from(entry.word_index);
        if length <= remaining.len()
            && remaining[..length] == *dictionary_word(length, word_index)
            && best.is_none_or(|matched: DictionaryMatch| length > matched.length)
        {
            best = Some(DictionaryMatch {
                length,
                distance: max_backward_distance + 1 + word_index,
            });
        }
        current = entry.next;
    }

    best
}

fn build_index() -> DictionaryIndex {
    let capacity = SIZE_BITS_BY_LENGTH
        .iter()
        .map(|&bits| if bits == 0 { 0 } else { 1_usize << bits })
        .sum();
    let mut heads = vec![NONE; HASH_SIZE];
    let mut entries = Vec::with_capacity(capacity);

    for (length, &bits) in SIZE_BITS_BY_LENGTH.iter().enumerate().take(25).skip(4) {
        if bits == 0 {
            continue;
        }

        for word_index in 0..1_usize << bits {
            let word = dictionary_word(length, word_index);
            let hash = hash4(word);
            let entry_index = u16::try_from(entries.len())
                .expect("Brotli static dictionary identity index fits in u16");
            entries.push(Entry {
                word_index: word_index as u16,
                next: heads[hash],
                length: length as u8,
            });
            heads[hash] = entry_index;
        }
    }

    DictionaryIndex { heads, entries }
}

fn dictionary_word(length: usize, word_index: usize) -> &'static [u8] {
    let offset = OFFSETS_BY_LENGTH[length] + word_index * length;
    &DICTIONARY[offset..offset + length]
}

fn hash4(bytes: &[u8]) -> usize {
    let value = u32::from_le_bytes(
        bytes[..4]
            .try_into()
            .expect("dictionary hash input has four bytes"),
    );
    ((value.wrapping_mul(0x1e35_a7bd)) >> (32 - HASH_BITS)) as usize
}

#[cfg(test)]
mod tests {
    use super::best_identity_match;

    #[test]
    fn finds_first_identity_dictionary_word() {
        let matched = best_identity_match(b"timeXYZQ", 0, 0).unwrap();
        assert_eq!(matched.length, 4);
        assert_eq!(matched.distance, 1);
    }

    #[test]
    fn dictionary_address_tracks_current_backward_limit() {
        let matched = best_identity_match(b"xxxtimeXYZQ", 3, 3).unwrap();
        assert_eq!(matched.length, 4);
        assert_eq!(matched.distance, 4);
    }
}
