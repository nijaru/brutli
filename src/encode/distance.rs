use super::bit_writer::BitWriter;

const SHORT_CODE_COUNT: u16 = 16;
const POSTFIX_CODE_COUNT: u16 = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DistanceCode {
    pub(super) symbol: u16,
    extra: u32,
    extra_bits: u8,
}

#[derive(Debug, Clone)]
pub(super) struct RecentDistances {
    values: [usize; 4],
}

impl Default for RecentDistances {
    fn default() -> Self {
        Self {
            values: [4, 11, 15, 16],
        }
    }
}

impl RecentDistances {
    pub(super) fn encode(&mut self, distance: usize, direct_codes: u16) -> DistanceCode {
        let code = match self.short_symbol(distance) {
            Some(symbol) => DistanceCode::for_short_symbol(symbol),
            None => DistanceCode::for_distance(distance, direct_codes),
        };
        if code.symbol != 0 {
            self.push(distance);
        }
        code
    }

    fn short_symbol(&self, distance: usize) -> Option<u16> {
        (0..SHORT_CODE_COUNT).find(|&symbol| self.resolve_short(symbol) == Some(distance))
    }

    fn resolve_short(&self, symbol: u16) -> Option<usize> {
        let (index, delta) = match symbol {
            0 => (0, 0),
            1 => (1, 0),
            2 => (2, 0),
            3 => (3, 0),
            4 => (0, -1),
            5 => (0, 1),
            6 => (0, -2),
            7 => (0, 2),
            8 => (0, -3),
            9 => (0, 3),
            10 => (1, -1),
            11 => (1, 1),
            12 => (1, -2),
            13 => (1, 2),
            14 => (1, -3),
            15 => (1, 3),
            _ => return None,
        };
        self.values[index]
            .checked_add_signed(delta)
            .filter(|&distance| distance != 0)
    }

    fn push(&mut self, distance: usize) {
        self.values.copy_within(..3, 1);
        self.values[0] = distance;
    }
}

impl DistanceCode {
    fn for_short_symbol(symbol: u16) -> Self {
        debug_assert!(symbol < SHORT_CODE_COUNT);
        Self {
            symbol,
            extra: 0,
            extra_bits: 0,
        }
    }

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
    use super::{DistanceCode, RecentDistances, alphabet_size};
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
    fn initial_recent_distances_match_the_spec() {
        let recent = RecentDistances::default();
        let expected = [4, 11, 15, 16, 3, 5, 2, 6, 1, 7, 10, 12, 9, 13, 8, 14];

        for (symbol, &distance) in expected.iter().enumerate() {
            assert_eq!(recent.resolve_short(symbol as u16), Some(distance));
        }
    }

    #[test]
    fn recent_distance_zero_does_not_push() {
        let mut recent = RecentDistances::default();
        assert_eq!(recent.encode(4, 4).symbol, 0);
        assert_eq!(recent.values, [4, 11, 15, 16]);
    }

    #[test]
    fn nonzero_recent_distance_pushes() {
        let mut recent = RecentDistances::default();
        assert_eq!(recent.encode(11, 4).symbol, 1);
        assert_eq!(recent.values, [11, 4, 11, 15]);
        assert_eq!(recent.encode(10, 4).symbol, 4);
        assert_eq!(recent.values, [10, 11, 4, 11]);
    }

    #[test]
    fn explicit_distance_becomes_reusable() {
        let mut recent = RecentDistances::default();
        assert!(recent.encode(100, 4).symbol >= 20);
        assert_eq!(recent.values[0], 100);
        assert_eq!(recent.encode(100, 4).symbol, 0);
        assert_eq!(recent.values[0], 100);
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
