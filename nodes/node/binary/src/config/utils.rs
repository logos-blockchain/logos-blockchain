pub(super) fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}
