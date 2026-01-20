pub trait Next {
    #[must_use]
    fn next(self) -> Self;
}

pub trait Metadata {
    type AppId;
    type Index: Next;

    fn metadata(&self) -> (Self::AppId, Self::Index);
}
