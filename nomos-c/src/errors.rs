#[repr(u8)]
pub enum NomosNodeErrorCode {
    None = 0x0,
    CouldNotInitialize = 0x1,
    StopError = 0x2,
    NullPtr = 0x3,
}

#[derive(PartialEq, Eq)]
#[repr(u8)]
pub enum OperationStatus {
    Ok = 0x0,
    NotFound = 0x1,
    NullPtr = 0x2,
    RelayError = 0x3,
    ChannelSendError = 0x4,
    ChannelReceiveError = 0x5,
    ServiceError = 0x6,
    RuntimeError = 0x7,
}

impl OperationStatus {
    pub fn is_ok(&self) -> bool {
        *self == Self::Ok
    }
}
