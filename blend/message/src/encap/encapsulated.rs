use core::num::NonZeroU64;

use derivative::Derivative;
use itertools::Itertools as _;
use lb_blend_crypto::{ZkHash, cipher::Cipher, pseudo_random_sized_bytes, random_sized_bytes};
use lb_blend_proofs::{
    quota::{self, VerifiedProofOfQuota},
    selection::{self, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_codec::{BinaryDecode, BinaryEncode, DecodeError, take};
use lb_key_management_system_keys::keys::{
    Ed25519PublicKey, Ed25519Signature, SharedKey, UnsecuredEd25519Key,
};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::{
    Error, PayloadType,
    crypto::{domains, key_ext::SharedKeyExt as _},
    encap::{
        ProofsVerifier,
        decapsulated::{PartDecapsulationOutput, PrivateHeaderDecapsulationOutput},
        validated::{
            EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
        },
    },
    input::EncapsulationInput,
    message::{
        BlendingHeader, Payload, PublicHeader,
        blending_header::BLENDING_HEADER_ENCODED_SIZE,
        payload::{PAYLOAD_ENCODED_SIZE, PaddedPayloadBody},
        public_header::VerifiedPublicHeader,
    },
};

pub type MessageIdentifier = ZkHash;

/// An unverified encapsulated message that is received from a peer.
#[derive(Derivative, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[derivative(Debug)]
pub struct EncapsulatedMessage {
    /// A public header that is not encapsulated.
    public_header: PublicHeader,
    /// Encapsulated parts
    #[derivative(Debug = "ignore")] // too long
    encapsulated_part: EncapsulatedPart,
}

impl EncapsulatedMessage {
    #[must_use]
    pub const fn from_components(
        public_header: PublicHeader,
        encapsulated_part: EncapsulatedPart,
    ) -> Self {
        Self {
            public_header,
            encapsulated_part,
        }
    }

    /// Consume the message to return its components.
    #[must_use]
    pub fn into_components(self) -> (PublicHeader, EncapsulatedPart) {
        (self.public_header, self.encapsulated_part)
    }

    /// Verify the message public header signature.
    pub fn verify_header_signature(
        self,
    ) -> Result<EncapsulatedMessageWithVerifiedSignature, Error> {
        let public_header_with_verified_signature =
            self.public_header.verify_signature(&signing_body(
                &self.encapsulated_part.private_header,
                &self.encapsulated_part.payload,
            ))?;
        Ok(EncapsulatedMessageWithVerifiedSignature::from_components(
            public_header_with_verified_signature,
            self.encapsulated_part,
        ))
    }

    /// Verify the message public header.
    pub fn verify_public_header<Verifier>(
        self,
        verifier: &Verifier,
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error>
    where
        Verifier: ProofsVerifier,
    {
        // Verify signature according to the Blend spec: <https://lip.logos.co/blockchain/raw/blend-protocol.html#processing>.
        self.public_header.verify_signature(&signing_body(
            &self.encapsulated_part.private_header,
            &self.encapsulated_part.payload,
        ))?;
        let (_, signing_key, proof_of_quota, signature) = self.public_header.into_components();
        // Verify the Proof of Quota according to the Blend spec: <https://lip.logos.co/blockchain/raw/blend-protocol.html#processing>.
        let verified_proof_of_quota = verifier
            .verify_proof_of_quota(proof_of_quota, &signing_key)
            .map_err(|_| Error::ProofOfQuotaVerificationFailed(quota::Error::InvalidProof))?;
        let verified_public_header =
            VerifiedPublicHeader::new(verified_proof_of_quota, signing_key, signature);
        Ok(
            EncapsulatedMessageWithVerifiedPublicHeader::from_components(
                verified_public_header,
                self.encapsulated_part,
            ),
        )
    }

    #[must_use]
    pub const fn id(&self) -> MessageIdentifier {
        self.public_header.proof_of_quota().key_nullifier()
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub const fn public_header_mut(&mut self) -> &mut PublicHeader {
        &mut self.public_header
    }
}

// Encoding (and sending) of unverified messages should not be done outside of
// tests, so this impl is only available in tests.
#[cfg(test)]
impl BinaryEncode for EncapsulatedMessage {
    fn encoded_length(&self) -> usize {
        self.public_header
            .encoded_length()
            .checked_add(self.encapsulated_part.encoded_length())
            .unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.public_header.encode_into(out);
        self.encapsulated_part.encode_into(out);
    }
}

impl BinaryDecode for EncapsulatedMessage {
    type Context = NonZeroU64;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, public_header) = PublicHeader::decode(input, &())?;
        let (input, encapsulated_part) = EncapsulatedPart::decode(input, context)?;
        Ok((
            input,
            Self {
                public_header,
                encapsulated_part,
            },
        ))
    }
}

/// Part of the message that should be encapsulated.
// TODO: Consider having `InitializedPart` that just finished the initialization step and doesn't
// have `decapsulate` method.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EncapsulatedPart {
    private_header: EncapsulatedPrivateHeader,
    payload: EncapsulatedPayload,
}

impl EncapsulatedPart {
    #[cfg(test)]
    #[must_use]
    pub fn new_unchecked(
        inputs: &[EncapsulationInput],
        payload_type: PayloadType,
        payload_body: PaddedPayloadBody,
        num_layers: usize,
    ) -> Self {
        Self {
            private_header: EncapsulatedPrivateHeader::new_unchecked(inputs, num_layers),
            payload: EncapsulatedPayload::initialize(&Payload::new(payload_type, payload_body)),
        }
    }

    /// Initializes the encapsulated part as preparation for actual
    /// encapsulations.
    ///
    /// `num_layers` is `ß_max`, the layer count every message on the wire
    /// carries regardless of how many times it is actually encapsulated.
    ///
    /// It returns an error if the slice of inputs is empty or holds more than
    /// `num_layers` inputs.
    pub(crate) fn try_initialize(
        inputs: &[EncapsulationInput],
        payload_type: PayloadType,
        payload_body: PaddedPayloadBody,
        num_layers: usize,
    ) -> Result<Self, Error> {
        Ok(Self {
            private_header: EncapsulatedPrivateHeader::try_initialize(inputs, num_layers)?,
            payload: EncapsulatedPayload::initialize(&Payload::new(payload_type, payload_body)),
        })
    }

    /// Add a layer of encapsulation.
    pub(crate) fn encapsulate(
        self,
        shared_key: &SharedKey,
        signing_key: &UnsecuredEd25519Key,
        proof_of_quota: &VerifiedProofOfQuota,
        proof_of_selection: VerifiedProofOfSelection,
        is_last: bool,
    ) -> Self {
        // Compute the signature of the current encapsulated part.
        let signature = self.sign(signing_key);

        // Encapsulate the private header.
        let private_header = self.private_header.encapsulate(
            shared_key,
            signing_key.public_key(),
            proof_of_quota,
            signature,
            proof_of_selection,
            is_last,
        );

        // Encapsulate the payload.
        let encapsulated_payload = self
            .payload
            .encapsulate(&mut shared_key.cipher(domains::PAYLOAD));

        Self {
            private_header,
            payload: encapsulated_payload,
        }
    }

    /// Decapsulate a layer.
    pub(super) fn decapsulate<Verifier>(
        self,
        key: &SharedKey,
        posel_verification_input: &VerifyInputs,
        verifier: &Verifier,
    ) -> Result<PartDecapsulationOutput, Error>
    where
        Verifier: ProofsVerifier,
    {
        match self
            .private_header
            .decapsulate(key, posel_verification_input, verifier)?
        {
            PrivateHeaderDecapsulationOutput::Incompleted {
                encapsulated_private_header,
                public_header,
                verified_proof_of_selection,
            } => {
                let decapsulated_payload =
                    self.payload.decapsulate(&mut key.cipher(domains::PAYLOAD));
                verify_intermediate_reconstructed_public_header(
                    &public_header,
                    &encapsulated_private_header,
                    &decapsulated_payload,
                    verifier,
                )?;
                Ok(PartDecapsulationOutput::Incompleted {
                    encapsulated_part: Self {
                        private_header: encapsulated_private_header,
                        payload: decapsulated_payload,
                    },
                    public_header: Box::new(public_header),
                    verified_proof_of_selection,
                })
            }
            PrivateHeaderDecapsulationOutput::Completed {
                encapsulated_private_header,
                public_header,
                verified_proof_of_selection,
            } => {
                let decapsulated_payload =
                    self.payload.decapsulate(&mut key.cipher(domains::PAYLOAD));
                verify_last_reconstructed_public_header(
                    &public_header,
                    &encapsulated_private_header,
                    &decapsulated_payload,
                )?;
                Ok(PartDecapsulationOutput::Completed {
                    payload: decapsulated_payload.try_deserialize()?,
                    verified_proof_of_selection,
                })
            }
        }
    }

    /// Signs the encapsulated part using the provided key.
    pub(crate) fn sign(&self, key: &UnsecuredEd25519Key) -> Ed25519Signature {
        key.sign_payload(&signing_body(&self.private_header, &self.payload))
    }
}

impl BinaryEncode for EncapsulatedPart {
    fn encoded_length(&self) -> usize {
        self.private_header
            .encoded_length()
            .checked_add(self.payload.encoded_length())
            .unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.private_header.encode_into(out);
        self.payload.encode_into(out);
    }
}

impl BinaryDecode for EncapsulatedPart {
    type Context = NonZeroU64;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, private_header) = EncapsulatedPrivateHeader::decode(input, context)?;
        let (input, payload) = EncapsulatedPayload::decode(input, &())?;
        Ok((
            input,
            Self {
                private_header,
                payload,
            },
        ))
    }
}

/// Verify the public header reconstructed when decapsulating all but the very
/// last private header.
///
/// Verification includes everything that is verified in
/// [`verify_last_reconstructed_public_header`], plus the `PoQ` of the
/// reconstructed header.
fn verify_intermediate_reconstructed_public_header<Verifier>(
    public_header: &PublicHeader,
    private_header: &EncapsulatedPrivateHeader,
    payload: &EncapsulatedPayload,
    verifier: &Verifier,
) -> Result<(), Error>
where
    Verifier: ProofsVerifier,
{
    verify_last_reconstructed_public_header(public_header, private_header, payload)?;
    // Verify the proof of quota in the reconstructed public header
    tracing::trace!("Verifying proof of quota of intermediate reconstructed public header.");
    public_header.verify_proof_of_quota(verifier)?;
    Ok(())
}

/// Verify the public header reconstructed when decapsulating the last private
/// header _only_.
///
/// Verification includes the signature over the private header and the
/// decapsulated payload, using the verification key included in the outer
/// public header.
fn verify_last_reconstructed_public_header(
    public_header: &PublicHeader,
    private_header: &EncapsulatedPrivateHeader,
    payload: &EncapsulatedPayload,
) -> Result<(), Error> {
    // Verify the signature in the reconstructed public header
    public_header.verify_signature(&signing_body(private_header, payload))?;
    Ok(())
}

/// Returns the body that should be signed.
fn signing_body(
    private_header: &EncapsulatedPrivateHeader,
    payload: &EncapsulatedPayload,
) -> Vec<u8> {
    private_header
        .iter_bytes()
        .chain(payload.iter_bytes())
        .collect::<Vec<_>>()
}

/// An encapsulated private header, which is a set of encapsulated blending
/// headers.
// TODO: Consider having `InitializedPrivateHeader`
// that just finished the initialization step and doesn't have `decapsulate` method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct EncapsulatedPrivateHeader(Box<[EncapsulatedBlendingHeader]>);

impl EncapsulatedPrivateHeader {
    #[cfg(test)]
    pub fn new_unchecked(inputs: &[EncapsulationInput], num_layers: usize) -> Self {
        Self::from_inputs(inputs, num_layers)
    }

    /// Initializes the private header as preparation for actual encapsulations.
    ///
    /// It returns an error if the slice of inputs is empty or holds more than
    /// `num_layers` inputs.
    pub(crate) fn try_initialize(
        inputs: &[EncapsulationInput],
        num_layers: usize,
    ) -> Result<Self, Error> {
        if inputs.is_empty() {
            return Err(Error::EmptyEncapsulationInputs);
        }
        if inputs.len() > num_layers {
            return Err(Error::EncapsulationCountExceeded);
        }

        Ok(Self::from_inputs(inputs, num_layers))
    }

    // Randomize the private header, then fill the last `inputs.len()` blending
    // headers in the reconstructable way, so that the corresponding signatures can
    // be verified later. Plus, encapsulate those last `inputs.len()` headers.
    //
    // The private header always holds `num_layers` (`ß_max`) blending headers,
    // however many times the message is actually encapsulated. When the sender
    // encapsulates fewer times, the leading `ß_max - inputs.len()` headers are
    // filled with non-reconstructable random bytes, so the encapsulation count
    // leaks neither through the message size (constant) nor through the header
    // contents. This follows steps 2-4 of the Message Initialization section of the
    // spec: <https://github.com/logos-co/logos-lips/blob/master/docs/blockchain/raw/message-encapsulation.md>.
    //
    // Example: for `num_layers` 3 and 2 inputs,
    // BlendingHeaders[0]: RANDOM
    // BlendingHeaders[1]: Enc(inputs[1], Enc(inputs[0], RND(inputs[1])))
    // BlendingHeaders[2]:               Enc(inputs[0], RND(inputs[0]))
    //
    // Notation:
    // - RANDOM: Pseudo-random bytes generated from fresh entropy, reconstructable
    //   by nobody
    // - RND(seed): Pseudo-random bytes generated from `seed` with the `HEADER` DST
    // - Enc(key, data): Encrypt `data` by XOR-ing with RND(key)
    fn from_inputs(inputs: &[EncapsulationInput], num_layers: usize) -> Self {
        let unused_layers = num_layers.saturating_sub(inputs.len());
        Self(
            core::iter::repeat_with(EncapsulatedBlendingHeader::random)
                .take(unused_layers)
                .chain(
                    inputs
                        .iter()
                        .map(EncapsulationInput::ephemeral_encryption_key)
                        .rev()
                        .map(|rng_key| {
                            let mut header = EncapsulatedBlendingHeader::initialize(
                                &BlendingHeader::pseudo_random(rng_key.as_slice()),
                            );
                            inputs
                                .iter()
                                .take_while_inclusive(|&input| {
                                    input.ephemeral_encryption_key() != rng_key
                                })
                                .for_each(|input| {
                                    let mut header_cipher =
                                        input.ephemeral_encryption_key().cipher(domains::HEADER);
                                    header.encapsulate(&mut header_cipher);
                                });
                            header
                        }),
                )
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    /// Encapsulates the private header.
    // TODO: Use two different types for encapsulated and unencapsulated blending
    // headers?
    fn encapsulate(
        mut self,
        shared_key: &SharedKey,
        signing_pubkey: Ed25519PublicKey,
        proof_of_quota: &VerifiedProofOfQuota,
        signature: Ed25519Signature,
        proof_of_selection: VerifiedProofOfSelection,
        is_last: bool,
    ) -> Self {
        // Shift blending headers by one rightward.
        self.shift_right();

        // Replace the first blending header with the new one.
        // We don't distinguish between locally-generated (valid)
        // `BlendingHeader`s and received (unverified) ones, so we use regular `PoQ` and
        // `PoSel` instead of their verified counterparts.
        self.replace_first(EncapsulatedBlendingHeader::initialize(&BlendingHeader {
            signing_pubkey,
            proof_of_quota: *proof_of_quota.as_ref(),
            signature,
            proof_of_selection: *proof_of_selection.as_ref(),
            is_last,
        }));

        // Encrypt all blending headers
        self.0.iter_mut().for_each(|header| {
            let mut header_cipher = shared_key.cipher(domains::HEADER);
            header.encapsulate(&mut header_cipher);
        });

        self
    }

    fn decapsulate<Verifier>(
        mut self,
        key: &SharedKey,
        posel_verification_input: &VerifyInputs,
        verifier: &Verifier,
    ) -> Result<PrivateHeaderDecapsulationOutput, Error>
    where
        Verifier: ProofsVerifier,
    {
        // We call a bunch of `.expect()`s in the following code, so we need to check we
        // are dealing with a message with at least one layer.
        if self.0.is_empty() {
            return Err(Error::EmptyEncapsulationInputs);
        }

        // Decrypt all blending headers
        self.0.iter_mut().for_each(|header| {
            let mut header_cipher = key.cipher(domains::HEADER);
            header.decapsulate(&mut header_cipher);
        });

        // Check if the first blending header which was correctly decrypted
        // by verifying the decrypted proof of selection.
        // If the `private_key` is not correct, the proof of selection is
        // badly decrypted and verification will fail.
        let BlendingHeader {
            is_last,
            proof_of_quota,
            proof_of_selection,
            signature,
            signing_pubkey,
        } = self.first().try_deserialize()?;
        // Verify PoSel according to the Blend spec: <https://lip.logos.co/blockchain/raw/blend-protocol.html#processing>.
        let verified_proof_of_selection = verifier
            .verify_proof_of_selection(proof_of_selection, posel_verification_input)
            .map_err(|_| {
                Error::ProofOfSelectionVerificationFailed(selection::Error::Verification)
            })?;

        // Build a new public header with the values in the first blending header.
        let public_header = PublicHeader::new(signing_pubkey, &proof_of_quota, signature);

        // Shift blending headers one leftward.
        self.shift_left();

        // Reconstruct/encrypt the last blending header
        // in the same way as the initialization step.
        let mut last_blending_header =
            EncapsulatedBlendingHeader::initialize(&BlendingHeader::pseudo_random(key.as_slice()));
        let mut header_cipher = key.cipher(domains::HEADER);
        last_blending_header.encapsulate(&mut header_cipher);
        self.replace_last(last_blending_header);

        if is_last {
            Ok(PrivateHeaderDecapsulationOutput::Completed {
                encapsulated_private_header: self,
                public_header,
                verified_proof_of_selection,
            })
        } else {
            Ok(PrivateHeaderDecapsulationOutput::Incompleted {
                encapsulated_private_header: self,
                public_header,
                verified_proof_of_selection,
            })
        }
    }

    fn shift_right(&mut self) {
        self.0.rotate_right(1);
    }

    fn shift_left(&mut self) {
        self.0.rotate_left(1);
    }

    fn first(&self) -> &EncapsulatedBlendingHeader {
        self.0
            .first()
            .expect("Private header always has at least one blending header.")
    }

    fn replace_first(&mut self, header: EncapsulatedBlendingHeader) {
        *self
            .0
            .first_mut()
            .expect("Private header always has at least one blending header.") = header;
    }

    fn replace_last(&mut self, header: EncapsulatedBlendingHeader) {
        *self
            .0
            .last_mut()
            .expect("Private header always has at least one blending header.") = header;
    }

    fn iter_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.0
            .iter()
            .flat_map(EncapsulatedBlendingHeader::iter_bytes)
    }
}

impl BinaryEncode for EncapsulatedPrivateHeader {
    fn encoded_length(&self) -> usize {
        self.0.iter().map(BinaryEncode::encoded_length).sum()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        for layer in &self.0 {
            layer.encode_into(out);
        }
    }
}

impl BinaryDecode for EncapsulatedPrivateHeader {
    type Context = NonZeroU64;

    fn decode<'input>(
        mut input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let mut layers = Vec::with_capacity(context.get() as usize);
        for _ in 0..context.get() {
            let (remaining, layer) = EncapsulatedBlendingHeader::decode(input, &())?;
            layers.push(layer);
            input = remaining;
        }
        Ok((input, Self(layers.into_boxed_slice())))
    }
}

/// A blending header encapsulated zero or more times.
///
/// Always exactly [`BLENDING_HEADER_ENCODED_SIZE`] bytes (the cipher is
/// length-preserving), so it is a fixed-size array — stored inline, so a whole
/// [`EncapsulatedPrivateHeader`] is one contiguous allocation.
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct EncapsulatedBlendingHeader(
    #[serde_as(as = "serde_with::Bytes")] [u8; BLENDING_HEADER_ENCODED_SIZE],
);

impl EncapsulatedBlendingHeader {
    /// Build a [`EncapsulatedBlendingHeader`] by serializing a
    /// [`BlendingHeader`] without any encapsulation.
    pub(crate) fn initialize(header: &BlendingHeader) -> Self {
        let mut bytes = Vec::with_capacity(BLENDING_HEADER_ENCODED_SIZE);
        header.encode_into(&mut bytes);
        Self(
            bytes
                .try_into()
                .expect("A BlendingHeader always encodes to BLENDING_HEADER_ENCODED_SIZE bytes."),
        )
    }

    /// Build a filler [`EncapsulatedBlendingHeader`] out of fresh entropy.
    ///
    /// Used for the `ß_max - h` layers of a message that is encapsulated fewer
    /// than `ß_max` times. Unlike the reconstructable filler of the last `h`
    /// layers, these bytes are derived from local entropy rather than from a
    /// shared key, so no party — not even the sender, after the fact — can
    /// reproduce them. They are never decrypted into a [`BlendingHeader`], they
    /// only ride along so that the layer count is not observable.
    fn random() -> Self {
        Self(pseudo_random_sized_bytes::<BLENDING_HEADER_ENCODED_SIZE>(
            domains::RANDOM_FILLER_HEADER,
            &random_sized_bytes::<32>(),
        ))
    }

    /// Try to deserialize into a [`BlendingHeader`].
    /// If there is no encapsulation left, and if the bytes are valid,
    /// the deserialization will succeed.
    fn try_deserialize(&self) -> Result<BlendingHeader, Error> {
        let (_remaining, header) = BlendingHeader::decode(&self.0, &())
            .map_err(|_| Error::PrivateHeaderDeserializationFailed)?;
        Ok(header)
    }

    /// Add a layer of encapsulation.
    fn encapsulate(&mut self, cipher: &mut Cipher) {
        cipher.encrypt(&mut self.0[..]);
    }

    /// Remove a layer of encapsulation.
    fn decapsulate(&mut self, cipher: &mut Cipher) {
        cipher.decrypt(&mut self.0[..]);
    }

    fn iter_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.0.iter().copied()
    }
}

// The encapsulated leaves already hold their raw ciphered bytes, so encoding is
// the identity and decoding takes a fixed-size slice of the layer/payload size.
// No length checks: the network-side size gate guarantees the input is large
// enough (`split_at`/`try_into` therefore never fail).
impl BinaryEncode for EncapsulatedBlendingHeader {
    fn encoded_length(&self) -> usize {
        BLENDING_HEADER_ENCODED_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }
}

impl BinaryDecode for EncapsulatedBlendingHeader {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (bytes, remaining) = take::<Self>(input, BLENDING_HEADER_ENCODED_SIZE)?;
        Ok((
            remaining,
            Self(bytes.try_into().expect("Take guarantees the length")),
        ))
    }
}

/// A payload encapsulated zero or more times.
///
/// Always exactly [`PAYLOAD_ENCODED_SIZE`] bytes; boxed because that is ~34 KiB
/// and must not be stored inline in
/// [`EncapsulatedPart`]/[`EncapsulatedMessage`].
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub(crate) struct EncapsulatedPayload(
    #[serde_as(as = "serde_with::Bytes")] Box<[u8; PAYLOAD_ENCODED_SIZE]>,
);

impl EncapsulatedPayload {
    /// Build a [`EncapsulatedPayload`] by serializing a [`Payload`]
    /// without any encapsulation.
    pub(crate) fn initialize(payload: &Payload) -> Self {
        let mut bytes = Vec::with_capacity(PAYLOAD_ENCODED_SIZE);
        payload.encode_into(&mut bytes);
        Self(
            bytes
                .into_boxed_slice()
                .try_into()
                .expect("A Payload always encodes to PAYLOAD_ENCODED_SIZE bytes."),
        )
    }

    /// Try to deserialize into a [`Payload`].
    /// If there is no encapsulation left, and if the bytes are valid,
    /// the deserialization will succeed.
    fn try_deserialize(&self) -> Result<Payload, Error> {
        let (_remaining, payload) =
            Payload::decode(&self.0[..], &()).map_err(|_| Error::PayloadDeserializationFailed)?;
        Ok(payload)
    }

    /// Add a layer of encapsulation.
    fn encapsulate(mut self, cipher: &mut Cipher) -> Self {
        cipher.encrypt(&mut self.0[..]);
        self
    }

    /// Remove a layer of encapsulation.
    fn decapsulate(mut self, cipher: &mut Cipher) -> Self {
        cipher.decrypt(&mut self.0[..]);
        self
    }

    fn iter_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.0.iter().copied()
    }
}

impl BinaryEncode for EncapsulatedPayload {
    fn encoded_length(&self) -> usize {
        PAYLOAD_ENCODED_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0[..]);
    }
}

impl BinaryDecode for EncapsulatedPayload {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (bytes, remaining) = take::<Self>(input, PAYLOAD_ENCODED_SIZE)?;
        let boxed = bytes
            .to_vec()
            .into_boxed_slice()
            .try_into()
            .expect("Take guarantees the length");
        Ok((remaining, Self(boxed)))
    }
}
