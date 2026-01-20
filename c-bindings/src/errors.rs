#[derive(Default, PartialEq, Eq)]
#[repr(C)]
pub enum OperationStatus {
    #[default]
    Ok = 0x0,
    NotFound = 0x1,
    NullPointer = 0x2,
    RelayError = 0x3,
    ChannelSendError = 0x4,
    ChannelReceiveError = 0x5,
    ServiceError = 0x6,
    RuntimeError = 0x7,
    DynError = 0x8,
    InitializationError = 0x9,
    StopError = 0xA,
}

impl OperationStatus {
    #[must_use]
    #[unsafe(no_mangle)]
    pub extern "C" fn is_ok(&self) -> bool {
        *self == Self::Ok
    }
}
