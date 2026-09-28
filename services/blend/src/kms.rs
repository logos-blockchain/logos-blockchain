use lb_key_management_system_service::{KMSService, backend::hd::HdKMSBackend};

pub type HdKmsService<RuntimeServiceId> = KMSService<HdKMSBackend, RuntimeServiceId>;
