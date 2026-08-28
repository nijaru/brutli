use super::history::History;

fn next_u32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

#[test]
fn bulk_history_matches_bytewise_reference_across_wraps() {
    let mut history = History::new(10);
    let mut model = Vec::new();
    let mut state = 0x6a09_e667_u32;

    for round in 0..256_usize {
        let literal_len = 1 + next_u32(&mut state) as usize % 1536;
        let mut literals = Vec::with_capacity(literal_len);
        for _ in 0..literal_len {
            literals.push(next_u32(&mut state) as u8);
        }

        history.push_slice(&literals);
        model.extend_from_slice(&literals);

        let max_distance = history.max_backward_distance();
        let distance = 1 + next_u32(&mut state) as usize % max_distance;
        let count = 1 + next_u32(&mut state) as usize % 2048;
        let output_len = 1 + next_u32(&mut state) as usize % 513;
        let produced = count.min(output_len);

        let mut expected = Vec::with_capacity(produced);
        for _ in 0..produced {
            let byte = model[model.len() - distance];
            expected.push(byte);
            model.push(byte);
        }

        let mut output = vec![0_u8; output_len];
        assert_eq!(
            history.copy_into(distance, count, &mut output),
            Ok(produced),
            "round={round} distance={distance} count={count} output_len={output_len}"
        );
        assert_eq!(
            &output[..produced],
            expected,
            "round={round} distance={distance} count={count} output_len={output_len}"
        );

        let expected_previous = model[model.len() - 1];
        let expected_second_previous = model[model.len() - 2];
        assert_eq!(
            history.previous_bytes(),
            (expected_previous, expected_second_previous),
            "round={round}"
        );
    }
}
