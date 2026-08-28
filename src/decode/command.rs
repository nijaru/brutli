use super::bit_reader::BitReader;

const COMMAND_SYMBOL_COUNT: u16 = 704;

const INSERT_BASE: [usize; 24] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090,
    2114, 6210, 22594,
];
const INSERT_EXTRA_BITS: [u8; 24] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];
const COPY_BASE: [usize; 24] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326,
    582, 1094, 2118,
];
const COPY_EXTRA_BITS: [u8; 24] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Command {
    pub(super) insert_length: usize,
    pub(super) copy_length: usize,
    pub(super) distance_context: u8,
    pub(super) implicit_distance_zero: bool,
}

#[derive(Debug)]
pub(super) struct CommandDecoder {
    spec: CommandSpec,
    state: State,
    insert_length: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Insert,
    Copy,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandSpec {
    insert_base: usize,
    insert_extra_bits: u8,
    copy_base: usize,
    copy_extra_bits: u8,
    implicit_distance_zero: bool,
}

impl CommandDecoder {
    pub(super) fn new(symbol: u16) -> Self {
        Self {
            spec: CommandSpec::from_symbol(symbol),
            state: State::Insert,
            insert_length: 0,
        }
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Option<Command> {
        loop {
            match self.state {
                State::Insert => {
                    let extra = if self.spec.insert_extra_bits == 0 {
                        0
                    } else {
                        reader.read_bits(input, cursor, u32::from(self.spec.insert_extra_bits))?
                            as usize
                    };
                    self.insert_length = self.spec.insert_base + extra;
                    self.state = State::Copy;
                }
                State::Copy => {
                    let extra = if self.spec.copy_extra_bits == 0 {
                        0
                    } else {
                        reader.read_bits(input, cursor, u32::from(self.spec.copy_extra_bits))?
                            as usize
                    };
                    let copy_length = self.spec.copy_base + extra;
                    self.state = State::Done;
                    return Some(Command {
                        insert_length: self.insert_length,
                        copy_length,
                        distance_context: distance_context(copy_length),
                        implicit_distance_zero: self.spec.implicit_distance_zero,
                    });
                }
                State::Done => unreachable!("command lengths decoded more than once"),
            }
        }
    }
}

impl CommandSpec {
    fn from_symbol(symbol: u16) -> Self {
        assert!(symbol < COMMAND_SYMBOL_COUNT);

        let range = symbol >> 6;
        let (insert_group, copy_group, implicit_distance_zero) = match range {
            0 => (0, 0, true),
            1 => (0, 8, true),
            2 => (0, 0, false),
            3 => (0, 8, false),
            4 => (8, 0, false),
            5 => (8, 8, false),
            6 => (0, 16, false),
            7 => (16, 0, false),
            8 => (8, 16, false),
            9 => (16, 8, false),
            10 => (16, 16, false),
            _ => unreachable!(),
        };
        let insert_code = insert_group + usize::from((symbol >> 3) & 7);
        let copy_code = copy_group + usize::from(symbol & 7);

        Self {
            insert_base: INSERT_BASE[insert_code],
            insert_extra_bits: INSERT_EXTRA_BITS[insert_code],
            copy_base: COPY_BASE[copy_code],
            copy_extra_bits: COPY_EXTRA_BITS[copy_code],
            implicit_distance_zero,
        }
    }
}

fn distance_context(copy_length: usize) -> u8 {
    match copy_length {
        2 => 0,
        3 => 1,
        4 => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandDecoder, CommandSpec, distance_context};
    use crate::decode::bit_reader::BitReader;

    #[derive(Default)]
    struct Bits {
        bits: Vec<bool>,
    }

    impl Bits {
        fn push(&mut self, value: u64, count: u8) {
            for bit in 0..count {
                self.bits.push((value >> bit) & 1 != 0);
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            let mut bytes = vec![0; self.bits.len().div_ceil(8)];
            for (index, bit) in self.bits.into_iter().enumerate() {
                if bit {
                    bytes[index / 8] |= 1 << (index % 8);
                }
            }
            bytes
        }
    }

    #[test]
    fn maps_command_ranges_from_the_spec() {
        let cases = [
            (0, 0, 2, true),
            (64, 0, 10, true),
            (128, 0, 2, false),
            (256, 10, 2, false),
            (384, 0, 70, false),
            (448, 130, 2, false),
            (512, 10, 70, false),
            (576, 130, 10, false),
            (640, 130, 70, false),
        ];

        for (symbol, insert_base, copy_base, implicit) in cases {
            let spec = CommandSpec::from_symbol(symbol);
            assert_eq!(spec.insert_base, insert_base, "symbol {symbol}");
            assert_eq!(spec.copy_base, copy_base, "symbol {symbol}");
            assert_eq!(spec.implicit_distance_zero, implicit, "symbol {symbol}");
        }
    }

    #[test]
    fn decodes_extra_bits() {
        let mut bits = Bits::default();
        bits.push(17, 6); // insert code 16: 130 + 17
        bits.push(9, 5); // copy code 16: 70 + 9
        let input = bits.into_bytes();

        let mut decoder = CommandDecoder::new(640);
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let command = decoder.decode(&mut reader, &input, &mut cursor).unwrap();

        assert_eq!(command.insert_length, 147);
        assert_eq!(command.copy_length, 79);
        assert_eq!(command.distance_context, 3);
        assert!(!command.implicit_distance_zero);
    }

    #[test]
    fn resumes_between_insert_and_copy_extras() {
        let mut bits = Bits::default();
        bits.push(0x2a, 6);
        bits.push(0x1f, 5);
        let input = bits.into_bytes();

        let mut decoder = CommandDecoder::new(640);
        let mut reader = BitReader::default();
        let mut first_cursor = 0;
        assert!(
            decoder
                .decode(&mut reader, &input[..1], &mut first_cursor)
                .is_none()
        );
        assert_eq!(first_cursor, 1);

        let mut second_cursor = 0;
        let command = decoder
            .decode(&mut reader, &input[1..], &mut second_cursor)
            .unwrap();
        assert_eq!(command.insert_length, 172);
        assert_eq!(command.copy_length, 101);
    }

    #[test]
    fn distance_contexts_follow_copy_length() {
        assert_eq!(distance_context(2), 0);
        assert_eq!(distance_context(3), 1);
        assert_eq!(distance_context(4), 2);
        assert_eq!(distance_context(5), 3);
        assert_eq!(distance_context(100), 3);
    }

    #[test]
    fn largest_command_codes_stay_in_usize_range() {
        let spec = CommandSpec::from_symbol(703);
        assert_eq!(spec.insert_base, 22594);
        assert_eq!(spec.insert_extra_bits, 24);
        assert_eq!(spec.copy_base, 2118);
        assert_eq!(spec.copy_extra_bits, 24);
    }
}
