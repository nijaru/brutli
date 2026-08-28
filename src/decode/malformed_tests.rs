use super::decoder::{Decoder, ProcessStatus};

const REPETITIVE_STREAM: &[u8] = &[
    0xe2, 0x0e, 0x00, 0x80, 0xc0, 0x0e, 0xd8, 0xdc, 0x65, 0x2e, 0x44, 0x6c, 0x71, 0x60, 0xdd,
    0x31,
];

const BINARY_PATTERN_STREAM: &[u8] = &[
    0xe2, 0x0f, 0x00, 0x80, 0x78, 0x00, 0x1c, 0x4f, 0x43, 0x7c, 0x01, 0x2c, 0xc1, 0xcf, 0x0a,
    0x28, 0xa7, 0xfb, 0xfc, 0x18, 0x00, 0xc2, 0xba, 0x01,
];

fn drive_bounded(input: &[u8]) {
    let mut decoder = Decoder::default();
    let mut input_offset = 0;
    let mut total_output = 0;

    for _ in 0..10_000 {
        let mut output = [0; 127];
        let result = match decoder.process(&input[input_offset..], &mut output) {
            Ok(result) => result,
            Err(_) => return,
        };

        input_offset += result.consumed;
        total_output += result.produced;
        assert!(input_offset <= input.len());

        if total_output > 64 * 1024 {
            return;
        }

        match result.status {
            ProcessStatus::Done => return,
            ProcessStatus::NeedInput if input_offset == input.len() => return,
            ProcessStatus::NeedInput | ProcessStatus::NeedOutput => {
                assert!(
                    result.consumed != 0 || result.produced != 0,
                    "decoder stalled with input remaining"
                );
            }
        }
    }

    panic!("decoder exceeded bounded malformed-input work");
}

#[test]
fn every_reference_stream_truncation_is_handled_without_panicking() {
    for stream in [REPETITIVE_STREAM, BINARY_PATTERN_STREAM] {
        for end in 0..stream.len() {
            drive_bounded(&stream[..end]);
        }
    }
}

#[test]
fn every_single_bit_reference_mutation_is_handled_without_panicking() {
    for stream in [REPETITIVE_STREAM, BINARY_PATTERN_STREAM] {
        for byte_index in 0..stream.len() {
            for bit in 0..8 {
                let mut mutated = stream.to_vec();
                mutated[byte_index] ^= 1 << bit;
                drive_bounded(&mutated);
            }
        }
    }
}

#[test]
fn deterministic_arbitrary_inputs_are_handled_without_panicking() {
    let mut state = 0x9e37_79b9_u32;

    for length in 1..=64 {
        for _ in 0..8 {
            let mut input = Vec::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                input.push(state as u8);
            }
            drive_bounded(&input);
        }
    }
}
