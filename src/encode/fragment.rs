//! Fast fragment encoding for qualities 0 and 1: an idiomatic port of
//! upstream `compress_fragment.c` (one-pass, q0) and
//! `compress_fragment_two_pass.c` (two-pass, q1).
//!
//! A *fragment* is a run of at most `1 << window_bits` bytes compressed
//! independently of earlier fragments: a fresh hash table, a skip-32 match
//! search with a candidate at the last match distance, and a literal tail.
//! Matches may reference any earlier position of the same fragment within
//! [`FRAGMENT_MAX_DISTANCE`], so later metablocks of a fragment reuse
//! matches found by earlier ones. Fragment metablocks are never last; the
//! stream ends with an empty last metablock, exactly like upstream fragment
//! output.
//!
//! Documented divergences from upstream (output stays standards-compatible;
//! verified by round-trip and reference-decoder interop):
//! - Commands are emitted as single RFC 7932 insert-and-copy symbols
//!   instead of upstream's internal 128-code insert/copy/distance space.
//!   That space exists so one Huffman build covers insert, copy, and
//!   distance symbols in C; on the wire every command is the standard
//!   combined symbol regardless. Single symbols also emit one symbol per
//!   command where upstream's split emits two for most matches.
//! - Each metablock's entropy codes are built from the actual emitted
//!   symbol histograms. Upstream q0 seeds each metablock's command code
//!   from the previous metablock's histogram (a one-pass buffering trick),
//!   and its q1 carries the same lag; exact histograms need no more memory
//!   than q1's existing two-pass buffers and produce smaller output.
//! - The compressed-vs-stored decision per metablock is an exact bit-size
//!   comparison instead of upstream's `ShouldMergeBlock` /
//!   `ShouldUseUncompressedMode` / `ShouldCompress` heuristics (the
//!   `ShouldMergeBlock` entropy test is still ported for q0's metablock
//!   sizing rhythm). Incompressible data still falls back to stored form.
//! - The recent-distance ring is threaded across metablocks and fragments
//!   (the decoder's ring persists too), so short distance codes apply from
//!   the first match of a metablock; upstream's fragment paths force an
//!   explicit distance per metablock start through a local `last_distance`
//!   variable.
//! - q0 metablock parses cover the whole merged metablock, so a copy may
//!   cross upstream's internal 64 KiB merge-block search restart points.

use super::bit_writer::BitWriter;
use super::command::{ExplicitCommand, InsertCommand};
use super::distance::{DistanceCode, RecentDistances, alphabet_size};
use super::greedy::{seed_empty_histogram, stored_chunk_bits};
use super::prefix_code::{PrefixEncoding, code_lengths};
use super::{
    COMMAND_ALPHABET_SIZE, EncoderConfig, LITERAL_ALPHABET_SIZE, write_compressed_metablock_header,
    write_final_empty_metablock, write_simple_compressed_header, write_uncompressed_metablock,
    write_window_bits,
};

/// Fragment match distance cap: the window gap below 2^18 bits, per
/// upstream `MAX_DISTANCE` in `compress_fragment.c`. The parse margins
/// keep every emitted distance at or below the wire window for any
/// configured window size, so this cap only binds for windows of 18 bits
/// and up.
const FRAGMENT_MAX_DISTANCE: usize = (1 << 18) - 16;

/// Minimum match length for quality 0 (upstream hashes 5 bytes).
const MIN_MATCH_LEN_Q0: usize = 5;

/// Quality 1 hashes 4 bytes for small hash tables, 6 for larger ones
/// (upstream `min_match = (B <= 15) ? 4 : 6`).
const MIN_MATCH_LEN_Q1_SMALL: usize = 4;
const MIN_MATCH_LEN_Q1_LARGE: usize = 6;

/// Upstream hash multiplier `kHashMul32`.
const HASH_MULTIPLIER: u64 = 0x1e35_a7bd;

/// Hash table bit caps (upstream `MaxHashTableSize`: 2^15 for q0, 2^17
/// for q1).
const MAX_TABLE_BITS_Q0: u32 = 15;
const MAX_TABLE_BITS_Q1: u32 = 17;

/// Quality 0 first metablock size (upstream `kFirstBlockSize = 3 << 15`).
const FIRST_BLOCK_SIZE_Q0: usize = 3 << 15;

/// Quality 0 merge block size (upstream `kMergeBlockSize = 1 << 16`).
const MERGE_BLOCK_SIZE_Q0: usize = 1 << 16;

/// Quality 0 merged metablock cap (upstream `total_block_size <= 1 << 20`).
const MAX_MERGED_BLOCK_Q0: usize = 1 << 20;

/// Quality 1 metablock size (upstream `kCompressFragmentTwoPassBlockSize`).
const TWO_PASS_BLOCK_SIZE: usize = 1 << 17;

/// Upstream `BROTLI_WINDOW_GAP`: the margin the parse keeps so hash probes
/// and interior updates stay inside the fragment.
const WINDOW_GAP: usize = 16;

/// Skip heuristic start (upstream `skip = 32`): after 32 misses the probe
/// stride grows to 2, after 64 to 3, and so on; it resets after each match.
const SKIP_START: usize = 32;

/// Sample rate for the merge-decision entropy estimate (upstream
/// `kSampleRate = 43` in `ShouldMergeBlock`).
const MERGE_SAMPLE_RATE: usize = 43;

/// Sample rate for the q0 input histogram (upstream `kSampleRate = 29` in
/// `BuildAndStoreLiteralPrefixCode` for blocks of 2^15 bytes and up).
const ONE_PASS_SAMPLE_RATE: usize = 29;

/// Input size at which the q0 histogram starts sampling.
const ONE_PASS_SAMPLE_THRESHOLD: usize = 1 << 15;

/// Q0 literal histogram balancing weight: the first 11 counts of each
/// symbol get weight 3 (upstream `adjust = 2 * min(count, 11)`).
const ONE_PASS_BALANCE_CAP: usize = 11;

/// Seed added to each sampled q0 histogram entry so every depth stays
/// nonzero when the histogram is only a sample.
const ONE_PASS_SAMPLE_SEED: usize = 1;

/// The smallest fragment hash table (upstream starts at 2^8).
const MIN_TABLE_BITS: u32 = 8;

/// A command found by the fragment matcher: literals followed by a copy.
/// Insert spans run from the previous command's end, so the command stream
/// covers the metablock exactly; the final command of a metablock is
/// insert-only (a `copy_len` of zero).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FragmentCommand {
    insert_len: usize,
    copy_len: usize,
    distance: usize,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Encodes `input` with the fast fragment path selected by the quality (0
/// = one-pass, 1 = two-pass): independent fragments of `1 << window_bits`
/// bytes, each a sequence of non-last metablocks, terminated by the empty
/// last metablock. The recent-distance ring persists across fragments,
/// mirroring decoder state.
pub(super) fn compress(input: &[u8], config: EncoderConfig) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, config.window_bits());
    let mut ring = RecentDistances::default();
    let fragment_size = 1_usize << config.window_bits();
    for fragment in input.chunks(fragment_size) {
        compress_fragment_range(fragment, &mut writer, config, &mut ring);
    }
    write_final_empty_metablock(&mut writer);
    writer.finish()
}

/// Compresses one fragment into `writer`: the unit the streaming encoder
/// emits. No stream header and no terminating metablock; the caller writes
/// those. The recent-distance ring is threaded through by the caller so
/// streaming and one-shot output stay byte-identical.
pub(super) fn compress_fragment_range(
    fragment: &[u8],
    writer: &mut BitWriter,
    config: EncoderConfig,
    ring: &mut RecentDistances,
) {
    if fragment.is_empty() {
        return;
    }
    if config.quality() == 0 {
        one_pass_fragment(fragment, writer, config, ring);
    } else {
        two_pass_fragment(fragment, writer, config, ring);
    }
}

// ---------------------------------------------------------------------------
// Quality 0: one-pass fragment compression
// ---------------------------------------------------------------------------

/// Quality 0 fragment structure: first metablocks of 96 KiB, extended by
/// merge blocks of 64 KiB while the merged metablock stays within 1 MiB
/// and the merge entropy test passes (upstream `ShouldMergeBlock`).
fn one_pass_fragment(
    fragment: &[u8],
    writer: &mut BitWriter,
    config: EncoderConfig,
    ring: &mut RecentDistances,
) {
    let table_bits = one_pass_table_bits(fragment.len());
    let mut table = fresh_table(table_bits);
    let mut start = 0_usize;

    while start < fragment.len() {
        let first_end = (start + FIRST_BLOCK_SIZE_Q0).min(fragment.len());
        // The merge test uses the literal code depths built from the first
        // block's input statistics, exactly as upstream builds the
        // metablock's literal code before deciding to extend it.
        let depths = merge_test_depths(&fragment[start..first_end]);

        let mut end = first_end;
        loop {
            let candidate_end = (end + MERGE_BLOCK_SIZE_Q0).min(fragment.len());
            if candidate_end == end
                || candidate_end - start > MAX_MERGED_BLOCK_Q0
                || !should_merge_block(&fragment[end..candidate_end], &depths)
            {
                break;
            }
            end = candidate_end;
        }

        let commands = parse_fragment(
            fragment,
            start,
            end,
            MIN_MATCH_LEN_Q0,
            &mut table,
            table_bits,
        );
        emit_fragment_metablock(fragment, start, end, &commands, writer, config, ring);
        start = end;
    }
}

/// Q0 hash table bits: the doubling sizing rule plus upstream's odd-bits
/// adjustment (`if ((htsize & 0xAAAAA) == 0) htsize <<= 1`), which keeps
/// q0 tables at 9/11/13/15 bits.
fn one_pass_table_bits(input_size: usize) -> u32 {
    let bits = hash_table_bits(MAX_TABLE_BITS_Q0, input_size);
    if (1_usize << bits) & 0xaaaaa == 0 {
        bits + 1
    } else {
        bits
    }
}

/// Upstream `BuildAndStoreLiteralPrefixCode` histogram construction: full
/// counts below 2^15 bytes, every 29th byte above, with the first 11 counts
/// of each symbol weighted 3 (and a +1 seed while sampling). The resulting
/// code depths feed the merge decision.
fn merge_test_depths(block: &[u8]) -> Vec<u8> {
    let mut frequencies = [0_usize; 256];
    if block.len() < ONE_PASS_SAMPLE_THRESHOLD {
        for &byte in block {
            frequencies[usize::from(byte)] += 1;
        }
        for frequency in &mut frequencies {
            *frequency += 2 * (*frequency).min(ONE_PASS_BALANCE_CAP);
        }
    } else {
        for &byte in block.iter().step_by(ONE_PASS_SAMPLE_RATE) {
            frequencies[usize::from(byte)] += 1;
        }
        for frequency in &mut frequencies {
            *frequency += ONE_PASS_SAMPLE_SEED + 2 * (*frequency).min(ONE_PASS_BALANCE_CAP);
        }
    }
    code_lengths(&frequencies)
}

/// Upstream `ShouldMergeBlock`: continue the current metablock when the
/// next block's sampled literal entropy stays profitable under the current
/// literal code depths.
fn should_merge_block(block: &[u8], depths: &[u8]) -> bool {
    let mut histogram = [0_usize; 256];
    for &byte in block.iter().step_by(MERGE_SAMPLE_RATE) {
        histogram[usize::from(byte)] += 1;
    }
    let total = block.len().div_ceil(MERGE_SAMPLE_RATE);
    let mut cost = (f64::log2(total as f64) + 0.5) * total as f64 + 200.0;
    for (symbol, &count) in histogram.iter().enumerate() {
        if count != 0 {
            cost -= count as f64 * (f64::from(depths[symbol]) + f64::log2(count as f64));
        }
    }
    cost >= 0.0
}

// ---------------------------------------------------------------------------
// Quality 1: two-pass fragment compression
// ---------------------------------------------------------------------------

/// Quality 1 fragment structure: metablocks of 128 KiB, each parsed with
/// buffered commands and emitted with exact histograms. The hash length
/// follows the table size (4 bytes at 15 bits and below, 6 above),
/// matching upstream `BrotliCompressFragmentTwoPassImpl`.
fn two_pass_fragment(
    fragment: &[u8],
    writer: &mut BitWriter,
    config: EncoderConfig,
    ring: &mut RecentDistances,
) {
    let table_bits = hash_table_bits(MAX_TABLE_BITS_Q1, fragment.len());
    let min_match = if table_bits <= 15 {
        MIN_MATCH_LEN_Q1_SMALL
    } else {
        MIN_MATCH_LEN_Q1_LARGE
    };
    let mut table = fresh_table(table_bits);
    let mut start = 0_usize;

    while start < fragment.len() {
        let end = (start + TWO_PASS_BLOCK_SIZE).min(fragment.len());
        let commands = parse_fragment(fragment, start, end, min_match, &mut table, table_bits);
        emit_fragment_metablock(fragment, start, end, &commands, writer, config, ring);
        start = end;
    }
}

// ---------------------------------------------------------------------------
// Fragment matcher
// ---------------------------------------------------------------------------

/// Parses the metablock range `[start, end)` of `fragment`, returning the
/// command stream. `table` is the fragment-wide hash table (positions are
/// fragment-relative; it persists across metablocks of the same fragment,
/// as upstream's does), with entries storing `position + 1` so that an
/// empty slot decodes to candidate 0 — upstream's `base_ip + table[hash]`
/// with a zeroed table treats an empty slot as a candidate at the fragment
/// start.
///
/// Mirrors upstream `CreateCommands` / the main loop of
/// `BrotliCompressFragmentFastImpl`: skip-32 stepping, last-distance
/// candidate, hash-table candidate, the `ip_limit` margin, interior table
/// updates after each copy, chained matches at the copy end, and the
/// insert-only tail.
fn parse_fragment(
    fragment: &[u8],
    start: usize,
    end: usize,
    min_match: usize,
    table: &mut [usize],
    table_bits: u32,
) -> Vec<FragmentCommand> {
    debug_assert!(start < end);
    let block_size = end - start;
    let mut commands = Vec::new();

    // Upstream parses only blocks of at least the window gap; smaller
    // blocks are pure literal tails.
    if block_size < WINDOW_GAP {
        commands.push(FragmentCommand {
            insert_len: block_size,
            copy_len: 0,
            distance: 0,
        });
        return commands;
    }

    // Upstream ip_limit: min(block_size - min_match, remaining -
    // kInputMarginBytes). Both margins keep every probe and interior hash
    // load inside the fragment and keep every emitted distance at or below
    // the wire window for any window size.
    let len_limit = (block_size - min_match).min(fragment.len() - WINDOW_GAP - start);
    let ip_limit = start + len_limit;

    let mut next_position = start + 1;
    let mut next_hash = hash_fragment(fragment, next_position, min_match, table_bits);
    let mut skip = SKIP_START;
    let mut next_emit = start;
    let mut last_distance: Option<usize> = None;
    let mut position;
    let mut candidate;

    'parse: loop {
        // Step 1: trawl forward for a match, probing the last distance and
        // the hash table, stepping by the skip heuristic between misses.
        // Upstream writes `table[hash] = ip` on every probe.
        'trawl: loop {
            position = next_position;
            let hash = next_hash;
            let step = skip >> 5;
            skip += 1;
            next_position = position + step;
            if next_position > ip_limit {
                break 'parse;
            }
            next_hash = hash_fragment(fragment, next_position, min_match, table_bits);

            if let Some(distance) = last_distance
                && let Some(last_candidate) = position.checked_sub(distance)
                && is_match_at(fragment, position, last_candidate, min_match)
            {
                table[hash as usize] = position + 1;
                candidate = last_candidate;
                break 'trawl;
            }

            let table_candidate = table[hash as usize];
            table[hash as usize] = position + 1;
            let table_candidate = table_candidate.saturating_sub(1);
            if table_candidate < position
                && is_match_at(fragment, position, table_candidate, min_match)
            {
                candidate = table_candidate;
                break 'trawl;
            }
        }

        // Distance check outside the hot loop (upstream `goto trawl`): an
        // over-far candidate restarts the search at the next probe.
        if position - candidate > FRAGMENT_MAX_DISTANCE {
            continue 'parse;
        }

        // Step 2: emit the literals before the match, then the copy.
        let matched = min_match
            + match_length(
                fragment,
                candidate + min_match,
                position + min_match,
                end - position - min_match,
            );
        let distance = position - candidate;
        commands.push(FragmentCommand {
            insert_len: position - next_emit,
            copy_len: matched,
            distance,
        });
        position += matched;
        next_emit = position;
        last_distance = Some(distance);
        if position >= ip_limit {
            break 'parse;
        }

        // Interior table updates: hash a few positions inside the copy so
        // overlapping matches are found, and chain from the copy-end
        // candidate.
        let mut chain_candidate =
            store_interior_positions(fragment, position, min_match, table, table_bits, true);

        while is_match_at(fragment, position, chain_candidate, min_match) {
            if position - chain_candidate > FRAGMENT_MAX_DISTANCE {
                break;
            }
            let matched = min_match
                + match_length(
                    fragment,
                    chain_candidate + min_match,
                    position + min_match,
                    end - position - min_match,
                );
            let distance = position - chain_candidate;
            commands.push(FragmentCommand {
                insert_len: 0,
                copy_len: matched,
                distance,
            });
            last_distance = Some(distance);
            position += matched;
            next_emit = position;
            if position >= ip_limit {
                break 'parse;
            }
            chain_candidate =
                store_interior_positions(fragment, position, min_match, table, table_bits, false);
        }

        // The next search starts one byte past the copy end with the skip
        // heuristic reset.
        position += 1;
        next_position = position;
        next_hash = hash_fragment(fragment, position, min_match, table_bits);
        skip = SKIP_START;
    }

    if next_emit < end {
        commands.push(FragmentCommand {
            insert_len: end - next_emit,
            copy_len: 0,
            distance: 0,
        });
    }
    commands
}

/// A fresh zeroed fragment table. Entries store `position + 1`.
fn fresh_table(table_bits: u32) -> Vec<usize> {
    vec![0_usize; 1_usize << table_bits]
}

/// Hash-table bit count for an input of `input_size` bytes, capped at
/// `max_bits` (upstream `HashTableSize`: start at 2^8, double while below
/// both the cap and the input size).
fn hash_table_bits(max_bits: u32, input_size: usize) -> u32 {
    let mut bits = MIN_TABLE_BITS;
    while bits < max_bits && (1_usize << bits) < input_size {
        bits += 1;
    }
    bits
}

/// Fragment hash (upstream `Hash` / `HashBytesAtOffset`): a little-endian
/// 8-byte load at `position`, shifted so the first `length` (4..=6) bytes
/// occupy the high end, multiplied by `kHashMul32`, keeping the top
/// `table_bits` bits. The parse margins guarantee 8 readable bytes at
/// every hashed position.
fn hash_fragment(input: &[u8], position: usize, length: usize, table_bits: u32) -> u32 {
    debug_assert!((4..=6).contains(&length));
    debug_assert!(position + 8 <= input.len());
    let load = u64::from_le_bytes(input[position..position + 8].try_into().unwrap());
    let product = (load << (8 * (8 - length))).wrapping_mul(HASH_MULTIPLIER);
    (product >> (64 - table_bits)) as u32
}

/// Match test at two fragment positions (upstream `IsMatch`): q0's 5-byte
/// form compares a 4-byte word plus a fifth byte; the length form compares
/// `length` bytes directly.
fn is_match_at(input: &[u8], position: usize, candidate: usize, min_match: usize) -> bool {
    debug_assert!(candidate < position);
    if min_match == MIN_MATCH_LEN_Q0 {
        input[position..position + 4] == input[candidate..candidate + 4]
            && input[position + 4] == input[candidate + 4]
    } else {
        input[position..position + min_match] == input[candidate..candidate + min_match]
    }
}

/// Matching bytes starting at two positions, capped at `limit` (upstream
/// `FindMatchLengthWithLimit`): 8-byte steps while a full word fits inside
/// the limit, then a byte tail. Never returns more than `limit`.
fn match_length(input: &[u8], a: usize, b: usize, limit: usize) -> usize {
    let mut length = 0_usize;
    while length + 8 <= limit {
        let difference = u64::from_le_bytes(input[a + length..a + length + 8].try_into().unwrap())
            ^ u64::from_le_bytes(input[b + length..b + length + 8].try_into().unwrap());
        if difference != 0 {
            return length + (difference.trailing_zeros() >> 3) as usize;
        }
        length += 8;
    }
    while length < limit && input[a + length] == input[b + length] {
        length += 1;
    }
    length
}

/// Stores hashes for a few positions inside the copy that just ended at
/// `position` (upstream's interior update blocks) and returns the chained
/// candidate: the hash slot's previous entry at the copy-end position.
///
/// The hashed positions follow upstream per minimum match length, including
/// the two-pass quirk where the first copy after a literal run hashes the
/// `ip - 3` bytes again for the `ip - 1` slot (upstream
/// `HashBytesAtOffset(input_bytes, 0, ...)` in the first-copy
/// `min_match == 4` branch); chained copies hash the slot properly.
fn store_interior_positions(
    fragment: &[u8],
    position: usize,
    min_match: usize,
    table: &mut [usize],
    table_bits: u32,
    after_literals: bool,
) -> usize {
    debug_assert!(position >= min_match);
    let store = |table: &mut [usize], hashed: usize, stored: usize| {
        let hash = hash_fragment(fragment, hashed, min_match, table_bits);
        table[hash as usize] = stored + 1;
    };

    match min_match {
        MIN_MATCH_LEN_Q1_SMALL => {
            store(table, position - 3, position - 3);
            store(table, position - 2, position - 2);
            let hashed = if after_literals {
                position - 3
            } else {
                position - 1
            };
            store(table, hashed, position - 1);
        }
        MIN_MATCH_LEN_Q0 => {
            for back in [3, 2, 1] {
                store(table, position - back, position - back);
            }
        }
        _ => {
            for back in [5, 4, 3, 2, 1] {
                store(table, position - back, position - back);
            }
        }
    }

    let current = hash_fragment(fragment, position, min_match, table_bits) as usize;
    let candidate = table[current];
    table[current] = position + 1;
    candidate.saturating_sub(1)
}

// ---------------------------------------------------------------------------
// Metablock emission
// ---------------------------------------------------------------------------

/// Emits one parsed metablock: build the entropy codes from the actual
/// symbol stream, serialize, and keep the smaller of the compressed and
/// stored forms. When the stored form wins, the recent-distance ring is
/// restored to its metablock-entry snapshot (the decoder never saw the
/// commands); the hash table keeps its updates, matching upstream, since
/// the stored bytes remain referenceable fragment content.
fn emit_fragment_metablock(
    fragment: &[u8],
    start: usize,
    end: usize,
    commands: &[FragmentCommand],
    writer: &mut BitWriter,
    config: EncoderConfig,
    ring: &mut RecentDistances,
) {
    debug_assert!(start < end);
    // The fragment paths never use distance params (upstream gates them
    // on quality 4).
    debug_assert_eq!(config.distance_postfix_bits(), 0);
    debug_assert_eq!(config.direct_distance_codes(), 0);
    let distance_alphabet = alphabet_size(0, 0);
    let max_distance = config.max_backward_distance();
    let ring_snapshot = ring.values();

    let mut literal_frequencies = vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)];
    let mut command_frequencies = vec![0_usize; usize::from(COMMAND_ALPHABET_SIZE)];
    let mut distance_frequencies = vec![0_usize; usize::from(distance_alphabet)];

    // Symbol stream: the combined insert-and-copy symbol per command, plus
    // the distance symbol and both extras. The final insert-only command
    // uses the insert-command form (its phantom copy never executes; the
    // metablock length ends the stream first).
    enum WireCommand {
        Tail(InsertCommand),
        Copy(ExplicitCommand),
    }

    struct Encoded {
        insert_start: usize,
        insert_len: usize,
        command: WireCommand,
        distance: Option<DistanceCode>,
    }

    let mut encoded = Vec::with_capacity(commands.len());
    let mut cursor = start;
    for command in commands {
        let insert_start = cursor;
        debug_assert!(insert_start + command.insert_len <= end);
        for &byte in &fragment[insert_start..insert_start + command.insert_len] {
            literal_frequencies[usize::from(byte)] += 1;
        }

        let (wire, distance) = if command.copy_len == 0 {
            let insert = InsertCommand::for_length(command.insert_len);
            command_frequencies[usize::from(insert.symbol)] += 1;
            (WireCommand::Tail(insert), None)
        } else {
            let raw_code = ring.compute_code(command.distance, max_distance);
            let copy = ExplicitCommand::for_insert_and_copy_code(
                command.insert_len,
                command.copy_len,
                raw_code == 0,
            );
            command_frequencies[usize::from(copy.symbol)] += 1;
            let distance = copy.requires_distance().then(|| {
                let code = DistanceCode::for_code(raw_code, 0, 0);
                distance_frequencies[usize::from(code.symbol)] += 1;
                code
            });
            // The decoder updates its ring for every distance code except
            // short code 0; mirror that exactly.
            if raw_code != 0 {
                ring.push(command.distance);
            }
            (WireCommand::Copy(copy), distance)
        };

        encoded.push(Encoded {
            insert_start,
            insert_len: command.insert_len,
            command: wire,
            distance,
        });

        cursor += command.insert_len + command.copy_len;
    }
    debug_assert_eq!(cursor, end);

    // Build the codes from the actual histograms; seed the empty cases so
    // every tree exists on the wire (the simple header form always writes
    // three trees).
    seed_empty_histogram(&mut literal_frequencies);
    seed_empty_histogram(&mut distance_frequencies);
    let literal_code = PrefixEncoding::from_frequencies(&literal_frequencies)
        .expect("seeded literal histogram is nonempty");
    let command_code = PrefixEncoding::from_frequencies(&command_frequencies)
        .expect("command histogram is nonempty");
    let distance_code = PrefixEncoding::from_frequencies(&distance_frequencies)
        .expect("seeded distance histogram is nonempty");

    // Serialize the compressed form; keep it only when it beats the stored
    // form bit-for-bit (replaces upstream's ratio heuristics).
    let mut compressed = BitWriter::default();
    write_compressed_metablock_header(&mut compressed, end - start, false);
    write_simple_compressed_header(&mut compressed, 0, 0);
    literal_code.write_tree(&mut compressed, LITERAL_ALPHABET_SIZE);
    command_code.write_tree(&mut compressed, COMMAND_ALPHABET_SIZE);
    distance_code.write_tree(&mut compressed, distance_alphabet);

    for item in &encoded {
        match &item.command {
            WireCommand::Tail(insert) => {
                command_code.write_symbol(&mut compressed, insert.symbol);
                insert.write_extra(&mut compressed);
            }
            WireCommand::Copy(copy) => {
                command_code.write_symbol(&mut compressed, copy.symbol);
                copy.write_extra(&mut compressed);
            }
        }
        for &byte in &fragment[item.insert_start..item.insert_start + item.insert_len] {
            literal_code.write_symbol(&mut compressed, u16::from(byte));
        }
        if let Some(distance) = item.distance {
            distance_code.write_symbol(&mut compressed, distance.symbol);
            distance.write_extra(&mut compressed);
        }
    }

    if compressed.bit_len() < stored_chunk_bits(end - start) {
        writer.append_writer(compressed);
    } else {
        ring.restore(ring_snapshot);
        write_uncompressed_metablock(writer, &fragment[start..end]);
    }
}
