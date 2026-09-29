//! `OpenAPI` schemas for the transaction columns.
//!
//! The borrowed columns serialize exactly like their owned counterparts, so
//! each pair shares one component.

use std::borrow::Cow;

use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        RefOr, Type,
        schema::{ArrayBuilder, ObjectBuilder, Schema},
    },
};

use crate::{
    mantle::{
        Op, OpProof, OpProofRef, OpRef,
        ledger::verification_mode::VerificationMode,
        ops::SignedOp,
        transactions::{MAX_OPS_PER_TX, states::VerificationState, tx_list::common::TxList},
    },
    openapi::{MantleTx, Schemas, collect, reference},
};

fn column<T: ToSchema>(description: &str) -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(reference::<T>())
        .max_items(Some(MAX_OPS_PER_TX))
        .description(Some(description))
        .into()
}

macro_rules! column_schema {
    ($name:literal, $item:ty, $description:literal => $($column:ty),+ $(,)?) => {
        $(
            impl PartialSchema for $column {
                fn schema() -> RefOr<Schema> {
                    column::<$item>($description)
                }
            }

            impl ToSchema for $column {
                fn name() -> Cow<'static, str> {
                    Cow::Borrowed($name)
                }

                fn schemas(schemas: &mut Schemas) {
                    collect::<$item>(schemas);
                }
            }
        )+
    };
}

column_schema!(
    "Ops", Op, "The operations of a transaction, in order." =>
        TxList<Op>, TxList<OpRef<'_>>,
);

column_schema!(
    "OpProofs", OpProof, "One proof per operation, at the index of the operation it authorizes." =>
        TxList<OpProof>, TxList<OpProofRef<'_>>,
);

impl<State: VerificationState, Mode: VerificationMode> PartialSchema
    for TxList<SignedOp<State, Mode>>
{
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .description(Some(
                "Signed Mantle transaction: the unsigned transaction, plus one proof per \
                 operation. `ops_proofs` must be as long as `mantle_tx.ops`.",
            ))
            .property("mantle_tx", reference::<MantleTx>())
            .property("ops_proofs", reference::<TxList<OpProof>>())
            .required("mantle_tx")
            .required("ops_proofs")
            .into()
    }
}

impl<State: VerificationState, Mode: VerificationMode> ToSchema for TxList<SignedOp<State, Mode>> {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("SignedOps")
    }

    fn schemas(schemas: &mut Schemas) {
        collect::<MantleTx>(schemas);
        collect::<TxList<OpProof>>(schemas);
    }
}
