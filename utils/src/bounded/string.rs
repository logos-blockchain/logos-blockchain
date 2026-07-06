use crate::bounded::{Bounded, BoundedError, BoundedLen};

impl BoundedLen for String {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

/// A `String` whose byte length is statically enforced to be in the range
/// `[MIN, MAX]`.
///
/// A thin alias over [`Bounded`]. Length checking, (de)serialization, `Display`
/// and unchecked construction all come from the generic wrapper; only the
/// string-flavoured conversions live here.
pub type BoundedString<const MIN: usize, const MAX: usize> = Bounded<String, MIN, MAX>;

impl<const MIN: usize, const MAX: usize> BoundedString<MIN, MAX> {
    /// Length in bytes (not `char`s), matching `str`/`String` semantics.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.as_inner().len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.as_inner().is_empty()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.as_inner()
    }
}

impl<const MIN: usize, const MAX: usize> TryFrom<String> for BoundedString<MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<const MIN: usize, const MAX: usize> TryFrom<&str> for BoundedString<MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use crate::bounded::{BoundedError, BoundedString};

    type Bounded3To5 = BoundedString<3, 5>;

    #[test]
    fn try_from_accepts_lengths_within_bounds() {
        assert_eq!(Bounded3To5::try_from("abc".to_owned()).unwrap().len(), 3);
        assert_eq!(Bounded3To5::try_from("abcde").unwrap().len(), 5);
    }

    #[test]
    fn try_from_rejects_too_short() {
        assert_eq!(
            Bounded3To5::try_from("ab".to_owned()),
            Err(BoundedError::TooFewItems { count: 2, min: 3 })
        );
    }

    #[test]
    fn try_from_rejects_empty() {
        assert_eq!(
            Bounded3To5::try_from(String::new()),
            Err(BoundedError::EmptyInput)
        );
    }

    #[test]
    fn try_from_rejects_too_long() {
        assert_eq!(
            Bounded3To5::try_from("abcdef"),
            Err(BoundedError::TooManyItems { count: 6, max: 5 })
        );
    }

    #[test]
    fn serialize_is_transparent() {
        let s = Bounded3To5::try_from("abcd").unwrap();
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"abcd\"");
    }

    #[test]
    fn deserialize_enforces_bounds() {
        let ok: Bounded3To5 = serde_json::from_str("\"abcd\"").unwrap();
        assert_eq!(ok.as_inner(), "abcd");

        let err = serde_json::from_str::<Bounded3To5>("\"ab\"").unwrap_err();
        assert!(
            err.to_string().contains("below minimum of 3"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn len_measures_bytes_not_chars() {
        // "é" is two UTF-8 bytes, so "éé" is 4 bytes — within [3, 5].
        #[expect(clippy::non_ascii_literal, reason = "Test case.")]
        let s = Bounded3To5::try_from("éé").unwrap();
        assert_eq!(s.len(), 4);
    }
}
