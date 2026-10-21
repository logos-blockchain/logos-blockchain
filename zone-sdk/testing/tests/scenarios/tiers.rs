//! The catalogue checks itself, and the second tier: core stories on
//! every ordered pair of base conditions.

use super::{configs::*, notes::*, publishing::*, *};

/// Every catalogue entry does what it says: with a sequencer posting on a
/// quiet channel for three cycles, the chain's own expectation holds and
/// every layer still does.
pub async fn chain_does_what_it_says(chain: Chain) {
    let steps = chain.steps_to_expect();
    let mut world = World::new(chain);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world
        .publish(a, "opens the channel")
        .await
        .expect("publish");
    world.steps(steps).await;
    world.settle_chain(200).await;
    world.finish();
}
scenario!(s00_chain_does_what_it_says, chain_does_what_it_says);

pairs!(p01_publish_and_index, publish_and_index);
pairs!(p11_config_orphans_pending, config_orphans_pending);
pairs!(
    p16_channel_wallet_tracks_deposits,
    channel_wallet_tracks_deposits
);
pairs!(p19_finalized_withdraw, finalized_withdraw);
