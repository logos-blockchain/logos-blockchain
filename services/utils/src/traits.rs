pub trait FromSettings {
    type Settings;
    fn from_settings(settings: &Self::Settings) -> Self;
}
