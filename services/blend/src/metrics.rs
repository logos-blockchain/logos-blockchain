mod imp {
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

    pub fn peers_connected(count: usize) {
        lb_tracing::metric_gauge_u64!(blend_peers_connected, count as u64);
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
}

pub use imp::*;
