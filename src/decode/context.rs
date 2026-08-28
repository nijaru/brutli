#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiteralContextMode {
    Lsb6,
    Msb6,
    Utf8,
    Signed,
}

impl LiteralContextMode {
    pub(crate) fn from_bits(bits: u8) -> Self {
        match bits {
            0 => Self::Lsb6,
            1 => Self::Msb6,
            2 => Self::Utf8,
            3 => Self::Signed,
            _ => unreachable!("literal context mode is encoded in two bits"),
        }
    }

    pub(crate) fn id(self, previous: u8, second_previous: u8) -> u8 {
        match self {
            Self::Lsb6 => previous & 0x3f,
            Self::Msb6 => previous >> 2,
            Self::Utf8 => utf8_previous(previous) | utf8_second_previous(second_previous),
            Self::Signed => (signed_bucket(previous) << 3) | signed_bucket(second_previous),
        }
    }
}

fn utf8_previous(byte: u8) -> u8 {
    match byte {
        0x09 | 0x0a | 0x0d => 4,
        b' ' => 8,
        b'\'' | b'"' => 16,
        b'%' => 20,
        b'(' | b'<' | b'[' | b'{' => 24,
        b')' | b'>' | b']' | b'}' => 28,
        b',' | b';' | b':' => 32,
        b'.' => 36,
        b'=' => 40,
        b'0'..=b'9' => 44,
        b'A' | b'E' | b'I' | b'O' | b'U' => 48,
        b'A'..=b'Z' => 52,
        b'a' | b'e' | b'i' | b'o' | b'u' => 56,
        b'a'..=b'z' => 60,
        0x21..=0x7e => 12,
        0x80..=0xbf => byte & 1,
        0xc0..=0xff => 2 | (byte & 1),
        _ => 0,
    }
}

fn utf8_second_previous(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' | b'A'..=b'Z' => 2,
        b'a'..=b'z' => 3,
        0x21..=0x7e => 1,
        0xd0..=0xff => 2,
        _ => 0,
    }
}

fn signed_bucket(byte: u8) -> u8 {
    match byte {
        0 => 0,
        1..=15 => 1,
        16..=63 => 2,
        64..=127 => 3,
        128..=191 => 4,
        192..=239 => 5,
        240..=254 => 6,
        255 => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::LiteralContextMode;

    #[test]
    fn lsb_and_msb_modes_use_the_previous_byte_only() {
        assert_eq!(LiteralContextMode::Lsb6.id(0xab, 0xff), 0x2b);
        assert_eq!(LiteralContextMode::Msb6.id(0xab, 0xff), 0x2a);
    }

    #[test]
    fn utf8_mode_matches_ascii_categories() {
        let mode = LiteralContextMode::Utf8;
        assert_eq!(mode.id(b'A', b'a'), 48 | 3);
        assert_eq!(mode.id(b'e', b'9'), 56 | 2);
        assert_eq!(mode.id(b' ', b'.'), 8 | 1);
        assert_eq!(mode.id(b'=', b' '), 40);
    }

    #[test]
    fn utf8_mode_handles_continuation_and_lead_bytes() {
        let mode = LiteralContextMode::Utf8;
        assert_eq!(mode.id(0x80, 0x80), 0);
        assert_eq!(mode.id(0x81, 0x80), 1);
        assert_eq!(mode.id(0xc0, b'A'), 2);
        assert_eq!(mode.id(0xc1, b'A'), 3);
        assert_eq!(mode.id(0x80, 0xd0), 2);
    }

    #[test]
    fn signed_mode_uses_seven_magnitude_boundaries() {
        assert_eq!(LiteralContextMode::Signed.id(0, 0), 0);
        assert_eq!(LiteralContextMode::Signed.id(1, 16), (1 << 3) | 2);
        assert_eq!(LiteralContextMode::Signed.id(64, 128), (3 << 3) | 4);
        assert_eq!(LiteralContextMode::Signed.id(240, 255), (6 << 3) | 7);
    }

    #[test]
    fn all_context_ids_fit_six_bits() {
        for mode_bits in 0..=3 {
            let mode = LiteralContextMode::from_bits(mode_bits);
            for previous in 0..=u8::MAX {
                for second_previous in 0..=u8::MAX {
                    assert!(mode.id(previous, second_previous) < 64);
                }
            }
        }
    }
}
