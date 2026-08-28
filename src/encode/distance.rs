use super::bit_writer::BitWriter;

const SHORT_CODE_COUNT: u16 = 16;
const POSTFIX_CODE_COUNT: u16 = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DistanceCode {
    pub(super) symbol: u16,
    extra: u32,
    extra_bits: u8,
}

impl DistanceCode {
    pub(super) fn for_distance(distance: usize, direct_codes: u16) -> Self {
        assert!(distance != 0);

        if distance <= usize::from(direct_codes) {
            return Self {
                symbol: SHORT_CODE_COUNT + distance as u16 - 1,
                extra: 0,
                extra_bits: 0,
            };
        }

        for code in 0..POSTFIX_CODE_COUNT {
            let bits = 1 + (code >> 1);
            let base = (((2 + usize::from(code & 1)) << bits) - 4) + usize::from(direct_codes) + 1;
            let range = 1_usize << bits;
            if distance >= base && distance - base < range {
                return Self {
                    symbol: SHORT_CODE_COUNT + direct_codes + code,
                    extra: (distance - base) as u32,
                    extra_bits: bits as u8,
                };
            }
        }

        panic!("distance exceeds the RFC 7932 window range");
    }

    pub(super) fn write_extra(self, writer: &mut BitWriter) {
        writer.write_bits(u64::from(self.extra), self.extra_bits);
    }
}

pub(super) const fn alphabet_size(direct_codes: u16) -> u16 {
    SHORT_CODE_COUNT + direct_codes + POSTFIX_CODE_COUNT
}

#[cfg(test)]
mod tests {
    use super::{DistanceCode, alphabet_size};
    use crate::encode::bit_writer::BitWriter;

    #[test]
    fn direct_distances_use_no_extra_bits() {
        for distance in 1..=4 {
            let code = DistanceCode::for_distance(distance, 4);
            assert_eq!(code.symbol, 15 + distance as u16);

            let mut writer = BitWriter::default();
            code.write_extra(&mut writer);
            assert!(writer.finish().is_empty());
        }
    }

    #[test]
    fn non_direct_ranges_are_contiguous() {
        for distance in 5..=4096 {
            let code = DistanceCode::for_distance(distance, 4);
            assert!((20..alphabet_size(4)).contains(&code.symbol));
        }
    }

    #[test]
    fn writes_distance_extra_bits() {
        let code = DistanceCode::for_distance(17, 4);
        let mut writer = BitWriter::default();
        code.write_extra(&mut writer);
        assert!(!writer.finish().is_empty());
    }
}
