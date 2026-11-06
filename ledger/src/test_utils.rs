//! Test-only helpers shared by the crate's tests.

use std::{num::NonZero, time::Duration};

use lb_core::{
    block::{SignedHeader, UncleHeaders},
    crypto::{Digest as _, Hasher},
    era::ForkDigest,
    header::{ContentId, Header, HeaderId, VerifiedBlockHeader},
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof},
};
use lb_cryptarchia_engine::{EraSchedule, Slot, UncleSlots};
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraTiming};
use time::OffsetDateTime;

use crate::config::{Config, EraScheduledConfig};

/// The fork digest the tests' transactions carry.
pub const FORK_DIGEST: ForkDigest = ForkDigest::new([0x11; 32]);

/// A schedule running `genesis` from genesis, then each config of
/// `after_genesis` from the epoch it is paired with.
///
/// The ledger reads neither the slot durations nor the genesis time, so they
/// are left at 1 s and the Unix epoch.
#[must_use]
pub fn schedule(
    genesis: Config,
    after_genesis: impl IntoIterator<Item = (u32, Config)>,
) -> EraScheduledConfig {
    let timing = |config: &Config| EraTiming {
        slot_duration: Duration::from_secs(1),
        epoch_length: EpochLength::epoch_length(config),
    };
    let after_genesis = after_genesis.into_iter().collect::<Vec<_>>();
    let timings_after_genesis =
        EraEntriesAfterGenesis::try_from_iter(after_genesis.iter().map(|(first_epoch, config)| {
            (
                NonZero::new(*first_epoch).expect("an era after genesis starts after epoch 0"),
                timing(config),
            )
        }))
        .expect("each epoch starts one era at most");
    let configs = std::iter::once(genesis)
        .chain(after_genesis.into_iter().map(|(_, config)| config))
        .collect::<Vec<_>>();
    EraSchedule::new(
        OffsetDateTime::UNIX_EPOCH,
        timing(&configs[0]),
        timings_after_genesis,
    )
    .expect("eras starting within the last slot resolve")
    .map(|era| configs[usize::from(era.era.into_inner())].clone())
}

/// A schedule running `config` alone, from genesis.
#[must_use]
pub fn single_era(config: Config) -> EraScheduledConfig {
    schedule(config, [])
}

/// Creates [`VerifiedBlockHeader`] by skipping the verifications.
///
/// It carries an uncle at each of `uncle_slots`, and `salt` sets its body root,
/// so that blocks at the same slot on the same parent get different ids.
#[must_use]
pub fn verified_header(
    parent: impl Into<HeaderId>,
    slot: Slot,
    proof: &impl LeaderProof,
    uncle_slots: &UncleSlots,
    salt: &[u8],
) -> VerifiedBlockHeader {
    let parent = parent.into();
    let body_root: [u8; 32] = Hasher::new()
        .chain_update(<[u8; 32]>::from(parent))
        .chain_update(slot.to_le_bytes())
        .chain_update(salt)
        .finalize()
        .into();
    let signature = Ed25519Signature::from_bytes(&[0; 64]);
    let uncles = uncle_slots
        .iter()
        .map(|uncle_slot| {
            SignedHeader::new(
                Header::new(
                    parent,
                    ContentId::from([0; 32]),
                    *uncle_slot,
                    Groth16LeaderProof::genesis(),
                ),
                signature,
            )
        })
        .collect();
    VerifiedBlockHeader::new_unchecked(
        Header::new(
            parent,
            ContentId::from(body_root),
            slot,
            Groth16LeaderProof::dummy(proof.entropy(), *proof.leader_key(), *proof.voucher_cm()),
        ),
        signature,
        UncleHeaders::try_new(uncles, u8::MAX).expect("uncles within the bound"),
    )
}
