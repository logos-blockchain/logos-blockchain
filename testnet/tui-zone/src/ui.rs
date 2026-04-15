use std::io::Write as _;

use crate::state::ZoneState;

/// Print current state.
pub fn render_state(state: &dyn ZoneState) {
    let canonical: Vec<&str> = state.canonical().iter().map(|m| m.text.as_str()).collect();
    let finalized = state.finalized();

    if canonical.is_empty() {
        eprintln!("  Canonical: (empty)");
    } else {
        eprintln!("  Canonical: [{}]", canonical.join(", "));
    }

    if !finalized.is_empty() {
        eprintln!("  Finalized:");
        for msg in finalized {
            eprintln!("    {}", msg.text);
        }
    }

    eprintln!();
}

/// Print the prompt character.
pub fn prompt() {
    eprint!("> ");
    std::io::stderr().flush().expect("flush stderr");
}
