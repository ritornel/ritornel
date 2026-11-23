//! What a listen played on, as words: the operating system, the hardware
//! the sound came out of, and how that device played it. Each describes the
//! device that made the sound, never one that controlled it. A source says
//! what it knows of each and nothing of the rest: no word is unknown or not
//! said, and `other` is named but outside these. Today Spotify's streams
//! carry them; Ritornel's own players will carry the same words. See
//! "Spotify streams" in docs/internals/history.md.

use serde::{Deserialize, Serialize};

/// An enum of words on the wire. A word it does not know, as a newer side
/// may send, is read as `Other`, so that an older client or server never
/// fails on it; and so the text is never repeated in an error or a log.
macro_rules! words {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $word:literal,)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[cfg_attr(test, derive(ts_rs::TS))]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$variant_meta])* $variant,)*
        }

        impl $name {
            /// Every word, in order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The word, as it is on the wire.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $word,)*
                }
            }

            /// The word for this text, `Other` when it is none of them.
            #[must_use]
            pub fn of(word: &str) -> Self {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|known| known.as_str() == word)
                    .unwrap_or(Self::Other)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Word;
                impl serde::de::Visitor<'_> for Word {
                    type Value = $name;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a word")
                    }
                    fn visit_str<E: serde::de::Error>(self, word: &str) -> Result<$name, E> {
                        Ok($name::of(word))
                    }
                }
                deserializer.deserialize_str(Word)
            }
        }
    };
}

words! {
    /// The operating system of the device that made the sound, and only
    /// that: a web player or a cast is how it played, never a system.
    Platform {
        /// iOS, iPadOS or watchOS.
        Ios => "ios",
        /// Android.
        Android => "android",
        /// macOS.
        Mac => "mac",
        /// Windows.
        Windows => "windows",
        /// Linux.
        Linux => "linux",
        /// Another, such as a television's or a set-top box's own.
        Other => "other",
    }
}

words! {
    /// The hardware the sound came out of.
    DeviceKind {
        /// A computer.
        Computer => "computer",
        /// A phone.
        Phone => "phone",
        /// A tablet.
        Tablet => "tablet",
        /// A watch.
        Watch => "watch",
        /// A speaker, a smart display or an AV receiver.
        Speaker => "speaker",
        /// A television.
        Tv => "tv",
        /// A car.
        Car => "car",
        /// A games console.
        Console => "console",
        /// Speakers played together as one.
        Group => "group",
        /// Another kind.
        Other => "other",
    }
}

words! {
    /// How the device that made the sound played it.
    PlayedVia {
        /// An app of its own.
        App => "app",
        /// A web player.
        Web => "web",
        /// Google Cast.
        Cast => "cast",
        /// A Spotify Connect receiver with no app of its own, such as an AV
        /// receiver or a network speaker.
        Connect => "connect",
        /// AirPlay.
        Airplay => "airplay",
        /// Another way.
        Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_word_reads_as_itself_and_one_unknown_as_other() {
        for platform in Platform::ALL {
            let word = serde_json::to_value(platform).unwrap();
            assert_eq!(word, json!(platform.as_str()));
            assert_eq!(serde_json::from_value::<Platform>(word).unwrap(), *platform);
        }
        for kind in DeviceKind::ALL {
            let word = serde_json::to_value(kind).unwrap();
            assert_eq!(serde_json::from_value::<DeviceKind>(word).unwrap(), *kind);
        }
        for via in PlayedVia::ALL {
            let word = serde_json::to_value(via).unwrap();
            assert_eq!(serde_json::from_value::<PlayedVia>(word).unwrap(), *via);
        }
        // A word a newer side added is read as `other`, not refused.
        assert_eq!(
            serde_json::from_value::<Platform>(json!("fuchsia")).unwrap(),
            Platform::Other
        );
        assert_eq!(
            serde_json::from_value::<DeviceKind>(json!("headphones")).unwrap(),
            DeviceKind::Other
        );
        assert_eq!(
            serde_json::from_value::<PlayedVia>(json!("bluetooth")).unwrap(),
            PlayedVia::Other
        );
    }
}
