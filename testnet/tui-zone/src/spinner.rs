use core::time::Duration;
use std::io::Write as _;

use tokio::task::JoinHandle;

/// A simple spinner that prints dots while an async operation is in progress.
pub struct Spinner {
    handle: Option<JoinHandle<()>>,
    cancel: tokio::sync::watch::Sender<bool>,
}

impl Spinner {
    pub fn start(message: &str) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let msg = message.to_owned();

        let handle = tokio::spawn(async move {
            #[expect(clippy::non_ascii_literal, reason = "UI animation")]
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut i = 0;
            loop {
                print!("\r  {} {}", frames[i % frames.len()], msg);
                drop(std::io::stdout().flush());
                i += 1;

                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(80)) => {}
                    _ = cancel_rx.changed() => {
                        // Clear the spinner line
                        print!("\r{}\r", " ".repeat(msg.len() + 6));
                        drop(std::io::stdout().flush());
                        return;
                    }
                }
            }
        });

        Self {
            handle: Some(handle),
            cancel: cancel_tx,
        }
    }

    pub async fn stop(mut self) {
        let _ = self.cancel.send(true);
        if let Some(handle) = self.handle.take() {
            drop(handle.await);
        }
    }
}
