use lb_binary_codec::canonical::BinaryDecode as _;
use lb_blend_service::message::{DataPayload, NetworkInfo, ProxyServiceMessage, ServiceMessage};
use lb_core::{
    mantle::{NoteId, TxHash, Value, traits::Hashable as _},
    sdp::{Locator, ServiceType},
};
use lb_network_service::backends::libp2p::PeerId;
use lb_sdp_service::SdpSubmission;
use lb_wallet_service::{TipResponse, api::WalletApi};

use super::{Error, Handle, cryptarchia::chain_api, request};
use crate::{BlendService, RuntimeServiceId, ValidatedTx, WalletService};

pub async fn info(handle: &Handle) -> Result<Option<NetworkInfo<PeerId>>, Error> {
    request::<BlendService, _>(handle, |reply| {
        ServiceMessage::GetNetworkInfo { reply }.into()
    })
    .await
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServiceNoteError {
    #[error("holds no notes")]
    NoNotes,
    #[error("all {locked} notes are already locked for blend")]
    AllLocked { locked: usize },
    #[error(
        "no single note meets min stake {threshold}: largest is {largest}, total {total}; consolidation may be possible"
    )]
    NoSingleNoteMeetsThreshold {
        largest: Value,
        total: Value,
        threshold: Value,
    },
    #[error("insufficient total collateral: {total} < min stake {threshold}")]
    InsufficientTotal { total: Value, threshold: Value },
}

/// Picks the smallest unlocked note that meets `threshold`, so the least
/// collateral is locked and larger notes stay free.
pub fn select_service_note<Id: Copy>(
    notes: &[(Id, Value)],
    is_locked: impl Fn(Id) -> bool,
    threshold: Value,
) -> Result<Id, ServiceNoteError> {
    if notes.is_empty() {
        return Err(ServiceNoteError::NoNotes);
    }

    let unlocked: Vec<_> = notes.iter().filter(|(id, _)| !is_locked(*id)).collect();
    if unlocked.is_empty() {
        return Err(ServiceNoteError::AllLocked {
            locked: notes.len(),
        });
    }

    if let Some((id, _)) = unlocked
        .iter()
        .filter(|(_, value)| *value >= threshold)
        .min_by_key(|(_, value)| *value)
    {
        return Ok(*id);
    }

    let total = unlocked.iter().map(|(_, value)| *value).sum();
    if total >= threshold {
        let largest = unlocked.iter().map(|(_, value)| *value).max().unwrap_or(0);
        return Err(ServiceNoteError::NoSingleNoteMeetsThreshold {
            largest,
            total,
            threshold,
        });
    }

    Err(ServiceNoteError::InsufficientTotal { total, threshold })
}

/// Joins the Blend network as a core node, with `service_note_id` as the
/// collateral or, when `None`, the smallest unlocked note held by this node's
/// blend ZK key that meets the minimum stake.
pub async fn join_network(
    handle: &Handle,
    locator: Locator,
    service_note_id: Option<NoteId>,
) -> Result<SdpSubmission, Error> {
    let service_note_id = match service_note_id {
        Some(note_id) => note_id,
        None => held_service_note(handle).await?,
    };
    request::<BlendService, _>(handle, |reply| ProxyServiceMessage::JoinAsCore {
        locator,
        service_note_id,
        reply,
    })
    .await?
    .map_err(Error::internal)
}

async fn held_service_note(handle: &Handle) -> Result<NoteId, Error> {
    let zk_id =
        request::<BlendService, _>(handle, |reply| ProxyServiceMessage::GetZkId { reply }).await?;

    let wallet =
        WalletApi::<WalletService, RuntimeServiceId>::new(handle.relay::<WalletService>().await?);
    let TipResponse { tip, response } = wallet
        .get_balance(None, zk_id)
        .await
        .map_err(Error::internal)?;
    let balance = response
        .ok_or_else(|| Error::NotFound(format!("wallet does not hold blend ZK key {zk_id:?}")))?;

    let chain = chain_api(handle).await;
    let ledger = chain
        .get_ledger_state(tip)
        .await?
        .ok_or_else(|| Error::internal(format!("no ledger state at tip {tip:?}")))?;
    let threshold = chain
        .get_ledger_config(ledger.slot())
        .await?
        .sdp_config
        .min_stake
        .threshold;
    let locked = ledger.mantle_ledger().service_notes();

    let notes: Vec<_> = balance.notes.into_iter().collect();
    select_service_note(
        &notes,
        |note_id| locked.is_used_for_service(&note_id, &ServiceType::BlendNetwork),
        threshold,
    )
    .map_err(|error| Error::NotFound(format!("blend ZK key {zk_id:?}: {error}")))
}

/// Sends a transaction through the Blend network without adding it to this
/// node's own mempool.
///
/// Returns the transaction's id. Getting one back means the transaction was
/// accepted for blending, not that it was sent.
pub async fn disperse_transaction(
    handle: &Handle,
    transaction: ValidatedTx,
) -> Result<TxHash, Error> {
    // Encoded the same way the mempool gossips transactions, so that whichever
    // node exits this one decodes what it expects.
    let payload = DataPayload::from_transaction(&transaction);

    // Refused right away if it is larger than a payload body of the current
    // epoch.
    request::<BlendService, _>(handle, |reply| ProxyServiceMessage::Blend {
        payload,
        reply,
    })
    .await?
    .map_err(Error::internal)?;

    Ok(transaction.hash())
}

/// The ids of the transactions this node is still waiting on a `PoW` solution
/// for.
///
/// Only those: once a transaction has been encapsulated it is in the
/// scheduler's hands, waiting for a release round, and no longer reported here.
// TODO: Have Blend hand back transactions rather than bytes, so that this can
// report their ids without decoding them again.
pub async fn pending_transactions(handle: &Handle) -> Result<Vec<TxHash>, Error> {
    request::<BlendService, _>(handle, |reply| {
        ServiceMessage::GetPendingTransactions { reply }.into()
    })
    .await?
    .iter()
    .map(|encoded_transaction| {
        // These are transactions this node encoded itself on the way in, so
        // a decoding failure here is this node disagreeing with itself.
        Ok(ValidatedTx::decode_all(encoded_transaction, &u32::MAX)
            .map_err(Error::internal)?
            .hash())
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_locked(_: u8) -> bool {
        false
    }

    #[test]
    fn picks_smallest_note_that_meets_threshold() {
        let notes = [(1u8, 1000), (2, 3000), (3, 10000), (4, 5050)];
        assert_eq!(select_service_note(&notes, never_locked, 5000), Ok(4));
    }

    #[test]
    fn exact_threshold_qualifies() {
        let notes = [(1u8, 5000)];
        assert_eq!(select_service_note(&notes, never_locked, 5000), Ok(1));
    }

    #[test]
    fn total_sufficient_but_no_single_note() {
        let notes = [(1u8, 1000), (2, 2000), (3, 3000)];
        assert_eq!(
            select_service_note(&notes, never_locked, 5000),
            Err(ServiceNoteError::NoSingleNoteMeetsThreshold {
                largest: 3000,
                total: 6000,
                threshold: 5000,
            })
        );
    }

    #[test]
    fn total_insufficient() {
        let notes = [(1u8, 1000), (2, 2000)];
        assert_eq!(
            select_service_note(&notes, never_locked, 5000),
            Err(ServiceNoteError::InsufficientTotal {
                total: 3000,
                threshold: 5000,
            })
        );
    }

    #[test]
    fn locked_notes_are_skipped() {
        let notes = [(1u8, 10000), (2, 6000)];
        assert_eq!(select_service_note(&notes, |id| id == 1, 5000), Ok(2));
    }

    #[test]
    fn all_locked() {
        let notes = [(1u8, 10000)];
        assert_eq!(
            select_service_note(&notes, |_| true, 5000),
            Err(ServiceNoteError::AllLocked { locked: 1 })
        );
    }

    #[test]
    fn no_notes() {
        let notes: [(u8, Value); 0] = [];
        assert_eq!(
            select_service_note(&notes, never_locked, 5000),
            Err(ServiceNoteError::NoNotes)
        );
    }
}
