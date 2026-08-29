use super::bit_writer::BitWriter;

const SHORT_CODE_COUNT: u16 = 16;
const POSTFIX_CODE_COUNT: u16 = 48;
const LAST_DISTANCE_CODES: [u16; 7] = [8, 6, 4, 0, 5, 7, 9];
const SECOND_LAST_DISTANCE_CODES: [u16; 7] = [14, 12, 10, 1, 11, 13, 15];

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
    pub(super) const fn values(&self) -> [usize; 4] {
        self.values
    }

    pub(super) fn compute_code(&self, distance: usize, max_distance: usize) -> usize {
        if distance <= max_distance {
            if distance == self.values[0] {
                return 0;
            }
            if distance == self.values[1] {
                return 1;
            }
            if let Some(offset) = short_offset(distance, self.values[0]) {
                return usize::from(LAST_DISTANCE_CODES[offset]);
            }
            if let Some(offset) = short_offset(distance, self.values[1]) {
                return usize::from(SECOND_LAST_DISTANCE_CODES[offset]);
            }
            if distance == self.values[2] {
                return 2;
            }
            if distance == self.values[3] {
                return 3;
            }
        }

        distance
            .checked_add(usize::from(SHORT_CODE_COUNT) - 1)
            .expect("distance code fits usize")
    }

    pub(super) fn push(&mut self, distance: usize) {
        self.values.copy_within(..3, 1);
        self.values[0] = distance;
    }

    #[cfg(test)]
    pub(super) fn encode(&mut self, distance: usize, direct_codes: u16) -> DistanceCode {
        let raw_code = self.compute_code(distance, usize::MAX);
        if raw_code != 0 {
            self.push(distance);
        }
        DistanceCode::for_code(raw_code, direct_codes)
    }

    #[cfg(test)]
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
}

fn short_offset(distance: usize, cached: usize) -> Option<usize> {
    distance
        .checked_add(3)
        .and_then(|value| value.checked_sub(cached))
        .filter(|&offset| offset < 7)
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

    pub(super) fn for_code(code: usize, direct_codes: u16) -> Self {
        if code < usize::from(SHORT_CODE_COUNT) {
            return Self::for_short_symbol(code as u16);
        }
        let distance = code - (usize::from(SHORT_CODE_COUNT) - 1);
        Self::for_distance(distance, direct_codes)
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

        let dist = distance + 3 - usize::from(direct_codes);
        let bucket = log2_floor_nonzero(dist) - 1;
        let prefix = (dist >> bucket) & 1;
        let offset = (2 + prefix) << bucket;
        let code = 2 * (bucket - 1) + prefix;
        assert!(code < usize::from(POSTFIX_CODE_COUNT), "distance exceeds the RFC 7932 window range");

        Self {
            symbol: SHORT_CODE_COUNT + direct_codes + code as u16,
            extra: (dist - offset) as u32,
            extra_bits: bucket as u8,
        }
    }

    pub(super) const fn extra_bit_count(self) -> u8 {
        self.extra_bits
    }

    pub(super) fn write_extra(self, writer: &mut BitWriter) {
        writer.write_bits(u64::from(self.extra), self.extra_bits);
    }
}

fn log2_floor_nonzero(value: usize) -> usize {
    debug_assert!(value != 0);
    usize::BITS as usize - 1 - value.leading_zeros() as usize
}

pub(super) const fn alphabet_size(direct_codes: u16) -> u16 {
    SHORT_CODE_COUNT + direct_codes + POSTFIX_CODE_COUNT
}

#[cfg(test)]
mod tests {
    use super::{DistanceCode, POSTFIX_CODE_COUNT, RecentDistances, SHORT_CODE_COUNT, alphabet_size};
    use crate::encode::bit_writer::BitWriter;

    #[test]
    fn direct_distances_use_no_extra_bits() {
        for distance in 1..=4 {
            let code = DistanceCode::for_distance(distance, 4);
            assert_eq!(code.symbol, 15 + distance as u16);
            assert_eq!(code.extra_bit_count(), 0);

            let mut writer = BitWriter::default();
            code.write_extra(&mut writer);
            assert!(writer.finish().is_empty());
        }
    }

    #[test]
    fn raw_distance_codes_preserve_short_codes() {
        for code in 0..16 {
            assert_eq!(DistanceCode::for_code(code, 4).symbol, code as u16);
        }
        assert_eq!(DistanceCode::for_code(16, 4).symbol, 16);
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
    fn reference_distance_code_priority_matches_encoder() {
        let recent = RecentDistances::default();
        assert_eq!(recent.compute_code(4, 100), 0);
        assert_eq!(recent.compute_code(11, 100), 1);
        assert_eq!(recent.compute_code(3, 100), 4);
        assert_eq!(recent.compute_code(15, 100), 2);
        assert_eq!(recent.compute_code(100, 100), 115);
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
    fn direct_formula_matches_reference_range_scan() {
        for direct_codes in [0_u16, 4, 12, 120] {
            for distance in 1..=1_000_000 {
                let direct = DistanceCode::for_distance(distance, direct_codes);
                let scanned = reference_distance_code(distance, direct_codes);
                assert_eq!(direct, scanned, "distance={distance}, direct_codes={direct_codes}");
            }
        }
    }

    #[test]
    fn reports_distance_extra_bits() {
        assert_eq!(DistanceCode::for_distance(17, 4).extra_bit_count(), 3);
    }

    #[test]
    fn writes_distance_extra_bits() {
        let code = DistanceCode::for_distance(17, 4);
        let mut writer = BitWriter::default();
        code.write_extra(&mut writer);
        assert!(!writer.finish().is_empty());
    }

    fn reference_distance_code(distance: usize, direct_codes: u16) -> DistanceCode {
        if distance <= usize::from(direct_codes) {
            return DistanceCode {
                symbol: SHORT_CODE_COUNT + distance as u16 - 1,
                extra: 0,
                extra_bits: 0,
            };
        }

        for code in 0..POSTFIX_CODE_COUNT {
            let bits = 1 + (code >> 1);
            let base = (((2 + usize::from(code & 1)) << bits) - 4)
                + usize::from(direct_codes)
                + 1;
            let range = 1_usize << bits;
            if distance >= base && distance - base < range {
                return DistanceCode {
                    symbol: SHORT_CODE_COUNT + direct_codes + code,
                    extra: (distance - base) as u32,
                    extra_bits: bits as u8,
                };
            }
        }

        panic!("distance exceeds the RFC 7932 window range");
    }
}
