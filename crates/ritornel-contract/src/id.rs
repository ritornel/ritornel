//! UUIDv7 IDs: what a server, a session and every stored entity is named by.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A UUIDv7: 48 bits of Unix milliseconds, then random bits. IDs made later
/// sort later, to the millisecond.
///
/// It is stored as its 16 bytes. On the wire it is a string, the usual
/// hyphenated form in lower case, in a field named for its kind
/// (`serverId`). A client treats that string as opaque and only compares
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id([u8; 16]);

impl Id {
    /// The ID for the time `unix_ms` with these random bytes. A time before
    /// 1970 counts as 1970. Making an ID is the engine's work, because the
    /// random bytes are: this is only where each bit goes.
    pub fn new_v7(unix_ms: i64, random: [u8; 10]) -> Self {
        let [_, _, t0, t1, t2, t3, t4, t5] = u64::try_from(unix_ms).unwrap_or(0).to_be_bytes();
        let [r0, r1, r2, r3, r4, r5, r6, r7, r8, r9] = random;
        // The 7 is the version, and the top two bits of the ninth byte are
        // the variant.
        let (version, variant) = (0x70 | (r0 & 0x0f), 0x80 | (r2 & 0x3f));
        Self([
            t0, t1, t2, t3, t4, t5, version, r1, variant, r3, r4, r5, r6, r7, r8, r9,
        ])
    }

    /// The ID with these bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The 16 bytes, as they are stored.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({self})")
    }
}

/// Text that is not an ID as [`Id`] writes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAnId;

impl fmt::Display for NotAnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not an ID: expected 32 lower-case hex digits in the hyphenated form")
    }
}

impl std::error::Error for NotAnId {}

impl FromStr for Id {
    type Err = NotAnId;

    /// Reads exactly what `Display` writes, so an ID has one text.
    fn from_str(text: &str) -> Result<Self, NotAnId> {
        let mut bytes = [0; 16];
        let mut digits = text.bytes().enumerate().filter_map(|(index, character)| {
            // A hyphen where one belongs is skipped. Anywhere else it is not
            // a digit, and fails below.
            let hyphen = matches!(index, 8 | 13 | 18 | 23) && character == b'-';
            (!hyphen).then_some(match character {
                b'0'..=b'9' => Some(character - b'0'),
                b'a'..=b'f' => Some(character - b'a' + 10),
                _ => None,
            })
        });
        if text.len() != 36 {
            return Err(NotAnId);
        }
        for byte in &mut bytes {
            let (Some(Some(high)), Some(Some(low))) = (digits.next(), digits.next()) else {
                return Err(NotAnId);
            };
            *byte = (high << 4) | low;
        }
        match digits.next() {
            None => Ok(Self(bytes)),
            Some(_) => Err(NotAnId),
        }
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// In TypeScript an ID is a `string`, as it is on the wire, wherever a field
/// holds one.
#[cfg(test)]
impl ts_rs::TS for Id {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn name(_: &ts_rs::Config) -> String {
        "string".to_owned()
    }

    fn inline(config: &ts_rs::Config) -> String {
        Self::name(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RANDOM: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    #[test]
    fn an_id_holds_its_time_then_the_version_and_variant_bits() {
        let id = Id::new_v7(0x0123_4567_89ab, RANDOM);
        assert_eq!(id.to_string(), "01234567-89ab-7001-8203-040506070809");
        // Whatever the random bytes are, the version and the variant stay.
        let id = Id::new_v7(0x0123_4567_89ab, [0xff; 10]);
        assert_eq!(id.to_string(), "01234567-89ab-7fff-bfff-ffffffffffff");
    }

    #[test]
    fn a_later_id_sorts_later() {
        assert!(Id::new_v7(1_000, [0xff; 10]) < Id::new_v7(2_000, [0; 10]));
    }

    #[test]
    fn a_time_before_1970_counts_as_1970() {
        let id = Id::new_v7(-5, RANDOM);
        assert!(id.to_string().starts_with("00000000-0000-7"));
    }

    #[test]
    fn bytes_round_trip_and_debug_shows_the_text() {
        let id = Id::from_bytes([0xab; 16]);
        assert_eq!(id.as_bytes(), &[0xab; 16]);
        assert_eq!(
            format!("{id:?}"),
            "Id(abababab-abab-abab-abab-abababababab)"
        );
    }

    #[test]
    fn the_text_reads_back_as_the_same_id() {
        let id = Id::new_v7(0x0123_4567_89ab, RANDOM);
        assert_eq!(id.to_string().parse(), Ok(id));
        assert_eq!(
            "abababab-abab-abab-abab-abababababab".parse(),
            Ok(Id::from_bytes([0xab; 16]))
        );
    }

    #[test]
    fn anything_but_the_one_text_of_an_id_is_refused() {
        for wrong in [
            "",
            "abababab-abab-abab-abab-ababababababab",
            "abababab-abab-abab-abab-ababababab",
            "ABABABAB-ABAB-ABAB-ABAB-ABABABABABAB",
            "abababababababababababababababababab",
            "abababab-abab-abab-abab-abababababag",
            "abababab-abab-abab-abab+abababababab",
            "abababa-babab-abab-abab-abababababab",
            "abababab-abab-abab-abab-ababababab\u{e9}",
        ] {
            assert_eq!(wrong.parse::<Id>(), Err(NotAnId), "{wrong:?}");
        }
    }

    #[test]
    fn on_the_wire_an_id_is_its_text() {
        let id = Id::from_bytes([0x11; 16]);
        let text = "\"11111111-1111-1111-1111-111111111111\"";
        assert_eq!(serde_json::to_string(&id).unwrap(), text);
        assert_eq!(serde_json::from_str::<Id>(text).unwrap(), id);
        assert!(serde_json::from_str::<Id>("\"not an id\"").is_err());
        assert!(serde_json::from_str::<Id>("17").is_err());
    }
}
