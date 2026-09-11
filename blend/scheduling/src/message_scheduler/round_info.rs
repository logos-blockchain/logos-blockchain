pub use lb_blend_primitives::time::{Round, RoundStream};

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
