use std::ffi::c_char;

#[repr(C)]
pub enum DeploymentType {
    WellKnown = 0,
    Custom = 1,
}

#[repr(C)]
pub enum WellKnownDeployment {
    Devnet = 0,
}

#[repr(C)]
pub struct Deployment {
    pub deployment_type: DeploymentType,

    // Only valid if deployment_type is WellKnown.
    pub well_known_deployment: WellKnownDeployment,

    // Only valid if deployment_type is Custom.
    pub custom_deployment_config_path: *const c_char,
}
