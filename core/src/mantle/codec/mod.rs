pub mod crypto;
pub mod primitives;

#[cfg(test)]
mod tests {
    use crate::mantle::{
        nom::NomDecode as _,
        ops::{channel::inscribe::InscriptionOp, sdp::SDPActiveOp},
    };

    #[test]
    fn test_decode_memory_safety_no_allocation_on_oversized_length() {
        // This test verifies that we reject oversized lengths WITHOUT
        // attempting to allocate the memory first

        // Test with an astronomically large inscription_len
        // (e.g., 4GB which would cause the original bug)
        let huge_len = u32::MAX; // 4GB - 1

        let mut malicious_input = Vec::new();
        malicious_input.extend_from_slice(&[0x42; 32]); // ChannelId
        malicious_input.extend_from_slice(&huge_len.to_le_bytes());

        // This should fail immediately without trying to allocate 4GB
        let result = InscriptionOp::decode(&malicious_input);
        assert!(result.is_err(), "Should reject huge inscription length");

        // Similar test for metadata
        let mut malicious_input2 = Vec::new();
        malicious_input2.extend_from_slice(&[0x42; 32]); // DeclarationId
        malicious_input2.extend_from_slice(&42u64.to_le_bytes()); // Nonce
        malicious_input2.extend_from_slice(&huge_len.to_le_bytes());

        let result2 = SDPActiveOp::decode(&malicious_input2);
        assert!(result2.is_err(), "Should reject huge metadata length");
    }
}
