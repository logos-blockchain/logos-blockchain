use lb_api_service::http::mantle::BlockWithChainState;
use lb_chain_service::Slot;
use lb_core::{
    block::{Block, SignedHeaderRef},
    header::{ContentId, HeaderId, HeaderRef},
    mantle::{
        SignedOps, ledger::verification_mode::VerificationMode,
        transactions::states::VerificationState,
    },
    proofs::leader_proof::Groth16LeaderProof,
};
use lb_key_management_system_service::keys::Ed25519Signature;
use serde::Serialize;

use crate::api::serializers::transactions::ApiSignedTransaction;

#[derive(Serialize)]
pub struct ApiBlock<'block> {
    header: ApiHeader<'block>,
    uncle_headers: Vec<ApiSignedHeader<'block>>,
    transactions: Vec<ApiSignedTransaction<'block>>,
}

impl<'block> ApiBlock<'block> {
    pub fn serialize<State: VerificationState, Mode: VerificationMode, Serializer>(
        block: &'block Block<SignedOps<State, Mode>>,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        Self::from(block).serialize(serializer)
    }
}

impl<'block, State: VerificationState, Mode: VerificationMode>
    From<&'block Block<SignedOps<State, Mode>>> for ApiBlock<'block>
{
    fn from(value: &'block Block<SignedOps<State, Mode>>) -> Self {
        let transactions = value
            .transactions()
            .iter()
            .map(ApiSignedTransaction::from)
            .collect();
        Self {
            header: value.header().into(),
            uncle_headers: value.uncle_headers().iter().map(Into::into).collect(),
            transactions,
        }
    }
}

/// The signed header of an uncle a block references.
#[derive(Serialize)]
pub struct ApiSignedHeader<'block> {
    header: ApiHeader<'block>,
    signature: &'block Ed25519Signature,
}

impl<'block> From<SignedHeaderRef<'block>> for ApiSignedHeader<'block> {
    fn from(value: SignedHeaderRef<'block>) -> Self {
        Self {
            header: value.header().into(),
            signature: value.signature(),
        }
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct ApiBlockOwned<State: VerificationState, Mode: VerificationMode> {
    #[serde(with = "ApiBlock")]
    block: Block<SignedOps<State, Mode>>,
}

impl<State: VerificationState, Mode: VerificationMode> From<Block<SignedOps<State, Mode>>>
    for ApiBlockOwned<State, Mode>
{
    fn from(value: Block<SignedOps<State, Mode>>) -> Self {
        Self { block: value }
    }
}

/// A header of any version, as the API shows it.
#[derive(Serialize)]
pub struct ApiHeader<'block> {
    id: HeaderId,
    parent_block: HeaderId,
    slot: Slot,
    body_root: &'block ContentId,
    proof_of_leadership: &'block Groth16LeaderProof,
}

impl<'block> From<HeaderRef<'block>> for ApiHeader<'block> {
    fn from(header: HeaderRef<'block>) -> Self {
        Self {
            id: header.id(),
            parent_block: header.parent(),
            slot: header.slot(),
            body_root: header.body_root(),
            proof_of_leadership: header.leader_proof(),
        }
    }
}

/// API response type for processed block events.
/// Includes the full block along with the current chain state (tip and LIB).
///
/// Note: The first event after subscribing may be an initial snapshot of the
/// current state. In this case, `block.header.id` can equal `tip` and does not
/// represent a newly processed block. Clients should handle events
/// idempotently.
#[derive(Serialize)]
pub struct ApiProcessedBlockEvent<'block, State: VerificationState, Mode: VerificationMode> {
    /// The processed block.
    #[serde(with = "ApiBlock")]
    pub block: &'block Block<SignedOps<State, Mode>>,
    /// The current canonical tip after processing this block.
    pub tip: &'block HeaderId,
    pub tip_slot: &'block Slot,
    /// The current Last Irreversible Block after processing this block.
    pub lib: &'block HeaderId,
    pub lib_slot: &'block Slot,
}

impl<'block, State: VerificationState, Mode: VerificationMode>
    ApiProcessedBlockEvent<'block, State, Mode>
{
    pub fn serialize<Serializer>(
        value: &'block BlockWithChainState<SignedOps<State, Mode>>,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        Self::from(value).serialize(serializer)
    }
}

impl<'block, State: VerificationState, Mode: VerificationMode>
    From<&'block BlockWithChainState<SignedOps<State, Mode>>>
    for ApiProcessedBlockEvent<'block, State, Mode>
{
    fn from(value: &'block BlockWithChainState<SignedOps<State, Mode>>) -> Self {
        Self {
            block: &value.block,
            tip: &value.tip,
            tip_slot: &value.tip_slot,
            lib: &value.lib,
            lib_slot: &value.lib_slot,
        }
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct ApiProcessedBlockEventOwned<State: VerificationState, Mode: VerificationMode> {
    #[serde(with = "ApiProcessedBlockEvent")]
    block_with_chain_state: BlockWithChainState<SignedOps<State, Mode>>,
}

impl<State: VerificationState, Mode: VerificationMode> ApiProcessedBlockEventOwned<State, Mode> {
    #[must_use]
    pub const fn block(&self) -> &Block<SignedOps<State, Mode>> {
        &self.block_with_chain_state.block
    }
}

impl<State: VerificationState, Mode: VerificationMode>
    From<BlockWithChainState<SignedOps<State, Mode>>> for ApiProcessedBlockEventOwned<State, Mode>
{
    fn from(value: BlockWithChainState<SignedOps<State, Mode>>) -> Self {
        Self {
            block_with_chain_state: value,
        }
    }
}
