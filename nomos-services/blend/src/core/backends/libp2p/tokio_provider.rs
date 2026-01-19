use core::{num::NonZeroU64, ops::RangeInclusive, time::Duration};
use std::hash::Hash;

use futures::StreamExt as _;
use nomos_blend::{
    network::core::with_core::behaviour::IntervalStreamProvider, scheduling::membership::Membership,
};
use nomos_utils::math::NonNegativeF64;
use tokio_stream::wrappers::IntervalStream;

use crate::core::{
    backends::libp2p::Libp2pBlendBackendSettings, settings::RunningBlendConfig as BlendConfig,
};

#[derive(Clone)]
/// Provider of a stream of observation windows used by the Blend connection
/// monitor to evaluate peers.
///
/// At each interval, it returns the [min,max] (inclusive) range of expected
/// messages from the peer, as per the specification.
pub struct ObservationWindowTokioIntervalProvider {
    round_duration_seconds: NonZeroU64,
    maximal_delay_rounds: NonZeroU64,
    blending_ops_per_message: u64,
    normalization_constant: NonNegativeF64,
    membership_size: NonZeroU64,
    rounds_per_observation_window: NonZeroU64,
    minimum_messages_coefficient: NonZeroU64,
}

impl ObservationWindowTokioIntervalProvider {
    fn calculate_expected_message_range(&self) -> RangeInclusive<u64> {
        // TODO: Remove unsafe arithmetic operations
        let mu = ((self.maximal_delay_rounds.get() as f64
            * self.blending_ops_per_message as f64
            * self.normalization_constant.get())
            / self.membership_size.get() as f64)
            .ceil() as u64;
        (mu * self.minimum_messages_coefficient.get())
            ..=(mu * self.rounds_per_observation_window.get())
    }
}

impl IntervalStreamProvider for ObservationWindowTokioIntervalProvider {
    type IntervalStream =
        Box<dyn futures::Stream<Item = RangeInclusive<u64>> + Send + Unpin + 'static>;
    type IntervalItem = RangeInclusive<u64>;

    fn interval_stream(&self) -> Self::IntervalStream {
        let expected_message_range = self.calculate_expected_message_range();
        Box::new(
            IntervalStream::new(tokio::time::interval(Duration::from_secs(
                (self.rounds_per_observation_window.get()) * self.round_duration_seconds.get(),
            )))
            .map(move |_| expected_message_range.clone()),
        )
    }
}

impl<NodeId>
    From<(
        &BlendConfig<Libp2pBlendBackendSettings>,
        &Membership<NodeId>,
    )> for ObservationWindowTokioIntervalProvider
where
    NodeId: Clone + Eq + Hash,
{
    fn from(
        (config, membership): (
            &BlendConfig<Libp2pBlendBackendSettings>,
            &Membership<NodeId>,
        ),
    ) -> Self {
        Self {
            blending_ops_per_message: config.num_blend_layers.into(),
            maximal_delay_rounds: config.scheduler.delayer.maximum_release_delay_in_rounds,
            // TODO: Replace with a session stream: https://github.com/logos-co/nomos/issues/1533
            membership_size: NonZeroU64::try_from(membership.size() as u64)
                .expect("Membership size cannot be zero."),
            minimum_messages_coefficient: config.backend.minimum_messages_coefficient,
            normalization_constant: config.backend.normalization_constant,
            round_duration_seconds: config
                .time
                .round_duration
                .as_secs()
                .try_into()
                .expect("Round duration cannot be zero."),
            rounds_per_observation_window: config.time.rounds_per_observation_window,
        }
    }
}
