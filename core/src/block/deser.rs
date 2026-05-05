#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Slot;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use crate::{
        block::{Block, tests::create_proof},
        mantle::MantleTx,
    };

    fn make_empty_block() -> Block<MantleTx> {
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        Block::create(
            [0u8; 32].into(),
            Slot::from(1u64),
            create_proof(),
            vec![],
            &signing_key,
        )
        .expect("block creation should succeed")
    }

    #[test]
    fn test_json_round_trip() {
        let block = make_empty_block();
        let json = serde_json::to_string(&block).expect("JSON serialization should succeed");
        let restored: Block<MantleTx> =
            serde_json::from_str(&json).expect("JSON deserialization should succeed");
        assert_eq!(block.header().id(), restored.header().id());
        assert_eq!(block.signature(), restored.signature());
    }

    #[test]
    fn test_json_signature_is_hex() {
        let block = make_empty_block();
        let json = serde_json::to_string(&block).expect("JSON serialization should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("should parse as JSON");
        let sig = value["signature"]
            .as_str()
            .expect("signature should be a string");
        assert_eq!(sig.len(), 128, "Ed25519 signature hex should be 128 chars");
        assert!(
            sig.chars().all(|c| c.is_ascii_hexdigit()),
            "signature should be hex"
        );
    }

    #[test]
    fn test_json_proof_is_hex() {
        let block = make_empty_block();
        let json = serde_json::to_string(&block).expect("JSON serialization should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).expect("should parse as JSON");
        let proof = value["header"]["proof_of_leadership"]["proof"]
            .as_str()
            .expect("proof should be a string");
        assert_eq!(proof.len(), 256, "PoLProof hex should be 256 chars");
        assert!(
            proof.chars().all(|c| c.is_ascii_hexdigit()),
            "proof should be hex"
        );
    }

    #[test]
    fn test_bincode_round_trip() {
        let block = make_empty_block();
        let bytes = bincode::serialize(&block).expect("bincode serialization should succeed");
        let restored: Block<MantleTx> =
            bincode::deserialize(&bytes).expect("bincode deserialization should succeed");
        assert_eq!(block.header().id(), restored.header().id());
        assert_eq!(block.signature(), restored.signature());
    }
}
