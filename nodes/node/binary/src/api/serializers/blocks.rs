use lb_api_service::http::mantle::BlockWithChainState;
use lb_chain_service::Slot;
use lb_core::{
    block::Block,
    header::{ContentId, Header, HeaderId},
    mantle::SignedMantleTx,
    proofs::leader_proof::Groth16LeaderProof,
};
use serde::Serialize;

#[derive(Serialize)]
#[serde(remote = "Block<SignedMantleTx>")]
pub struct ApiBlockSerializer {
    #[serde(getter = "Block::header")]
    #[serde(with = "ApiHeaderSerializer")]
    header: Header,
    #[serde(getter = "Block::transactions_vec")]
    #[serde(with = "crate::api::serializers::transactions::signed_api_transaction_vec")]
    transactions: Vec<SignedMantleTx>,
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
    #[serde(getter = "Header::block_root")]
    block_root: ContentId,
    #[serde(getter = "Header::leader_proof")]
    proof_of_leadership: Groth16LeaderProof,
}

#[derive(Serialize)]
pub struct ApiBlock(#[serde(with = "ApiBlockSerializer")] Block<SignedMantleTx>);

impl From<Block<SignedMantleTx>> for ApiBlock {
    fn from(value: Block<SignedMantleTx>) -> Self {
        Self(value)
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
pub struct ApiProcessedBlockEvent {
    /// The processed block.
    #[serde(with = "ApiBlockSerializer")]
    pub block: Block<SignedMantleTx>,
    /// The current canonical tip after processing this block.
    pub tip: HeaderId,
    /// The current Last Irreversible Block after processing this block.
    pub lib: HeaderId,
}

impl From<BlockWithChainState<SignedMantleTx>> for ApiProcessedBlockEvent {
    fn from(value: BlockWithChainState<SignedMantleTx>) -> Self {
        Self {
            block: value.block,
            tip: value.tip,
            lib: value.lib,
        }
    }
}
