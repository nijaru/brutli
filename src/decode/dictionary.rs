const DICTIONARY: &[u8; 122_784] = include_bytes!("dictionary.bin");

const SIZE_BITS_BY_LENGTH: [u8; 32] = [
    0, 0, 0, 0, 10, 10, 11, 11, 10, 10, 10, 10, 10, 9, 9, 8, 7, 7, 8, 7, 7, 6, 6, 5, 5, 0, 0, 0, 0,
    0, 0, 0,
];

const OFFSETS_BY_LENGTH: [usize; 32] = [
    0, 0, 0, 0, 0, 4096, 9216, 21504, 35840, 44032, 53248, 63488, 74752, 87040, 93696, 100864,
    104704, 106752, 108928, 113536, 115968, 118528, 119872, 121280, 122016, 122784, 122784, 122784,
    122784, 122784, 122784, 122784,
];

const PREFIX_SUFFIX: &[u8; 217] = b"\x01 \x02, \x08 of the \x04 of \x02s \x01.\x05 and \x04 in \x01\"\x04 to \x02\">\x01\n\x02. \x01]\x05 for \x03 a \x06 that \x01'\x06 with \x06 from \x04 by \x01(\x06. The \x04 on \x04 as \x04 is \x04ing \x02\n\t\x01:\x03ed \x02=\"\x04 at \x03ly \x01,\x02='\x05.com/\x07. This \x05 not \x03er \x03al \x04ful \x04ive \x05less \x04est \x04ize \x02\xc2\xa0\x04ous \x05 the \x02e \x00";

const PREFIX_SUFFIX_MAP: [u16; 50] = [
    0, 2, 5, 14, 19, 22, 24, 30, 35, 37, 42, 45, 47, 50, 52, 58, 62, 69, 71, 78, 85, 90, 92, 99,
    104, 109, 114, 119, 122, 124, 128, 131, 136, 140, 142, 145, 151, 159, 165, 169, 173, 178, 183,
    189, 194, 199, 202, 207, 213, 216,
];

// RFC 7932 transforms, stored as [prefix_id, transform_type, suffix_id].
const TRANSFORMS: [u8; 363] = [
    49, 0, 49, 49, 0, 0, 0, 0, 0, 49, 12, 49, 49, 10, 0, 49, 0, 47, 0, 0, 49, 4, 0, 0, 49, 0, 3,
    49, 10, 49, 49, 0, 6, 49, 13, 49, 49, 1, 49, 1, 0, 0, 49, 0, 1, 0, 10, 0, 49, 0, 7, 49, 0, 9,
    48, 0, 0, 49, 0, 8, 49, 0, 5, 49, 0, 10, 49, 0, 11, 49, 3, 49, 49, 0, 13, 49, 0, 14, 49, 14,
    49, 49, 2, 49, 49, 0, 15, 49, 0, 16, 0, 10, 49, 49, 0, 12, 5, 0, 49, 0, 0, 1, 49, 15, 49, 49,
    0, 18, 49, 0, 17, 49, 0, 19, 49, 0, 20, 49, 16, 49, 49, 17, 49, 47, 0, 49, 49, 4, 49, 49, 0,
    22, 49, 11, 49, 49, 0, 23, 49, 0, 24, 49, 0, 25, 49, 7, 49, 49, 1, 26, 49, 0, 27, 49, 0, 28, 0,
    0, 12, 49, 0, 29, 49, 20, 49, 49, 18, 49, 49, 6, 49, 49, 0, 21, 49, 10, 1, 49, 8, 49, 49, 0,
    31, 49, 0, 32, 47, 0, 3, 49, 5, 49, 49, 9, 49, 0, 10, 1, 49, 10, 8, 5, 0, 21, 49, 11, 0, 49,
    10, 10, 49, 0, 30, 0, 0, 5, 35, 0, 49, 47, 0, 2, 49, 10, 17, 49, 0, 36, 49, 0, 33, 5, 0, 0, 49,
    10, 21, 49, 10, 5, 49, 0, 37, 0, 0, 30, 49, 0, 38, 0, 11, 0, 49, 0, 39, 0, 11, 49, 49, 0, 34,
    49, 11, 8, 49, 10, 12, 0, 0, 21, 49, 0, 40, 0, 10, 12, 49, 0, 41, 49, 0, 42, 49, 11, 17, 49, 0,
    43, 0, 10, 5, 49, 11, 10, 0, 0, 34, 49, 10, 33, 49, 0, 44, 49, 11, 5, 45, 0, 49, 0, 0, 33, 49,
    10, 30, 49, 11, 30, 49, 0, 46, 49, 11, 1, 49, 10, 34, 0, 10, 33, 0, 11, 30, 0, 11, 1, 49, 11,
    33, 49, 11, 21, 49, 11, 12, 0, 11, 5, 49, 11, 34, 0, 11, 12, 0, 10, 30, 0, 11, 34, 0, 10, 34,
];

const TRANSFORM_COUNT: usize = TRANSFORMS.len() / 3;
const IDENTITY: u8 = 0;
const UPPERCASE_FIRST: u8 = 10;
const UPPERCASE_ALL: u8 = 11;
const OMIT_FIRST_BASE: u8 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DictionaryError {
    CopyLength,
    Distance,
    Transform,
    Word,
}

pub(super) fn transform(
    distance: usize,
    copy_length: usize,
    max_backward_distance: usize,
) -> Result<Vec<u8>, DictionaryError> {
    let shift = *SIZE_BITS_BY_LENGTH
        .get(copy_length)
        .filter(|&&shift| shift != 0)
        .ok_or(DictionaryError::CopyLength)?;

    let address = distance
        .checked_sub(max_backward_distance)
        .and_then(|distance| distance.checked_sub(1))
        .ok_or(DictionaryError::Distance)?;
    let word_mask = (1_usize << shift) - 1;
    let word_index = address & word_mask;
    let transform_index = address >> shift;
    if transform_index >= TRANSFORM_COUNT {
        return Err(DictionaryError::Transform);
    }

    let offset = OFFSETS_BY_LENGTH[copy_length]
        .checked_add(
            word_index
                .checked_mul(copy_length)
                .ok_or(DictionaryError::Word)?,
        )
        .ok_or(DictionaryError::Word)?;
    let end = offset
        .checked_add(copy_length)
        .ok_or(DictionaryError::Word)?;
    let word = DICTIONARY.get(offset..end).ok_or(DictionaryError::Word)?;

    transform_word(word, transform_index)
}

fn transform_word(word: &[u8], transform_index: usize) -> Result<Vec<u8>, DictionaryError> {
    let base = transform_index * 3;
    let prefix = affix(TRANSFORMS[base])?;
    let transform_type = TRANSFORMS[base + 1];
    let suffix = affix(TRANSFORMS[base + 2])?;

    let (start, end) = match transform_type {
        1..=9 => (0, word.len().saturating_sub(usize::from(transform_type))),
        12..=20 => {
            let omitted = usize::from(transform_type - OMIT_FIRST_BASE);
            (omitted.min(word.len()), word.len())
        }
        IDENTITY | UPPERCASE_FIRST | UPPERCASE_ALL => (0, word.len()),
        _ => return Err(DictionaryError::Transform),
    };

    let transformed_word = &word[start..end];
    let mut output = Vec::with_capacity(prefix.len() + transformed_word.len() + suffix.len());
    output.extend_from_slice(prefix);
    let word_start = output.len();
    output.extend_from_slice(transformed_word);
    let word_end = output.len();

    match transform_type {
        UPPERCASE_FIRST if word_start != word_end => {
            uppercase_at(&mut output[word_start..word_end], 0);
        }
        UPPERCASE_ALL => {
            let mut offset = 0;
            while offset < word_end - word_start {
                offset += uppercase_at(&mut output[word_start..word_end], offset);
            }
        }
        _ => {}
    }

    output.extend_from_slice(suffix);
    Ok(output)
}

fn affix(id: u8) -> Result<&'static [u8], DictionaryError> {
    let start = usize::from(
        *PREFIX_SUFFIX_MAP
            .get(usize::from(id))
            .ok_or(DictionaryError::Transform)?,
    );
    let length = usize::from(*PREFIX_SUFFIX.get(start).ok_or(DictionaryError::Transform)?);
    PREFIX_SUFFIX
        .get(start + 1..start + 1 + length)
        .ok_or(DictionaryError::Transform)
}

fn uppercase_at(word: &mut [u8], offset: usize) -> usize {
    let first = word[offset];
    if first < 0xc0 {
        if first.is_ascii_lowercase() {
            word[offset] ^= 32;
        }
        1
    } else if first < 0xe0 {
        if offset + 1 < word.len() {
            word[offset + 1] ^= 32;
            2
        } else {
            1
        }
    } else if offset + 2 < word.len() {
        word[offset + 2] ^= 5;
        3
    } else {
        word.len() - offset
    }
}

#[cfg(test)]
mod tests {
    use super::{DictionaryError, transform};

    fn distance(word_index: usize, transform_index: usize, max_backward: usize) -> usize {
        max_backward + 1 + (transform_index << 10) + word_index
    }

    #[test]
    fn decodes_first_length_four_word() {
        assert_eq!(transform(1, 4, 0).unwrap(), b"time");
    }

    #[test]
    fn applies_prefix_suffix_and_case_transforms() {
        assert_eq!(transform(distance(0, 1, 0), 4, 0).unwrap(), b"time ");
        assert_eq!(transform(distance(0, 2, 0), 4, 0).unwrap(), b" time ");
        assert_eq!(transform(distance(0, 4, 0), 4, 0).unwrap(), b"Time ");
        assert_eq!(transform(distance(0, 44, 0), 4, 0).unwrap(), b"TIME");
    }

    #[test]
    fn applies_omit_transforms() {
        assert_eq!(transform(distance(0, 3, 0), 4, 0).unwrap(), b"ime");
        assert_eq!(transform(distance(0, 12, 0), 4, 0).unwrap(), b"tim");
    }

    #[test]
    fn address_is_relative_to_current_backward_distance() {
        assert_eq!(transform(1009, 4, 1008).unwrap(), b"time");
    }

    #[test]
    fn rejects_invalid_dictionary_addresses() {
        assert_eq!(transform(1, 3, 0), Err(DictionaryError::CopyLength));
        assert_eq!(transform(4, 4, 4), Err(DictionaryError::Distance));
        assert_eq!(
            transform(distance(0, 121, 0), 4, 0),
            Err(DictionaryError::Transform)
        );
    }
}
