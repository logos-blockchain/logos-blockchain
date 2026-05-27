use std::io::Write as _;

use crate::{
    message::Msg,
    state::{InMemoryZoneState, ZoneState as _},
};

/// Print current state as three sections: Finalized, Adopted, Published.
pub fn render_state(state: &InMemoryZoneState) {
    eprintln!();
    if let Some(view) = state.channel_view() {
        eprintln!("=== Sequencer ===");
        eprintln!("  Channel: {}", hex::encode(view.channel_id.as_ref()));
        eprintln!("  Slot: {}", view.current_slot.into_inner());
        eprintln!(
            "  Accredited keys: {}",
            view.accredited_key_count.unwrap_or_default()
        );
        eprintln!(
            "  This sequencer: {}",
            view.own_key_index
                .map_or_else(|| "not accredited".to_owned(), |idx| format!("index {idx}"))
        );
        eprintln!(
            "  Authorized sequencer: {}",
            view.authorized_key_index
                .map_or_else(|| "unknown".to_owned(), |idx| format!("index {idx}"))
        );
        eprintln!(
            "  Status: {}",
            if view.is_our_turn {
                "our turn"
            } else {
                "waiting for turn"
            }
        );
        eprintln!(
            "  Posting timeframe: {}",
            view.posting_timeframe
                .map_or_else(|| "unknown".to_owned(), |slots| format!("{slots} slots"))
        );
        eprintln!(
            "  Posting timeout: {}",
            view.posting_timeout
                .map_or_else(|| "unknown".to_owned(), |slots| format!("{slots} slots"))
        );
        eprintln!("  Queued messages: {}", view.queued_messages);
        eprintln!("  Tip message: {}", hex::encode(view.tip_message.as_ref()));
        eprintln!();
    }
    print_section("Finalized", state.finalized());
    print_section("Adopted", state.adopted());
    print_section("Published", state.published());
}

fn print_section(label: &str, msgs: &[Msg]) {
    if msgs.is_empty() {
        return;
    }
    eprintln!("=== {label} ===");
    for m in msgs {
        eprintln!("  {}", m.text);
    }
    eprintln!();
}

/// Print the prompt character.
pub fn prompt() {
    eprint!("> ");
    std::io::stderr().flush().expect("flush stderr");
}
