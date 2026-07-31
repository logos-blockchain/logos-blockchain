use std::fmt::{Debug, Formatter};

use lb_cryptarchia_engine::Slot;
use lb_groth16::CompressedGroth16Proof;
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use lb_utils::bounded::{BoundedError, UpperBoundedVec};
use serde::{Deserialize, Serialize};

use crate::{
    block::{Block, BlockTransactions},
    header::Header,
    mantle::{
        Note, Op, OpProof, SignedMantleTx,
        ledger::{BoundedOutputs, Inputs, Outputs},
        ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp, transfer::TransferOp},
        transactions::{
            GenesisTx, MAX_OPS_PER_TX, Ops, OpsProofs, VerificationError, genesis_tx,
            mantle_tx::MantleTx,
        },
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
    #[error("add_notes called with empty iterator")]
    EmptyNotes,
    #[error("too few notes for genesis transfer outputs: attempted {actual}, min {min}")]
    TooFewNotes { actual: usize, min: usize },
    #[error("too many notes for genesis transfer outputs: attempted {actual}, max {max}")]
    TooManyNotes { actual: usize, max: usize },
    #[error("Index {index} is out of bounds for length {len}")]
    IndexOutOfBounds { index: usize, len: usize },
}

/// Convenience [`Result`](core::result::Result) alias for genesis block
/// construction.
pub type Result<T> = core::result::Result<T, Error>;

const fn map_notes_bounded_error(error: &BoundedError) -> Error {
    match error {
        BoundedError::TooManyItems { count: actual, max } => Error::TooManyNotes {
            actual: *actual,
            max: *max,
        },
        BoundedError::TooFewItems { count: actual, min } => Error::TooFewNotes {
            actual: *actual,
            min: *min,
        },
        BoundedError::IndexOutOfBounds { index, len } => Error::IndexOutOfBounds {
            index: *index,
            len: *len,
        },
        BoundedError::EmptyInput => Error::EmptyNotes,
    }
}

fn collect_non_empty_notes<I, N>(notes: I) -> Result<BoundedOutputs>
where
    I: IntoIterator<Item = N>,
    N: Into<Note>,
{
    let mut notes_iter = notes.into_iter().map(Into::into).peekable();
    if notes_iter.peek().is_none() {
        return Err(Error::EmptyNotes);
    }
    BoundedOutputs::try_from_iter(notes_iter).map_err(|error| map_notes_bounded_error(&error))
}

fn push_note(mut notes: BoundedOutputs, note: Note) -> Result<BoundedOutputs> {
    notes
        .try_push(note)
        .map_err(|error| map_notes_bounded_error(&error))?;
    Ok(notes)
}

fn extend_non_empty_notes<I, N>(existing: BoundedOutputs, notes: I) -> Result<BoundedOutputs>
where
    I: IntoIterator<Item = N>,
    N: Into<Note>,
{
    let mut notes_iter = notes.into_iter().map(Into::into).peekable();
    if notes_iter.peek().is_none() {
        return Err(Error::EmptyNotes);
    }
    BoundedOutputs::try_from_iter(existing.into_iter().chain(notes_iter))
        .map_err(|error| map_notes_bounded_error(&error))
}

/// A [`Block`] whose transactions are all [`GenesisTx`] values.
///
/// The block carries a sentinel
/// [`Groth16LeaderProof`](crate::proofs::leader_proof::Groth16LeaderProof)
/// and an all-zero signature; it is not produced by a normal slot leader
/// election.
#[derive(Clone, Debug, Serialize)]
pub struct GenesisBlock(Block<GenesisTx>);

impl<'de> Deserialize<'de> for GenesisBlock {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawGenesisBlock {
            header: Header,
            signature: Ed25519Signature,
            transactions: BlockTransactions<GenesisTx>,
        }

        let raw = RawGenesisBlock::deserialize(deserializer)?;

        if raw.header.slot() != Slot::genesis() {
            return Err(serde::de::Error::custom("expected genesis slot"));
        }

        if raw.transactions.len() != 1 {
            return Err(serde::de::Error::custom(
                "genesis block must contain exactly one transaction",
            ));
        }

        let block = Block {
            header: raw.header,
            signature: raw.signature,
            transactions: raw.transactions,
        };
        block
            .validate_block_root()
            .map_err(serde::de::Error::custom)?;

        Ok(Self(block))
    }
}

impl GenesisBlock {
    /// Create a genesis block from the given transaction.
    ///
    /// Genesis blocks use a sentinel leader proof and an all-zero signature;
    /// they are not signed by any real key because the genesis leader proof
    /// carries an all-zero public key that has no corresponding private key.
    #[must_use]
    pub fn genesis(genesis_tx: GenesisTx) -> Self {
        let header = Header::genesis(&genesis_tx);
        let signature = Ed25519Signature::from_bytes(&[0; 64]);
        let transactions = BlockTransactions::from([genesis_tx]);
        Self(Block {
            header,
            signature,
            transactions,
        })
    }

    #[must_use]
    pub fn genesis_tx(&self) -> GenesisTx {
        self.0.transactions()[0].clone()
    }

    #[must_use]
    pub fn into_inner(self) -> Block<GenesisTx> {
        self.0
    }
}

impl AsRef<Block<GenesisTx>> for GenesisBlock {
    fn as_ref(&self) -> &Block<GenesisTx> {
        &self.0
    }
}

impl core::ops::Deref for GenesisBlock {
    type Target = Block<GenesisTx>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// ── Typestate markers
// ─────────────────────────────────────────────────────────

/// The genesis transaction structure is always `Transfer` + `ChannelInscribe` +
/// zero or more `SDPDeclareOp`, with maximum operations bounded in `Ops`.
const GENESIS_REQUIRED_OPS: usize = 2;

pub const MAX_GENESIS_DECLARATIONS: usize = MAX_OPS_PER_TX - GENESIS_REQUIRED_OPS; // 253
pub type GenesisSDPDeclareOps = UpperBoundedVec<SDPDeclareOp, MAX_GENESIS_DECLARATIONS>;

/// Typestate marker: builder has no input yet.
pub struct Empty;

/// Typestate marker: builder holds a pre-validated [`GenesisTx`].
pub struct WithGenesisTx {
    tx: GenesisTx,
}

/// Typestate marker: builder has genesis transfer output notes only.
pub struct WithNotes {
    notes: BoundedOutputs,
}

/// Typestate marker: builder has a genesis inscription only.
pub struct WithInscription {
    inscription: InscriptionOp,
}

/// Typestate marker: builder has SDP service-declaration ops only.
pub struct WithDeclarations {
    sdp_declarations: GenesisSDPDeclareOps,
}

/// Typestate marker: builder has genesis notes and an inscription.
pub struct WithNotesAndInscription {
    notes: BoundedOutputs,
    inscription: InscriptionOp,
}

/// Typestate marker: builder has genesis notes and SDP declarations.
pub struct WithNotesAndDeclarations {
    notes: BoundedOutputs,
    sdp_declarations: GenesisSDPDeclareOps,
}

/// Typestate marker: builder has a genesis inscription and SDP declarations.
pub struct WithInscriptionAndDeclarations {
    inscription: InscriptionOp,
    sdp_declarations: GenesisSDPDeclareOps,
}

#[expect(
    clippy::too_long_first_doc_paragraph,
    reason = "Necessary documentation"
)]
/// Typestate marker: builder holds all three pieces required to assemble a
/// [`GenesisTx`] — notes, an inscription, and optional SDP declaration.
/// This is the only state that exposes [`GenesisBlockBuilder::build`].
pub struct WithAll {
    notes: BoundedOutputs,
    inscription: InscriptionOp,
    sdp_declarations: GenesisSDPDeclareOps,
}

// ── Builder
// ───────────────────────────────────────────────────────────────────

/// Staged builder for a [`GenesisBlock`].
///
/// The builder is parameterised over a typestate that enforces a valid
/// construction sequence at compile time.  There are two independent paths:
///
/// 1. **Pre-built transaction** — supply an already-validated [`GenesisTx`]
///    directly:
///
///    ```rust,ignore
///    GenesisBlockBuilder::new()
///        .with_genesis_tx(tx)
///        .build() // infallible
///    ```
///
/// 2. **Op-accumulation** — add [`Note`]s (genesis transfer outputs), an
///    [`InscriptionOp`], and [`SDPDeclareOp`]s in any order.  `build()` becomes
///    available once all three are present:
///
///    ```rust,ignore
///    // any order is fine
///    GenesisBlockBuilder::new()
///        .add_note(note1)
///        .add_declaration(decl1)
///        .set_inscription(inscription) // can also overwrite an earlier one
///        .add_note(note2)
///        .add_declaration(decl2)
///        .build() // fallible — returns Result<GenesisBlock>
///    ```
///
///    Non-emptiness of notes and declarations is guaranteed by the typestate:
///    the first element creates the relevant state; subsequent calls append.
///    Calling `set_inscription` again replaces the previous value.
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

// ── Empty ─────────────────────────────────────────────────────────────────────

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

    /// Add the first genesis transfer output note, transitioning to
    /// [`WithNotes`].
    #[must_use]
    pub fn add_note(self, note: Note) -> GenesisBlockBuilder<WithNotes> {
        GenesisBlockBuilder {
            state: WithNotes {
                notes: [note].into(),
            },
        }
    }

    /// Try add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotes`].
    pub fn try_add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<GenesisBlockBuilder<WithNotes>> {
        let notes = collect_non_empty_notes(notes)?;

        Ok(GenesisBlockBuilder {
            state: WithNotes { notes },
        })
    }

    /// Add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotes`].
    #[must_use]
    pub fn add_notes<const N: usize>(self, notes: [Note; N]) -> GenesisBlockBuilder<WithNotes> {
        GenesisBlockBuilder {
            state: WithNotes {
                notes: notes.into(),
            },
        }
    }

    /// Set the genesis inscription, transitioning to [`WithInscription`].
    #[must_use]
    pub const fn set_inscription(
        self,
        inscription: InscriptionOp,
    ) -> GenesisBlockBuilder<WithInscription> {
        GenesisBlockBuilder {
            state: WithInscription { inscription },
        }
    }

    /// Add the first SDP service-declaration op, transitioning to
    /// [`WithDeclarations`].
    #[must_use]
    pub fn add_declaration(
        self,
        declaration: SDPDeclareOp,
    ) -> GenesisBlockBuilder<WithDeclarations> {
        GenesisBlockBuilder {
            state: WithDeclarations {
                sdp_declarations: [declaration].into(),
            },
        }
    }

    /// Add multiple SDP service-declaration ops at once, transitioning to
    /// [`WithDeclarations`].
    ///
    /// # Panics and Errors
    ///
    /// Panics if `declarations` is empty.
    /// Returns an error if the number of declarations exceeds the maximum
    /// allowed.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<GenesisBlockBuilder<WithDeclarations>> {
        let iter = require_non_empty(declarations, "add_declarations");
        Ok(GenesisBlockBuilder {
            state: WithDeclarations {
                sdp_declarations: try_collect_sdp_declarations(iter)?,
            },
        })
    }
}

// ── WithNotes
// ─────────────────────────────────────────────────────────────────

impl GenesisBlockBuilder<WithNotes> {
    /// Append another genesis transfer output note.
    pub fn try_add_note(self, note: Note) -> Result<Self> {
        let Self {
            state: WithNotes { mut notes },
        } = self;
        notes = push_note(notes, note)?;
        Ok(Self {
            state: WithNotes { notes },
        })
    }

    /// Try append multiple genesis transfer output notes at once.
    pub fn try_add_notes(
        self,
        notes_to_add: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<Self> {
        let Self {
            state: WithNotes { mut notes },
        } = self;
        notes = extend_non_empty_notes(notes, notes_to_add)?;
        Ok(Self {
            state: WithNotes { notes },
        })
    }

    /// Set the genesis inscription, transitioning to
    /// [`WithNotesAndInscription`].
    #[must_use]
    pub fn set_inscription(
        self,
        inscription: InscriptionOp,
    ) -> GenesisBlockBuilder<WithNotesAndInscription> {
        let Self {
            state: WithNotes { notes },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndInscription { notes, inscription },
        }
    }

    /// Add the first SDP declaration, transitioning to
    /// [`WithNotesAndDeclarations`].
    #[must_use]
    pub fn add_declaration(
        self,
        declaration: SDPDeclareOp,
    ) -> GenesisBlockBuilder<WithNotesAndDeclarations> {
        let Self {
            state: WithNotes { notes },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations: [declaration].into(),
            },
        }
    }

    /// Add multiple SDP declarations at once, transitioning to
    /// [`WithNotesAndDeclarations`].
    ///
    /// # Panics and Errors
    ///
    /// Panics if `declarations` is empty.
    /// Errors if the number of declarations exceeds the maximum allowed.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<GenesisBlockBuilder<WithNotesAndDeclarations>> {
        let iter = require_non_empty(declarations, "add_declarations");
        let Self {
            state: WithNotes { notes },
        } = self;
        Ok(GenesisBlockBuilder {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations: try_collect_sdp_declarations(iter)?,
            },
        })
    }
}

// ── WithInscription
// ───────────────────────────────────────────────────────────

impl GenesisBlockBuilder<WithInscription> {
    /// Add the first genesis transfer output note, transitioning to
    /// [`WithNotesAndInscription`].
    #[must_use]
    pub fn add_note(self, note: Note) -> GenesisBlockBuilder<WithNotesAndInscription> {
        let Self {
            state: WithInscription { inscription },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndInscription {
                notes: [note].into(),
                inscription,
            },
        }
    }

    /// Try add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotesAndInscription`].
    pub fn try_add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<GenesisBlockBuilder<WithNotesAndInscription>> {
        let Self {
            state: WithInscription { inscription },
        } = self;
        Ok(GenesisBlockBuilder {
            state: WithNotesAndInscription {
                notes: collect_non_empty_notes(notes)?,
                inscription,
            },
        })
    }

    /// Add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotesAndInscription`].
    #[must_use]
    pub fn add_notes<const N: usize>(
        self,
        notes: [Note; N],
    ) -> GenesisBlockBuilder<WithNotesAndInscription> {
        let Self {
            state: WithInscription { inscription },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndInscription {
                notes: notes.into(),
                inscription,
            },
        }
    }

    /// Replace the current inscription.
    #[must_use]
    pub fn set_inscription(self, inscription: InscriptionOp) -> Self {
        Self {
            state: WithInscription { inscription },
        }
    }

    /// Add the first SDP declaration, transitioning to
    /// [`WithInscriptionAndDeclarations`].
    #[must_use]
    pub fn add_declaration(
        self,
        declaration: SDPDeclareOp,
    ) -> GenesisBlockBuilder<WithInscriptionAndDeclarations> {
        let Self {
            state: WithInscription { inscription },
        } = self;
        GenesisBlockBuilder {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations: [declaration].into(),
            },
        }
    }

    /// Add multiple SDP declarations at once, transitioning to
    /// [`WithInscriptionAndDeclarations`].
    ///
    /// # Panics and Errors
    ///
    /// Panics if `declarations` is empty.
    /// Errors if the number of declarations exceeds the maximum allowed.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<GenesisBlockBuilder<WithInscriptionAndDeclarations>> {
        let iter = require_non_empty(declarations, "add_declarations");
        let Self {
            state: WithInscription { inscription },
        } = self;
        Ok(GenesisBlockBuilder {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations: try_collect_sdp_declarations(iter)?,
            },
        })
    }
}

// ── WithDeclarations
// ──────────────────────────────────────────────────────────

impl GenesisBlockBuilder<WithDeclarations> {
    /// Add the first genesis transfer output note, transitioning to
    /// [`WithNotesAndDeclarations`].
    #[must_use]
    pub fn add_note(self, note: Note) -> GenesisBlockBuilder<WithNotesAndDeclarations> {
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndDeclarations {
                notes: [note].into(),
                sdp_declarations,
            },
        }
    }

    /// Try add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotesAndDeclarations`].
    pub fn try_add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<GenesisBlockBuilder<WithNotesAndDeclarations>> {
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        Ok(GenesisBlockBuilder {
            state: WithNotesAndDeclarations {
                notes: collect_non_empty_notes(notes)?,
                sdp_declarations,
            },
        })
    }

    /// Add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotesAndDeclarations`].
    #[must_use]
    pub fn add_notes<const N: usize>(
        self,
        notes: [Note; N],
    ) -> GenesisBlockBuilder<WithNotesAndDeclarations> {
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndDeclarations {
                notes: notes.into(),
                sdp_declarations,
            },
        }
    }

    /// Set the genesis inscription, transitioning to
    /// [`WithInscriptionAndDeclarations`].
    #[must_use]
    pub fn set_inscription(
        self,
        inscription: InscriptionOp,
    ) -> GenesisBlockBuilder<WithInscriptionAndDeclarations> {
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        GenesisBlockBuilder {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Append another SDP declaration.
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Result<Self> {
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        let sdp_declarations = try_push_genesis_declaration(sdp_declarations, declaration)?;
        Ok(Self {
            state: WithDeclarations { sdp_declarations },
        })
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<Self> {
        let iter = require_non_empty(declarations, "add_declarations");
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        let sdp_declarations =
            try_extend_genesis_declarations(sdp_declarations, iter.map(Into::into))?;
        Ok(Self {
            state: WithDeclarations { sdp_declarations },
        })
    }
}

// ── WithNotesAndInscription
// ───────────────────────────────────────────────────

impl GenesisBlockBuilder<WithNotesAndInscription> {
    /// Append another genesis transfer output note.
    pub fn add_note(self, note: Note) -> Result<Self> {
        let Self {
            state:
                WithNotesAndInscription {
                    mut notes,
                    inscription,
                },
        } = self;
        notes = push_note(notes, note)?;
        Ok(Self {
            state: WithNotesAndInscription { notes, inscription },
        })
    }

    /// Append multiple genesis transfer output notes at once.
    pub fn add_notes(
        self,
        notes_to_add: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<Self> {
        let Self {
            state:
                WithNotesAndInscription {
                    mut notes,
                    inscription,
                },
        } = self;
        notes = extend_non_empty_notes(notes, notes_to_add)?;
        Ok(Self {
            state: WithNotesAndInscription { notes, inscription },
        })
    }

    /// Replace the current inscription.
    #[must_use]
    pub fn set_inscription(self, inscription: InscriptionOp) -> Self {
        let Self {
            state: WithNotesAndInscription { notes, .. },
        } = self;
        Self {
            state: WithNotesAndInscription { notes, inscription },
        }
    }

    /// Add the first SDP declaration, completing all three pieces and
    /// transitioning to [`WithAll`].
    #[must_use]
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> GenesisBlockBuilder<WithAll> {
        let Self {
            state: WithNotesAndInscription { notes, inscription },
        } = self;
        GenesisBlockBuilder {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations: [declaration].into(),
            },
        }
    }

    /// Add multiple SDP declarations at once, completing all three pieces and
    /// transitioning to [`WithAll`].
    ///
    /// # Panics and Errors
    ///
    /// Panics if `declarations` is empty.
    /// Errors if the number of declarations exceeds the maximum allowed.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<GenesisBlockBuilder<WithAll>> {
        let iter = require_non_empty(declarations, "add_declarations");
        let Self {
            state: WithNotesAndInscription { notes, inscription },
        } = self;
        Ok(GenesisBlockBuilder {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations: try_collect_sdp_declarations(iter)?,
            },
        })
    }

    // Build a block with empty declarations but properly set inscription and
    // transfer.
    pub fn build(self) -> Result<GenesisBlock> {
        GenesisBlockBuilder {
            state: WithAll {
                notes: self.state.notes,
                inscription: self.state.inscription,
                sdp_declarations: GenesisSDPDeclareOps::empty(),
            },
        }
        .build()
    }
}

// ── Helpers
// ──────────────────────────────────────────────────

fn require_non_empty<I>(iterable: I, context: &'static str) -> impl Iterator<Item = I::Item>
where
    I: IntoIterator,
{
    let mut iter = iterable.into_iter();

    let Some(first) = iter.next() else {
        panic!("{context} called with empty iterator");
    };

    std::iter::once(first).chain(iter)
}

fn try_collect_sdp_declarations<I, D>(declarations: I) -> Result<GenesisSDPDeclareOps>
where
    I: IntoIterator<Item = D>,
    D: Into<SDPDeclareOp>,
{
    GenesisSDPDeclareOps::try_from_iter(declarations.into_iter().map(Into::into)).map_err(|error| {
        match error {
            BoundedError::TooManyItems { count, .. } => {
                Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps {
                    count: GENESIS_REQUIRED_OPS + count,
                })
            }
            BoundedError::EmptyInput | BoundedError::TooFewItems { .. } => {
                unreachable!("GenesisSDPDeclareOps has a zero minimum bound")
            }
            BoundedError::IndexOutOfBounds { .. } => {
                unreachable!("construction cannot produce an index error")
            }
        }
    })
}

fn try_push_genesis_declaration(
    mut declarations: GenesisSDPDeclareOps,
    declaration: SDPDeclareOp,
) -> Result<GenesisSDPDeclareOps> {
    declarations.try_push(declaration).map_err(|_| {
        Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps {
            count: GENESIS_REQUIRED_OPS + declarations.len() + 1,
        })
    })?;
    Ok(declarations)
}

fn try_extend_genesis_declarations<I>(
    mut declarations: GenesisSDPDeclareOps,
    new_items: I,
) -> Result<GenesisSDPDeclareOps>
where
    I: IntoIterator<Item = SDPDeclareOp>,
{
    for item in new_items {
        declarations.try_push(item).map_err(|_| {
            Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps {
                count: GENESIS_REQUIRED_OPS + declarations.len() + 1,
            })
        })?;
    }
    Ok(declarations)
}

// ── WithNotesAndDeclarations
// ──────────────────────────────────────────────────

impl GenesisBlockBuilder<WithNotesAndDeclarations> {
    /// Append another genesis transfer output note.
    pub fn add_note(self, note: Note) -> Result<Self> {
        let Self {
            state:
                WithNotesAndDeclarations {
                    mut notes,
                    sdp_declarations,
                },
        } = self;
        notes = push_note(notes, note)?;
        Ok(Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        })
    }

    /// Append multiple genesis transfer output notes at once.
    pub fn add_notes(
        self,
        notes_to_add: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<Self> {
        let Self {
            state:
                WithNotesAndDeclarations {
                    mut notes,
                    sdp_declarations,
                },
        } = self;
        notes = extend_non_empty_notes(notes, notes_to_add)?;
        Ok(Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        })
    }

    /// Set the genesis inscription, completing all three pieces and
    /// transitioning to [`WithAll`].
    #[must_use]
    pub fn set_inscription(self, inscription: InscriptionOp) -> GenesisBlockBuilder<WithAll> {
        let Self {
            state:
                WithNotesAndDeclarations {
                    notes,
                    sdp_declarations,
                },
        } = self;
        GenesisBlockBuilder {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Append another SDP declaration.
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Result<Self> {
        let Self {
            state:
                WithNotesAndDeclarations {
                    notes,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations = try_push_genesis_declaration(sdp_declarations, declaration)?;
        Ok(Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        })
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<Self> {
        let iter = require_non_empty(declarations, "add_declarations");
        let Self {
            state:
                WithNotesAndDeclarations {
                    notes,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations = try_extend_genesis_declarations(sdp_declarations, iter.map(Into::into))?;
        Ok(Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        })
    }
}

// ── WithInscriptionAndDeclarations
// ────────────────────────────────────────────

impl GenesisBlockBuilder<WithInscriptionAndDeclarations> {
    /// Add the first genesis transfer output note, completing all three pieces
    /// and transitioning to [`WithAll`].
    #[must_use]
    pub fn add_note(self, note: Note) -> GenesisBlockBuilder<WithAll> {
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    inscription,
                    sdp_declarations,
                },
        } = self;
        GenesisBlockBuilder {
            state: WithAll {
                notes: [note].into(),
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Add multiple genesis transfer output notes at once, completing all three
    /// pieces and transitioning to [`WithAll`].
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    pub fn add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<GenesisBlockBuilder<WithAll>> {
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    inscription,
                    sdp_declarations,
                },
        } = self;
        Ok(GenesisBlockBuilder {
            state: WithAll {
                notes: collect_non_empty_notes(notes)?,
                inscription,
                sdp_declarations,
            },
        })
    }

    /// Replace the current inscription.
    #[must_use]
    pub fn set_inscription(self, inscription: InscriptionOp) -> Self {
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    sdp_declarations, ..
                },
        } = self;
        Self {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Append another SDP declaration.
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Result<Self> {
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    inscription,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations = try_push_genesis_declaration(sdp_declarations, declaration)?;
        Ok(Self {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations,
            },
        })
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<Self> {
        let iter = require_non_empty(declarations, "add_declarations");
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    inscription,
                    sdp_declarations,
                },
        } = self;
        let sdp_declarations =
            try_extend_genesis_declarations(sdp_declarations, iter.map(Into::into))?;
        Ok(Self {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations,
            },
        })
    }
}

// ── WithAll
// ───────────────────────────────────────────────────────────────────

impl GenesisBlockBuilder<WithAll> {
    /// Append another genesis transfer output note.
    pub fn add_note(self, note: Note) -> Result<Self> {
        let Self {
            state:
                WithAll {
                    mut notes,
                    inscription,
                    sdp_declarations,
                },
        } = self;
        notes = push_note(notes, note)?;
        Ok(Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        })
    }

    /// Append multiple genesis transfer output notes at once.
    pub fn add_notes(
        self,
        notes_to_add: impl IntoIterator<Item = impl Into<Note>>,
    ) -> Result<Self> {
        let Self {
            state:
                WithAll {
                    mut notes,
                    inscription,
                    sdp_declarations,
                },
        } = self;
        notes = extend_non_empty_notes(notes, notes_to_add)?;
        Ok(Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        })
    }

    /// Replace the current inscription.
    #[must_use]
    pub fn set_inscription(self, inscription: InscriptionOp) -> Self {
        let Self {
            state:
                WithAll {
                    notes,
                    sdp_declarations,
                    ..
                },
        } = self;
        Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Append another SDP declaration.
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Result<Self> {
        let Self {
            state:
                WithAll {
                    notes,
                    inscription,
                    sdp_declarations,
                },
        } = self;
        let sdp_declarations = try_push_genesis_declaration(sdp_declarations, declaration)?;
        Ok(Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        })
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Result<Self> {
        let iter = require_non_empty(declarations, "add_declarations");
        let Self {
            state:
                WithAll {
                    notes,
                    inscription,
                    sdp_declarations,
                },
        } = self;
        let sdp_declarations =
            try_extend_genesis_declarations(sdp_declarations, iter.map(Into::into))?;
        Ok(Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        })
    }

    /// Assemble the accumulated pieces into a [`GenesisTx`] and wrap it in a
    /// [`GenesisBlock`].
    ///
    /// Ops are ordered as required by [`GenesisTx`]:
    /// `[Transfer(outputs=notes, inputs=[]), ChannelInscribe, SDPDeclare…]`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGenesisTx`] if the [`InscriptionOp`] does not
    /// satisfy genesis inscription invariants (`parent`, `channel_id`, and
    /// `signer` must all be zero/root).
    pub fn build(self) -> Result<GenesisBlock> {
        let Self {
            state:
                WithAll {
                    notes,
                    inscription,
                    sdp_declarations,
                },
        } = self;
        // Order is important to keep here
        let ops: Vec<Op> = std::iter::once(Op::Transfer(TransferOp::new(
            Inputs::empty(),
            Outputs::new(notes),
        )))
        .chain(std::iter::once(Op::ChannelInscribe(inscription)))
        .chain(sdp_declarations.into_iter().map(Op::SDPDeclare))
        .collect();
        let n = ops.len();
        let Ok(capped_ops) = Ops::try_from(ops) else {
            // This should never happen because the builder doesn't allow more
            // ops than can fit in a genesis tx, but we have to handle the error
            // just in case.
            return Err(Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps {
                count: n,
            }));
        };
        let mut ops_proofs = OpsProofs::from([
            OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(
                &[0u8; 128],
            ))),
            OpProof::Ed25519Sig(Ed25519Signature::zero()),
        ]);
        for _ in 0..n - 2 {
            ops_proofs
                .try_push(OpProof::ZkAndEd25519Sigs {
                    zk_sig: ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
                    ed25519_sig: Ed25519Signature::zero(),
                })
                .expect("genesis transaction proofs are bounded");
        }
        let signed_tx = SignedMantleTx::new_trusted(MantleTx(capped_ops), ops_proofs);
        Ok(GenesisBlock::genesis(GenesisTx::from_tx(signed_tx)?))
    }
}

// ── WithGenesisTx
// ─────────────────────────────────────────────────────────────

impl GenesisBlockBuilder<WithGenesisTx> {
    /// Wrap the pre-built [`GenesisTx`] in a [`GenesisBlock`].
    #[must_use]
    pub fn build(self) -> GenesisBlock {
        GenesisBlock::genesis(self.state.tx)
    }
}

#[cfg(test)]
mod tests {
    use lb_codec::BinaryEncode as _;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        header::HeaderId,
        mantle::{
            CryptarchiaParameter, GenesisTime, NoteId,
            ops::channel::{ChannelId, MsgId, inscribe::Inscription},
            traits::genesis::GenesisTx as _,
            transactions::states::Preverified,
        },
        sdp::{Locator, ProviderId, ServiceType},
    };
    // ── helpers ───────────────────────────────────────────────────────────────

    fn valid_inscription() -> InscriptionOp {
        InscriptionOp {
            channel_id: ChannelId::from([0; 32]),
            inscription: Inscription::new_unchecked(
                CryptarchiaParameter {
                    chain_id: "test-chain".to_owned().try_into().unwrap(),
                    genesis_time: GenesisTime::new(1000),
                    epoch_nonce: Fr::ZERO,
                }
                .encode_to_vec(),
            ),
            parent: MsgId::root(),
            signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
        }
    }

    fn invalid_inscription() -> InscriptionOp {
        InscriptionOp {
            channel_id: ChannelId::from([1; 32]), // non-zero — invalid
            inscription: Inscription::new_unchecked(
                CryptarchiaParameter {
                    chain_id: "test-chain".to_owned().try_into().unwrap(),
                    genesis_time: GenesisTime::new(1000),
                    epoch_nonce: Fr::ZERO,
                }
                .encode_to_vec(),
            ),
            parent: MsgId::root(),
            signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
        }
    }

    fn make_note(value: u64) -> Note {
        Note::new(value, ZkPublicKey::from(BigUint::from(value + 1)))
    }

    fn make_sdp_decl(id: u8) -> SDPDeclareOp {
        // Distinguish declarations by locked_note_id and zk_id; always use the
        // zero Ed25519 key since not all 32-byte arrays are valid curve points.
        SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: NoteId(Fr::from(u64::from(id))),
            zk_id: ZkPublicKey::from(BigUint::from(u64::from(id) + 1)),
            provider_id: ProviderId(Ed25519PublicKey::from_bytes(&[0; 32]).unwrap()),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        }
    }

    /// Build a valid [`GenesisBlock`] through the op-accumulation path using
    /// the given ordering function, and assert basic structural invariants.
    fn assert_block_valid(block: &GenesisBlock) {
        assert_eq!(block.header().slot(), Slot::from(0u64));
        assert_eq!(block.header().parent(), HeaderId::from([0u8; 32]));
        assert_eq!(block.transactions_iter().len(), 1);
    }

    // ── helpers for the with_genesis_tx path ──────────────────────────────────

    fn make_signed_genesis_tx(extra_ops: Vec<Op>) -> SignedMantleTx<Preverified> {
        let mut ops = vec![
            Op::Transfer(TransferOp::new(
                Inputs::empty(),
                Outputs::new([make_note(1_000)]),
            )),
            Op::ChannelInscribe(valid_inscription()),
        ];
        ops.extend(extra_ops);

        let ops_proofs = OpsProofs::try_from_iter(ops.iter().map(|op| match op {
            Op::ChannelInscribe(_) => OpProof::Ed25519Sig(Ed25519Signature::zero()),
            Op::Transfer(_) => OpProof::ZkSig(ZkSignature::new(
                CompressedGroth16Proof::from_bytes(&[0u8; 128]),
            )),
            Op::SDPDeclare(_) => OpProof::ZkAndEd25519Sigs {
                zk_sig: ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
                ed25519_sig: Ed25519Signature::zero(),
            },
            other => unreachable!("unexpected genesis op in tests: {}", other.as_str()),
        }))
        .expect("genesis transaction proofs are bounded");

        SignedMantleTx::new_trusted(MantleTx(Ops::new_unchecked(ops)), ops_proofs)
    }

    fn make_genesis_tx(extra_ops: Vec<Op>) -> GenesisTx {
        GenesisTx::from_tx(make_signed_genesis_tx(extra_ops)).expect("valid genesis tx")
    }

    // ── with_genesis_tx path ──────────────────────────────────────────────────

    #[test]
    fn with_genesis_tx_builds_block() {
        let block = GenesisBlockBuilder::new()
            .with_genesis_tx(make_genesis_tx(vec![]))
            .build();
        assert_block_valid(&block);
    }

    #[test]
    fn with_genesis_tx_with_sdp_decl() {
        let block = GenesisBlockBuilder::new()
            .with_genesis_tx(make_genesis_tx(vec![Op::SDPDeclare(make_sdp_decl(0))]))
            .build();
        assert_block_valid(&block);
    }

    // ── GenesisBlockBuilder traits ────────────────────────────────────────────

    #[test]
    fn default_equals_new() {
        let tx1 = make_genesis_tx(vec![]);
        let tx2 = tx1.clone();
        let id_new = GenesisBlockBuilder::new()
            .with_genesis_tx(tx1)
            .build()
            .header()
            .id();
        let id_default = GenesisBlockBuilder::default()
            .with_genesis_tx(tx2)
            .build()
            .header()
            .id();
        assert_eq!(id_new, id_default);
    }

    #[test]
    fn debug_format() {
        assert_eq!(
            format!("{:?}", GenesisBlockBuilder::new()),
            "GenesisBlockBuilder"
        );
    }

    // ── op-accumulation happy paths (all six orderings) ───────────────────────

    #[test]
    fn order_note_inscription_declaration() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    #[test]
    fn order_note_declaration_inscription() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .add_declaration(make_sdp_decl(0))
            .set_inscription(valid_inscription())
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    #[test]
    fn order_inscription_note_declaration() {
        let block = GenesisBlockBuilder::new()
            .set_inscription(valid_inscription())
            .add_note(make_note(100))
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    #[test]
    fn order_inscription_declaration_note() {
        let block = GenesisBlockBuilder::new()
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .add_note(make_note(100))
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    #[test]
    fn order_declaration_note_inscription() {
        let block = GenesisBlockBuilder::new()
            .add_declaration(make_sdp_decl(0))
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    #[test]
    fn order_declaration_inscription_note() {
        let block = GenesisBlockBuilder::new()
            .add_declaration(make_sdp_decl(0))
            .set_inscription(valid_inscription())
            .add_note(make_note(100))
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    // ── accumulated content is preserved ─────────────────────────────────────

    #[test]
    fn multiple_notes_are_preserved() {
        let block = GenesisBlockBuilder::new()
            .add_notes([make_note(100), make_note(200), make_note(300)])
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap();

        let tx = block.transactions_iter().next().unwrap();
        assert_eq!(tx.genesis_transfer().outputs.len(), 3);
    }

    #[test]
    fn multiple_declarations_are_preserved() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .add_declaration(make_sdp_decl(1))
            .unwrap()
            .add_declaration(make_sdp_decl(2))
            .unwrap()
            .build()
            .unwrap();

        let tx = block.transactions_iter().next().unwrap();
        assert_eq!(tx.sdp_declarations().count(), 3);
    }

    #[test]
    fn interleaved_adds_preserve_all_content() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(10))
            .add_declaration(make_sdp_decl(0))
            .add_note(make_note(20))
            .unwrap()
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(1))
            .unwrap()
            .add_note(make_note(30))
            .unwrap()
            .build()
            .unwrap();

        let tx = block.transactions_iter().next().unwrap();
        assert_eq!(tx.genesis_transfer().outputs.len(), 3);
        assert_eq!(tx.sdp_declarations().count(), 2);
    }

    // ── set_inscription overwrites ────────────────────────────────────────────

    #[test]
    fn set_inscription_overwrites_previous() {
        // Build once with invalid inscription then overwrite with a valid one.
        let block = GenesisBlockBuilder::new()
            .set_inscription(invalid_inscription())
            .set_inscription(valid_inscription()) // overwrite
            .add_note(make_note(100))
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    #[test]
    fn set_inscription_in_with_all_overwrites() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(invalid_inscription())
            .add_declaration(make_sdp_decl(0))
            .set_inscription(valid_inscription()) // overwrite after reaching WithAll
            .build()
            .unwrap();
        assert_block_valid(&block);
    }

    // ── invalid inscription is rejected at build time ─────────────────────────

    #[test]
    fn invalid_inscription_fails_at_build() {
        let err = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(invalid_inscription())
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap_err();

        assert!(
            matches!(
                err,
                Error::InvalidGenesisTx(genesis_tx::Error::InvalidInscription(_))
            ),
            "expected InvalidInscription, got {err:?}"
        );
    }

    // ── add_notes / add_declarations batch helpers ────────────────────────────

    #[test]
    fn add_declarations_accepts_maximum_capacity() {
        let builder = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0));

        // One declaration already exists, so add the remaining declarations.
        let builder = builder
            .add_declarations(
                std::iter::repeat_with(|| make_sdp_decl(0)).take(MAX_GENESIS_DECLARATIONS - 1),
            )
            .expect("maximum declaration capacity should be accepted");

        let block = builder
            .build()
            .expect("maximum-size genesis block should build");
        let tx = block
            .transactions_iter()
            .next()
            .expect("genesis transaction");

        assert_eq!(tx.sdp_declarations().count(), MAX_GENESIS_DECLARATIONS);
        assert_eq!(tx.mantle_tx().ops().len(), MAX_OPS_PER_TX);
    }

    #[test]
    fn add_declarations_rejects_beyond_maximum_capacity() {
        let builder = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .add_declarations(
                std::iter::repeat_with(|| make_sdp_decl(0)).take(MAX_GENESIS_DECLARATIONS - 1),
            )
            .expect("filling declaration capacity should succeed");

        let error = builder
            .add_declaration(make_sdp_decl(0))
            .expect_err("one declaration beyond capacity should fail");

        assert!(matches!(
            error,
            Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps { count })
                if count == MAX_OPS_PER_TX + 1
        ));
    }

    #[test]
    fn too_many_sdp_declarations_returns_error() {
        let declarations = GenesisSDPDeclareOps::empty();

        let error = try_extend_genesis_declarations(
            declarations,
            std::iter::repeat_with(|| make_sdp_decl(0)).take(MAX_GENESIS_DECLARATIONS + 1),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps { count })
                if count == MAX_OPS_PER_TX + 1
        ));
    }

    #[test]
    fn initial_declarations_reject_beyond_maximum_capacity() {
        let error = GenesisBlockBuilder::new()
            .add_declarations(
                std::iter::repeat_with(|| make_sdp_decl(0)).take(MAX_GENESIS_DECLARATIONS + 1),
            )
            .expect_err("too many initial declarations should fail");

        assert!(
            matches!(
                error,
                Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps { count })
                    if count == MAX_OPS_PER_TX + 1
            ),
            "expected TooManyOps({}), got {error:?}",
            MAX_OPS_PER_TX + 1,
        );
    }

    #[test]
    fn add_notes_batch_preserved() {
        let block = GenesisBlockBuilder::new()
            .add_notes([make_note(10), make_note(20), make_note(30)])
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap();

        let tx = block.transactions_iter().next().unwrap();
        assert_eq!(tx.genesis_transfer().outputs.len(), 3);
    }

    #[test]
    fn add_declarations_batch_preserved() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .add_declarations([make_sdp_decl(0), make_sdp_decl(1), make_sdp_decl(2)])
            .unwrap()
            .build()
            .unwrap();

        let tx = block.transactions_iter().next().unwrap();
        assert_eq!(tx.sdp_declarations().count(), 3);
    }

    #[test]
    fn add_notes_and_add_declarations_interleaved_with_batch() {
        let block = GenesisBlockBuilder::new()
            .add_notes([make_note(1), make_note(2), make_note(3)])
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .add_declarations([make_sdp_decl(1), make_sdp_decl(2)])
            .unwrap()
            .build()
            .unwrap();

        let tx = block.transactions_iter().next().unwrap();
        assert_eq!(tx.genesis_transfer().outputs.len(), 3);
        assert_eq!(tx.sdp_declarations().count(), 3);
    }

    #[test]
    fn try_add_notes_errors_on_empty_from_empty() {
        let err = GenesisBlockBuilder::new()
            .try_add_notes(std::iter::empty::<Note>())
            .unwrap_err();
        assert!(matches!(err, Error::EmptyNotes));
    }

    #[test]
    fn try_add_notes_errors_on_empty_from_with_notes() {
        let err = GenesisBlockBuilder::new()
            .add_note(make_note(1))
            .try_add_notes(std::iter::empty::<Note>())
            .unwrap_err();
        assert!(matches!(err, Error::EmptyNotes));
    }

    #[test]
    #[should_panic(expected = "add_declarations called with empty iterator")]
    fn add_declarations_panics_on_empty_from_empty() {
        drop(GenesisBlockBuilder::new().add_declarations(std::iter::empty::<SDPDeclareOp>()));
    }

    #[test]
    #[should_panic(expected = "add_declarations called with empty iterator")]
    fn add_declarations_panics_on_empty_from_with_declarations() {
        drop(
            GenesisBlockBuilder::new()
                .add_declaration(make_sdp_decl(0))
                .add_declarations(std::iter::empty::<SDPDeclareOp>()),
        );
    }

    // ── op ordering is correct ────────────────────────────────────────────────

    #[test]
    fn ops_are_ordered_transfer_inscription_declarations() {
        let block = GenesisBlockBuilder::new()
            .add_declaration(make_sdp_decl(0)) // added first, must end up last
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .build()
            .unwrap();

        let tx = block.transactions_iter().next().unwrap();
        let ops = tx.mantle_tx().ops();
        assert!(matches!(ops[0], Op::Transfer(_)));
        assert!(matches!(ops[1], Op::ChannelInscribe(_)));
        assert!(matches!(ops[2], Op::SDPDeclare(_)));
    }

    #[test]
    fn genesis_block_serde_roundtrip_wrapper() {
        let block = GenesisBlockBuilder::new()
            .with_genesis_tx(make_genesis_tx(vec![]))
            .build();

        let json = serde_json::to_string(&block).expect("genesis block serialize");
        let decoded: GenesisBlock = serde_json::from_str(&json).expect("genesis block deserialize");

        assert_eq!(decoded.header().slot(), Slot::genesis());
        assert_eq!(decoded.transactions_iter().len(), 1);
        assert_eq!(decoded.header().id(), block.header().id());
    }

    #[test]
    fn genesis_block_deserialize_rejects_non_genesis_slot() {
        // Build a valid genesis block first.
        let block = GenesisBlockBuilder::new()
            .with_genesis_tx(make_genesis_tx(vec![]))
            .build();

        // Mutate only slot in JSON to a non-genesis value.
        let mut value = serde_json::to_value(&block).expect("to_value should work");
        value["header"]["slot"] = serde_json::json!(1);

        let err = serde_json::from_value::<GenesisBlock>(value).unwrap_err();
        assert!(
            err.to_string().contains("expected genesis slot"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn genesis_block_deserialize_rejects_transaction_count_not_one() {
        // Build a valid genesis block first.
        let block = GenesisBlockBuilder::new()
            .with_genesis_tx(make_genesis_tx(vec![]))
            .build();

        let mut value = serde_json::to_value(&block).expect("to_value should work");

        // Duplicate the tx so count becomes 2.
        let tx0 = value["transactions"][0].clone();
        value["transactions"] = serde_json::Value::Array(vec![tx0.clone(), tx0]);

        let err = serde_json::from_value::<GenesisBlock>(value).unwrap_err();
        assert!(
            err.to_string()
                .contains("genesis block must contain exactly one transaction"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn genesis_declaration_helpers_preserve_items_and_enforce_bounds() {
        let empty = try_collect_sdp_declarations(std::iter::empty::<SDPDeclareOp>())
            .expect("empty declaration set should be valid");

        assert!(empty.is_empty());

        let declarations =
            require_non_empty([make_sdp_decl(0), make_sdp_decl(1)], "test declarations");

        let mut declarations =
            try_collect_sdp_declarations(declarations).expect("initial declarations should fit");

        assert_eq!(declarations.len(), 2);

        declarations = try_push_genesis_declaration(declarations, make_sdp_decl(2))
            .expect("single declaration should fit");

        assert_eq!(declarations.len(), 3);

        declarations =
            try_extend_genesis_declarations(declarations, [make_sdp_decl(3), make_sdp_decl(4)])
                .expect("multiple declarations should fit");

        assert_eq!(declarations.len(), 5);

        let full = try_collect_sdp_declarations(
            std::iter::repeat_with(|| make_sdp_decl(0)).take(MAX_GENESIS_DECLARATIONS),
        )
        .expect("maximum declaration capacity should fit");

        let error = try_push_genesis_declaration(full, make_sdp_decl(0))
            .expect_err("pushing beyond capacity should fail");

        assert!(matches!(
            error,
            Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps { count })
                if count == MAX_OPS_PER_TX + 1
        ));

        let error = try_extend_genesis_declarations(
            GenesisSDPDeclareOps::empty(),
            std::iter::repeat_with(|| make_sdp_decl(0)).take(MAX_GENESIS_DECLARATIONS + 1),
        )
        .expect_err("extending beyond capacity should fail");

        assert!(matches!(
            error,
            Error::InvalidGenesisTx(genesis_tx::Error::TooManyOps { count })
                if count == MAX_OPS_PER_TX + 1
        ));
    }

    #[test]
    fn genesis_block_deserialize_rejects_block_root_mismatch() {
        let block = GenesisBlockBuilder::new()
            .with_genesis_tx(make_genesis_tx(vec![]))
            .build();

        let mut value = serde_json::to_value(&block).expect("to_value should work");
        value["header"]["block_root"] = serde_json::json!("00".repeat(32));

        let err = serde_json::from_value::<GenesisBlock>(value).unwrap_err();
        assert!(
            err.to_string()
                .contains("Block root mismatch: calculated content does not match header"),
            "unexpected error: {err}"
        );
    }
}
