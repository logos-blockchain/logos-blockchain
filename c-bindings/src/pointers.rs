use std::{ops::Deref, ptr::NonNull};

#[repr(transparent)]
pub struct OwnedPointer<T> {
    inner: NonNull<T>,
}

impl<T> OwnedPointer<T> {
    pub fn new(value: T) -> Self {
        let ptr = NonNull::from(Box::leak(Box::new(value)));
        Self { inner: ptr }
    }

    fn rebuild_box(&self) -> Box<T> {
        unsafe { Box::from_raw(self.inner.as_ptr()) }
    }

    pub fn into_box(self) -> Box<T> {
        let boxed_pointer = self.rebuild_box();
        #[expect(
            clippy::mem_forget,
            reason = "Ownership is transferred to a Box. Keeping Drop enabled would trigger a double-free."
        )]
        std::mem::forget(self);
        boxed_pointer
    }
}

impl<T> From<T> for OwnedPointer<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> Deref for OwnedPointer<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.inner.as_ref() }
    }
}

impl<T> Drop for OwnedPointer<T> {
    fn drop(&mut self) {
        drop(self.rebuild_box());
    }
}
