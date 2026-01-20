use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Key, secured_key::SecuredKey as _},
};

#[must_use]
pub fn key_id_for_preload_backend(key: &Key) -> KeyId {
    let key_id_bytes = match key {
        Key::Ed25519(ed25519_secret_key) => ed25519_secret_key.as_public_key().to_bytes(),
        Key::Zk(zk_secret_key) => fr_to_bytes(zk_secret_key.as_public_key().as_fr()),
    };
    hex::encode(key_id_bytes)
}
