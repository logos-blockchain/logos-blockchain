use core::num::NonZeroU64;

use derivative::Derivative;
use itertools::Itertools as _;
use lb_blend_crypto::{ZkHash, cipher::Cipher};
use lb_blend_proofs::{
    quota::{self, VerifiedProofOfQuota},
    selection::{self, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
use lb_key_management_system_keys::keys::{
    Ed25519PublicKey, Ed25519Signature, SharedKey, UnsecuredEd25519Key,
};
use serde::{Deserialize, Serialize};

use crate::{
    Error, PayloadType,
    codec::WireEncode,
    crypto::{domains, key_ext::SharedKeyExt as _},
    encap::{
        ProofsVerifier,
        decapsulated::{PartDecapsulationOutput, PrivateHeaderDecapsulationOutput},
        expected_serialized_message_len,
        validated::{
            EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
        },
    },
    input::EncapsulationInput,
    message::{
        BlendingHeader, Payload, PublicHeader,
        payload::PaddedPayloadBody,
        public_header::{ENCODED_PUBLIC_HEADER_SIZE, VerifiedPublicHeader},
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

    /// Serialize the message to its fixed-size, prefix-free wire
    /// representation: `public_header || layer_0 || .. || layer_{N-1} ||
    /// payload`, with no length framing at the message level. The layer
    /// count is fixed by the network-wide configuration, so it is not
    /// encoded on the wire.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_message_components(&self.public_header, &self.encapsulated_part)
    }

    /// Decode a message received from an untrusted remote peer.
    ///
    /// Because the format is fixed-size, we reject in O(1) — before allocating
    /// a single layer — any input whose length is not exactly that of a
    /// well-formed `num_layers`-layer message. A message encoding, say, 20
    /// layers when we expect 3 has a different length and is discarded up
    /// front. The bytes are then decoded component by component (see
    /// [`WireDecode`]); the size gate guarantees each fixed-size slice
    /// lands on a component boundary and that the whole input is consumed,
    /// so the layout is enforced by construction.
    pub fn decode(bytes: &[u8], num_layers: NonZeroU64) -> Result<Self, Error> {
        if bytes.len() as u64 != expected_serialized_message_len(num_layers) {
            return Err(Error::MessageDeserializationFailed);
        }

        let (remaining, public_header) = PublicHeader::decode(bytes, ())?;
        let (remaining, encapsulated_part) = EncapsulatedPart::decode(remaining, num_layers)?;
        debug_assert!(
            remaining.is_empty(),
            "The size gate guarantees the wire bytes are fully consumed"
        );
        Ok(Self::from_components(public_header, encapsulated_part))
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
        // Verify signature according to the Blend spec: <https://www.notion.so/nomos-tech/Blend-Protocol-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df81859cebf5e3d2a5cd8f>.
        self.public_header.verify_signature(&signing_body(
            &self.encapsulated_part.private_header,
            &self.encapsulated_part.payload,
        ))?;
        let (_, signing_key, proof_of_quota, signature) = self.public_header.into_components();
        // Verify the Proof of Quota according to the Blend spec: <https://www.notion.so/nomos-tech/Blend-Protocol-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df81b593ddce00cffd24a8>.
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
    ) -> Self {
        Self {
            private_header: EncapsulatedPrivateHeader::new_unchecked(inputs),
            payload: EncapsulatedPayload::initialize(&Payload::new(payload_type, payload_body)),
        }
    }

    /// Initializes the encapsulated part as preparation for actual
    /// encapsulations.
    ///
    /// It returns an error if the slice of inputs is empty.
    pub(super) fn try_initialize(
        inputs: &[EncapsulationInput],
        payload_type: PayloadType,
        payload_body: PaddedPayloadBody,
    ) -> Result<Self, Error> {
        Ok(Self {
            private_header: EncapsulatedPrivateHeader::try_initialize(inputs)?,
            payload: EncapsulatedPayload::initialize(&Payload::new(payload_type, payload_body)),
        })
    }

    /// Add a layer of encapsulation.
    pub(super) fn encapsulate(
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
    pub(super) fn sign(&self, key: &UnsecuredEd25519Key) -> Ed25519Signature {
        key.sign_payload(&signing_body(&self.private_header, &self.payload))
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
pub(super) struct EncapsulatedPrivateHeader(Box<[EncapsulatedBlendingHeader]>);

impl EncapsulatedPrivateHeader {
    #[cfg(test)]
    pub fn new_unchecked(inputs: &[EncapsulationInput]) -> Self {
        Self::from_inputs(inputs)
    }

    /// Initializes the private header as preparation for actual encapsulations.
    ///
    /// It returns an error if the slice of inputs is empty.
    fn try_initialize(inputs: &[EncapsulationInput]) -> Result<Self, Error> {
        if inputs.is_empty() {
            return Err(Error::EmptyEncapsulationInputs);
        }

        Ok(Self::from_inputs(inputs))
    }

    // Randomize the private header in the reconstructable way,
    // so that the corresponding signatures can be verified later.
    // Plus, encapsulate the last `inputs.len()` blending headers.
    //
    // Example: for 2 inputs,
    // BlendingHeaders[0]: Enc(inputs[1], Enc(inputs[0], RND(inputs[1])))
    // BlendingHeaders[1]:               Enc(inputs[0], RND(inputs[0]))
    //
    // Notation:
    // - RND(seed): Pseudo-random bytes generated from `seed` with the `HEADER` DST
    // - Enc(key, data): Encrypt `data` by XOR-ing with RND(key)
    fn from_inputs(inputs: &[EncapsulationInput]) -> Self {
        Self(
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
                        .take_while_inclusive(|&input| input.ephemeral_encryption_key() != rng_key)
                        .for_each(|input| {
                            let mut header_cipher =
                                input.ephemeral_encryption_key().cipher(domains::HEADER);
                            header.encapsulate(&mut header_cipher);
                        });
                    header
                })
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
        // Verify PoSel according to the Blend spec: <https://www.notion.so/nomos-tech/Blend-Protocol-215261aa09df81ae8857d71066a80084?source=copy_link#215261aa09df81dd8cbedc8af4649a6a>.
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

/// A blending header encapsulated zero or more times.
// TODO: Consider having `SerializedBlendingHeader` (not encapsulated).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
struct EncapsulatedBlendingHeader(Vec<u8>);

impl EncapsulatedBlendingHeader {
    /// Build a [`EncapsulatedBlendingHeader`] by serializing a
    /// [`BlendingHeader`] without any encapsulation.
    fn initialize(header: &BlendingHeader) -> Self {
        Self(
            header
                .to_bytes()
                .expect("BlendingHeader should be able to be serialized")
                .to_vec(),
        )
    }

    /// Try to deserialize into a [`BlendingHeader`].
    /// If there is no encapsulation left, and if the bytes are valid,
    /// the deserialization will succeed.
    fn try_deserialize(&self) -> Result<BlendingHeader, Error> {
        BlendingHeader::from_bytes(&self.0).map_err(|_| Error::PrivateHeaderDeserializationFailed)
    }

    /// Add a layer of encapsulation.
    fn encapsulate(&mut self, cipher: &mut Cipher) {
        cipher.encrypt(self.0.as_mut_slice());
    }

    /// Remove a layer of encapsulation.
    fn decapsulate(&mut self, cipher: &mut Cipher) {
        cipher.decrypt(self.0.as_mut_slice());
    }

    fn iter_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.0.iter().copied()
    }
}

/// A payload encapsulated zero or more times.
// TODO: Consider having `SerializedPayload` (not encapsulated).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
struct EncapsulatedPayload(Vec<u8>);

impl EncapsulatedPayload {
    /// Build a [`EncapsulatedPayload`] by serializing a [`Payload`]
    /// without any encapsulation.
    fn initialize(payload: &Payload) -> Self {
        Self(
            payload
                .to_bytes()
                .expect("Payload should be able to be serialized")
                .to_vec(),
        )
    }

    /// Try to deserialize into a [`Payload`].
    /// If there is no encapsulation left, and if the bytes are valid,
    /// the deserialization will succeed.
    fn try_deserialize(&self) -> Result<Payload, Error> {
        Payload::from_bytes(&self.0).map_err(|_| Error::PayloadDeserializationFailed)
    }

    /// Add a layer of encapsulation.
    fn encapsulate(mut self, cipher: &mut Cipher) -> Self {
        cipher.encrypt(self.0.as_mut_slice());
        self
    }

    /// Remove a layer of encapsulation.
    fn decapsulate(mut self, cipher: &mut Cipher) -> Self {
        cipher.decrypt(self.0.as_mut_slice());
        self
    }

    fn iter_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.0.iter().copied()
    }
}

/// Encode a full message as `public_header || part`, the shared shape of all
/// three message types.
///
/// Every public-header variant ([`PublicHeader`],
/// [`PublicHeaderWithVerifiedSignature`], [`VerifiedPublicHeader`]) encodes to
/// the same fixed-size bytes, so the output is byte-identical regardless of
/// which is passed — which is what lets a peer serialize a verified variant and
/// the receiver decode an (unverified) [`EncapsulatedMessage`].
pub(super) fn encode_message_components<PublicHeader>(
    public_header: &PublicHeader,
    part: &EncapsulatedPart,
) -> Vec<u8>
where
    PublicHeader: WireEncode,
{
    let mut bytes =
        Vec::with_capacity(expected_serialized_message_len(part.encapsulation_layers()));
    public_header.encode_into(&mut bytes);
    part.encode_into(&mut bytes);
    bytes
}

/// Split off the first `len` bytes of `input` as `(taken, rest)`, erroring if
/// `input` is too short. This is unreachable once
/// [`EncapsulatedMessage::decode`]'s size gate has run, but it keeps every
/// component decode self-contained.
const fn take(input: &[u8], len: usize) -> Result<(&[u8], &[u8]), Error> {
    if input.len() < len {
        return Err(Error::MessageDeserializationFailed);
    }
    Ok(input.split_at(len))
}

// The encapsulated leaves already hold their raw ciphered bytes, so encoding is
// the identity and decoding takes a fixed-size slice.
impl WireEncode for EncapsulatedBlendingHeader {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }
}

impl WireDecode for EncapsulatedBlendingHeader {
    type Context = ();

    fn decode(input: &[u8], (): ()) -> Result<(&[u8], Self), Error> {
        let (bytes, rest) = take(input, BLENDING_HEADER_SIZE)?;
        Ok((rest, Self(bytes.to_vec())))
    }
}

impl WireEncode for EncapsulatedPayload {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }
}

impl WireDecode for EncapsulatedPayload {
    type Context = ();

    fn decode(input: &[u8], (): ()) -> Result<(&[u8], Self), Error> {
        let (bytes, rest) = take(input, PAYLOAD_SIZE)?;
        Ok((rest, Self(bytes.to_vec())))
    }
}

// The private header is exactly `num_layers` blending headers, back to back.
impl WireEncode for EncapsulatedPrivateHeader {
    fn encode_into(&self, out: &mut Vec<u8>) {
        for layer in &self.0 {
            layer.encode_into(out);
        }
    }
}

impl WireDecode for EncapsulatedPrivateHeader {
    type Context = NonZeroU64;

    fn decode(mut input: &[u8], num_layers: NonZeroU64) -> Result<(&[u8], Self), Error> {
        let mut layers = Vec::with_capacity(num_layers.get() as usize);
        for _ in 0..num_layers.get() {
            let (rest, layer) = EncapsulatedBlendingHeader::decode(input, ())?;
            layers.push(layer);
            input = rest;
        }
        Ok((input, Self(layers.into_boxed_slice())))
    }
}

// A part is its private header followed by its payload.
impl WireEncode for EncapsulatedPart {
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.private_header.encode_into(out);
        self.payload.encode_into(out);
    }
}

impl WireDecode for EncapsulatedPart {
    type Context = NonZeroU64;

    fn decode(input: &[u8], num_layers: NonZeroU64) -> Result<(&[u8], Self), Error> {
        let (input, private_header) = EncapsulatedPrivateHeader::decode(input, num_layers)?;
        let (input, payload) = EncapsulatedPayload::decode(input, ())?;
        Ok((
            input,
            Self {
                private_header,
                payload,
            },
        ))
    }
}
