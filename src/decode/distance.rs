use super::bit_reader::BitReader;

const SHORT_CODE_COUNT: u16 = 16;
const MAX_DISTANCE_BITS: u16 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DistanceError {
    InvalidSymbol,
    InvalidShortDistance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Distance {
    pub(super) value: usize,
    pub(super) should_push: bool,
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
    pub(super) fn last(&self) -> usize {
        self.values[0]
    }

    pub(super) fn push(&mut self, distance: usize) {
        self.values.copy_within(..3, 1);
        self.values[0] = distance;
    }

    fn resolve_short(&self, symbol: u16) -> Result<usize, DistanceError> {
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
            _ => return Err(DistanceError::InvalidSymbol),
        };

        self.values[index]
            .checked_add_signed(delta)
            .filter(|&distance| distance != 0)
            .ok_or(DistanceError::InvalidShortDistance)
    }
}

#[derive(Debug)]
pub(super) struct DistanceDecoder {
    state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Resolved(Distance),
    Extra {
        bits: u8,
        postfix_bits: u8,
        direct_codes: u16,
        hcode: u16,
        lcode: u16,
    },
    Done,
}

impl DistanceDecoder {
    pub(super) fn new(
        symbol: u16,
        postfix_bits: u8,
        direct_codes: u16,
        recent: &RecentDistances,
    ) -> Result<Self, DistanceError> {
        let alphabet_size = distance_alphabet_size(postfix_bits, direct_codes);
        if symbol >= alphabet_size {
            return Err(DistanceError::InvalidSymbol);
        }

        let state = if symbol < SHORT_CODE_COUNT {
            State::Resolved(Distance {
                value: recent.resolve_short(symbol)?,
                should_push: symbol != 0,
            })
        } else if symbol < SHORT_CODE_COUNT + direct_codes {
            State::Resolved(Distance {
                value: usize::from(symbol - SHORT_CODE_COUNT + 1),
                should_push: true,
            })
        } else {
            let code = symbol - direct_codes - SHORT_CODE_COUNT;
            let hcode = code >> postfix_bits;
            let postfix_mask = (1_u16 << postfix_bits) - 1;
            let lcode = code & postfix_mask;
            let bits = 1 + (code >> (postfix_bits + 1));
            debug_assert!(bits <= MAX_DISTANCE_BITS);
            State::Extra {
                bits: bits as u8,
                postfix_bits,
                direct_codes,
                hcode,
                lcode,
            }
        };

        Ok(Self { state })
    }

    pub(super) fn decode(
        &mut self,
        reader: &mut BitReader,
        input: &[u8],
        cursor: &mut usize,
    ) -> Option<Distance> {
        match self.state {
            State::Resolved(distance) => {
                self.state = State::Done;
                Some(distance)
            }
            State::Extra {
                bits,
                postfix_bits,
                direct_codes,
                hcode,
                lcode,
            } => {
                let extra = reader.read_bits(input, cursor, u32::from(bits))? as usize;
                let offset = ((2 + usize::from(hcode & 1)) << bits) - 4;
                let value = ((offset + extra) << postfix_bits)
                    + usize::from(lcode)
                    + usize::from(direct_codes)
                    + 1;
                self.state = State::Done;
                Some(Distance {
                    value,
                    should_push: true,
                })
            }
            State::Done => unreachable!("distance decoded more than once"),
        }
    }
}

fn distance_alphabet_size(postfix_bits: u8, direct_codes: u16) -> u16 {
    debug_assert!(postfix_bits <= 3);
    SHORT_CODE_COUNT + direct_codes + (48 << postfix_bits)
}

#[cfg(test)]
mod tests {
    use super::{DistanceDecoder, DistanceError, RecentDistances, distance_alphabet_size};
    use crate::decode::bit_reader::BitReader;

    #[test]
    fn initial_recent_distances_match_the_spec() {
        let recent = RecentDistances::default();
        let expected = [4, 11, 15, 16, 3, 5, 2, 6, 1, 7, 10, 12, 9, 13, 8, 14];

        for (symbol, &distance) in expected.iter().enumerate() {
            assert_eq!(recent.resolve_short(symbol as u16), Ok(distance));
        }
    }

    #[test]
    fn rejects_non_positive_short_distance() {
        let mut recent = RecentDistances::default();
        recent.push(1);
        assert_eq!(
            recent.resolve_short(4),
            Err(DistanceError::InvalidShortDistance)
        );
    }

    #[test]
    fn pushes_recent_distances_newest_first() {
        let mut recent = RecentDistances::default();
        recent.push(23);
        recent.push(42);

        assert_eq!(recent.resolve_short(0), Ok(42));
        assert_eq!(recent.resolve_short(1), Ok(23));
        assert_eq!(recent.resolve_short(2), Ok(4));
        assert_eq!(recent.resolve_short(3), Ok(11));
    }

    #[test]
    fn decodes_direct_distances_without_extra_bits() {
        let recent = RecentDistances::default();
        let mut reader = BitReader::default();
        let mut cursor = 0;

        for (symbol, expected) in [(16, 1), (17, 2), (18, 3), (19, 4)] {
            let mut decoder = DistanceDecoder::new(symbol, 0, 4, &recent).unwrap();
            let distance = decoder.decode(&mut reader, &[], &mut cursor).unwrap();
            assert_eq!(distance.value, expected);
            assert!(distance.should_push);
        }
    }

    #[test]
    fn decodes_postfix_distance_extra_bits() {
        let recent = RecentDistances::default();
        let mut decoder = DistanceDecoder::new(28, 2, 12, &recent).unwrap();
        let mut reader = BitReader::default();
        let mut cursor = 0;

        let distance = decoder.decode(&mut reader, &[1], &mut cursor).unwrap();
        assert_eq!(distance.value, 17);
        assert!(distance.should_push);
    }

    #[test]
    fn symbol_zero_reuses_without_requesting_push() {
        let recent = RecentDistances::default();
        let mut decoder = DistanceDecoder::new(0, 0, 0, &recent).unwrap();
        let mut reader = BitReader::default();
        let mut cursor = 0;
        let distance = decoder.decode(&mut reader, &[], &mut cursor).unwrap();

        assert_eq!(distance.value, recent.last());
        assert!(!distance.should_push);
    }

    #[test]
    fn validates_dynamic_alphabet() {
        assert_eq!(distance_alphabet_size(0, 0), 64);
        assert_eq!(distance_alphabet_size(3, 120), 520);

        let recent = RecentDistances::default();
        assert!(matches!(
            DistanceDecoder::new(520, 3, 120, &recent),
            Err(DistanceError::InvalidSymbol)
        ));
    }
}
