use lb_blake2btree::{Blake2bTree, LeafHash};
use lb_groth16::fr_to_bytes;
use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{NoteId, ops::channel::ChannelId},
};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Note is not in a channel: {0:?}")]
    NotInChannel(NoteId),
    #[error("Note {note_id:?} is not a channel note of channel {channel_id:?}")]
    NotAChannelNote {
        note_id: NoteId,
        channel_id: ChannelId,
    },
    #[error("Note {note_id:?} already in a channel {channel_id:?}")]
    AlreadyAChannelNote {
        note_id: NoteId,
        channel_id: ChannelId,
    },
}

// The leaf binds a note to the channel owning it, so that releasing it or
// handing it over to another channel changes the root.
impl LeafHash<NoteId> for ChannelId {
    fn leaf_hash(&self, note_id: &NoteId) -> Hash {
        let mut h = Hasher::new();
        h.update(fr_to_bytes(note_id.as_fr()));
        h.update(self.as_ref());
        h.finalize().into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChannelNotes {
    channel_notes: Blake2bTree<NoteId, ChannelId>,
}

impl ChannelNotes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            channel_notes: Blake2bTree::new(),
        }
    }

    #[must_use]
    pub fn root(&self) -> Hash {
        self.channel_notes.root()
    }

    #[must_use]
    pub fn contains(&self, id: &NoteId) -> bool {
        self.channel_notes.contains(id)
    }

    /// Returns the channel owning `id`, if it is a channel note.
    #[must_use]
    pub fn get(&self, id: &NoteId) -> Option<ChannelId> {
        self.channel_notes.get(id)
    }

    pub fn into_channel(mut self, note_id: &NoteId, channel_id: &ChannelId) -> Result<Self, Error> {
        if let Some(channel) = self.channel_notes.get(note_id) {
            return Err(Error::AlreadyAChannelNote {
                note_id: *note_id,
                channel_id: channel,
            });
        }
        self.channel_notes = self.channel_notes.insert(*note_id, *channel_id).0;

        Ok(self)
    }

    #[must_use]
    pub fn is_a_channel(&self, note_id: &NoteId, channel_id: &ChannelId) -> bool {
        self.channel_notes.get_ref(note_id) == Some(channel_id)
    }

    pub fn into_bedrock(mut self, note_id: &NoteId, channel_id: &ChannelId) -> Result<Self, Error> {
        match self.channel_notes.get(note_id) {
            Some(channel) if channel == *channel_id => {
                (self.channel_notes, _) = self
                    .channel_notes
                    .remove(note_id)
                    .expect("channel note is in the tree");
                Ok(self)
            }
            Some(_) => Err(Error::NotAChannelNote {
                note_id: *note_id,
                channel_id: *channel_id,
            }),
            None => Err(Error::NotInChannel(*note_id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;

    use super::*;

    fn note_id(seed: u64) -> NoteId {
        NoteId::from(Fr::from(seed))
    }

    // The encoding is spelled out field by field, so any change to `leaf_hash`
    // has to be mirrored here and in the specification.
    #[test]
    fn channel_note_leaf_matches_the_specified_encoding() {
        let note_id = note_id(1);
        let channel_id = ChannelId::from([0u8; 32]);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&fr_to_bytes(note_id.as_fr()));
        bytes.extend_from_slice(channel_id.as_ref());

        let expected: Hash = Hasher::digest(&bytes).into();
        assert_eq!(channel_id.leaf_hash(&note_id), expected);
    }

    #[test]
    fn channel_note_leaf_binds_the_owning_channel() {
        let note_id = note_id(1);

        assert_ne!(
            ChannelId::from([0u8; 32]).leaf_hash(&note_id),
            ChannelId::from([1u8; 32]).leaf_hash(&note_id)
        );
    }

    #[test]
    fn releasing_a_note_restores_the_root() {
        let note_id = note_id(1);
        let channel_id = ChannelId::from([0u8; 32]);

        let channel_notes = ChannelNotes::new();
        let empty_root = channel_notes.root();

        let channel_notes = channel_notes.into_channel(&note_id, &channel_id).unwrap();
        assert_ne!(channel_notes.root(), empty_root);

        let channel_notes = channel_notes.into_bedrock(&note_id, &channel_id).unwrap();
        assert_eq!(channel_notes.root(), empty_root);
    }

    // The slot freed by a removal is reused, so the same notes registered in the
    // same order commit to the same root whatever happened in between.
    #[test]
    fn root_follows_the_order_of_apparition() {
        let first = note_id(1);
        let second = note_id(2);
        let channel_id = ChannelId::from([0u8; 32]);

        let channel_notes = ChannelNotes::new()
            .into_channel(&first, &channel_id)
            .unwrap()
            .into_channel(&second, &channel_id)
            .unwrap();

        let reordered = ChannelNotes::new()
            .into_channel(&second, &channel_id)
            .unwrap()
            .into_channel(&first, &channel_id)
            .unwrap();

        assert_ne!(channel_notes.root(), reordered.root());
    }
}
