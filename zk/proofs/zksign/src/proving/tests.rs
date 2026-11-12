use std::path::Path;

use lb_groth16::{
    Fr, Groth16Proof, Groth16VerificationKey, Groth16VerificationKeyJsonDeser, fr_to_bytes,
    groth16_verify,
};
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
use num_bigint::BigUint;
use rand::RngCore as _;

use super::{ZkSignPrivateKeysData, ZkSignWitnessInputs, prove};
use crate::{batch_verify, verify};

#[test]
fn test_full_flow() {
    let mut rng = rand::thread_rng();
    let sks: [Fr; 32] = std::iter::repeat_with(|| BigUint::from(rng.next_u64()).into())
        .take(32)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let sks: ZkSignPrivateKeysData = sks.into();
    let msg_hash = Poseidon2Bn254Hasher::digest(&[BigUint::from_bytes_le(b"foo_bar").into()]);
    let input = ZkSignWitnessInputs::from_witness_data_and_message_hash(sks, msg_hash);
    let (proof, verifier_inputs) = prove(input).unwrap();
    assert!(verify(&proof, &verifier_inputs).unwrap());
    assert!(batch_verify(&[(proof, verifier_inputs.clone()), (proof, verifier_inputs)]).unwrap());
}

#[test]
#[ignore = "Regenerates checked-in circuit assets; run explicitly after a circuit upgrade"]
fn regenerate_signature_fixture() {
    let keys = core::array::from_fn(|index| Fr::from(index as u64 + 1));
    let inputs = ZkSignWitnessInputs::from_witness_data_and_message_hash(
        ZkSignPrivateKeysData::from(keys),
        Fr::from(42u64),
    );
    let (proof, inputs) = prove(inputs).unwrap();

    // A circuit bump can leave the currently embedded key stale. Validate
    // against the newly pinned native key before replacing either asset.
    let key_bytes = lbc_signature_sys::artifacts::VERIFICATION_KEY;
    let key_json: Groth16VerificationKeyJsonDeser = serde_json::from_slice(key_bytes).unwrap();
    let key: Groth16VerificationKey = key_json.try_into().unwrap();
    let expanded_proof = Groth16Proof::try_from(&proof).unwrap();
    assert!(groth16_verify(&key.into_prepared(), &expanded_proof, &inputs.as_inputs()).unwrap());

    let proof_json = serde_json::to_string(proof.to_bytes().as_slice()).unwrap();
    let input_rows = inputs
        .as_inputs()
        .iter()
        .map(|input| serde_json::to_string(fr_to_bytes(input).as_slice()).unwrap())
        .collect::<Vec<_>>()
        .join(",\n    ");
    let fixture =
        format!("{{\n  \"proof\": {proof_json},\n  \"inputs\": [\n    {input_rows}\n  ]\n}}\n");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    std::fs::write(
        root.join("verification_key/verification_key.json"),
        key_bytes,
    )
    .unwrap();
    std::fs::write(root.join("test_data/signature.json"), fixture).unwrap();
}
