use std::str::FromStr;

/// Parse environment variable as `T`.
///
/// Returns `None` when variable is missing or parsing fails.
#[must_use]
pub fn env_opt<T>(key: &str) -> Option<T>
where
    T: FromStr,
{
    std::env::var(key).ok()?.parse::<T>().ok()
}

/// Parse positive environment variable as `u64`.
///
/// Returns `None` when missing, invalid, or zero.
#[must_use]
pub fn env_opt_u64(key: &str) -> Option<u64> {
    env_opt::<u64>(key).filter(|value| *value > 0)
}

/// Parse positive environment variable as `u32`.
///
/// Returns `None` when missing, invalid, zero, or out of `u32` range.
#[must_use]
pub fn env_opt_u32(key: &str) -> Option<u32> {
    let value = env_opt_u64(key)?;
    u32::try_from(value).ok()
}

/// Parse positive environment variable as `u64` with fallback default.
#[must_use]
pub fn env_u64(key: &str, default: u64) -> u64 {
    env_opt_u64(key).unwrap_or(default)
}

/// Parse boolean-like environment variable.
///
/// Accepted truthy values: `1`, `true`, `yes`, `on` (case-insensitive).
/// Missing or any other value resolves to `false`.
#[must_use]
pub fn env_flag(key: &str) -> bool {
    let Ok(raw) = std::env::var(key) else {
        return false;
    };

    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}
