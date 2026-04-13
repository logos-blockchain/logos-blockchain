use std::fmt::{Debug, Formatter};

use lb_key_management_system_keys::keys::Ed25519Signature;

use crate::{
    block::Block,
    header::Header,
    mantle::{
        Op, OpProof, SignedMantleTx,
        genesis_tx::{self, GenesisTx},
        ops::sdp::SDPDeclareOp,
        tx::VerificationError,
        tx_builder::MantleTxBuilder,
    },
};

/// Errors that can occur when building a genesis block via
/// [`GenesisBlockBuilder`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The op proofs supplied to [`SignedMantleTx`] failed verification.
    #[error("Transaction verification failed: {0}")]
    Verification(#[from] VerificationError),
    /// The constructed transaction does not satisfy genesis transaction
    /// invariants (e.g. non-zero gas price, missing transfer/inscription,
    /// unsupported ops).
    #[error("Invalid genesis transaction: {0}")]
    InvalidGenesisTx(#[from] genesis_tx::Error),
}

/// Convenience [`Result`](core::result::Result) alias for genesis block
/// construction.
pub type Result<T> = core::result::Result<T, Error>;

/// A [`Block`] whose transactions are all [`GenesisTx`] values.
///
/// The block carries a sentinel
/// [`Groth16LeaderProof`](crate::proofs::leader_proof::Groth16LeaderProof)
/// and an all-zero signature; it is not produced by a normal slot leader
/// election.
pub type GenesisBlock = Block<GenesisTx>;

impl GenesisBlock {
    /// Create a genesis block from the given transactions.
    ///
    /// Genesis blocks use a sentinel leader proof and an all-zero signature;
    /// they are not signed by any real key because the genesis leader proof
    /// carries an all-zero public key that has no corresponding private key.
    #[must_use]
    pub fn genesis(genesis_tx: GenesisTx) -> Self {
        let header = Header::genesis(&genesis_tx);
        let signature = Ed25519Signature::from_bytes(&[0; 64]);
        let transactions = vec![genesis_tx];
        Self {
            header,
            signature,
            transactions,
        }
    }
}

/// Typestate marker for a [`GenesisBlockBuilder`] that has not yet received any
/// input.
struct Empty;

/// Typestate marker for a [`GenesisBlockBuilder`] that holds a fully-formed
/// [`GenesisTx`].
struct WithGenesisTx {
    tx: GenesisTx,
}

/// Typestate marker for a [`GenesisBlockBuilder`] that is accumulating
/// SDP service-declaration ops to be bundled into the genesis transaction.
struct WithDeclarations {
    sdp_declarations: Vec<(SDPDeclareOp, OpProof)>,
}

/// Staged builder for a [`GenesisBlock`].
///
/// The builder is parameterised over a typestate that enforces a valid
/// construction sequence at compile time.  There are two independent paths:
///
/// 1. **Pre-built transaction** — supply an already-validated [`GenesisTx`]
///    directly: ```rust,ignore GenesisBlockBuilder::new() .with_genesis_tx(tx)
///    .build()          // infallible ```
///
/// 2. **Declaration-first** — accumulate [`SDPDeclareOp`] entries and let the
///    builder construct the transaction: ```rust,ignore
///    GenesisBlockBuilder::new() .add_declaration(decl1, proof1)
///    .add_declaration(decl2, proof2) .build()          // fallible — returns
///    Result<GenesisBlock> ```
pub struct GenesisBlockBuilder<State> {
    state: State,
}

impl Default for GenesisBlockBuilder<Empty> {
    fn default() -> Self {
        Self::new()
    }
}

impl<State> Debug for GenesisBlockBuilder<State> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("GenesisBlockBuilder")
    }
}

impl GenesisBlockBuilder<Empty> {
    /// Create a new, empty builder.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: Empty }
    }

    /// Transition to the [`WithGenesisTx`] state by supplying a pre-validated
    /// [`GenesisTx`].  Use this path when the transaction has already been
    /// constructed and verified externally.
    #[must_use]
    pub const fn with_genesis_tx(self, tx: GenesisTx) -> GenesisBlockBuilder<WithGenesisTx> {
        GenesisBlockBuilder {
            state: WithGenesisTx { tx },
        }
    }

    /// Transition to the [`WithDeclarations`] state by adding the first SDP
    /// service-declaration op and its associated proof.  Further declarations
    /// can be appended with
    /// [`GenesisBlockBuilder<WithDeclarations>::add_declaration`].
    #[must_use]
    pub fn add_declaration(
        self,
        declaration: SDPDeclareOp,
        proof: OpProof,
    ) -> GenesisBlockBuilder<WithDeclarations> {
        GenesisBlockBuilder {
            state: WithDeclarations {
                sdp_declarations: vec![(declaration, proof)],
            },
        }
    }
}

impl GenesisBlockBuilder<WithDeclarations> {
    /// Append another SDP service-declaration op and its proof.
    #[must_use]
    pub fn add_declaration(self, declaration: SDPDeclareOp, proof: OpProof) -> Self {
        let Self {
            state: WithDeclarations {
                mut sdp_declarations,
            },
        } = self;
        sdp_declarations.push((declaration, proof));
        Self {
            state: WithDeclarations { sdp_declarations },
        }
    }

    /// Assemble the accumulated declarations into a [`GenesisTx`] and wrap it
    /// in a [`GenesisBlock`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Verification`] if the op proofs fail signature/proof
    /// verification, or [`Error::InvalidGenesisTx`] if the resulting
    /// transaction does not satisfy genesis transaction invariants.
    pub fn build(self) -> Result<GenesisBlock> {
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        let (declarations, proofs): (Vec<_>, Vec<_>) = sdp_declarations.into_iter().unzip();
        let tx = MantleTxBuilder::new()
            .extend_ops(declarations.into_iter().map(Op::SDPDeclare))
            .build();
        let signed_tx = SignedMantleTx::new(tx, proofs)?;
        Ok(GenesisBlock::genesis(GenesisTx::from_tx(signed_tx)?))
    }
}

impl GenesisBlockBuilder<WithGenesisTx> {
    /// Wrap the pre-built [`GenesisTx`] in a [`GenesisBlock`].
    #[must_use]
    pub fn build(self) -> GenesisBlock {
        GenesisBlock::genesis(self.state.tx)
    }
}
