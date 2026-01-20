use core::{
    fmt,
    fmt::{Display, Formatter},
};

use futures::Stream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Session(u128);

impl From<u128> for Session {
    fn from(value: u128) -> Self {
        Self(value)
    }
}

impl From<Session> for u128 {
    fn from(session: Session) -> Self {
        session.0
    }
}

impl Display for Session {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Information regarding a session that the message scheduler needs to
/// initialize its sub-streams.
#[derive(Debug, Clone, Copy)]
pub struct SessionInfo {
    /// The initial quota that the cover message scheduler will use to
    /// pre-compute the rounds to yield a new message.
    pub core_quota: u64,
    /// The identifier for the current session.
    pub session_number: Session,
}

pub type SessionClock = Box<dyn Stream<Item = SessionInfo> + Unpin>;
