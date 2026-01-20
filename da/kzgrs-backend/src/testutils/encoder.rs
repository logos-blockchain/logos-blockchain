use crate::encoder;

const ENCODER_DOMAIN_SIZE: usize = 16;

#[must_use]
pub fn get_encoder() -> encoder::DaEncoder {
    let params = encoder::DaEncoderParams::default_with(ENCODER_DOMAIN_SIZE);
    encoder::DaEncoder::new(params)
}
