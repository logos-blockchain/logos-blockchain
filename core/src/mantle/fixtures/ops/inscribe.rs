use lb_binary_codec::canonical::codec_fixtures;

use crate::mantle::ops::{channel::inscribe::Inscription, inscribe::InscribeOp};

// The `INSCRIBE` payload of the Mantle test vectors: a delivery inscription
// after its length.
codec_fixtures!(
    InscribeOp,
    context = u32::MAX,
    Self {
        inscription: Inscription::new_unchecked(
            hex::decode("4e4f54455f44454c49564552595f5631a352841f541aba1aab7d3abbb1eb4487e71756050f5c5acb757a62f8d8a14f91").unwrap(),
        ),
    } => "300000004e4f54455f44454c49564552595f5631a352841f541aba1aab7d3abbb1eb4487e71756050f5c5acb757a62f8d8a14f91",
    Self {
        inscription: Inscription::new_unchecked(vec![0xff]),
    } => "01000000ff",
);
