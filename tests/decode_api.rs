use brutli::{DecodeError, DecodeStatus, Decoder, decompress};

const COMPRESSED: &[u8] = &[
    0xe2, 0x0e, 0x00, 0x80, 0xc0, 0x0e, 0xd8, 0xdc, 0x65, 0x2e, 0x44, 0x6c, 0x71, 0x60, 0xdd, 0x31,
];

fn expected() -> Vec<u8> {
    b"abc123".repeat(20)
}

#[test]
fn incremental_api_resumes_across_small_output_buffers() {
    let mut decoder = Decoder::new();
    let mut input_offset = 0;
    let mut decoded = Vec::new();

    for _ in 0..1024 {
        let mut output = [0_u8; 3];
        let progress = decoder
            .process(&COMPRESSED[input_offset..], &mut output)
            .unwrap();
        input_offset += progress.consumed;
        decoded.extend_from_slice(&output[..progress.produced]);

        match progress.status {
            DecodeStatus::NeedInput => panic!("complete reference stream unexpectedly needs input"),
            DecodeStatus::NeedOutput => {}
            DecodeStatus::Done => {
                assert_eq!(input_offset, COMPRESSED.len());
                assert_eq!(decoder.total_output(), expected().len());
                assert_eq!(decoded, expected());
                return;
            }
        }
    }

    panic!("incremental decoder did not terminate");
}

#[test]
fn one_shot_decode_is_strict_and_bounded() {
    assert_eq!(
        decompress(COMPRESSED, expected().len()).unwrap(),
        expected()
    );

    assert_eq!(
        decompress(COMPRESSED, expected().len() - 1),
        Err(DecodeError::OutputLimitExceeded {
            limit: expected().len() - 1,
        })
    );

    let mut with_trailing = COMPRESSED.to_vec();
    with_trailing.push(0);
    assert_eq!(
        decompress(&with_trailing, expected().len()),
        Err(DecodeError::TrailingData { remaining: 1 })
    );
}

#[test]
fn finish_reports_truncated_input() {
    let mut decoder = Decoder::new();
    let mut output = [0_u8; 256];
    let progress = decoder
        .process(&COMPRESSED[..COMPRESSED.len() - 1], &mut output)
        .unwrap();
    assert_eq!(progress.status, DecodeStatus::NeedInput);

    assert_eq!(decoder.finish(&mut output), Err(DecodeError::UnexpectedEof));
}
