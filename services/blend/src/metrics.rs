mod imp {
    use lb_blend::message::PayloadType;

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

    pub fn outbound_forward_err() {
        lb_tracing::increase_counter_u64!(
            blend_outbound_messages_failed_total,
            1,
            action = ACTION_FORWARD
        );
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

    /// Reports core peers blocked for spamming, labelled with what they were
    /// caught doing — an invalid `PoQ` among the reasons.
    pub fn core_peer_blocked(reason: &'static str) {
        lb_tracing::increase_counter_u64!(blend_core_peers_blocked_total, 1, reason = reason);
    }

    /// Reports a payload the Blend network failed to deliver within the
    /// delivery deadline, and that this node therefore broadcast in the clear.
    pub fn payload_bypassed_blend(payload_type: PayloadType) {
        lb_tracing::increase_counter_u64!(
            blend_payloads_bypassed_total,
            1,
            payload_type = payload_type.as_ref()
        );
    }
}

pub use imp::*;
