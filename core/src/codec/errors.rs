use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Serialized input size {size} exceeds maximum of {max} bytes")]
    InputTooLarge { size: usize, max: u64 },
    #[error("Failed to serialize message: {0}")]
    Serialize(Box<dyn std::error::Error + Send + Sync>),
    #[error("Failed to deserialize message: {0}")]
    Deserialize(Box<dyn std::error::Error + Send + Sync>),
}

impl Clone for Error {
    fn clone(&self) -> Self {
        match self {
            Self::InputTooLarge { size, max } => Self::InputTooLarge {
                size: *size,
                max: *max,
            },
            Self::Serialize(e) => Self::Serialize(format!("{e}").into()),
            Self::Deserialize(e) => Self::Deserialize(format!("{e}").into()),
        }
    }
}

impl From<Error> for io::Error {
    fn from(value: Error) -> Self {
        Self::new(io::ErrorKind::InvalidData, value)
    }
}
