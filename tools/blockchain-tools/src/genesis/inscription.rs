use lb_config::consensus::{EMPTY_CHANNEL_ID, EMPTY_ED25519_PUBLIC_KEY};
use lb_core::{
    crypto::ZkDigest,
    mantle::{
        CryptarchiaParameter,
        ops::channel::{ChannelId, MsgId, inscribe::InscriptionOp},
    },
};
use lb_groth16::{FrBytes, fr_from_bytes};
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use serde_with::{hex::Hex, serde_as};
use time::OffsetDateTime;

#[serde_as]
#[derive(serde::Deserialize)]
pub struct InscribeParams {
    pub chain_id: String,
    #[serde(with = "time::serde::iso8601")]
    pub genesis_time: OffsetDateTime,
    #[serde_as(as = "Vec<Hex>")]
    pub entropy_sources: Vec<FrBytes>,
}

pub fn inscribe<D: ZkDigest>(
    chain_id: String,
    genesis_time: OffsetDateTime,
    entropy_sources: impl IntoIterator<Item = FrBytes>,
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

    let params = CryptarchiaParameter {
        chain_id,
        genesis_time,
        epoch_nonce: genesis_nonce_fr,
    };

    InscriptionOp {
        channel_id: ChannelId::from(EMPTY_CHANNEL_ID),
        inscription: params
            .encode()
            .try_into()
            .expect("CryptarchiaParameter encoding exceeded MAX_BYTES"),
        parent: MsgId::root(),
        signer: Ed25519PublicKey::from_bytes(&EMPTY_ED25519_PUBLIC_KEY)
            .expect("Constant EMPTY_ED25519_PUBLIC_KEY should be valid"),
    }
}
