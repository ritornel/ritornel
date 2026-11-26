//! Socket tickets: what a page opens the socket with, since it cannot put
//! the credential in a header. See "Query-string credentials and origin
//! checks" in docs/internals/contract.md and `ritornel_contract::SocketTicket`.
//!
//! A ticket is 32 random bytes, written as 64 hex digits. It is never the
//! session token and says nothing about it. The server keeps the tickets
//! that are waiting to be used here, in memory and nowhere else: a ticket
//! is in no file, no database and no log line, and a server that restarts
//! has none. A ticket opens one socket. Presenting it takes it off the
//! list, whatever becomes of the request that presented it, and so does
//! its time running out.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ritornel_contract::SocketTicket;
use ritornel_engine::Entropy;
use subtle::ConstantTimeEq;

use crate::Error;
use crate::session::SessionGrant;

/// How many tickets may be waiting to be used at once. A ticket is asked
/// for by a program that is about to open a socket, and waits for
/// milliseconds, so one owner's programs have a handful. When the list is
/// full the oldest gives way: only a holder of the credential can fill it.
const MAX_WAITING: usize = 64;

/// The bytes of a ticket.
type Secret = [u8; 32];

/// A ticket that has been issued and not yet presented.
struct Waiting {
    secret: Secret,
    /// The permissions the credential that asked for it holds, and so
    /// what the socket it opens holds.
    grant: SessionGrant,
    /// When it was made. It opens nothing once its life has passed.
    made: Instant,
}

/// The tickets a running server has issued that are still waiting to be
/// used.
pub(crate) struct Tickets {
    entropy: Arc<dyn Entropy>,
    life: Duration,
    waiting: Mutex<Vec<Waiting>>,
}

impl Tickets {
    /// No tickets yet. Each one made will work for `life`.
    pub(crate) fn new(entropy: Arc<dyn Entropy>, life: Duration) -> Self {
        Self {
            entropy,
            life,
            waiting: Mutex::default(),
        }
    }

    /// Makes a ticket that opens one socket with this grant.
    pub(crate) fn issue(&self, grant: SessionGrant) -> Result<SocketTicket, Error> {
        self.issue_at(grant, Instant::now())
    }

    /// The grant `presented` opens, if it is a ticket that is waiting.
    /// It is not waiting afterwards: a ticket works once.
    pub(crate) fn redeem(&self, presented: &str) -> Option<SessionGrant> {
        self.redeem_at(presented, Instant::now())
    }

    fn issue_at(&self, grant: SessionGrant, now: Instant) -> Result<SocketTicket, Error> {
        let mut secret = [0; 32];
        self.entropy.fill(&mut secret)?;

        let mut waiting = self.lock();
        self.forget_expired(&mut waiting, now);
        if waiting.len() >= MAX_WAITING {
            // The list is in the order the tickets were made.
            waiting.remove(0);
        }
        waiting.push(Waiting {
            secret,
            grant,
            made: now,
        });
        Ok(SocketTicket {
            ticket: secret.iter().map(|byte| format!("{byte:02x}")).collect(),
            expires_in_ms: u64::try_from(self.life.as_millis()).unwrap_or(u64::MAX),
        })
    }

    fn redeem_at(&self, presented: &str, now: Instant) -> Option<SessionGrant> {
        let presented = secret_of(presented)?;
        let mut waiting = self.lock();
        self.forget_expired(&mut waiting, now);
        // Every waiting ticket is compared, and each comparison takes the
        // same time wherever the two first differ, so how long this takes
        // says nothing about what is waiting.
        let mut found = None;
        for (index, ticket) in waiting.iter().enumerate() {
            if bool::from(ticket.secret.ct_eq(&presented)) {
                found = Some(index);
            }
        }
        found.map(|index| waiting.remove(index).grant)
    }

    /// Drops every ticket whose life has passed.
    fn forget_expired(&self, waiting: &mut Vec<Waiting>, now: Instant) {
        waiting.retain(|ticket| now.saturating_duration_since(ticket.made) < self.life);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Waiting>> {
        // Nothing done under this lock can leave the list half changed.
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many tickets are waiting, expired ones included until the next
    /// ticket is issued or presented.
    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.lock().len()
    }
}

/// What is waiting opens sockets, so none of it is printed.
impl fmt::Debug for Tickets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tickets")
            .field("life", &self.life)
            .finish_non_exhaustive()
    }
}

/// The bytes a ticket's text stands for, if it is written as the server
/// writes one: 64 hex digits, in lower case.
fn secret_of(text: &str) -> Option<Secret> {
    let (pairs, []) = text.as_bytes().as_chunks::<2>() else {
        return None;
    };
    let mut secret = [0; 32];
    if pairs.len() != secret.len() {
        return None;
    }
    for (byte, [high, low]) in secret.iter_mut().zip(pairs) {
        *byte = (hex(*high)? << 4) | hex(*low)?;
    }
    Some(secret)
}

fn hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ritornel_contract::TokenRole;
    use ritornel_engine::OsEntropy;

    use super::*;

    const LIFE: Duration = Duration::from_secs(10);

    fn tickets() -> Tickets {
        Tickets::new(Arc::new(OsEntropy), LIFE)
    }

    #[test]
    fn a_ticket_is_64_hex_digits_and_says_how_long_it_works_for() {
        let issued = tickets().issue(SessionGrant::OWNER).unwrap();
        assert_eq!(issued.ticket.len(), 64);
        assert!(issued.ticket.bytes().all(|digit| hex(digit).is_some()));
        assert_eq!(issued.expires_in_ms, 10_000);
    }

    #[test]
    fn a_ticket_works_once_and_opens_the_grant_it_was_made_for() {
        let tickets = tickets();
        let owner = tickets.issue(SessionGrant::OWNER).unwrap().ticket;
        let guest_grant = SessionGrant::for_role(TokenRole::Guest);
        let guest = tickets.issue(guest_grant).unwrap().ticket;
        assert_ne!(owner, guest);

        assert_eq!(tickets.redeem(&guest), Some(guest_grant));
        assert_eq!(tickets.redeem(&owner), Some(SessionGrant::OWNER));
        assert_eq!(tickets.redeem(&owner), None);
        assert_eq!(tickets.redeem(&guest), None);
        assert_eq!(tickets.waiting(), 0);
    }

    #[test]
    fn a_ticket_stops_working_when_its_time_is_up() {
        let tickets = tickets();
        let made = Instant::now();
        let just_in_time = tickets.issue_at(SessionGrant::OWNER, made).unwrap().ticket;
        let too_late = tickets.issue_at(SessionGrant::OWNER, made).unwrap().ticket;

        let last_instant = made + LIFE - Duration::from_nanos(1);
        assert_eq!(
            tickets.redeem_at(&just_in_time, last_instant),
            Some(SessionGrant::OWNER)
        );
        assert_eq!(tickets.redeem_at(&too_late, made + LIFE), None);
        // It is gone, and not only refused: an earlier clock would not
        // bring it back.
        assert_eq!(tickets.redeem_at(&too_late, made), None);
        assert_eq!(tickets.waiting(), 0);
    }

    #[test]
    fn a_ticket_with_no_life_never_works() {
        let tickets = Tickets::new(Arc::new(OsEntropy), Duration::ZERO);
        let issued = tickets.issue(SessionGrant::OWNER).unwrap();
        assert_eq!(issued.expires_in_ms, 0);
        assert_eq!(tickets.redeem(&issued.ticket), None);
    }

    #[test]
    fn what_was_never_issued_opens_nothing_and_spends_nothing() {
        let tickets = tickets();
        let real = tickets.issue(SessionGrant::OWNER).unwrap().ticket;
        let mut one_digit_off = real.clone().into_bytes();
        one_digit_off[63] = if one_digit_off[63] == b'0' {
            b'1'
        } else {
            b'0'
        };
        for made_up in [
            String::new(),
            "0".repeat(64),
            "f".repeat(64),
            real[..63].to_owned(),
            format!("{real}0"),
            "A".repeat(64),
            "g".repeat(64),
            "é".repeat(32),
            String::from_utf8(one_digit_off).unwrap(),
        ] {
            assert_eq!(tickets.redeem(&made_up), None, "{} bytes", made_up.len());
        }
        assert_eq!(tickets.redeem(&real), Some(SessionGrant::OWNER));
    }

    #[test]
    fn with_the_list_full_the_oldest_ticket_gives_way() {
        let tickets = tickets();
        let oldest = tickets.issue(SessionGrant::OWNER).unwrap().ticket;
        let second = tickets.issue(SessionGrant::OWNER).unwrap().ticket;
        for _ in 2..MAX_WAITING {
            tickets.issue(SessionGrant::OWNER).unwrap();
        }
        assert_eq!(tickets.waiting(), MAX_WAITING);

        let newest = tickets.issue(SessionGrant::OWNER).unwrap().ticket;
        assert_eq!(tickets.waiting(), MAX_WAITING);
        assert_eq!(tickets.redeem(&oldest), None);
        assert_eq!(tickets.redeem(&second), Some(SessionGrant::OWNER));
        assert_eq!(tickets.redeem(&newest), Some(SessionGrant::OWNER));
    }

    #[test]
    fn expired_tickets_make_room_for_new_ones() {
        let tickets = tickets();
        let made = Instant::now();
        for _ in 0..MAX_WAITING {
            tickets.issue_at(SessionGrant::OWNER, made).unwrap();
        }
        tickets.issue_at(SessionGrant::OWNER, made + LIFE).unwrap();
        assert_eq!(tickets.waiting(), 1);
    }

    #[test]
    fn what_is_waiting_is_not_printed() {
        let tickets = tickets();
        let issued = tickets.issue(SessionGrant::OWNER).unwrap();
        let printed = format!("{tickets:?}");
        assert!(!printed.contains(&issued.ticket), "{printed}");
        assert_eq!(printed, "Tickets { life: 10s, .. }");
    }
}
