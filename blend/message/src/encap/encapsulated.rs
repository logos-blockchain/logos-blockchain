use std::num::NonZeroU64;

use derivative::Derivative;
use itertools::Itertools as _;
use lb_blend_crypto::{ZkHash, cipher::Cipher};
use lb_blend_proofs::{
    quota::{self, PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
    selection::{self, PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_core::codec::{DeserializeOp as _, SerializeOp};
use lb_key_management_system_keys::keys::{
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519PublicKey, Ed25519Signature, SharedKey,
    UnsecuredEd25519Key,
};
use serde::{Deserialize, Serialize};

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
        payload::{MAX_PAYLOAD_BODY_SIZE, PaddedPayloadBody},
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

    /// The exact serialized size, in bytes, of any well-formed message with
    /// `num_layers` encapsulation layers.
    ///
    /// The wire format is a fixed-size, prefix-free concatenation (see the Blend
    /// Payload Formatting spec and [`Self::encode`]): the public header, every
    /// encapsulated blending header, and the payload all have a constant size,
    /// so the total is strictly linear in the number of layers and fully
    /// determined by `num_layers`.
    #[must_use]
    pub const fn expected_serialized_len(num_layers: NonZeroU64) -> u64 {
        (PUBLIC_HEADER_SIZE + num_layers.get() as usize * BLENDING_HEADER_SIZE + PAYLOAD_SIZE)
            as u64
    }

    /// Serialize the message to its fixed-size, prefix-free wire representation:
    /// `public_header || layer_0 || .. || layer_{N-1} || payload`, with no
    /// length framing at the message level. The layer count is fixed by the
    /// network-wide configuration, so it is not encoded on the wire.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_message(&self.public_header, &self.encapsulated_part)
    }

    /// Deserialize a message received from an untrusted remote peer.
    ///
    /// Because the format is fixed-size, we reject in O(1) — before allocating a
    /// single layer — any input whose length is not exactly that of a
    /// well-formed `num_layers`-layer message. A message encoding, say, 20 layers
    /// when we expect 3 has a different length and is discarded up front. The
    /// bytes are then sliced positionally into the public header, exactly
    /// `num_layers` fixed-size layers, and the fixed-size payload, so the layout
    /// is enforced by construction.
    pub fn deserialize_from_remote(bytes: &[u8], num_layers: NonZeroU64) -> Result<Self, Error> {
        if bytes.len() as u64 != Self::expected_serialized_len(num_layers) {
            return Err(Error::UnexpectedMessageSize);
        }

        let (public_header_bytes, part_bytes) = bytes.split_at(PUBLIC_HEADER_SIZE);
        let public_header = PublicHeader::from_bytes(public_header_bytes)
            .map_err(|_| Error::MessageDeserializationFailed)?;
        let encapsulated_part = EncapsulatedPart::decode(part_bytes, num_layers);
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

    /// Append the part's fixed-size, prefix-free wire bytes to `out`: every
    /// layer's bytes in order, followed by the payload's bytes.
    pub(super) fn encode_into(&self, out: &mut Vec<u8>) {
        self.private_header.encode_into(out);
        out.extend_from_slice(&self.payload.0);
    }

    /// Reconstruct a part from the bytes following the public header.
    ///
    /// `part_bytes` is expected to be exactly
    /// `num_layers * BLENDING_HEADER_SIZE + PAYLOAD_SIZE` bytes long. The caller
    /// ([`EncapsulatedMessage::deserialize_from_remote`]) guarantees this via the
    /// O(1) size gate, so the split points always land on layer/payload
    /// boundaries and the layout is enforced by construction.
    pub(super) fn decode(part_bytes: &[u8], num_layers: NonZeroU64) -> Self {
        let layers_len = num_layers.get() as usize * BLENDING_HEADER_SIZE;
        let (layers_bytes, payload_bytes) = part_bytes.split_at(layers_len);
        Self {
            private_header: EncapsulatedPrivateHeader::decode(layers_bytes),
            payload: EncapsulatedPayload(payload_bytes.to_vec()),
        }
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

    /// Append every layer's fixed-size bytes to `out`, in order and without any
    /// framing.
    fn encode_into(&self, out: &mut Vec<u8>) {
        for header in &self.0 {
            out.extend_from_slice(&header.0);
        }
    }

    /// Split `layers_bytes` into fixed-size layers. The length is guaranteed by
    /// the caller to be an exact multiple of [`BLENDING_HEADER_SIZE`], so
    /// `chunks_exact` leaves no remainder.
    fn decode(layers_bytes: &[u8]) -> Self {
        Self(
            layers_bytes
                .chunks_exact(BLENDING_HEADER_SIZE)
                .map(|chunk| EncapsulatedBlendingHeader(chunk.to_vec()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
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

/// Encode a full message as `public_header || part` in the fixed-size,
/// prefix-free wire format (see [`EncapsulatedMessage::encode`]).
///
/// The public header is written as its own fixed-size bytes (bincode over
/// fixed-width fields adds no length framing), followed by the part's raw layer
/// and payload bytes. All three public-header variants
/// ([`PublicHeader`], [`PublicHeaderWithVerifiedSignature`],
/// [`VerifiedPublicHeader`]) serialize to the same fixed-size bytes, so this
/// yields byte-identical output regardless of which one is passed — which is
/// what lets a peer serialize a verified variant and the receiver decode an
/// [`EncapsulatedMessage`].
pub(super) fn encode_message<Header: SerializeOp>(
    public_header: &Header,
    part: &EncapsulatedPart,
) -> Vec<u8> {
    let mut bytes = public_header
        .to_bytes()
        .expect("A public header is always serializable.")
        .to_vec();
    bytes.reserve(part.private_header.0.len() * BLENDING_HEADER_SIZE + PAYLOAD_SIZE);
    part.encode_into(&mut bytes);
    bytes
}

// Fixed sizes of the message components. Every component is either a primitive,
// a fixed-size crypto object, or the payload padded to [`MAX_PAYLOAD_BODY_SIZE`],
// so all of these are compile-time constants.
//
// The message *framing* is now prefix-free (the layers and payload are
// concatenated by [`encode_message`] with no length prefixes; the layer count
// is fixed by the network configuration). The component bytes themselves are
// still produced by the bincode codec, however, so `PAYLOAD_SIZE` and
// `PUBLIC_HEADER_SIZE` account for that internal encoding — e.g. the enum
// discriminant and the `serde_with::Bytes` length prefix inside a serialized
// `Payload`. The `serialized_size_constants_match_wire_format` test pins these
// constants to the real encoding.

/// bincode encodes a `Vec`/byte-string length as a `u64`.
const SEQUENCE_LENGTH_PREFIX_SIZE: usize = size_of::<u64>();

/// bincode encodes an enum discriminant as a `u32`.
const ENUM_DISCRIMINANT_SIZE: usize = size_of::<u32>();

/// Serialized size of one layer: a [`BlendingHeader`] with all fixed-size
/// fields. On the wire the layers are concatenated with no framing, so this is
/// also the per-layer contribution to a message's size.
const BLENDING_HEADER_SIZE: usize = ED25519_PUBLIC_KEY_SIZE
    + PROOF_OF_QUOTA_SIZE
    + ED25519_SIGNATURE_SIZE
    + PROOF_OF_SELECTION_SIZE
    + size_of::<bool>(); // `is_last`

/// Serialized size of a [`Payload`]: the body is always padded to
/// [`MAX_PAYLOAD_BODY_SIZE`], so the whole payload has a constant size.
///
/// `PaddedPayloadBody::padded` is serialized via `serde_with::Bytes`, i.e. as a
/// byte string, which bincode length-prefixes (unlike a bare fixed-size array).
const PAYLOAD_SIZE: usize = ENUM_DISCRIMINANT_SIZE // `PayloadHeader::payload_type`
    + size_of::<u16>() // `PayloadHeader::body_len`
    + SEQUENCE_LENGTH_PREFIX_SIZE + MAX_PAYLOAD_BODY_SIZE // `PaddedPayloadBody::padded`
    + size_of::<u16>(); // `PaddedPayloadBody::actual_len`

/// Serialized size of a [`PublicHeader`]: a version byte plus fixed-size fields.
const PUBLIC_HEADER_SIZE: usize =
    size_of::<u8>() + ED25519_PUBLIC_KEY_SIZE + PROOF_OF_QUOTA_SIZE + ED25519_SIGNATURE_SIZE;
