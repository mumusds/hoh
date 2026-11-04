//! Serde helper for optional members that may also be `null` (`Option<Option<T>>`): `None` when the member is absent,
//! `Some(None)` when it is `null`, `Some(Some(v))` otherwise. Use with
//! `#[serde(default, skip_serializing_if = "Option::is_none", with = "crate::nullable")]` so a round trip keeps the
//! difference between an absent member and an explicit null.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub fn serialize<T: Serialize, S: Serializer>(
    value: &Option<Option<T>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(inner) => inner.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}
