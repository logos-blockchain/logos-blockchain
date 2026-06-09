use lb_cryptarchia_engine::Epoch;

/// Information regarding an epoch that the message scheduler needs to
/// initialize its sub-streams.
#[derive(Debug, Clone, Copy)]
pub struct EpochInfo {
    /// The initial quota that the cover message scheduler will use to
    /// pre-compute the rounds to yield a new message.
    pub core_quota: u64,
    /// The identifier for the current epoch.
    pub epoch: Epoch,
}
