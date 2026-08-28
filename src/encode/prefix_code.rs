use super::bit_writer::BitWriter;

pub(super) fn write_var_len_u8(writer: &mut BitWriter, value: u8) {
    match value {
        0 => writer.write_bits(0, 1),
        1 => {
            writer.write_bits(1, 1);
            writer.write_bits(0, 3);
        }
        _ => {
            let width = (u8::BITS - value.leading_zeros() - 1) as u8;
            writer.write_bits(1, 1);
            writer.write_bits(u64::from(width), 3);
            writer.write_bits(u64::from(value - (1_u8 << width)), width);
        }
    }
}

pub(super) fn write_simple_prefix_code(
    writer: &mut BitWriter,
    symbols: &[u16],
    alphabet_size: u16,
) {
    assert!((1..=4).contains(&symbols.len()));
    assert!(alphabet_size != 0);
    assert!(symbols.iter().all(|&symbol| symbol < alphabet_size));

    writer.write_bits(1, 2); // simple representation
    writer.write_bits((symbols.len() - 1) as u64, 2);

    let alphabet_bits = (u16::BITS - (alphabet_size - 1).leading_zeros()) as u8;
    for &symbol in symbols {
        writer.write_bits(u64::from(symbol), alphabet_bits);
    }

    if symbols.len() == 4 {
        writer.write_bits(0, 1); // four codes of length 2
    }
}

pub(super) fn write_simple_symbol(
    writer: &mut BitWriter,
    symbol_index: usize,
    symbol_count: usize,
) {
    let (code, bits) = match symbol_count {
        1 => (0, 0),
        2 => (symbol_index as u16, 1),
        3 => match symbol_index {
            0 => (0, 1),
            1 => (0b10, 2),
            2 => (0b11, 2),
            _ => unreachable!(),
        },
        4 => (symbol_index as u16, 2),
        _ => unreachable!(),
    };
    writer.write_prefix(code, bits);
}

#[cfg(test)]
mod tests {
    use super::{write_simple_prefix_code, write_simple_symbol, write_var_len_u8};
    use crate::encode::bit_writer::BitWriter;

    #[test]
    fn variable_length_zero_is_one_zero_bit() {
        let mut writer = BitWriter::default();
        write_var_len_u8(&mut writer, 0);
        assert_eq!(writer.finish(), [0]);
    }

    #[test]
    fn simple_two_symbol_code_has_expected_layout() {
        let mut writer = BitWriter::default();
        write_simple_prefix_code(&mut writer, &[2, 5], 8);
        assert_eq!(writer.finish(), [0b0101_0101, 0]);
    }

    #[test]
    fn emits_canonical_simple_symbols() {
        let mut writer = BitWriter::default();
        for index in 0..3 {
            write_simple_symbol(&mut writer, index, 3);
        }
        assert_eq!(writer.finish(), [0b0001_1010]);
    }
}
