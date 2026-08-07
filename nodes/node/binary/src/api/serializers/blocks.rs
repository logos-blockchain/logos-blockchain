use lb_api_service::http::mantle::BlockWithChainState;
use lb_chain_service::Slot;
use lb_core::{
    block::Block,
    header::{ContentId, Header, HeaderId},
    mantle::{SignedMantleTx, transactions::states::VerificationState},
    proofs::leader_proof::Groth16LeaderProof,
};
use serde::Serialize;

use crate::api::serializers::transactions::ApiSignedTransaction;

#[derive(Serialize)]
pub struct ApiBlock<'block> {
    #[serde(with = "ApiHeaderSerializer")]
    header: &'block Header,
    transactions: Vec<ApiSignedTransaction<'block>>,
}

impl<'block> ApiBlock<'block> {
    pub fn serialize<State: VerificationState, Serializer>(
        block: &'block Block<SignedMantleTx<State>>,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        Self::from(block).serialize(serializer)
    }
}

impl<'block, State: VerificationState> From<&'block Block<SignedMantleTx<State>>>
    for ApiBlock<'block>
{
    fn from(value: &'block Block<SignedMantleTx<State>>) -> Self {
        let transactions = value
            .transactions()
            .iter()
            .map(ApiSignedTransaction::from)
            .collect();
        Self {
            header: value.header(),
            transactions,
        }
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct ApiBlockOwned<State: VerificationState> {
    #[serde(with = "ApiBlock")]
    block: Block<SignedMantleTx<State>>,
}

impl<State: VerificationState> From<Block<SignedMantleTx<State>>> for ApiBlockOwned<State> {
    fn from(value: Block<SignedMantleTx<State>>) -> Self {
        Self { block: value }
    }
}

#[derive(Serialize)]
#[serde(remote = "Header")]
pub struct ApiHeaderSerializer {
    #[serde(getter = "Header::id")]
    id: HeaderId,
    #[serde(getter = "Header::parent_block")]
    parent_block: HeaderId,
    #[serde(getter = "Header::slot")]
    slot: Slot,
    #[serde(getter = "Header::body_root")]
    body_root: ContentId,
    #[serde(getter = "Header::leader_proof")]
    proof_of_leadership: Groth16LeaderProof,
}

/// API response type for processed block events.
/// Includes the full block along with the current chain state (tip and LIB).
///
/// Note: The first event after subscribing may be an initial snapshot of the
/// current state. In this case, `block.header.id` can equal `tip` and does not
/// represent a newly processed block. Clients should handle events
/// idempotently.
#[derive(Serialize)]
pub struct ApiProcessedBlockEvent<'block, State: VerificationState> {
    /// The processed block.
    #[serde(with = "ApiBlock")]
    pub block: &'block Block<SignedMantleTx<State>>,
    /// The current canonical tip after processing this block.
    pub tip: &'block HeaderId,
    pub tip_slot: &'block Slot,
    /// The current Last Irreversible Block after processing this block.
    pub lib: &'block HeaderId,
    pub lib_slot: &'block Slot,
}

impl<'block, State: VerificationState> ApiProcessedBlockEvent<'block, State> {
    pub fn serialize<Serializer>(
        value: &'block BlockWithChainState<SignedMantleTx<State>>,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        Self::from(value).serialize(serializer)
    }
}

impl<'block, State: VerificationState> From<&'block BlockWithChainState<SignedMantleTx<State>>>
    for ApiProcessedBlockEvent<'block, State>
{
    fn from(value: &'block BlockWithChainState<SignedMantleTx<State>>) -> Self {
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
pub struct ApiProcessedBlockEventOwned<State: VerificationState> {
    #[serde(with = "ApiProcessedBlockEvent")]
    block_with_chain_state: BlockWithChainState<SignedMantleTx<State>>,
}

impl<State: VerificationState> ApiProcessedBlockEventOwned<State> {
    #[must_use]
    pub const fn block(&self) -> &Block<SignedMantleTx<State>> {
        &self.block_with_chain_state.block
    }
}

impl<State: VerificationState> From<BlockWithChainState<SignedMantleTx<State>>>
    for ApiProcessedBlockEventOwned<State>
{
    fn from(value: BlockWithChainState<SignedMantleTx<State>>) -> Self {
        Self {
            block_with_chain_state: value,
        }
    }
}
