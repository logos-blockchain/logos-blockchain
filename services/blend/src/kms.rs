use lb_key_management_system_service::{
    KMSService, backend::hd_and_preload::HdAndPreloadKMSBackend,
};

pub type HdAndPreloadKmsService<RuntimeServiceId> =
    KMSService<HdAndPreloadKMSBackend, RuntimeServiceId>;
