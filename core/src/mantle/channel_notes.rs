use serde::{Deserialize, Serialize};

use crate::mantle::{NoteId, ops::channel::ChannelId};

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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChannelNotes {
    channel_notes: rpds::HashTrieMapSync<NoteId, ChannelId>,
}

impl ChannelNotes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            channel_notes: rpds::HashTrieMapSync::new_sync(),
        }
    }

    #[must_use]
    pub fn contains(&self, id: &NoteId) -> bool {
        self.channel_notes.contains_key(id)
    }

    pub fn into_channel(mut self, note_id: &NoteId, channel_id: &ChannelId) -> Result<Self, Error> {
        if let Some(channel) = self.channel_notes.get(note_id) {
            return Err(Error::AlreadyAChannelNote {
                note_id: *note_id,
                channel_id: *channel,
            });
        }
        self.channel_notes = self.channel_notes.insert(*note_id, *channel_id);

        Ok(self)
    }

    #[must_use]
    pub fn is_a_channel(&self, note_id: &NoteId, channel_id: &ChannelId) -> bool {
        self.channel_notes.get(note_id) == Some(channel_id)
    }

    pub fn into_bedrock(mut self, note_id: &NoteId, channel_id: &ChannelId) -> Result<Self, Error> {
        match self.channel_notes.get(note_id) {
            Some(channel) if channel == channel_id => {
                self.channel_notes = self.channel_notes.remove(note_id);
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
