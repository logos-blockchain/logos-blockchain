use std::{num::NonZeroU64, sync::LazyLock};

use derivative::Derivative;
use itertools::Itertools as _;
use lb_blend_crypto::{ZkHash, cipher::Cipher};
use lb_blend_proofs::{
    quota::{self, PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
    selection::{self, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
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
        BlendingHeader, Payload, PublicHeader, payload::PaddedPayloadBody,
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
    /// The wire format is fixed-size (see the Blend Payload Formatting spec):
    /// the public header, every encapsulated blending header, and the payload
    /// all have a constant size, so the total is strictly linear in the number
    /// of layers and fully determined by `num_layers`.
    #[must_use]
    pub fn expected_serialized_len(num_layers: NonZeroU64) -> u64 {
        let SizeModel { base, per_layer } = *SIZE_MODEL;
        base + num_layers.get() * per_layer
    }

    /// Deserialize a message received from an untrusted remote peer, enforcing
    /// the fixed layout mandated by the Blend Payload Formatting spec.
    ///
    /// Before allocating anything we reject any input whose length is not
    /// exactly that of a well-formed `num_layers`-layer message. Because each
    /// layer contributes a fixed number of bytes, this O(1) check bounds the
    /// work an adversary can force on us: a message encoding, say, 20 layers
    /// when we expect 3 has a different length and is discarded before a single
    /// layer is parsed. Decoding is then re-checked against the expected layout,
    /// since the total length alone does not pin down how the bytes are split
    /// between layers and payload.
    pub fn deserialize_from_remote(bytes: &[u8], num_layers: NonZeroU64) -> Result<Self, Error> {
        if bytes.len() as u64 != Self::expected_serialized_len(num_layers) {
            return Err(Error::UnexpectedMessageSize);
        }

        let message = Self::from_bytes(bytes).map_err(|_| Error::MessageDeserializationFailed)?;
        message.encapsulated_part.validate_layout(num_layers)?;
        Ok(message)
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

    /// Verifies that a decoded part matches the fixed layout of a well-formed
    /// `num_layers`-layer message: exactly `num_layers` layers, each of the
    /// fixed per-layer size, and a fixed-size payload.
    fn validate_layout(&self, num_layers: NonZeroU64) -> Result<(), Error> {
        if self.private_header.layer_count() as u64 != num_layers.get() {
            return Err(Error::UnexpectedEncapsulationLayerCount);
        }
        if !self.private_header.has_fixed_size_layers() || self.payload.byte_len() != *PAYLOAD_LEN {
            return Err(Error::MalformedEncapsulatedMessage);
        }
        Ok(())
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

    fn layer_count(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` iff every layer has the fixed serialized size mandated by
    /// the Blend Payload Formatting spec.
    fn has_fixed_size_layers(&self) -> bool {
        self.0
            .iter()
            .all(|header| header.byte_len() == *BLENDING_HEADER_LEN)
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

    const fn byte_len(&self) -> usize {
        self.0.len()
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

    const fn byte_len(&self) -> usize {
        self.0.len()
    }
}

/// The wire size of a message is `base + num_layers * per_layer` bytes. We
/// recover these two coefficients once, by measuring two canonically-built
/// messages, so that no assumption about the underlying codec's framing is
/// hard-coded here.
struct SizeModel {
    base: u64,
    per_layer: u64,
}

static SIZE_MODEL: LazyLock<SizeModel> = LazyLock::new(|| {
    let size_of = |num_layers| {
        canonical_message(num_layers)
            .bytes_size()
            .expect("A canonical message is always serializable.")
    };
    let one_layer = size_of(1);
    let two_layers = size_of(2);
    let per_layer = two_layers - one_layer;
    SizeModel {
        base: one_layer - per_layer,
        per_layer,
    }
});

/// The fixed serialized size of a single encapsulated blending header (layer).
static BLENDING_HEADER_LEN: LazyLock<usize> = LazyLock::new(|| {
    EncapsulatedBlendingHeader::initialize(&BlendingHeader::pseudo_random(&[0])).byte_len()
});

/// The fixed serialized size of an encapsulated payload.
static PAYLOAD_LEN: LazyLock<usize> =
    LazyLock::new(|| EncapsulatedPayload::initialize(&canonical_payload()).byte_len());

fn canonical_payload() -> Payload {
    Payload::new(
        PayloadType::Cover,
        PaddedPayloadBody::try_from([0u8; 0].as_slice())
            .expect("An empty payload body is always valid."),
    )
}

/// Builds a structurally-valid (but cryptographically meaningless) message with
/// `num_layers` layers, used solely to measure the fixed wire size.
fn canonical_message(num_layers: usize) -> EncapsulatedMessage {
    let public_header = PublicHeader::new(
        UnsecuredEd25519Key::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).public_key(),
        &VerifiedProofOfQuota::from_bytes_unchecked([0; PROOF_OF_QUOTA_SIZE]).into_inner(),
        Ed25519Signature::from_bytes(&[0; ED25519_SIGNATURE_SIZE]),
    );
    let private_header = EncapsulatedPrivateHeader(
        (0..num_layers)
            .map(|layer| {
                EncapsulatedBlendingHeader::initialize(&BlendingHeader::pseudo_random(&[layer as u8]))
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    EncapsulatedMessage::from_components(
        public_header,
        EncapsulatedPart {
            private_header,
            payload: EncapsulatedPayload::initialize(&canonical_payload()),
        },
    )
}
