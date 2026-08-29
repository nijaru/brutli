use super::bit_writer::BitWriter;

const INSERT_BASE: [usize; 24] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090, 2114, 6210,
    22594,
];
const INSERT_EXTRA_BITS: [u8; 24] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];
const COPY_BASE: [usize; 24] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326, 582, 1094, 2118,
];
const COPY_EXTRA_BITS: [u8; 24] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LengthCode {
    code: usize,
    extra: u32,
    extra_bits: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InsertCommand {
    pub(super) symbol: u16,
    insert: LengthCode,
    copy: LengthCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExplicitCommand {
    pub(super) symbol: u16,
    insert: LengthCode,
    copy: LengthCode,
}

impl InsertCommand {
    pub(super) fn for_length(length: usize) -> Self {
        let insert = get_insert_length_code(length);
        let copy = get_copy_length_code(4);
        let symbol = combine_length_codes(insert.code, copy.code, false);
        Self {
            symbol,
            insert,
            copy,
        }
    }

    pub(super) const fn extra_bit_count(self) -> u8 {
        self.insert.extra_bits + self.copy.extra_bits
    }

    pub(super) fn write_extra(self, writer: &mut BitWriter) {
        write_length_extra(writer, self.insert);
        write_length_extra(writer, self.copy);
    }
}

impl ExplicitCommand {
    pub(super) fn for_lengths(insert_length: usize, copy_length: usize) -> Self {
        Self::for_insert_and_copy_code(insert_length, copy_length, false)
    }

    pub(super) fn for_insert_and_copy_code(
        insert_length: usize,
        copy_length_code: usize,
        use_last_distance: bool,
    ) -> Self {
        let insert = get_insert_length_code(insert_length);
        let copy = get_copy_length_code(copy_length_code);
        let symbol = combine_length_codes(insert.code, copy.code, use_last_distance);
        Self {
            symbol,
            insert,
            copy,
        }
    }

    pub(super) const fn requires_distance(self) -> bool {
        self.symbol >= 128
    }

    pub(super) const fn extra_bit_count(self) -> u8 {
        self.insert.extra_bits + self.copy.extra_bits
    }

    pub(super) fn write_extra(self, writer: &mut BitWriter) {
        write_length_extra(writer, self.insert);
        write_length_extra(writer, self.copy);
    }
}

fn get_insert_length_code(length: usize) -> LengthCode {
    let code = if length < 6 {
        length
    } else if length < 130 {
        let nbits = log2_floor_nonzero(length - 2) - 1;
        (nbits << 1) + ((length - 2) >> nbits) + 2
    } else if length < 2114 {
        log2_floor_nonzero(length - 66) + 10
    } else if length < 6210 {
        21
    } else if length < 22594 {
        22
    } else {
        23
    };
    make_length_code(length, code, &INSERT_BASE, &INSERT_EXTRA_BITS)
}

fn get_copy_length_code(length: usize) -> LengthCode {
    debug_assert!(length >= 2);
    let code = if length < 10 {
        length - 2
    } else if length < 134 {
        let nbits = log2_floor_nonzero(length - 6) - 1;
        (nbits << 1) + ((length - 6) >> nbits) + 4
    } else if length < 2118 {
        log2_floor_nonzero(length - 70) + 12
    } else {
        23
    };
    make_length_code(length, code, &COPY_BASE, &COPY_EXTRA_BITS)
}

fn make_length_code(
    length: usize,
    code: usize,
    bases: &[usize; 24],
    extra_bits: &[u8; 24],
) -> LengthCode {
    LengthCode {
        code,
        extra: (length - bases[code]) as u32,
        extra_bits: extra_bits[code],
    }
}

fn log2_floor_nonzero(value: usize) -> usize {
    debug_assert!(value != 0);
    usize::BITS as usize - 1 - value.leading_zeros() as usize
}

fn combine_length_codes(insert_code: usize, copy_code: usize, use_last_distance: bool) -> u16 {
    let low_bits = (copy_code & 7) | ((insert_code & 7) << 3);
    if use_last_distance && insert_code < 8 && copy_code < 16 {
        return (if copy_code < 8 {
            low_bits
        } else {
            low_bits | 64
        }) as u16;
    }

    let mut offset = 2 * ((copy_code >> 3) + 3 * (insert_code >> 3));
    offset = (offset << 5) + 0x40 + ((0x520d40_usize >> offset) & 0xc0);
    (offset | low_bits) as u16
}

fn write_length_extra(writer: &mut BitWriter, code: LengthCode) {
    writer.write_bits(u64::from(code.extra), code.extra_bits);
}

#[cfg(test)]
mod tests {
    use super::{
        COPY_BASE, COPY_EXTRA_BITS, ExplicitCommand, INSERT_BASE, INSERT_EXTRA_BITS, InsertCommand,
        get_copy_length_code, get_insert_length_code,
    };
    use crate::encode::bit_writer::BitWriter;

    #[test]
    fn insert_commands_match_reference_length_combination() {
        assert_eq!(InsertCommand::for_length(5).symbol, 170);
        assert_eq!(InsertCommand::for_length(10).symbol, 258);
        assert_eq!(InsertCommand::for_length(130).symbol, 450);
    }

    #[test]
    fn writes_insert_extra_bits() {
        let command = InsertCommand::for_length(147);

        let mut writer = BitWriter::default();
        command.write_extra(&mut writer);
        assert_eq!(writer.finish(), [17]);
    }

    #[test]
    fn maps_explicit_command_groups() {
        assert_eq!(ExplicitCommand::for_lengths(0, 2).symbol, 128);
        assert_eq!(ExplicitCommand::for_lengths(10, 2).symbol, 256);
        assert_eq!(ExplicitCommand::for_lengths(130, 2).symbol, 448);
        assert_eq!(ExplicitCommand::for_lengths(10, 70).symbol, 512);
        assert_eq!(ExplicitCommand::for_lengths(130, 70).symbol, 640);
    }

    #[test]
    fn last_distance_uses_compact_command_range() {
        let command = ExplicitCommand::for_insert_and_copy_code(0, 2, true);
        assert_eq!(command.symbol, 0);
        assert!(!command.requires_distance());

        let command = ExplicitCommand::for_insert_and_copy_code(5, 7, true);
        assert_eq!(command.symbol, 45);
        assert!(!command.requires_distance());
    }

    #[test]
    fn reports_explicit_extra_bit_count() {
        assert_eq!(ExplicitCommand::for_lengths(5, 7).extra_bit_count(), 0);
        assert_eq!(ExplicitCommand::for_lengths(147, 79).extra_bit_count(), 11);
    }

    #[test]
    fn writes_insert_then_copy_extras() {
        let command = ExplicitCommand::for_lengths(147, 79);
        assert_eq!(command.symbol, 640);

        let mut writer = BitWriter::default();
        command.write_extra(&mut writer);
        assert_eq!(writer.finish(), [0b0101_0001, 0b0000_0010]);
    }

    #[test]
    fn direct_insert_length_codes_cover_reference_ranges() {
        for length in 0..100_000 {
            let code = get_insert_length_code(length);
            let base = INSERT_BASE[code.code];
            let range = 1_usize << INSERT_EXTRA_BITS[code.code];
            assert!(length >= base && length - base < range, "length={length}");
        }
    }

    #[test]
    fn direct_copy_length_codes_cover_reference_ranges() {
        for length in 2..100_000 {
            let code = get_copy_length_code(length);
            let base = COPY_BASE[code.code];
            let range = 1_usize << COPY_EXTRA_BITS[code.code];
            assert!(length >= base && length - base < range, "length={length}");
        }
    }
}
