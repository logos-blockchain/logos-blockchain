//! `OpenAPI` schemas for the JSON (human-readable `serde`) form of this crate's
//! types.
//!
//! Most types document themselves next to their definition. This module holds
//! the shared encodings they reference, and schema-only stand-ins for wire
//! shapes that have no Rust type of their own, to be used as `value_type`.

use lb_utils::openapi::hex_bytes_schema;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        Ref, RefOr, Type,
        schema::{ArrayBuilder, ObjectBuilder, Schema, SchemaType},
    },
};

use crate::mantle::transactions::Ops;

pub(crate) type Schemas = Vec<(String, RefOr<Schema>)>;

/// A `$ref` to `T`'s component.
pub(crate) fn reference<T: ToSchema>() -> RefOr<Schema> {
    Ref::from_schema_name(T::name()).into()
}

/// Registers `T`'s component, and those it references, in `schemas`.
pub(crate) fn collect<T: ToSchema>(schemas: &mut Schemas) {
    schemas.push((T::name().into_owned(), T::schema()));
    T::schemas(schemas);
}

fn described(schema: RefOr<Schema>, description: &str) -> RefOr<Schema> {
    match schema {
        RefOr::T(Schema::Object(mut object)) => {
            object.description = Some(description.to_owned());
            RefOr::T(Schema::Object(object))
        }
        other => other,
    }
}

/// A BN254 scalar field element, encoded through `lb_groth16::serde::serde_fr`.
pub(crate) fn fr() -> RefOr<Schema> {
    described(
        hex_bytes_schema(size_of::<lb_groth16::FrBytes>()),
        "BN254 scalar field element: 32 bytes, little-endian, hex encoded. The value must be \
         below the field modulus.",
    )
}

/// A compressed Groth16 proof, encoded through
/// `lb_utils::serde::serialize_bytes_array`.
pub(crate) fn compressed_proof() -> RefOr<Schema> {
    described(
        hex_bytes_schema(lb_groth16::COMPRESSED_PROOF_SIZE),
        "Compressed Groth16 proof: 128 bytes, hex encoded.",
    )
}

fn byte_value() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(0))
        .maximum(Some(u8::MAX))
}

/// Bytes serialized by `serde`'s default impls, i.e. as an array of numbers
/// rather than as a hex string.
pub(crate) fn byte_values(min: usize, max: usize, description: &str) -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(byte_value())
        .min_items((min > 0).then_some(min))
        .max_items(Some(max))
        .description(Some(description))
        .into()
}

/// A [`Hash`](crate::crypto::Hash), a bare `[u8; 32]`.
pub(crate) fn hash_byte_values() -> RefOr<Schema> {
    byte_values(
        32,
        32,
        "32-byte hash, serialized as an array of 32 byte values (not as a hex string).",
    )
}

/// Bytes encoded through `lb_utils::serde::serde_bytes_slice`.
pub(crate) fn hex_blob(max_bytes: usize, description: &str) -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some("^(0x)?([0-9a-fA-F]{2})*$"))
        .max_length(Some(max_bytes.saturating_mul(2)))
        .description(Some(description))
        .into()
}

pub(crate) fn u16_value() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(0))
        .maximum(Some(u16::MAX))
        .into()
}

fn epoch_builder() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(0))
        .maximum(Some(u32::MAX))
        .description(Some("Epoch number."))
}

/// An [`Epoch`](lb_cryptarchia_engine::Epoch), a transparent `u32`.
pub(crate) fn epoch() -> RefOr<Schema> {
    epoch_builder().into()
}

pub(crate) fn optional_epoch() -> RefOr<Schema> {
    epoch_builder()
        .schema_type(SchemaType::from_iter([Type::Integer, Type::Null]))
        .into()
}

/// Schema-only stand-in for the specification's *unsigned* transaction, the
/// `{ "ops": [...] }` shape produced by
/// [`mantle_spec`](crate::mantle::transactions::tx_list::ops::mantle_spec).
pub struct MantleTx;

impl PartialSchema for MantleTx {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .description(Some("Unsigned Mantle transaction."))
            .property("ops", reference::<Ops>())
            .required("ops")
            .into()
    }
}

impl ToSchema for MantleTx {
    fn schemas(schemas: &mut Schemas) {
        collect::<Ops>(schemas);
    }
}

/// Schema-only stand-in for [`lb_blend_proofs::quota::ProofOfQuota`].
pub struct ProofOfQuota;

impl PartialSchema for ProofOfQuota {
    fn schema() -> RefOr<Schema> {
        let point = |bytes| byte_values(bytes, bytes, "Compressed curve point.");
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .description(Some("Blend Proof of Quota."))
            .property("key_nullifier", fr())
            .property(
                "proof",
                ObjectBuilder::new()
                    .schema_type(Type::Object)
                    .description(Some(
                        "Compressed Groth16 proof. Unlike other proofs, its points are \
                         serialized as arrays of byte values, not as hex strings.",
                    ))
                    .property("pi_a", point(32))
                    .property("pi_b", point(64))
                    .property("pi_c", point(32))
                    .required("pi_a")
                    .required("pi_b")
                    .required("pi_c"),
            )
            .required("key_nullifier")
            .required("proof")
            .into()
    }
}

impl ToSchema for ProofOfQuota {}

/// Schema-only stand-in for [`lb_blend_proofs::selection::ProofOfSelection`].
pub struct ProofOfSelection;

impl PartialSchema for ProofOfSelection {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .description(Some("Blend Proof of Selection."))
            .property("selection_randomness", fr())
            .required("selection_randomness")
            .into()
    }
}

impl ToSchema for ProofOfSelection {}

/// Implements [`ToSchema`] for a newtype over a field element encoded through
/// `serde_fr`.
macro_rules! fr_newtype_schema {
    ($($newtype:ty),* $(,)?) => {
        $(
            impl utoipa::PartialSchema for $newtype {
                fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                    $crate::openapi::fr()
                }
            }

            impl utoipa::ToSchema for $newtype {}
        )*
    };
}

pub(crate) use fr_newtype_schema;

#[cfg(test)]
mod schema_conformance_tests {
    use std::sync::Arc;

    use boon::{Compiler, SchemaIndex, Schemas as CompiledSchemas};
    use lb_cryptarchia_engine::{Epoch, Slot};
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{
        Ed25519Key, Ed25519PublicKey, UnverifiedEd25519PublicKey, ZkPublicKey, ZkSignature,
    };
    use serde::{Serialize, de::DeserializeOwned};
    use utoipa::{ToSchema, openapi::ComponentsBuilder};

    use super::{MantleTx, Schemas, collect};
    use crate::{
        block::SignedHeader,
        events::{DepositNote, Event, Events, HeaderEvent, TxEvent, TxEventPayload},
        header::{ContentId, Header, HeaderId, Version},
        mantle::{
            Note, NoteId, Op, OpProof, SignedOps, TxHash, Utxo,
            channel::{ChannelState, SlotTimeframe, SlotTimeout},
            fixtures::{
                ops::op_values::{
                    CHANNEL_CONFIG, CHANNEL_TRANSFER, CHANNEL_WITHDRAW, CLAIM_POW_REWARD, DEPOSIT,
                    INSCRIPTION, LEADER_CLAIM, SDP_ACTIVE, SDP_DECLARE, SDP_WITHDRAW, TRANSFER,
                },
                proofs::proof_values::{
                    CHANNEL_MULTI_SIG, ED25519_SIG, POC, ZK_AND_ED25519_SIGS, ZK_SIG,
                },
            },
            gas::{Gas, GasCost, GasPrice},
            ledger::verification_mode::StandardMode,
            ops::{
                NoOpProof, SignedOp, SignedOperation,
                channel::{ChannelId, MsgId, inscribe::InscriptionOp},
                leader_claim::{VoucherCm, VoucherNullifier},
                pow::PowNullifier,
                transfer::TransferOp,
            },
            transactions::{
                GasPrices, MantleTxBuilder, OpRefs, Ops, states::Unverified,
                tx_list::ops::mantle_spec,
            },
        },
        proofs::{
            channel_multi_sig_proof::ChannelMultiSigProof,
            leader_claim_proof::Groth16LeaderClaimProof, leader_proof::Groth16LeaderProof,
        },
        sdp::{Declaration, DeclarationId, DeclarationMessage, ServiceType},
    };

    type AnySignedOps = SignedOps<Unverified, StandardMode>;

    /// Every component this crate publishes, with everything they reference,
    /// assembled the way `#[derive(OpenApi)]` does.
    fn document() -> serde_json::Value {
        let mut schemas = Schemas::new();
        collect::<AnySignedOps>(&mut schemas);
        collect::<MantleTx>(&mut schemas);
        collect::<OpRefs<'_>>(&mut schemas);
        collect::<MantleTxBuilder>(&mut schemas);
        collect::<Declaration>(&mut schemas);
        collect::<ChannelState>(&mut schemas);
        collect::<Events>(&mut schemas);
        collect::<SignedHeader>(&mut schemas);
        collect::<GasCost>(&mut schemas);
        collect::<GasPrice>(&mut schemas);
        collect::<Gas>(&mut schemas);
        collect::<GasPrices>(&mut schemas);
        collect::<VoucherCm>(&mut schemas);
        collect::<VoucherNullifier>(&mut schemas);
        collect::<lb_key_management_system_keys::openapi::ZkPublicKeys>(&mut schemas);

        let components = ComponentsBuilder::new().schemas_from_iter(schemas).build();
        serde_json::json!({ "components": components })
    }

    fn compile(component: &str) -> (CompiledSchemas, SchemaIndex) {
        let mut schemas = CompiledSchemas::new();
        let mut compiler = Compiler::new();
        compiler
            .add_resource("openapi.json", document())
            .expect("document is a valid resource");
        let index = compiler
            .compile(
                &format!("openapi.json#/components/schemas/{component}"),
                &mut schemas,
            )
            .unwrap_or_else(|error| panic!("component {component} is not a valid schema: {error}"));
        (schemas, index)
    }

    fn is_valid(component: &str, instance: &serde_json::Value) -> bool {
        let (schemas, index) = compile(component);
        schemas.validate(instance, index).is_ok()
    }

    /// Asserts that what `serde` emits for `value` satisfies `T`'s component.
    fn assert_emits_valid<T: Serialize + ToSchema>(value: &T) -> serde_json::Value {
        assert_serialized_valid(&T::name(), value)
    }

    fn assert_serialized_valid(component: &str, value: &impl Serialize) -> serde_json::Value {
        let emitted = serde_json::to_value(value).expect("value serializes");
        assert!(
            is_valid(component, &emitted),
            "{component} emits {emitted}, which its schema rejects",
        );
        emitted
    }

    /// Asserts that `instance`, which `serde` accepts for `T`, also satisfies
    /// `T`'s component.
    fn assert_accepts<T: DeserializeOwned + ToSchema>(instance: &serde_json::Value) {
        let component = T::name();
        serde_json::from_value::<T>(instance.clone()).unwrap_or_else(|error| {
            panic!("{component} does not deserialize from {instance}: {error}")
        });
        assert!(
            is_valid(&component, instance),
            "{component} accepts {instance}, which its schema rejects",
        );
    }

    /// Asserts that `instance`, which `serde` rejects for `T`, is also rejected
    /// by `T`'s component.
    fn assert_rejects<T: DeserializeOwned + ToSchema>(instance: &serde_json::Value) {
        let component = T::name();
        assert!(
            serde_json::from_value::<T>(instance.clone()).is_err(),
            "{component} unexpectedly deserializes from {instance}",
        );
        assert!(
            !is_valid(&component, instance),
            "{component} rejects {instance}, which its schema accepts",
        );
    }

    fn signed_ops() -> AnySignedOps {
        macro_rules! signed {
            ($variant:ident, $op:ident, $proof:expr) => {
                SignedOp::$variant(SignedOperation::new($op.clone(), $proof).into_state_trusted())
            };
        }

        SignedOps::from([
            signed!(Transfer, TRANSFER, ZK_SIG.clone()),
            signed!(ChannelConfig, CHANNEL_CONFIG, CHANNEL_MULTI_SIG.clone()),
            signed!(ChannelInscribe, INSCRIPTION, *ED25519_SIG),
            signed!(ChannelDeposit, DEPOSIT, ZK_SIG.clone()),
            signed!(ChannelWithdraw, CHANNEL_WITHDRAW, CHANNEL_MULTI_SIG.clone()),
            signed!(ChannelTransfer, CHANNEL_TRANSFER, CHANNEL_MULTI_SIG.clone()),
            signed!(SDPDeclare, SDP_DECLARE, ZK_AND_ED25519_SIGS.clone()),
            signed!(SDPWithdraw, SDP_WITHDRAW, ZK_SIG.clone()),
            signed!(SDPActive, SDP_ACTIVE, ZK_SIG.clone()),
            signed!(LeaderClaim, LEADER_CLAIM, POC.clone()),
            signed!(ClaimPowReward, CLAIM_POW_REWARD, NoOpProof),
        ])
    }

    fn ed25519_key(seed: u8) -> Ed25519PublicKey {
        Ed25519Key::from_bytes(&[seed; 32]).public_key()
    }

    fn utxo(seed: u8) -> Utxo {
        Utxo::new(
            [seed; 32],
            usize::from(seed),
            Note::new(
                u64::from(seed),
                ZkPublicKey::from(Fr::from(u64::from(seed))),
            ),
        )
    }

    fn header(seed: u8) -> Header {
        Header::new(
            HeaderId::from([seed; 32]),
            ContentId::from([0x22u8; 32]),
            Slot::from(42u64),
            Groth16LeaderProof::from_parts(
                lb_pol::PoLProof::from_bytes(&[0x22u8; _]),
                Fr::from(0x5555u64),
                ed25519_key(0x33),
                VoucherCm::from(Fr::from(0x4444u64)),
            ),
        )
    }

    fn hex_of(value: &impl Serialize) -> String {
        serde_json::to_value(value)
            .expect("value serializes")
            .as_str()
            .expect("value serializes as a string")
            .to_owned()
    }

    #[test]
    fn every_component_compiles() {
        let document = document();
        let components = document["components"]["schemas"]
            .as_object()
            .expect("components are an object");
        for name in components.keys() {
            // A primitive's name here means a type alias leaked into a derive
            // as a reference instead of being documented inline.
            assert!(
                name.starts_with(char::is_uppercase),
                "unexpected component {name}"
            );
            drop(compile(name));
        }
    }

    #[test]
    fn every_op_matches_op() {
        let ops = Ops::sample();
        assert_eq!(ops.len(), 11, "one sample per Op variant");
        for op in &ops {
            let emitted = assert_emits_valid(op);
            assert_serialized_valid("Op", &op.by_ref());
            assert_accepts::<Op>(&emitted);
        }
        let emitted = assert_emits_valid(&ops);
        assert_accepts::<Ops>(&emitted);
        assert_eq!(assert_emits_valid(&OpRefs::from(&ops)), emitted);
    }

    #[test]
    fn every_op_payload_matches_its_own_component() {
        let emitted = assert_emits_valid(&*TRANSFER);
        assert_accepts::<TransferOp>(&emitted);
        assert_emits_valid(&*CHANNEL_CONFIG);
        assert_emits_valid(&*INSCRIPTION);
        assert_emits_valid(&*DEPOSIT);
        assert_emits_valid(&*CHANNEL_WITHDRAW);
        assert_emits_valid(&*CHANNEL_TRANSFER);
        let emitted = assert_emits_valid(&*SDP_DECLARE);
        assert_accepts::<DeclarationMessage>(&emitted);
        assert_emits_valid(&*SDP_WITHDRAW);
        assert_emits_valid(&*SDP_ACTIVE);
        assert_emits_valid(&SDP_ACTIVE.metadata);
        assert_emits_valid(&*LEADER_CLAIM);
        assert_emits_valid(&*CLAIM_POW_REWARD);
    }

    #[test]
    fn every_op_proof_matches_op_proof() {
        let proofs = [
            OpProof::Ed25519Sig(*ED25519_SIG),
            OpProof::ZkSig(ZK_SIG.clone()),
            OpProof::ZkAndEd25519Sigs(ZK_AND_ED25519_SIGS.clone()),
            OpProof::PoC(POC.clone()),
            OpProof::ChannelMultiSigProof(CHANNEL_MULTI_SIG.clone()),
            OpProof::None(NoOpProof),
        ];
        for proof in &proofs {
            let emitted = assert_emits_valid(proof);
            assert_serialized_valid("OpProof", &proof.by_ref());
            assert_accepts::<OpProof>(&emitted);
        }
        // And the sample every op pairs with.
        for op in &Ops::sample() {
            assert_emits_valid(&op.sample_proof());
        }
    }

    #[test]
    fn signed_ops_match_signed_ops() {
        let signed_ops = signed_ops();
        let emitted = assert_emits_valid(&signed_ops);
        assert_accepts::<AnySignedOps>(&emitted);
        assert_serialized_valid("OpProofs", &signed_ops.op_proof_refs());

        let unsigned = mantle_spec::serialize(&signed_ops.op_refs(), serde_json::value::Serializer)
            .expect("the unsigned transaction serializes");
        assert!(is_valid("MantleTx", &unsigned));
        assert_eq!(unsigned, emitted["mantle_tx"]);
    }

    #[test]
    fn mantle_tx_builder_matches_mantle_tx_builder() {
        let builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(INSCRIPTION.clone()))
            .and_then(|builder| builder.push_op(Op::ChannelWithdraw(CHANNEL_WITHDRAW.clone())))
            .and_then(|builder| builder.add_ledger_input(utxo(5)))
            .and_then(|builder| builder.add_ledger_output(Note::new(4, ZkPublicKey::zero())))
            .expect("the builder is within bounds");
        let mut emitted = assert_emits_valid(&builder);

        // The proofs have no builder method; they are only filled in over the
        // wire.
        emitted["channel_multi_sig_proofs"] = serde_json::json!({
            "1": serde_json::to_value(&*CHANNEL_MULTI_SIG).expect("the proof serializes"),
        });
        assert_accepts::<MantleTxBuilder>(&emitted);
        let restored: MantleTxBuilder =
            serde_json::from_value(emitted).expect("the builder deserializes");
        assert_eq!(restored.channel_multi_sig_proofs().len(), 1);
        assert_emits_valid(&restored);
    }

    #[test]
    fn sdp_types_match_their_components() {
        let message = DeclarationMessage::sample();
        assert_emits_valid(&message);
        assert_emits_valid(&message.service_type);
        assert_emits_valid(&message.provider_id);

        let mut declaration = Declaration::new(Epoch::new(3), &message);
        let emitted = assert_emits_valid(&declaration);
        assert_eq!(emitted["withdraw_at"], serde_json::Value::Null);
        assert_accepts::<Declaration>(&emitted);
        declaration.withdraw_at = Some(Epoch::new(u32::MAX));
        assert_emits_valid(&declaration);

        assert_rejects::<ServiceType>(&serde_json::json!("BlendNetwork"));
    }

    #[test]
    fn channel_state_matches_channel_state() {
        let state = ChannelState {
            accredited_keys: Arc::new([ed25519_key(1).into_unverified()].into()),
            configuration_threshold: 1,
            tip_message: MsgId::from([2u8; 32]),
            config_tip_hash: MsgId::root(),
            tip_slot: Slot::from(3u64),
            tip_sequencer: 0,
            tip_sequencer_starting_slot: Slot::from(4u64),
            posting_timeframe: SlotTimeframe::from(5u32),
            posting_timeout: SlotTimeout::from(6u32),
            transfer_threshold: u16::MAX,
        };
        let emitted = assert_emits_valid(&state);
        assert_accepts::<ChannelState>(&emitted);
        assert_emits_valid(&ChannelId::from([7u8; 32]));
    }

    #[test]
    fn headers_match_their_components() {
        let header = header(0x11);
        let emitted = assert_emits_valid(&header);
        assert_accepts::<Header>(&emitted);
        assert_emits_valid(header.leader_proof());
        assert_emits_valid(&SignedHeader::new(header, *ED25519_SIG));
    }

    #[test]
    fn events_match_events() {
        let deposit_notes = [DepositNote {
            note_id: NoteId::from(Fr::from(1u64)),
            value: u64::MAX,
            pk: ZkPublicKey::from(Fr::from(2u64)),
        }];
        let events: Events = [
            Event::Tx(TxEvent::new(
                TxHash([1u8; 32]),
                [2u8; 32],
                TxEventPayload::Deposit {
                    channel_id: ChannelId::from([3u8; 32]),
                    amount: 4,
                    metadata: b"metadata".to_vec().try_into().expect("within bounds"),
                    notes: deposit_notes.into(),
                },
            )),
            Event::Tx(TxEvent::new(
                TxHash([5u8; 32]),
                [6u8; 32],
                TxEventPayload::LeaderRewardClaimed {
                    voucher_nullifier: Fr::from(7u64).into(),
                    utxo: utxo(8),
                },
            )),
            Event::Tx(TxEvent::new(
                TxHash([9u8; 32]),
                [10u8; 32],
                TxEventPayload::PoWRewardClaimed {
                    pow_nullifier: PowNullifier::default(),
                    utxo: utxo(11),
                },
            )),
            Event::Header(HeaderEvent::SdpNoteUnlocked {
                note_id: NoteId::from(Fr::from(12u64)),
                service_type: ServiceType::BlendNetwork,
                declaration_id: DeclarationId([13u8; 32]),
            }),
            Event::Header(HeaderEvent::SdpRewardDistributed {
                service_type: ServiceType::BlendNetwork,
                utxo: utxo(14),
            }),
        ]
        .into_iter()
        .collect();

        let emitted = assert_emits_valid(&events);
        assert!(emitted.is_array(), "events serialize as an array");
        assert_accepts::<Events>(&emitted);
    }

    #[test]
    fn leaf_types_match_their_components() {
        assert_emits_valid(&GasCost::new(u64::MAX));
        assert_emits_valid(&GasPrice::new(1));
        assert_emits_valid(&Gas::new(2));
        assert_emits_valid(&GasPrices::default());
        assert_emits_valid(&VoucherCm::from(Fr::from(3u64)));
        assert_emits_valid(&VoucherNullifier::from(Fr::from(4u64)));
        assert_emits_valid(&ZkPublicKey::from(Fr::from(5u64)));
        assert_emits_valid(&ed25519_key(6));
        assert_emits_valid(&ed25519_key(6).into_unverified());
        assert_emits_valid(&*ED25519_SIG);
        assert_emits_valid(&*ZK_SIG);
        assert_emits_valid(&Version::Bedrock);
        assert_serialized_valid(
            "ZkPublicKeys",
            &[ZkPublicKey::zero(), ZkPublicKey::from(Fr::from(1u64))],
        );
    }

    /// Inputs `serde` accepts beyond what it emits.
    #[test]
    fn permissive_inputs_match_their_components() {
        // Fixed-size hex also accepts a `0x` prefix and uppercase digits.
        let proof = serde_json::to_value(&*POC).expect("the proof serializes");
        assert_accepts::<Groth16LeaderClaimProof>(&serde_json::json!({
            "proof": format!("0x{}", hex_of(&proof["proof"]).to_uppercase()),
        }));
        assert_accepts::<Ed25519PublicKey>(&serde_json::json!(format!(
            "0x{}",
            hex_of(&ed25519_key(1))
        )));
        assert_accepts::<UnverifiedEd25519PublicKey>(&serde_json::json!(hex_of(&ed25519_key(1))));

        // Inscriptions are variable-length hex, with the same optional prefix.
        let mut inscription = serde_json::to_value(&*INSCRIPTION).expect("serializes");
        inscription["inscription"] = serde_json::json!("0xDEADbeef");
        assert_accepts::<InscriptionOp>(&inscription);
        inscription["inscription"] = serde_json::json!("abc");
        assert_rejects::<InscriptionOp>(&inscription);

        // ZkSign proof points are also accepted as arrays of byte values.
        assert_accepts::<ZkSignature>(&serde_json::json!({
            "pi_a": vec![1u8; 32],
            "pi_b": format!("0x{}", "02".repeat(64)),
            "pi_c": vec![3u8; 32],
        }));
        assert_rejects::<ZkSignature>(&serde_json::json!({
            "pi_a": vec![1u8; 31],
            "pi_b": "02".repeat(64),
            "pi_c": vec![3u8; 32],
        }));

        // The version is matched case-insensitively.
        assert_accepts::<Version>(&serde_json::json!("BEDROCK"));
        assert_rejects::<Version>(&serde_json::json!("Bedrock2"));

        // Unknown fields are ignored.
        let mut op = serde_json::to_value(Op::Transfer(TRANSFER.clone())).expect("serializes");
        op["extra"] = serde_json::json!(true);
        assert_accepts::<Op>(&op);
    }

    /// The tagging is strict enough to tell every variant apart.
    #[test]
    fn mistagged_values_are_rejected() {
        // An opcode that does not match the payload.
        let mut op = serde_json::to_value(Op::Transfer(TRANSFER.clone())).expect("serializes");
        op["opcode"] = serde_json::json!(0x11);
        assert_rejects::<Op>(&op);

        // A proof carrying two variants.
        assert_rejects::<OpProof>(&serde_json::json!({
            "None": null,
            "Ed25519Sig": serde_json::to_value(*ED25519_SIG).expect("serializes"),
        }));
        // A unit-like variant written as a bare string.
        assert_rejects::<OpProof>(&serde_json::json!("None"));

        // Signature indices must be strictly increasing, which the schema
        // documents but cannot express.
        let proof = serde_json::to_value(ChannelMultiSigProof::sample_with_signatures(2))
            .expect("serializes");
        assert!(is_valid("ChannelMultiSigProof", &proof));
    }
}
