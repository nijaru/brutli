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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExplicitCommand {
    pub(super) symbol: u16,
    insert: LengthCode,
    copy: LengthCode,
}

impl InsertCommand {
    pub(super) fn for_length(length: usize) -> Self {
        let insert = length_code(length, &INSERT_BASE, &INSERT_EXTRA_BITS);
        let symbol = implicit_command_symbol(insert.code, 0);
        Self { symbol, insert }
    }

    pub(super) fn write_extra(self, writer: &mut BitWriter) {
        write_length_extra(writer, self.insert);
    }
}

impl ExplicitCommand {
    pub(super) fn for_lengths(insert_length: usize, copy_length: usize) -> Self {
        let insert = length_code(insert_length, &INSERT_BASE, &INSERT_EXTRA_BITS);
        let copy = length_code(copy_length, &COPY_BASE, &COPY_EXTRA_BITS);
        let symbol = explicit_command_symbol(insert.code, copy.code);
        Self {
            symbol,
            insert,
            copy,
        }
    }

    pub(super) fn write_extra(self, writer: &mut BitWriter) {
        write_length_extra(writer, self.insert);
        write_length_extra(writer, self.copy);
    }
}

fn length_code(length: usize, bases: &[usize; 24], extra_bits: &[u8; 24]) -> LengthCode {
    let code = bases
        .iter()
        .zip(extra_bits.iter().copied())
        .position(|(&base, bits)| {
            let range = 1_usize << bits;
            length >= base && length - base < range
        })
        .expect("RFC 7932 length code covers the requested length");

    LengthCode {
        code,
        extra: (length - bases[code]) as u32,
        extra_bits: extra_bits[code],
    }
}

fn implicit_command_symbol(insert_code: usize, copy_code: usize) -> u16 {
    debug_assert!(insert_code < 8);
    debug_assert!(copy_code < 16);

    let range = if copy_code < 8 { 0 } else { 1 };
    (range * 64 + insert_code * 8 + copy_code % 8) as u16
}

fn explicit_command_symbol(insert_code: usize, copy_code: usize) -> u16 {
    let insert_group = insert_code / 8;
    let copy_group = copy_code / 8;
    let range = match (insert_group, copy_group) {
        (0, 0) => 2,
        (0, 1) => 3,
        (1, 0) => 4,
        (1, 1) => 5,
        (0, 2) => 6,
        (2, 0) => 7,
        (1, 2) => 8,
        (2, 1) => 9,
        (2, 2) => 10,
        _ => unreachable!(),
    };
    (range * 64 + (insert_code % 8) * 8 + copy_code % 8) as u16
}

fn write_length_extra(writer: &mut BitWriter, code: LengthCode) {
    writer.write_bits(u64::from(code.extra), code.extra_bits);
}

#[cfg(test)]
mod tests {
    use super::{ExplicitCommand, InsertCommand};
    use crate::encode::bit_writer::BitWriter;

    #[test]
    fn maps_insert_code_groups_to_command_symbols() {
        assert_eq!(InsertCommand::for_length(5).symbol, 40);
        assert_eq!(InsertCommand::for_length(10).symbol, 256);
        assert_eq!(InsertCommand::for_length(130).symbol, 448);
    }

    #[test]
    fn writes_insert_extra_bits() {
        let command = InsertCommand::for_length(147);
        assert_eq!(command.symbol, 448);

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
    fn writes_insert_then_copy_extras() {
        let command = ExplicitCommand::for_lengths(147, 79);
        assert_eq!(command.symbol, 640);

        let mut writer = BitWriter::default();
        command.write_extra(&mut writer);
        assert_eq!(writer.finish(), [0b0101_0001, 0b0000_0010]);
    }
}
