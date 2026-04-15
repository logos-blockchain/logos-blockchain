use std::{
    io::Write as _,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::state::ZoneState;

/// Tracks how many lines the last render used, so we can clear and redraw.
static LAST_RENDER_LINES: AtomicUsize = AtomicUsize::new(0);

/// Clear the previous render and redraw canonical + finalized state.
pub fn render_state(state: &dyn ZoneState) {
    let lines = LAST_RENDER_LINES.load(Ordering::Relaxed);
    for _ in 0..lines {
        eprint!("\x1b[1A\x1b[2K");
    }

    let canonical: Vec<&str> = state.canonical().iter().map(|m| m.text.as_str()).collect();
    let finalized = state.finalized();

    let mut count = 0;

    if canonical.is_empty() {
        eprintln!("  Canonical: (empty)");
    } else {
        eprintln!("  Canonical: [{}]", canonical.join(", "));
    }
    count += 1;

    if !finalized.is_empty() {
        eprintln!("  Finalized:");
        count += 1;
        for msg in finalized {
            eprintln!("    {}", msg.text);
            count += 1;
        }
    }

    eprintln!();
    count += 1;

    LAST_RENDER_LINES.store(count, Ordering::Relaxed);
}

/// Print the prompt character.
pub fn prompt() {
    eprint!("> ");
    std::io::stderr().flush().expect("flush stderr");
}
