use super::{DIRECT_DISTANCE_CODES, command::ExplicitCommand, distance::DistanceCode};

const TABLE_BITS: usize = 16;
const TABLE_SIZE: usize = 1 << TABLE_BITS;
const BUCKET_SIZE: usize = 2;
const MIN_MATCH: usize = 4;
const MAX_LAZY_MATCH: usize = 16;
const MATCH_WORD_BYTES: usize = 8;
const MAX_BACKWARD_DISTANCE: usize = (1 << 22) - 16;
const LITERAL_BIT_ESTIMATE: isize = 8;
const EMPTY: u32 = u32::MAX;

type MatchBucket = [u32; BUCKET_SIZE];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MatchCommand {
    pub(super) insert_start: usize,
    pub(super) insert_length: usize,
    pub(super) copy_length: usize,
    pub(super) distance: usize,
}

#[derive(Debug, Default)]
pub(super) struct Parse {
    pub(super) commands: Vec<MatchCommand>,
    pub(super) tail_start: usize,
}

pub(super) fn greedy_parse(input: &[u8]) -> Parse {
    assert!(
        u32::try_from(input.len()).is_ok(),
        "match input exceeds u32 position range"
    );

    if input.len() < MIN_MATCH * 2 {
        return Parse {
            commands: Vec::new(),
            tail_start: 0,
        };
    }

    let mut table = vec![[EMPTY; BUCKET_SIZE]; TABLE_SIZE];
    let mut cursors = vec![0_u8; TABLE_SIZE];
    let mut commands = Vec::new();
    let mut position = 0_usize;
    let mut literal_start = 0_usize;

    while position + MIN_MATCH <= input.len() {
        let key = hash4(input, position);
        let candidates = table[key];
        insert_position(&mut table[key], &mut cursors[key], position as u32);

        let Some((previous_position, match_length)) = best_match(input, position, &candidates)
        else {
            position += 1;
            continue;
        };

        if match_length <= MAX_LAZY_MATCH
            && should_defer_match(
                input,
                position,
                literal_start,
                previous_position,
                match_length,
                &table,
            )
        {
            position += 1;
            continue;
        }

        commands.push(MatchCommand {
            insert_start: literal_start,
            insert_length: position - literal_start,
            copy_length: match_length,
            distance: position - previous_position,
        });

        let end = position + match_length;
        for skipped in position + 1..end {
            if skipped + MIN_MATCH <= input.len() {
                let key = hash4(input, skipped);
                insert_position(&mut table[key], &mut cursors[key], skipped as u32);
            }
        }
        position = end;
        literal_start = end;
    }

    Parse {
        commands,
        tail_start: literal_start,
    }
}

fn should_defer_match(
    input: &[u8],
    position: usize,
    literal_start: usize,
    previous_position: usize,
    match_length: usize,
    table: &[MatchBucket],
) -> bool {
    let next_position = position + 1;
    if next_position + MIN_MATCH > input.len() {
        return false;
    }

    let next_candidates = table[hash4(input, next_position)];
    let Some((next_previous, next_length)) = best_match(input, next_position, &next_candidates)
    else {
        return false;
    };

    let insert_length = position - literal_start;
    let current_gain =
        estimated_match_gain(insert_length, match_length, position - previous_position);
    let next_gain = estimated_match_gain(
        insert_length + 1,
        next_length,
        next_position - next_previous,
    ) - LITERAL_BIT_ESTIMATE;
    next_gain > current_gain
}

fn estimated_match_gain(insert_length: usize, copy_length: usize, distance: usize) -> isize {
    let command = ExplicitCommand::for_lengths(insert_length, copy_length);
    let distance = DistanceCode::for_distance(distance, DIRECT_DISTANCE_CODES);
    let copied_literal_bits = copy_length.saturating_mul(8) as isize;
    let extra_bits =
        isize::from(command.extra_bit_count()) + isize::from(distance.extra_bit_count());
    copied_literal_bits - extra_bits
}

fn best_match(input: &[u8], position: usize, candidates: &MatchBucket) -> Option<(usize, usize)> {
    let mut best_position = 0_usize;
    let mut best_length = 0_usize;

    for &previous in candidates {
        if previous == EMPTY {
            continue;
        }
        let previous_position = previous as usize;
        if position - previous_position > MAX_BACKWARD_DISTANCE
            || input[previous_position..previous_position + MIN_MATCH]
                != input[position..position + MIN_MATCH]
        {
            continue;
        }

        let match_length = extend_match(input, previous_position, position);
        if match_length > best_length
            || (match_length == best_length && previous_position > best_position)
        {
            best_position = previous_position;
            best_length = match_length;
        }
    }

    (best_length >= MIN_MATCH).then_some((best_position, best_length))
}

fn insert_position(bucket: &mut MatchBucket, cursor: &mut u8, position: u32) {
    bucket[usize::from(*cursor)] = position;
    *cursor = (*cursor + 1) & (BUCKET_SIZE as u8 - 1);
}

fn hash4(input: &[u8], position: usize) -> usize {
    let value = u32::from_le_bytes(
        input[position..position + 4]
            .try_into()
            .expect("hash input has four bytes"),
    );
    ((value.wrapping_mul(0x1e35_a7bd)) >> (32 - TABLE_BITS)) as usize
}

fn extend_match(input: &[u8], previous: usize, current: usize) -> usize {
    let mut length = MIN_MATCH;
    let limit = input.len() - current;

    while length + MATCH_WORD_BYTES <= limit {
        let previous_word = u64::from_ne_bytes(
            input[previous + length..previous + length + MATCH_WORD_BYTES]
                .try_into()
                .expect("match word has eight bytes"),
        );
        let current_word = u64::from_ne_bytes(
            input[current + length..current + length + MATCH_WORD_BYTES]
                .try_into()
                .expect("match word has eight bytes"),
        );
        if previous_word != current_word {
            break;
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
        EMPTY, best_match, estimated_match_gain, extend_match, greedy_parse, insert_position,
    };

    #[test]
    fn finds_repeated_phrase() {
        let source = b"prefix-prefix-suffix";
        let parse = greedy_parse(source);
        assert!(!parse.commands.is_empty());

        let first = parse.commands[0];
        assert_eq!(first.insert_start, 0);
        assert_eq!(first.insert_length, 7);
        assert_eq!(first.distance, 7);
        assert!(first.copy_length >= 7);
    }

    #[test]
    fn finds_overlapping_runs() {
        let parse = greedy_parse(b"aaaaaaaaaaaaaaaa");
        assert_eq!(parse.commands.len(), 1);
        assert_eq!(parse.commands[0].insert_length, 1);
        assert_eq!(parse.commands[0].copy_length, 15);
        assert_eq!(parse.commands[0].distance, 1);
        assert_eq!(parse.tail_start, 16);
    }

    #[test]
    fn incompressible_input_remains_literal_tail() {
        let parse = greedy_parse(b"abcdefghijklmno");
        assert!(parse.commands.is_empty());
        assert_eq!(parse.tail_start, 0);
    }

    #[test]
    fn match_gain_accounts_for_format_extra_bits() {
        assert_eq!(estimated_match_gain(0, 7, 4), 56);
        assert!(estimated_match_gain(0, 20, 4) > estimated_match_gain(0, 10, 4));
        assert!(estimated_match_gain(0, 10, 4) > estimated_match_gain(0, 10, 100));
    }

    #[test]
    fn ring_bucket_keeps_two_most_recent_positions() {
        let mut bucket = [EMPTY; 2];
        let mut cursor = 0;
        for position in [2, 4, 7, 9, 10, 12] {
            insert_position(&mut bucket, &mut cursor, position);
        }
        bucket.sort_unstable();
        assert_eq!(bucket, [10, 12]);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn selects_longest_candidate_then_nearest_tie() {
        let source = b"abcdWXYZabcdQRSTabcdWXYZ";
        let candidates = [0, 8];
        assert_eq!(best_match(source, 16, &candidates), Some((0, 8)));

        let tied = b"abcdxxxxabcdyyyyabcdzzzz";
        let candidates = [0, 8];
        assert_eq!(best_match(tied, 16, &candidates), Some((8, 4)));
    }

    #[test]
    fn word_extension_stops_at_first_mismatch() {
        let source = b"abcdefghijklmnopabcdefghijklXnop";
        assert_eq!(extend_match(source, 0, 16), 12);
    }
}
