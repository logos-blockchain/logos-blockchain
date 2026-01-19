use key_management_system_service::{KMSService, backend::preload::PreloadKMSBackend};

pub type PreloadKmsService<RuntimeServiceId> = KMSService<PreloadKMSBackend, RuntimeServiceId>;
