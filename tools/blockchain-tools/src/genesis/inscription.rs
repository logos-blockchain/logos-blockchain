use core::num::NonZero;

use lb_binary_codec::canonical::BinaryEncode as _;
use lb_config::consensus::{EMPTY_CHANNEL_ID, EMPTY_ED25519_PUBLIC_KEY};
use lb_core::{
    crypto::ZkDigest,
    mantle::{
        GenesisParameters, GenesisTime, Value,
        ops::channel::{
            ChannelId, MsgId,
            inscribe::{Inscription, InscriptionOp, MiniBlocks},
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
    /// `S_cap`: the most tokens the chain will ever hold.
    pub max_supply: Value,
    /// The balance the `PoW` reward pool starts with.
    pub pow_reward_pool: Value,
    /// The balance the rewards reserve starts with.
    pub rewards_reserve: Value,
    /// The storage gas price of the first epoch.
    pub storage_gas_price: NonZero<u64>,
    /// The execution base fee of the first block.
    pub execution_gas_price: NonZero<u64>,
}

#[must_use]
pub fn inscribe<D: ZkDigest>(
    InscribeParams {
        chain_id,
        genesis_time,
        entropy_sources,
        max_supply,
        pow_reward_pool,
        rewards_reserve,
        storage_gas_price,
        execution_gas_price,
    }: InscribeParams,
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
        max_supply,
        pow_reward_pool,
        rewards_reserve,
        storage_gas_price,
        execution_gas_price,
    };

    InscriptionOp {
        channel_id: ChannelId::from(EMPTY_CHANNEL_ID),
        inscription: Inscription::new_unchecked(params.encode_to_vec()),
        parent: MsgId::root(),
        signer: UnverifiedEd25519PublicKey::from_bytes(&EMPTY_ED25519_PUBLIC_KEY)
            .expect("Constant EMPTY_ED25519_PUBLIC_KEY should be valid"),
        mini_blocks: MiniBlocks::default(),
    }
}
