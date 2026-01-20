pub mod enumerated {
    /// Checks if all references in a slice point to the same variant of an
    /// enum.
    ///
    /// # Parameters
    ///
    /// - `values`: A slice of references to values of type `T`.
    ///
    /// # Returns
    ///
    /// - `true` if the slice is non-empty and all references in the slice point
    ///   to the same variant, otherwise returns `false`.
    ///
    /// - An empty slice is considered to have all elements the same, thus
    ///   returns `true`. Following `all`'s behaviour.
    ///
    /// # Example
    ///
    /// ```
    /// use lb_utils::types::enumerated::is_same_variant;
    ///
    /// enum Color {
    ///     Red,
    ///     Blue,
    /// }
    ///
    /// let values = vec![&Color::Red, &Color::Red];
    /// assert!(is_same_variant(&values));
    ///
    /// let empty_values: Vec<&Color> = vec![];
    /// assert!(is_same_variant(&empty_values));
    ///
    /// let mixed_values = vec![&Color::Red, &Color::Blue];
    /// assert!(!is_same_variant(&mixed_values));
    /// ```
    pub fn is_same_variant<T>(values: &[&T]) -> bool {
        let mut discriminants = values.iter().map(|item| std::mem::discriminant::<T>(item));
        let Some(head) = discriminants.next() else {
            return true;
        };
        discriminants.all(|tail_item| tail_item == head)
    }

    #[cfg(test)]
    mod tests {
        use crate::types::enumerated;

        enum TestEnum {
            A,
            B,
            C,
        }

        #[test]
        fn test_is_same_variant() {
            let values = [TestEnum::A, TestEnum::B, TestEnum::C];
            assert!(!enumerated::is_same_variant(&[
                &values[0], &values[1], &values[2]
            ]));

            let values: [&TestEnum; 0] = [];
            assert!(enumerated::is_same_variant(&values));

            let values = [TestEnum::A, TestEnum::A, TestEnum::A];
            assert!(enumerated::is_same_variant(&[
                &values[0], &values[1], &values[2]
            ]));
        }
    }
}
