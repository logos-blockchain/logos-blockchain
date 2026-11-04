use lb_binary_codec::canonical::{BinaryDecode as _, codec_fixtures};

use crate::mantle::programs::ProgramVerificationKey;

/// The key of the "Program Id" vector of Mantle: alpha is `[1]G1`, beta, gamma
/// and delta are `[2]G2`, `[3]G2` and `[4]G2`, the ic points `[5]G1` to `[8]G1`.
const SAMPLE_KEY_HEX: &str = "0100000000000000000000000000000000000000000000000000000000000000b9b3b4620913f849ee2aa6a9cfd35c9d146f3e7c27596cc3e8d311fd3472dc2779ad28398ced57998435d8c63164b86d7033733ab82101b6379bf1b45d203ea0f5c7fb9406fade12005e9ab08c477e8d5e7192e12628e551900eb14d784e0606856eb6ff324f82c9a7a078686b1504bcebe4cf5dcd9151734297bb572f7714102ce236b4ddb40977fd454c08e570151342c4cc50a86380c3472f6a6ff8dbdd296f237195c2856d5a3f8ca190f6666f2084d776f9935a48a2d57a569e476806a9a93f16faa7a849e89ca35389d8dee46243772b760402bc66f7e0fe0edf39c117f94a688c5b3dfe2a762ab3d315927156987792e7d92f79e0f4523f1a41caf40978e0ffab866b3a9876bd01b8ecc66fcb86936277f425539a758dbbd32e2b0717636a302c88373904f75a15fe21c13882a8fe9c3be9f572f4100c48231dd5b188";

pub fn sample_key() -> ProgramVerificationKey {
    let bytes = hex::decode(SAMPLE_KEY_HEX).expect("the sample key is hex");
    ProgramVerificationKey::decode_all(&bytes, &()).expect("the sample key is canonical")
}

codec_fixtures!(
    ProgramVerificationKey,
    sample_key() => SAMPLE_KEY_HEX
);
