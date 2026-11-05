use lb_binary_codec::canonical::BinaryEncode as _;
use lb_config::consensus::{EMPTY_CHANNEL_ID, EMPTY_ED25519_PUBLIC_KEY};
use lb_core::{
    crypto::ZkDigest,
    mantle::{
        ConsensusExcludedKeys, GenesisParameters, GenesisTime,
        ops::{
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
            pow::PowReward,
        },
        transactions::genesis_tx::ChainId,
    },
};
use lb_groth16::{FrBytes, fr_from_bytes};
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use serde_with::{hex::Hex, serde_as};
use time::OffsetDateTime;

#[serde_as]
#[derive(serde::Deserialize, Debug)]
pub struct InscribeParams {
    pub chain_id: ChainId,
    #[serde(with = "time::serde::iso8601")]
    pub genesis_time: OffsetDateTime,
    #[serde_as(as = "Vec<Hex>")]
    pub entropy_sources: Vec<FrBytes>,
    /// The balance the `PoW` reward pool starts with.
    pub pow_reward_pool: PowReward,
}

pub fn inscribe<D: ZkDigest>(
    chain_id: ChainId,
    genesis_time: OffsetDateTime,
    entropy_sources: impl IntoIterator<Item = FrBytes>,
    pow_reward_pool: PowReward,
    consensus_excluded_keys: ConsensusExcludedKeys,
) -> InscriptionOp {
    let mut hasher = D::new();

    let mut entropy_sources = entropy_sources.into_iter().peekable();
    assert!(
        entropy_sources.peek().is_some(),
        "Entropy sources must contain at least one item"
    );
    for bytes in entropy_sources {
        let fr = fr_from_bytes(&bytes).expect("Invalid entropy source for Fr conversion");
        hasher.update(&fr);
    }

    let genesis_nonce_fr = hasher.finalize();

    let genesis_time = GenesisTime::try_from(genesis_time).expect("Invalid genesis time");

    let params = GenesisParameters {
        chain_id,
        start_time: genesis_time,
        epoch_nonce: genesis_nonce_fr,
        pow_reward_pool,
        consensus_excluded_keys,
    };

    InscriptionOp {
        channel_id: ChannelId::from(EMPTY_CHANNEL_ID),
        inscription: params
            .encode_to_vec()
            .try_into()
            .expect("GenesisParameters encoding exceeded MAX_BYTES"),
        parent: MsgId::root(),
        signer: UnverifiedEd25519PublicKey::from_bytes(&EMPTY_ED25519_PUBLIC_KEY)
            .expect("Constant EMPTY_ED25519_PUBLIC_KEY should be valid"),
    }
}
