use std::{any::type_name, borrow::Cow};

/// A failure while decoding a value from its bytes.
///
/// The structured variants cover the common cases; [`DecodeError::Custom`] is
/// the escape hatch for anything else.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("Unexpected end of input while decoding {type_name}: needed {needed} more byte(s)")]
    UnexpectedEnd {
        type_name: &'static str,
        needed: usize,
    },
    #[error("Invalid encoding for {type_name}: {message}")]
    InvalidValue {
        type_name: &'static str,
        message: Cow<'static, str>,
    },
    #[error("Unknown discriminant {discriminant:#x} for {type_name}")]
    UnknownDiscriminant {
        type_name: &'static str,
        discriminant: u64,
    },
    #[error("Length {len} out of bounds [{min}, {max}] for {type_name}")]
    LengthOutOfBounds {
        type_name: &'static str,
        len: usize,
        min: usize,
        max: usize,
    },
    #[error("{0}")]
    Custom(Cow<'static, str>),
}

impl DecodeError {
    /// The input ran out `needed` bytes short while decoding a `T`.
    #[must_use]
    pub fn end_of_input<T>(needed: usize) -> Self
    where
        T: ?Sized,
    {
        Self::UnexpectedEnd {
            type_name: type_name::<T>(),
            needed,
        }
    }

    /// The bytes for a `T` were well-sized but semantically invalid.
    #[must_use]
    pub fn invalid_value<T>(message: impl Into<Cow<'static, str>>) -> Self
    where
        T: ?Sized,
    {
        Self::InvalidValue {
            type_name: type_name::<T>(),
            message: message.into(),
        }
    }

    /// A `T` tag/discriminant did not match any known variant.
    #[must_use]
    pub fn unknown_discriminant<T>(discriminant: u64) -> Self
    where
        T: ?Sized,
    {
        Self::UnknownDiscriminant {
            type_name: type_name::<T>(),
            discriminant,
        }
    }

    /// A `T`'s decoded length fell outside its `[min, max]` bound.
    #[must_use]
    pub fn length_out_of_bounds<T>(len: usize, min: usize, max: usize) -> Self
    where
        T: ?Sized,
    {
        Self::LengthOutOfBounds {
            type_name: type_name::<T>(),
            len,
            min,
            max,
        }
    }

    /// An arbitrary decode failure that the structured variants do not capture.
    #[must_use]
    pub fn custom<Message>(message: Message) -> Self
    where
        Message: Into<Cow<'static, str>>,
    {
        Self::Custom(message.into())
    }
}
