use std::fmt::Display;

use lb_chain_service::Slot;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BlockSlotRangeError {
    InvalidRange { slot_from: Slot, slot_to: Slot },
    SlotToExceedsLibSlot { slot_to: Slot, lib_slot: Slot },
    SlotToExceedsTipSlot { slot_to: Slot, tip_slot: Slot },
}

impl Display for BlockSlotRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRange { slot_from, slot_to } => {
                write!(
                    f,
                    "slot_to must be greater than or equal to slot_from: slot_from={slot_from:?}, \
                    slot_to={slot_to:?}"
                )
            }
            Self::SlotToExceedsLibSlot { slot_to, lib_slot } => {
                write!(
                    f,
                    "slot_to must be <= lib_slot when immutable_only=true: slot_to={slot_to:?}, \
                    lib_slot={lib_slot:?}"
                )
            }
            Self::SlotToExceedsTipSlot { slot_to, tip_slot } => {
                write!(
                    f,
                    "slot_to must be <= tip_slot: slot_to={slot_to:?}, tip_slot={tip_slot:?}"
                )
            }
        }
    }
}
