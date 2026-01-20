use std::fmt::Debug;

use lb_blend::scheduling::{
    SessionMessageScheduler,
    message_scheduler::{ProcessedMessageScheduler as _, Settings, session_info::SessionInfo},
};

/// A wrapper around a [`MessageScheduler`] that allows creation with a set of
/// initial messages.
pub struct SchedulerWrapper<Rng, ProcessedMessage, DataMessage> {
    /// The inner message scheduler.
    scheduler: SessionMessageScheduler<Rng, ProcessedMessage, DataMessage>,
}

impl<Rng, ProcessedMessage, DataMessage> SchedulerWrapper<Rng, ProcessedMessage, DataMessage>
where
    Rng: rand::Rng + Clone + Unpin,
    ProcessedMessage: Debug + Unpin,
    DataMessage: Debug + Unpin,
{
    pub fn new_with_initial_messages<ProcessedMessages, DataMessages>(
        session_info: SessionInfo,
        rng: Rng,
        settings: Settings,
        processed_messages: ProcessedMessages,
        data_messages: DataMessages,
    ) -> Self
    where
        ProcessedMessages: Iterator<Item = ProcessedMessage>,
        DataMessages: Iterator<Item = DataMessage>,
    {
        let mut scheduler = SessionMessageScheduler::new(session_info, rng, settings);
        processed_messages.for_each(|m| scheduler.schedule_processed_message(m));
        data_messages.for_each(|m| scheduler.queue_data_message(m));
        Self { scheduler }
    }
}

impl<Rng, ProcessedMessage, DataMessage> From<SchedulerWrapper<Rng, ProcessedMessage, DataMessage>>
    for SessionMessageScheduler<Rng, ProcessedMessage, DataMessage>
{
    fn from(wrapper: SchedulerWrapper<Rng, ProcessedMessage, DataMessage>) -> Self {
        wrapper.scheduler
    }
}
