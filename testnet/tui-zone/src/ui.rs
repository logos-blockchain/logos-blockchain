use std::io::Write as _;

use crate::{message::Msg, state::ZoneState};

/// Print current state as three sections: Finalized, Adopted, Published.
pub fn render_state(state: &dyn ZoneState) {
    eprintln!();
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
