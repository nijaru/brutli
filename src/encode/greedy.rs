use super::bit_writer::BitWriter;
use super::command::{ExplicitCommand, InsertCommand};
use super::distance::{DistanceCode, RecentDistances, alphabet_size};
use super::match_finder::{MatchCommand, greedy_parse};
use super::prefix_code::{
    PrefixEncoding, write_simple_prefix_code, write_simple_symbol, write_var_len_u8,
};
use super::{
    COMMAND_ALPHABET_SIZE, DIRECT_DISTANCE_CODES, LITERAL_ALPHABET_SIZE, MAX_META_BLOCK_SIZE,
    write_final_compressed_header, write_simple_compressed_header, write_window_bits,
};

const LITERAL_CONTEXT_COUNT: usize = 64;
const TEXT_CONTEXT_START: usize = 44;

#[derive(Debug, Clone, Copy)]
struct EncodedMatch {
    parsed: MatchCommand,
    command: ExplicitCommand,
    distance: DistanceCode,
}

#[derive(Debug)]
struct LiteralContextModel {
    map: [u8; LITERAL_CONTEXT_COUNT],
    codes: [PrefixEncoding; 2],
}

pub(super) fn try_compress(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_META_BLOCK_SIZE {
        return None;
    }

    let parse = greedy_parse(input);
    if parse.commands.is_empty() {
        return None;
    }

    let distance_alphabet = alphabet_size(DIRECT_DISTANCE_CODES);
    let mut literal_frequencies = vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)];
    let mut command_frequencies = vec![0_usize; usize::from(COMMAND_ALPHABET_SIZE)];
    let mut distance_frequencies = vec![0_usize; usize::from(distance_alphabet)];
    let mut commands = Vec::with_capacity(parse.commands.len());
    let mut recent_distances = RecentDistances::default();

    for parsed in parse.commands {
        let command = ExplicitCommand::for_lengths(parsed.insert_length, parsed.copy_length);
        let distance = recent_distances.encode(parsed.distance, DIRECT_DISTANCE_CODES);
        command_frequencies[usize::from(command.symbol)] += 1;
        distance_frequencies[usize::from(distance.symbol)] += 1;
        count_literals(
            &mut literal_frequencies,
            &input[parsed.insert_start..parsed.insert_start + parsed.insert_length],
        );
        commands.push(EncodedMatch {
            parsed,
            command,
            distance,
        });
    }

    let tail_start = parse.tail_start;
    let tail = &input[tail_start..];
    let tail_command = if tail.is_empty() {
        None
    } else {
        let command = InsertCommand::for_length(tail.len());
        command_frequencies[usize::from(command.symbol)] += 1;
        count_literals(&mut literal_frequencies, tail);
        Some(command)
    };

    let literal_code = PrefixEncoding::from_frequencies(&literal_frequencies)?;
    let command_code = PrefixEncoding::from_frequencies(&command_frequencies)?;
    let distance_code = PrefixEncoding::from_frequencies(&distance_frequencies)?;

    let baseline = encode_single_literal_tree(
        input,
        &commands,
        tail_start,
        tail_command,
        &literal_code,
        &command_code,
        &distance_code,
        distance_alphabet,
    );

    let baseline_literal_bits = literal_code.data_bits(&literal_frequencies);
    let Some(model) =
        build_literal_context_model(input, &commands, tail_start, baseline_literal_bits)
    else {
        return Some(baseline);
    };

    let contextual = encode_contextual_literal_trees(
        input,
        &commands,
        tail_start,
        tail_command,
        &model,
        &command_code,
        &distance_code,
        distance_alphabet,
    );
    if contextual.len() < baseline.len() {
        Some(contextual)
    } else {
        Some(baseline)
    }
}

fn encode_single_literal_tree(
    input: &[u8],
    commands: &[EncodedMatch],
    tail_start: usize,
    tail_command: Option<InsertCommand>,
    literal_code: &PrefixEncoding,
    command_code: &PrefixEncoding,
    distance_code: &PrefixEncoding,
    distance_alphabet: u16,
) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, input.len());
    write_simple_compressed_header(&mut writer, DIRECT_DISTANCE_CODES);
    literal_code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    command_code.write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    distance_code.write_tree(&mut writer, distance_alphabet);

    for encoded in commands {
        command_code.write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_literal_slice(
            &mut writer,
            literal_code,
            &input[encoded.parsed.insert_start
                ..encoded.parsed.insert_start + encoded.parsed.insert_length],
        );
        distance_code.write_symbol(&mut writer, encoded.distance.symbol);
        encoded.distance.write_extra(&mut writer);
    }

    if let Some(command) = tail_command {
        command_code.write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_literal_slice(&mut writer, literal_code, &input[tail_start..]);
    }

    writer.finish()
}

fn encode_contextual_literal_trees(
    input: &[u8],
    commands: &[EncodedMatch],
    tail_start: usize,
    tail_command: Option<InsertCommand>,
    model: &LiteralContextModel,
    command_code: &PrefixEncoding,
    distance_code: &PrefixEncoding,
    distance_alphabet: u16,
) -> Vec<u8> {
    let mut writer = BitWriter::default();
    write_window_bits(&mut writer, super::DEFAULT_WINDOW_BITS);
    write_final_compressed_header(&mut writer, input.len());
    write_two_tree_compressed_header(&mut writer, DIRECT_DISTANCE_CODES, &model.map);
    for code in &model.codes {
        code.write_tree(&mut writer, LITERAL_ALPHABET_SIZE);
    }
    command_code.write_tree(&mut writer, COMMAND_ALPHABET_SIZE);
    distance_code.write_tree(&mut writer, distance_alphabet);

    for encoded in commands {
        command_code.write_symbol(&mut writer, encoded.command.symbol);
        encoded.command.write_extra(&mut writer);
        write_contextual_literal_range(
            &mut writer,
            input,
            encoded.parsed.insert_start,
            encoded.parsed.insert_start + encoded.parsed.insert_length,
            model,
        );
        distance_code.write_symbol(&mut writer, encoded.distance.symbol);
        encoded.distance.write_extra(&mut writer);
    }

    if let Some(command) = tail_command {
        command_code.write_symbol(&mut writer, command.symbol);
        command.write_extra(&mut writer);
        write_contextual_literal_range(&mut writer, input, tail_start, input.len(), model);
    }

    writer.finish()
}

fn build_literal_context_model(
    input: &[u8],
    commands: &[EncodedMatch],
    tail_start: usize,
    baseline_bits: usize,
) -> Option<LiteralContextModel> {
    let map = literal_context_map();
    let mut frequencies = [
        vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)],
        vec![0_usize; usize::from(LITERAL_ALPHABET_SIZE)],
    ];

    for encoded in commands {
        count_contextual_literals(
            input,
            encoded.parsed.insert_start,
            encoded.parsed.insert_start + encoded.parsed.insert_length,
            &map,
            &mut frequencies,
        );
    }
    count_contextual_literals(input, tail_start, input.len(), &map, &mut frequencies);

    let first = PrefixEncoding::from_frequencies(&frequencies[0])?;
    let second = PrefixEncoding::from_frequencies(&frequencies[1])?;
    let contextual_bits = first.data_bits(&frequencies[0]) + second.data_bits(&frequencies[1]);
    if contextual_bits >= baseline_bits {
        return None;
    }

    Some(LiteralContextModel {
        map,
        codes: [first, second],
    })
}

fn literal_context_map() -> [u8; LITERAL_CONTEXT_COUNT] {
    let mut map = [0_u8; LITERAL_CONTEXT_COUNT];
    map[TEXT_CONTEXT_START..].fill(1);
    map
}

fn count_contextual_literals(
    input: &[u8],
    start: usize,
    end: usize,
    map: &[u8; LITERAL_CONTEXT_COUNT],
    frequencies: &mut [Vec<usize>; 2],
) {
    for position in start..end {
        let context = utf8_context_id(input, position);
        let tree = usize::from(map[context]);
        frequencies[tree][usize::from(input[position])] += 1;
    }
}

fn write_contextual_literal_range(
    writer: &mut BitWriter,
    input: &[u8],
    start: usize,
    end: usize,
    model: &LiteralContextModel,
) {
    for position in start..end {
        let context = utf8_context_id(input, position);
        let tree = usize::from(model.map[context]);
        model.codes[tree].write_symbol(writer, u16::from(input[position]));
    }
}

fn utf8_context_id(input: &[u8], position: usize) -> usize {
    let previous = position.checked_sub(1).map_or(0, |index| input[index]);
    let second_previous = position.checked_sub(2).map_or(0, |index| input[index]);
    usize::from(utf8_previous(previous) | utf8_second_previous(second_previous))
}

fn utf8_previous(byte: u8) -> u8 {
    match byte {
        0x09 | 0x0a | 0x0d => 4,
        b' ' => 8,
        b'\'' | b'"' => 16,
        b'%' => 20,
        b'(' | b'<' | b'[' | b'{' => 24,
        b')' | b'>' | b']' | b'}' => 28,
        b',' | b';' | b':' => 32,
        b'.' => 36,
        b'=' => 40,
        b'0'..=b'9' => 44,
        b'A' | b'E' | b'I' | b'O' | b'U' => 48,
        b'A'..=b'Z' => 52,
        b'a' | b'e' | b'i' | b'o' | b'u' => 56,
        b'a'..=b'z' => 60,
        0x21..=0x7e => 12,
        0x80..=0xbf => byte & 1,
        0xc0..=0xff => 2 | (byte & 1),
        _ => 0,
    }
}

fn utf8_second_previous(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' | b'A'..=b'Z' => 2,
        b'a'..=b'z' => 3,
        0x21..=0x7e => 1,
        0xd0..=0xff => 2,
        _ => 0,
    }
}

fn write_two_tree_compressed_header(
    writer: &mut BitWriter,
    direct_distance_codes: u16,
    context_map: &[u8; LITERAL_CONTEXT_COUNT],
) {
    debug_assert!(direct_distance_codes <= 15);

    write_var_len_u8(writer, 0); // one literal block type
    write_var_len_u8(writer, 0); // one insert-and-copy block type
    write_var_len_u8(writer, 0); // one distance block type
    writer.write_bits(0, 2); // NPOSTFIX
    writer.write_bits(u64::from(direct_distance_codes), 4); // NDIRECT
    writer.write_bits(2, 2); // UTF8 literal context mode
    write_var_len_u8(writer, 1); // two literal trees
    write_two_tree_context_map(writer, context_map);
    write_var_len_u8(writer, 0); // one distance tree
}

fn write_two_tree_context_map(writer: &mut BitWriter, context_map: &[u8; LITERAL_CONTEXT_COUNT]) {
    writer.write_bits(0, 1); // RLEMAX = 0
    write_simple_prefix_code(writer, &[0, 1], 2);
    for &tree in context_map {
        write_simple_symbol(writer, usize::from(tree), 2);
    }
    writer.write_bits(0, 1); // no inverse MTF
}

fn count_literals(frequencies: &mut [usize], literals: &[u8]) {
    for &literal in literals {
        frequencies[usize::from(literal)] += 1;
    }
}

fn write_literal_slice(writer: &mut BitWriter, code: &PrefixEncoding, literals: &[u8]) {
    for &literal in literals {
        code.write_symbol(writer, u16::from(literal));
    }
}

#[cfg(test)]
mod tests {
    use super::{literal_context_map, try_compress, utf8_context_id};
    use crate::decompress;

    #[test]
    fn greedy_stream_round_trips_text() {
        let source = b"the quick brown fox jumps over the lazy dog; the quick brown fox jumps over the lazy dog.";
        let encoded = try_compress(source).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn greedy_stream_round_trips_mixed_distances() {
        let source =
            b"alpha beta gamma alpha beta delta alpha beta gamma alpha beta delta".repeat(64);
        let encoded = try_compress(&source).unwrap();
        assert_eq!(decompress(&encoded, source.len()).unwrap(), source);
    }

    #[test]
    fn text_context_map_separates_alphanumeric_previous_bytes() {
        let map = literal_context_map();
        assert!(map[..44].iter().all(|&tree| tree == 0));
        assert!(map[44..].iter().all(|&tree| tree == 1));

        assert_eq!(map[utf8_context_id(b" a", 1)], 0);
        assert_eq!(map[utf8_context_id(b"aa", 1)], 1);
        assert_eq!(map[utf8_context_id(b"9a", 1)], 1);
    }

    #[test]
    fn reference_decoder_accepts_greedy_output() {
        let source = b"general greedy LZ77 should interoperate with the Brotli reference decoder. "
            .repeat(128);
        let encoded = try_compress(&source).unwrap();
        let mut decoded = vec![0_u8; source.len() + 1];
        let info = brotli_decompressor::brotli_decode(&encoded, &mut decoded);

        assert!(matches!(
            info.result,
            brotli_decompressor::BrotliResult::ResultSuccess
        ));
        assert_eq!(info.decoded_size, source.len());
        assert_eq!(&decoded[..info.decoded_size], source);
    }
}
