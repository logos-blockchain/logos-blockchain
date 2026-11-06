//! What a sequencer must get right, stacked: each layer is only meaningful
//! when the ones below it hold, so a run reports the lowest broken layer.

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// The non-finalized view is the canonical chain's channel content, in
    /// order, under every chain shape and delivery.
    Chain,
    /// Every finalized tx is reported once, in chain order; each planned
    /// payload finalizes once, in one message lineage.
    Lineage,
    /// The turn view follows the canonical branch's config lineage.
    Configs,
    /// The channel wallet is the ledger's note set; deposits are reported
    /// once, from canonical blocks.
    Notes,
    /// A sequencer resumed from a checkpoint reports what one that never
    /// stopped would.
    Restarts,
}

/// Violations recorded by one sequencer's checkers, tagged by layer.
#[derive(Debug, Default)]
pub struct Violations {
    entries: Vec<(Layer, String)>,
}

impl Violations {
    pub fn record(&mut self, layer: Layer, message: String) {
        tracing::error!("{layer:?} layer violated: {message}");
        self.entries.push((layer, message));
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Layer, &str)> {
        self.entries
            .iter()
            .map(|(layer, message)| (*layer, message.as_str()))
    }
}

/// A report of violations gathered from every sequencer: the lowest broken
/// layer in full, the layers above it counted.
#[must_use]
pub fn report<'a>(violations: impl IntoIterator<Item = (Layer, &'a str)>) -> Option<String> {
    let mut entries: Vec<(Layer, &str)> = violations.into_iter().collect();
    entries.sort_by_key(|(layer, _)| *layer);
    let (lowest, _) = *entries.first()?;
    let mut out = format!("{lowest:?} layer violated:\n");
    let mut above = 0;
    for (layer, message) in &entries {
        if *layer == lowest {
            let _ = writeln!(out, "  {message}");
        } else {
            above += 1;
        }
    }
    if above > 0 {
        let _ = writeln!(out, "and {above} more in the layers above");
    }
    Some(out)
}
