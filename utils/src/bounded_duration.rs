use std::{fmt::Display, marker::PhantomData};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
use serde_with::{DeserializeAs, SerializeAs};
use time::Duration;

// TODO: This type can be fancier once const for generic types is implemented
// https://doc.rust-lang.org/beta/unstable-book/language-features/adt-const-params.html
// This means that we could add different bound from a const configuration
// checked on compile time.
/// This is a proxy type to be used to bound `Duration` deserialization checks.
///
/// It embeds the checks at type level. It implements the pertinent types from
/// the `serde_with` crate so it can be used within `serde_as` macros.
///
/// # Examples
/// ```rust
///     use serde_with::serde_as;
///     use time::Duration;
///     use logos_blockchain_utils::bounded_duration::{SECOND, MinimalBoundedDuration};
///
///     #[serde_as]
///     #[derive(serde::Serialize, serde::Deserialize)]
///     struct Foo {
///         #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
///         duration: Duration,
///     }
/// ```
#[expect(
    private_bounds,
    reason = "The `TimeTag` trait is not public, is just to tie up types for `MinimalBoundedDuration` and its implemented only by the supported types."
)]
#[derive(Serialize, Copy, Clone, Debug)]
#[serde(transparent)]
pub struct MinimalBoundedDuration<const MIN_DURATION: usize, TAG: TimeTag> {
    duration: Duration,
    #[serde(skip_serializing)]
    _tag: PhantomData<*const TAG>,
}

pub(crate) trait TimeTag {
    fn inner() -> char;
}

pub struct BoundTag<const TAG: char>;
impl<const TAG: char> TimeTag for BoundTag<TAG> {
    fn inner() -> char {
        TAG
    }
}

/// *Nanoseconds* tag type for `MinimalBoundedDuration`
pub type NANO = BoundTag<'n'>;
/// *Milliseconds* tag type for `MinimalBoundedDuration`
pub type MILLI = BoundTag<'l'>;
/// *Seconds* tag type for `MinimalBoundedDuration`
pub type SECOND = BoundTag<'s'>;
/// *Minute* tag type for `MinimalBoundedDuration`
pub type MINUTE = BoundTag<'m'>;
/// *Hour* tag type for `MinimalBoundedDuration`
pub type HOUR = BoundTag<'h'>;
/// *Day* tag type for `MinimalBoundedDuration`
pub type DAY = BoundTag<'d'>;

// we have a limitation to const types, so we better use the ones defined.
fn fill_duration_measure<T: TimeTag>() -> Result<String, impl Display> {
    match T::inner() {
        v @ ('d' | 'h' | 'm' | 's') => Ok(v.to_string()),
        'l' => Ok("ms".to_owned()),
        'n' => Ok("ns".to_owned()),
        other => Err(format!("'{other}' measure not supported")),
    }
}

impl<const MIN_DURATION: usize, TAG: TimeTag> SerializeAs<Duration>
    for MinimalBoundedDuration<MIN_DURATION, TAG>
{
    fn serialize_as<S>(source: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        source.serialize(serializer)
    }
}

impl<'de, const MIN_DURATION: usize, TAG: TimeTag> DeserializeAs<'de, Duration>
    for MinimalBoundedDuration<MIN_DURATION, TAG>
{
    fn deserialize_as<D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let duration: Self = Self::deserialize(deserializer)?;
        Ok(duration.duration)
    }
}

impl<const MIN_DURATION: usize, TAG: TimeTag> SerializeAs<core::time::Duration>
    for MinimalBoundedDuration<MIN_DURATION, TAG>
{
    fn serialize_as<S>(source: &core::time::Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let to_serialize: Self = Self {
            duration: Duration::new(
                source.as_secs().try_into().map_err(S::Error::custom)?,
                source.subsec_nanos().try_into().map_err(S::Error::custom)?,
            ),
            _tag: PhantomData,
        };
        to_serialize.serialize(serializer)
    }
}
impl<'de, const MIN_DURATION: usize, TAG: TimeTag> DeserializeAs<'de, core::time::Duration>
    for MinimalBoundedDuration<MIN_DURATION, TAG>
{
    fn deserialize_as<D>(deserializer: D) -> Result<core::time::Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let duration: Self = Self::deserialize(deserializer)?;
        if duration.duration.is_negative() {
            return Err(D::Error::custom(
                "Negative duration is not supported for std::time::Duration",
            ));
        }
        duration.try_into().map_err(D::Error::custom)
    }
}

impl<'de, const MIN_DURATION: usize, TAG: TimeTag> Deserialize<'de>
    for MinimalBoundedDuration<MIN_DURATION, TAG>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value: Duration = Duration::deserialize(deserializer)?;

        let reformat_measure = fill_duration_measure::<TAG>().map_err(D::Error::custom)?;
        let parsed_duration =
            humantime::parse_duration(format!("{MIN_DURATION}{reformat_measure}").as_str())
                .map_err(D::Error::custom)?;
        let min_duration = Duration::new(
            parsed_duration
                .as_secs()
                .try_into()
                .map_err(D::Error::custom)?,
            parsed_duration
                .subsec_nanos()
                .try_into()
                .map_err(D::Error::custom)?,
        );
        if value < min_duration {
            return Err(D::Error::custom(format!(
                "Minimal duration is {min_duration} but got {value}"
            )));
        }
        Ok(Self {
            duration: value,
            _tag: PhantomData,
        })
    }
}

impl<const MIN_DURATION: usize, TAG: TimeTag> From<MinimalBoundedDuration<MIN_DURATION, TAG>>
    for Duration
{
    fn from(value: MinimalBoundedDuration<MIN_DURATION, TAG>) -> Self {
        value.duration
    }
}

impl<const MIN_DURATION: usize, TAG: TimeTag> TryFrom<MinimalBoundedDuration<MIN_DURATION, TAG>>
    for core::time::Duration
{
    type Error = std::io::Error;

    fn try_from(value: MinimalBoundedDuration<MIN_DURATION, TAG>) -> Result<Self, Self::Error> {
        let secs = value
            .duration
            .whole_seconds()
            .try_into()
            .map_err(Self::Error::other)?;
        let nanos = value
            .duration
            .subsec_nanoseconds()
            .try_into()
            .map_err(Self::Error::other)?;
        Ok(Self::new(secs, nanos))
    }
}

#[cfg(test)]
mod test {
    use serde_json::json;
    use serde_with::serde_as;
    use time::Duration;

    use crate::bounded_duration::{DAY, MinimalBoundedDuration, SECOND};

    #[test]
    fn success_deserialize() {
        let json_value = json!([1, 0]);
        let value = serde_json::to_value(json_value).unwrap();
        let _duration: MinimalBoundedDuration<1, SECOND> = serde_json::from_value(value).unwrap();
    }

    #[test]
    #[should_panic(expected = "Minimal duration is 1d but got 1s")]
    fn fail_deserialize_with_type_bound() {
        let value = serde_json::to_value(Duration::seconds(1)).unwrap();

        let _duration: MinimalBoundedDuration<1, DAY> = serde_json::from_value(value).unwrap();
    }

    #[serde_as]
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Foo {
        #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
        duration: Duration,
    }

    #[test]
    fn deserialize_proxy_type_success() {
        let foo = Foo {
            duration: Duration::seconds(10),
        };
        let _foo: Foo = serde_json::from_value(serde_json::to_value(&foo).unwrap()).unwrap();
    }

    #[test]
    fn deserialize_proxy_type_success_for_std() {
        #[serde_as]
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Foo {
            #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
            duration: std::time::Duration,
        }
        let foo = Foo {
            duration: std::time::Duration::from_secs(10),
        };
        let value = serde_json::to_value(&foo).unwrap();
        let _foo: Foo = serde_json::from_value(value).unwrap();
    }

    #[test]
    #[should_panic(expected = "Minimal duration is 1s but got 10ms")]
    fn deserialize_proxy_type_fails() {
        let foo = Foo {
            duration: Duration::milliseconds(10),
        };
        let _foo: Foo = serde_json::from_value(serde_json::to_value(&foo).unwrap()).unwrap();
    }
}
