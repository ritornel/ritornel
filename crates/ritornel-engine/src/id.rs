//! Making IDs. The type is the contract's, because an ID is what a client
//! sees. Making one needs random bytes, and those are the engine's.

use ritornel_contract::Id;

use crate::{Entropy, Error};

/// A new UUIDv7 for the time `now_ms`. A time before 1970 counts as 1970.
pub fn new_id(now_ms: i64, entropy: &dyn Entropy) -> Result<Id, Error> {
    let mut random = [0; 10];
    entropy.fill(&mut random)?;
    Ok(Id::new_v7(now_ms, random))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::CountingEntropy;

    #[test]
    fn an_id_takes_its_time_and_ten_bytes_from_the_source() {
        let entropy = CountingEntropy::default();
        let id = new_id(0x0123_4567_89ab, &entropy).unwrap();
        // The counting source gives bytes 00, 01, 02 and so on.
        assert_eq!(id.to_string(), "01234567-89ab-7001-8203-040506070809");
        // The next ID draws the next ten.
        let next = new_id(0x0123_4567_89ab, &entropy).unwrap();
        assert_eq!(next.to_string(), "01234567-89ab-7a0b-8c0d-0e0f10111213");
    }
}
