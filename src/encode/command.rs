use super::bit_writer::BitWriter;

const INSERT_BASE: [usize; 24] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090, 2114, 6210,
    22594,
];
const INSERT_EXTRA_BITS: [u8; 24] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InsertCommand {
    pub(super) symbol: u16,
    extra: u32,
    extra_bits: u8,
}

impl InsertCommand {
    pub(super) fn for_length(length: usize) -> Self {
        let code = INSERT_BASE
            .iter()
            .zip(INSERT_EXTRA_BITS)
            .position(|(&base, extra_bits)| {
                let range = 1_usize << extra_bits;
                length >= base && length - base < range
            })
            .expect("RFC 7932 insert code covers a meta-block length");

        let symbol = match code {
            0..=7 => (code as u16) << 3,
            8..=15 => 256 + (((code - 8) as u16) << 3),
            16..=23 => 448 + (((code - 16) as u16) << 3),
            _ => unreachable!(),
        };

        Self {
            symbol,
            extra: (length - INSERT_BASE[code]) as u32,
            extra_bits: INSERT_EXTRA_BITS[code],
        }
    }

    pub(super) fn write_extra(self, writer: &mut BitWriter) {
        writer.write_bits(u64::from(self.extra), self.extra_bits);
    }
}

#[cfg(test)]
mod tests {
    use super::InsertCommand;
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
}
