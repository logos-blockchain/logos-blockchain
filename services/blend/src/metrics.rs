mod imp {
    use lb_blend::network::core::with_core::behaviour::blacklist::BlacklistReason;

    use crate::message::DataPayloadType;

    const ACTION_PUBLISH: &str = "publish";
    const ACTION_FORWARD: &str = "forward";

    #[derive(Clone, Copy, Debug)]
    pub enum InboundMessageType {
        Core,
        Edge,
    }

    impl InboundMessageType {
        const fn to_str(self) -> &'static str {
            match self {
                Self::Core => "core",
                Self::Edge => "edge",
            }
        }
    }

    pub fn mix_packets_processed_total() {
        lb_tracing::increase_counter_u64!(blend_mix_packets_processed_total, 1);
    }

    pub fn core_peers_negotiated(count: usize) {
        lb_tracing::metric_observable_gauge_u64_set!(blend_core_peers_negotiated, count as u64);
    }

    pub fn peers_negotiated_stop_reporting() {
        lb_tracing::metric_observable_gauge_u64_clear!(blend_core_peers_negotiated);
    }

    pub fn outbound_publish_ok() {
        lb_tracing::increase_counter_u64!(blend_messages_sent_total, 1, action = ACTION_PUBLISH);
    }

    pub fn outbound_publish_err() {
        lb_tracing::increase_counter_u64!(
            blend_outbound_messages_failed_total,
            1,
            action = ACTION_PUBLISH
        );
    }

    pub fn outbound_forward_ok() {
        lb_tracing::increase_counter_u64!(blend_messages_sent_total, 1, action = ACTION_FORWARD);
    }

    pub fn inbound_message_ok() {
        lb_tracing::increase_counter_u64!(blend_messages_received_total, 1);
    }

    pub fn inbound_message_err(message_type: InboundMessageType) {
        lb_tracing::increase_counter_u64!(
            blend_inbound_messages_failed_total,
            1,
            message_type = message_type.to_str()
        );
    }

    /// Reports incoming messages that were dropped before the event loop could
    /// process them because the consumer lagged behind the broadcast producer.
    pub fn inbound_messages_dropped(count: u64) {
        lb_tracing::increase_counter_u64!(blend_inbound_messages_dropped_total, count);
    }

    /// Reports core peers blacklisted, labelled with the reason: a frame that
    /// did not decode, a header signature that did not verify, or an invalid
    /// `PoQ`. Volume is never a reason, and neither is a duplicate.
    pub fn core_peer_blacklisted(reason: BlacklistReason) {
        lb_tracing::increase_counter_u64!(
            blend_core_peers_blocked_total,
            1,
            reason = reason.as_ref()
        );
    }

    /// Reports how many peers are blacklisted right now.
    ///
    /// A gauge rather than a counter: entries expire silently after `W`, so the
    /// value goes down as well as up and no event marks the moment it does. It
    /// is re-read whenever a connection is established or closed, which is
    /// often enough to follow the window.
    pub fn core_blacklist_size(count: usize) {
        lb_tracing::metric_observable_gauge_u64_set!(blend_core_blacklist_size, count as u64);
    }

    /// Reports a data payload the Blend network failed to deliver within the
    /// delivery deadline, and that this node therefore broadcast in the clear.
    pub fn data_payload_bypassed_blend(payload_type: DataPayloadType) {
        lb_tracing::increase_counter_u64!(
            blend_payloads_bypassed_total,
            1,
            payload_type = payload_type.as_ref()
        );
    }
}

pub use imp::*;
