use lb_api_service::http::mantle::BlockWithChainState;
use lb_chain_service::Slot;
use lb_core::{
    block::Block,
    header::{ContentId, Header, HeaderId},
    mantle::{SignedMantleTx, transactions::states::VerificationState},
    proofs::leader_proof::Groth16LeaderProof,
};
use serde::Serialize;

pub mod api_block {
    use serde::ser::SerializeStruct as _;

    use super::{ApiHeaderRef, Block, SignedMantleTx, VerificationState};
    use crate::api::serializers::transactions::adapters::ApiTransactionsRefAdapter;

    pub fn serialize<S, State: VerificationState>(
        block: &Block<SignedMantleTx<State>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Block", 2)?;
        state.serialize_field("header", &ApiHeaderRef::from(block.header()))?;
        state.serialize_field(
            "transactions",
            &ApiTransactionsRefAdapter(block.transactions_vec()),
        )?;
        state.end()
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
    #[serde(getter = "Header::block_root")]
    block_root: ContentId,
    #[serde(getter = "Header::leader_proof")]
    proof_of_leadership: Groth16LeaderProof,
}

#[derive(Serialize)]
struct ApiHeaderRef<'a>(#[serde(with = "ApiHeaderSerializer")] &'a Header);

impl<'a> From<&'a Header> for ApiHeaderRef<'a> {
    fn from(value: &'a Header) -> Self {
        Self(value)
    }
}

#[derive(Serialize)]
pub struct ApiBlock<State: VerificationState>(
    #[serde(with = "api_block")] Block<SignedMantleTx<State>>,
);

impl<State: VerificationState> From<Block<SignedMantleTx<State>>> for ApiBlock<State> {
    fn from(value: Block<SignedMantleTx<State>>) -> Self {
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
pub struct ApiProcessedBlockEvent<State: VerificationState> {
    /// The processed block.
    #[serde(with = "api_block")]
    pub block: Block<SignedMantleTx<State>>,
    /// The current canonical tip after processing this block.
    pub tip: HeaderId,
    pub tip_slot: Slot,
    /// The current Last Irreversible Block after processing this block.
    pub lib: HeaderId,
    pub lib_slot: Slot,
}

impl<State: VerificationState> From<BlockWithChainState<SignedMantleTx<State>>>
    for ApiProcessedBlockEvent<State>
{
    fn from(value: BlockWithChainState<SignedMantleTx<State>>) -> Self {
        Self {
            block: value.block,
            tip: value.tip,
            tip_slot: value.tip_slot,
            lib: value.lib,
            lib_slot: value.lib_slot,
        }
    }
}
