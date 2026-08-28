#![no_main]

use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 64 * 1024;

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT)];
    let encoded = brutli::compress(input);
    let decoded = brutli::decompress(&encoded, input.len()).expect("Brutli must decode its output");
    assert_eq!(decoded, input);
});
