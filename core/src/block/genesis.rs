use std::fmt::{Debug, Formatter};

use lb_key_management_system_keys::keys::Ed25519Signature;

use crate::{
    block::Block,
    header::Header,
    mantle::{
        Note, Op, OpProof, SignedMantleTx,
        genesis_tx::{self, GenesisTx},
        ledger::{Inputs, Outputs},
        ops::{channel::inscribe::InscriptionOp, sdp::SDPDeclareOp, transfer::TransferOp},
        tx::VerificationError,
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
    /// Create a genesis block from the given transaction.
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

    #[must_use]
    pub fn genesis_tx(&self) -> GenesisTx {
        self.transactions[0].clone()
    }
}

// ── Typestate markers
// ─────────────────────────────────────────────────────────

/// Typestate marker: builder has no input yet.
pub struct Empty;

/// Typestate marker: builder holds a pre-validated [`GenesisTx`].
pub struct WithGenesisTx {
    tx: GenesisTx,
}

/// Typestate marker: builder has genesis transfer output notes only.
pub struct WithNotes {
    notes: Vec<Note>,
}

/// Typestate marker: builder has a genesis inscription only.
pub struct WithInscription {
    inscription: InscriptionOp,
}

/// Typestate marker: builder has SDP service-declaration ops only.
pub struct WithDeclarations {
    sdp_declarations: Vec<SDPDeclareOp>,
}

/// Typestate marker: builder has genesis notes and an inscription.
pub struct WithNotesAndInscription {
    notes: Vec<Note>,
    inscription: InscriptionOp,
}

/// Typestate marker: builder has genesis notes and SDP declarations.
pub struct WithNotesAndDeclarations {
    notes: Vec<Note>,
    sdp_declarations: Vec<SDPDeclareOp>,
}

/// Typestate marker: builder has a genesis inscription and SDP declarations.
pub struct WithInscriptionAndDeclarations {
    inscription: InscriptionOp,
    sdp_declarations: Vec<SDPDeclareOp>,
}

#[expect(
    clippy::too_long_first_doc_paragraph,
    reason = "Necessary documentation"
)]
/// Typestate marker: builder holds all three pieces required to assemble a
/// [`GenesisTx`] — notes, an inscription, and at least one SDP declaration.
/// This is the only state that exposes [`GenesisBlockBuilder::build`].
pub struct WithAll {
    notes: Vec<Note>,
    inscription: InscriptionOp,
    sdp_declarations: Vec<SDPDeclareOp>,
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
            state: WithNotes { notes: vec![note] },
        }
    }

    /// Add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotes`].
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    #[must_use]
    pub fn add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> GenesisBlockBuilder<WithNotes> {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        GenesisBlockBuilder {
            state: WithNotes {
                notes: iter.map(Into::into).collect(),
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
                sdp_declarations: vec![declaration],
            },
        }
    }

    /// Add multiple SDP service-declaration ops at once, transitioning to
    /// [`WithDeclarations`].
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> GenesisBlockBuilder<WithDeclarations> {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        GenesisBlockBuilder {
            state: WithDeclarations {
                sdp_declarations: iter.map(Into::into).collect(),
            },
        }
    }
}

// ── WithNotes
// ─────────────────────────────────────────────────────────────────

impl GenesisBlockBuilder<WithNotes> {
    /// Append another genesis transfer output note.
    #[must_use]
    pub fn add_note(self, note: Note) -> Self {
        let Self {
            state: WithNotes { mut notes },
        } = self;
        notes.push(note);
        Self {
            state: WithNotes { notes },
        }
    }

    /// Append multiple genesis transfer output notes at once.
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    #[must_use]
    pub fn add_notes(self, notes: impl IntoIterator<Item = impl Into<Note>>) -> Self {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        let Self {
            state: WithNotes { mut notes },
        } = self;
        notes.extend(iter.map(Into::into));
        Self {
            state: WithNotes { notes },
        }
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
                sdp_declarations: vec![declaration],
            },
        }
    }

    /// Add multiple SDP declarations at once, transitioning to
    /// [`WithNotesAndDeclarations`].
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> GenesisBlockBuilder<WithNotesAndDeclarations> {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        let Self {
            state: WithNotes { notes },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations: iter.map(Into::into).collect(),
            },
        }
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
                notes: vec![note],
                inscription,
            },
        }
    }

    /// Add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotesAndInscription`].
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    #[must_use]
    pub fn add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> GenesisBlockBuilder<WithNotesAndInscription> {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        let Self {
            state: WithInscription { inscription },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndInscription {
                notes: iter.map(Into::into).collect(),
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
                sdp_declarations: vec![declaration],
            },
        }
    }

    /// Add multiple SDP declarations at once, transitioning to
    /// [`WithInscriptionAndDeclarations`].
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> GenesisBlockBuilder<WithInscriptionAndDeclarations> {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        let Self {
            state: WithInscription { inscription },
        } = self;
        GenesisBlockBuilder {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations: iter.map(Into::into).collect(),
            },
        }
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
                notes: vec![note],
                sdp_declarations,
            },
        }
    }

    /// Add multiple genesis transfer output notes at once, transitioning to
    /// [`WithNotesAndDeclarations`].
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    #[must_use]
    pub fn add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> GenesisBlockBuilder<WithNotesAndDeclarations> {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        let Self {
            state: WithDeclarations { sdp_declarations },
        } = self;
        GenesisBlockBuilder {
            state: WithNotesAndDeclarations {
                notes: iter.map(Into::into).collect(),
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
    #[must_use]
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Self {
        let Self {
            state: WithDeclarations {
                mut sdp_declarations,
            },
        } = self;
        sdp_declarations.push(declaration);
        Self {
            state: WithDeclarations { sdp_declarations },
        }
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Self {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        let Self {
            state: WithDeclarations {
                mut sdp_declarations,
            },
        } = self;
        sdp_declarations.extend(iter.map(Into::into));
        Self {
            state: WithDeclarations { sdp_declarations },
        }
    }
}

// ── WithNotesAndInscription
// ───────────────────────────────────────────────────

impl GenesisBlockBuilder<WithNotesAndInscription> {
    /// Append another genesis transfer output note.
    #[must_use]
    pub fn add_note(self, note: Note) -> Self {
        let Self {
            state:
                WithNotesAndInscription {
                    mut notes,
                    inscription,
                },
        } = self;
        notes.push(note);
        Self {
            state: WithNotesAndInscription { notes, inscription },
        }
    }

    /// Append multiple genesis transfer output notes at once.
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    #[must_use]
    pub fn add_notes(self, notes: impl IntoIterator<Item = impl Into<Note>>) -> Self {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        let Self {
            state:
                WithNotesAndInscription {
                    mut notes,
                    inscription,
                },
        } = self;
        notes.extend(iter.map(Into::into));
        Self {
            state: WithNotesAndInscription { notes, inscription },
        }
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
                sdp_declarations: vec![declaration],
            },
        }
    }

    /// Add multiple SDP declarations at once, completing all three pieces and
    /// transitioning to [`WithAll`].
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> GenesisBlockBuilder<WithAll> {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        let Self {
            state: WithNotesAndInscription { notes, inscription },
        } = self;
        GenesisBlockBuilder {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations: iter.map(Into::into).collect(),
            },
        }
    }

    // Build a block with empty declarations but properly set inscription and
    // transfer.
    pub fn build(self) -> Result<GenesisBlock> {
        GenesisBlockBuilder {
            state: WithAll {
                notes: self.state.notes,
                inscription: self.state.inscription,
                sdp_declarations: vec![],
            },
        }
        .build()
    }
}

// ── WithNotesAndDeclarations
// ──────────────────────────────────────────────────

impl GenesisBlockBuilder<WithNotesAndDeclarations> {
    /// Append another genesis transfer output note.
    #[must_use]
    pub fn add_note(self, note: Note) -> Self {
        let Self {
            state:
                WithNotesAndDeclarations {
                    mut notes,
                    sdp_declarations,
                },
        } = self;
        notes.push(note);
        Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        }
    }

    /// Append multiple genesis transfer output notes at once.
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    #[must_use]
    pub fn add_notes(self, notes: impl IntoIterator<Item = impl Into<Note>>) -> Self {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        let Self {
            state:
                WithNotesAndDeclarations {
                    mut notes,
                    sdp_declarations,
                },
        } = self;
        notes.extend(iter.map(Into::into));
        Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        }
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
    #[must_use]
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Self {
        let Self {
            state:
                WithNotesAndDeclarations {
                    notes,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations.push(declaration);
        Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        }
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Self {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        let Self {
            state:
                WithNotesAndDeclarations {
                    notes,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations.extend(iter.map(Into::into));
        Self {
            state: WithNotesAndDeclarations {
                notes,
                sdp_declarations,
            },
        }
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
                notes: vec![note],
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
    #[must_use]
    pub fn add_notes(
        self,
        notes: impl IntoIterator<Item = impl Into<Note>>,
    ) -> GenesisBlockBuilder<WithAll> {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    inscription,
                    sdp_declarations,
                },
        } = self;
        GenesisBlockBuilder {
            state: WithAll {
                notes: iter.map(Into::into).collect(),
                inscription,
                sdp_declarations,
            },
        }
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
    #[must_use]
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Self {
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    inscription,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations.push(declaration);
        Self {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Self {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        let Self {
            state:
                WithInscriptionAndDeclarations {
                    inscription,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations.extend(iter.map(Into::into));
        Self {
            state: WithInscriptionAndDeclarations {
                inscription,
                sdp_declarations,
            },
        }
    }
}

// ── WithAll
// ───────────────────────────────────────────────────────────────────

impl GenesisBlockBuilder<WithAll> {
    /// Append another genesis transfer output note.
    #[must_use]
    pub fn add_note(self, note: Note) -> Self {
        let Self {
            state:
                WithAll {
                    mut notes,
                    inscription,
                    sdp_declarations,
                },
        } = self;
        notes.push(note);
        Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Append multiple genesis transfer output notes at once.
    ///
    /// # Panics
    ///
    /// Panics if `notes` is empty.
    #[must_use]
    pub fn add_notes(self, notes: impl IntoIterator<Item = impl Into<Note>>) -> Self {
        let mut iter = notes.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_notes called with empty iterator"
        );
        let Self {
            state:
                WithAll {
                    mut notes,
                    inscription,
                    sdp_declarations,
                },
        } = self;
        notes.extend(iter.map(Into::into));
        Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        }
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
    #[must_use]
    pub fn add_declaration(self, declaration: SDPDeclareOp) -> Self {
        let Self {
            state:
                WithAll {
                    notes,
                    inscription,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations.push(declaration);
        Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        }
    }

    /// Append multiple SDP declarations at once.
    ///
    /// # Panics
    ///
    /// Panics if `declarations` is empty.
    #[must_use]
    pub fn add_declarations(
        self,
        declarations: impl IntoIterator<Item = impl Into<SDPDeclareOp>>,
    ) -> Self {
        let mut iter = declarations.into_iter().peekable();
        assert!(
            iter.peek().is_some(),
            "add_declarations called with empty iterator"
        );
        let Self {
            state:
                WithAll {
                    notes,
                    inscription,
                    mut sdp_declarations,
                },
        } = self;
        sdp_declarations.extend(iter.map(Into::into));
        Self {
            state: WithAll {
                notes,
                inscription,
                sdp_declarations,
            },
        }
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
            Inputs::new(vec![]),
            Outputs::new(notes),
        )))
        .chain(std::iter::once(Op::ChannelInscribe(inscription)))
        .chain(sdp_declarations.into_iter().map(Op::SDPDeclare))
        .collect();
        let n = ops.len();
        let signed_tx = SignedMantleTx::new_unverified(
            ops.into(),
            vec![OpProof::Ed25519Sig(Ed25519Signature::zero()); n],
        );
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
    use lb_cryptarchia_engine::Slot;
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};
    use num_bigint::BigUint;
    use time::OffsetDateTime;

    use super::*;
    use crate::{
        header::HeaderId,
        mantle::{
            CryptarchiaParameter, GenesisTx as _, NoteId,
            ops::channel::{ChannelId, MsgId},
        },
        sdp::{Locator, ProviderId, ServiceType},
    };

    // ── helpers ───────────────────────────────────────────────────────────────

    fn valid_inscription() -> InscriptionOp {
        InscriptionOp {
            channel_id: ChannelId::from([0; 32]),
            inscription: CryptarchiaParameter {
                chain_id: "test-chain".into(),
                genesis_time: OffsetDateTime::from_unix_timestamp(1000).unwrap(),
                epoch_nonce: Fr::ZERO,
            }
            .encode(),
            parent: MsgId::root(),
            signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
        }
    }

    fn invalid_inscription() -> InscriptionOp {
        InscriptionOp {
            channel_id: ChannelId::from([1; 32]), // non-zero — invalid
            inscription: CryptarchiaParameter {
                chain_id: "test-chain".into(),
                genesis_time: OffsetDateTime::from_unix_timestamp(1000).unwrap(),
                epoch_nonce: Fr::ZERO,
            }
            .encode(),
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
        assert_eq!(block.transactions().len(), 1);
    }

    // ── helpers for the with_genesis_tx path ──────────────────────────────────

    fn make_signed_genesis_tx(extra_ops: Vec<Op>) -> SignedMantleTx {
        let mut ops = vec![
            Op::Transfer(TransferOp::new(
                Inputs::new(vec![]),
                Outputs::new(vec![make_note(1_000)]),
            )),
            Op::ChannelInscribe(valid_inscription()),
        ];
        ops.extend(extra_ops);
        let n = ops.len();
        SignedMantleTx::new_unverified(
            ops.into(),
            vec![OpProof::Ed25519Sig(Ed25519Signature::from_bytes(&[0u8; 64])); n],
        )
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
            .add_note(make_note(100))
            .add_note(make_note(200))
            .add_note(make_note(300))
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap();

        let tx = block.transactions().next().unwrap();
        assert_eq!(tx.genesis_transfer().outputs.len(), 3);
    }

    #[test]
    fn multiple_declarations_are_preserved() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .add_declaration(make_sdp_decl(1))
            .add_declaration(make_sdp_decl(2))
            .build()
            .unwrap();

        let tx = block.transactions().next().unwrap();
        assert_eq!(tx.sdp_declarations().count(), 3);
    }

    #[test]
    fn interleaved_adds_preserve_all_content() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(10))
            .add_declaration(make_sdp_decl(0))
            .add_note(make_note(20))
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(1))
            .add_note(make_note(30))
            .build()
            .unwrap();

        let tx = block.transactions().next().unwrap();
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
    fn add_notes_batch_preserved() {
        let block = GenesisBlockBuilder::new()
            .add_notes([make_note(10), make_note(20), make_note(30)])
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .build()
            .unwrap();

        let tx = block.transactions().next().unwrap();
        assert_eq!(tx.genesis_transfer().outputs.len(), 3);
    }

    #[test]
    fn add_declarations_batch_preserved() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(100))
            .set_inscription(valid_inscription())
            .add_declarations([make_sdp_decl(0), make_sdp_decl(1), make_sdp_decl(2)])
            .build()
            .unwrap();

        let tx = block.transactions().next().unwrap();
        assert_eq!(tx.sdp_declarations().count(), 3);
    }

    #[test]
    fn add_notes_and_add_declarations_interleaved_with_batch() {
        let block = GenesisBlockBuilder::new()
            .add_note(make_note(1))
            .add_notes([make_note(2), make_note(3)])
            .set_inscription(valid_inscription())
            .add_declaration(make_sdp_decl(0))
            .add_declarations([make_sdp_decl(1), make_sdp_decl(2)])
            .build()
            .unwrap();

        let tx = block.transactions().next().unwrap();
        assert_eq!(tx.genesis_transfer().outputs.len(), 3);
        assert_eq!(tx.sdp_declarations().count(), 3);
    }

    #[test]
    #[should_panic(expected = "add_notes called with empty iterator")]
    fn add_notes_panics_on_empty_from_empty() {
        drop(GenesisBlockBuilder::new().add_notes(std::iter::empty::<Note>()));
    }

    #[test]
    #[should_panic(expected = "add_notes called with empty iterator")]
    fn add_notes_panics_on_empty_from_with_notes() {
        drop(
            GenesisBlockBuilder::new()
                .add_note(make_note(1))
                .add_notes(std::iter::empty::<Note>()),
        );
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

        let tx = block.transactions().next().unwrap();
        let ops = tx.mantle_tx().ops();
        assert!(matches!(ops[0], Op::Transfer(_)));
        assert!(matches!(ops[1], Op::ChannelInscribe(_)));
        assert!(matches!(ops[2], Op::SDPDeclare(_)));
    }
}
