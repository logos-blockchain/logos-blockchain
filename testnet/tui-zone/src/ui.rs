use std::io::Write as _;

use crate::state::ZoneState;

/// Number of fixed lines the UI uses above the prompt:
/// 1 for "Canonical: [...]"
/// 1 for blank separator
const CANONICAL_LINES: usize = 2;

/// Clear N lines above the cursor and move back up.
fn clear_lines_above(n: usize) {
    for _ in 0..n {
        // Move up one line, clear it
        eprint!("\x1b[1A\x1b[2K");
    }
}

/// Render the canonical line and prompt. Overwrites previous canonical display.
pub fn render_canonical(state: &dyn ZoneState) {
    clear_lines_above(CANONICAL_LINES);

    let texts: Vec<&str> = state.canonical().iter().map(|m| m.text.as_str()).collect();
    if texts.is_empty() {
        eprintln!("  Canonical: (empty)");
    } else {
        eprintln!("  Canonical: [{}]", texts.join(", "));
    }
    eprintln!(); // separator
}

/// Print a newly finalized message (appends, never overwrites).
pub fn print_finalized(text: &str) {
    eprintln!("  \x1b[32m+finalized\x1b[0m {text}");
}

/// Print the initial UI header.
pub fn print_header(state: &dyn ZoneState) {
    let texts: Vec<&str> = state.canonical().iter().map(|m| m.text.as_str()).collect();
    if texts.is_empty() {
        eprintln!("  Canonical: (empty)");
    } else {
        eprintln!("  Canonical: [{}]", texts.join(", "));
    }
    eprintln!(); // separator before prompt
}

/// Print the prompt character.
pub fn prompt() {
    eprint!("> ");
    std::io::stderr().flush().ok();
}
