use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes};
use nom::{IResult, Parser as _, bytes::complete::take, combinator::map_res};

pub fn decode_field_element(input: &[u8]) -> IResult<&[u8], Fr> {
    // FieldElement = 32BYTE
    map_res(take(32usize), |bytes: &[u8]| {
        fr_from_bytes(bytes).map_err(|_| "Invalid field element")
    })
    .parse(input)
}

pub fn encode_field_element(fr: &Fr) -> Vec<u8> {
    fr_to_bytes(fr).to_vec()
}
