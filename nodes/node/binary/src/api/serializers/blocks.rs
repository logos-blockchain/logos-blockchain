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
