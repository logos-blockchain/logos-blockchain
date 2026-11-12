use lb_groth16::{AdditiveGroup as _, Fr};
use lb_zksign::{ZkSignError, ZkSignPrivateKeysData, ZkSignWitnessInputs};

use super::{Signature, UnsecuredZkKey, ZkKey, private::SecretKey};
use crate::keys::SigningKey;

pub(super) fn sign_secret_keys(keys: &[SecretKey], data: &Fr) -> Result<Signature, ZkSignError> {
    let sk_inputs = try_from_secret_keys(keys)?;
    let inputs = ZkSignWitnessInputs::from_witness_data_and_message_hash(sk_inputs, *data);
    let (signature, _) = lb_zksign::prove(inputs).expect("Signature should succeed");
    Ok(Signature::new(signature))
}

impl SigningKey for ZkKey {
    fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error> {
        Ok(self.0.sign(payload)?)
    }

    fn sign_multiple(
        keys: &[&Self],
        payload: &Self::Payload,
    ) -> Result<Self::Signature, Self::Error> {
        Ok(UnsecuredZkKey::multi_sign(
            &keys.iter().map(|key| key.0.clone()).collect::<Vec<_>>(),
            payload,
        )?)
    }
}

fn try_from_secret_keys(keys: &[SecretKey]) -> Result<ZkSignPrivateKeysData, ZkSignError> {
    let len = keys.len();
    if len > 32 {
        return Err(ZkSignError::TooManyKeys(len));
    }
    let mut buff: [Fr; 32] = [Fr::ZERO; 32];
    for (i, sk) in keys.iter().enumerate() {
        buff[i] = sk.clone().into_inner();
    }
    Ok(buff.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::zk::public_inputs_from_pks;

    #[test]
    fn signing_trait_returns_verifiable_signatures() {
        let keys = [ZkKey::new(Fr::from(1u64)), ZkKey::new(Fr::from(2u64))];
        let payload = Fr::from(42u64);
        let signature = SigningKey::sign(&keys[0], &payload).unwrap();
        let inputs = public_inputs_from_pks(payload.into(), &[keys[0].to_public_key()]).unwrap();
        assert!(lb_zksign::verify(signature.as_proof(), &inputs).unwrap());

        let signature = ZkKey::sign_multiple(&[&keys[0], &keys[1]], &payload).unwrap();
        let inputs =
            public_inputs_from_pks(payload.into(), &keys.map(|key| key.to_public_key())).unwrap();
        assert!(lb_zksign::verify(signature.as_proof(), &inputs).unwrap());
    }
}
