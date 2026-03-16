use tokio::sync::watch;
use tracing::info;

use crate::LOG_TARGET;

pub struct ChainOnlineNotifier {
    channel: watch::Sender<bool>,
}

impl ChainOnlineNotifier {
    pub fn new(cryptarchia_state: lb_cryptarchia_engine::State) -> Self {
        info!(target: LOG_TARGET, "Initializing chain online notifier with {cryptarchia_state:?}");
        let (channel, _) = watch::channel(cryptarchia_state.is_online());
        Self { channel }
    }

    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.channel.subscribe()
    }

    pub fn notify(&self) {
        info!(target: LOG_TARGET, "Notifying chain online subscribers");

        // NOTE: Use `send_replace` to always make a new value available for future
        // receivers, even if no receiver currently exists
        let prev_value = self.channel.send_replace(true);
        assert!(
            !prev_value, // must be `false`
            "Chain online subscribers must be notified only once because chain switches to online only once"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_with_bootstrapping() {
        let notifier = ChainOnlineNotifier::new(lb_cryptarchia_engine::State::Bootstrapping);
        let subscriber = notifier.subscribe();
        assert!(!*subscriber.borrow()); // should be `false`

        notifier.notify(); // notify that chain is online

        assert!(*subscriber.borrow()); // should be `true`

        // new subscriber should also see the latest value
        assert!(*notifier.subscribe().borrow());
    }

    #[test]
    fn init_with_online() {
        let notifier = ChainOnlineNotifier::new(lb_cryptarchia_engine::State::Online);
        assert!(*notifier.subscribe().borrow()); // should be `true` immediately
    }

    #[test]
    #[should_panic(
        expected = "Chain online subscribers must be notified only once because chain switches to online only once"
    )]
    fn panic_if_notified_twice() {
        let notifier = ChainOnlineNotifier::new(lb_cryptarchia_engine::State::Online);
        notifier.notify(); // must panic because the initial state is already Online
    }
}
