use lb_blend::provers::crypto::EncapsulatedMessageWithVerifiedPublicHeader;

use crate::message::ProcessedMessage;

pub mod retiring;
pub mod running;
pub mod transitioning;

type OldEpochScheduler<Rng> = lb_blend::scheduling::message_scheduler::OldEpochMessageScheduler<
    Rng,
    ProcessedMessage,
    EncapsulatedMessageWithVerifiedPublicHeader,
>;
