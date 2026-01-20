use core::{
    fmt,
    fmt::{Display, Formatter},
};

use futures::Stream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Round(u128);

impl Round {
    #[must_use]
    pub const fn inner(&self) -> u128 {
        self.0
    }
}

impl From<u128> for Round {
    fn from(value: u128) -> Self {
        Self(value)
    }
}

impl From<Round> for u128 {
    fn from(round: Round) -> Self {
        round.0
    }
}

impl Display for Round {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Information can the message scheduler can yield when being polled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundInfo<ProcessedMessage, DataMessage> {
    /// The list of data messages to be released. This can happen at any round.
    pub data_messages: Vec<DataMessage>,
    /// Additional "types" of this round.
    pub release_type: Option<RoundReleaseType<ProcessedMessage>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundReleaseType<ProcessedMessage> {
    OnlyProcessedMessages(Vec<ProcessedMessage>),
    OnlyCoverMessage,
    ProcessedAndCoverMessages(Vec<ProcessedMessage>),
}

impl<ProcessedMessage> RoundReleaseType<ProcessedMessage> {
    #[must_use]
    pub fn into_components(self) -> (Vec<ProcessedMessage>, bool) {
        match self {
            Self::OnlyCoverMessage => (vec![], true),
            Self::OnlyProcessedMessages(processed_messages) => (processed_messages, false),
            Self::ProcessedAndCoverMessages(processed_messages) => (processed_messages, true),
        }
    }
}

pub type RoundClock = Box<dyn Stream<Item = Round> + Send + Unpin>;
