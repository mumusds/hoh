//! Serde deserializers for JSON Schema `integer` members. JSON Schema counts `1.0` and `1e1` as integers, so a
//! schema-valid document may carry them; decoding must accept them (M1T01R-01). Values the Rust type cannot hold are
//! an error, never a truncation. Encoding writes canonical integers.

use serde::de::{self, Deserializer, Visitor};
use std::fmt;
use std::marker::PhantomData;

struct Integer<T>(PhantomData<T>);

impl<T: TryFrom<u64>> Visitor<'_> for Integer<T> {
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a non-negative integer within range")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<T, E> {
        T::try_from(v).map_err(|_| E::invalid_value(de::Unexpected::Unsigned(v), &self))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<T, E> {
        u64::try_from(v)
            .map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
            .and_then(|v| self.visit_u64(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<T, E> {
        // Every integral f64 below 2^64 converts to u64 exactly.
        if v.is_finite() && v.fract() == 0.0 && (0.0..18_446_744_073_709_551_616.0).contains(&v) {
            self.visit_u64(v as u64)
        } else {
            Err(E::invalid_value(de::Unexpected::Float(v), &self))
        }
    }
}

/// `#[serde(deserialize_with = "crate::integer::deserialize")]` for an integer member.
pub fn deserialize<'de, D: Deserializer<'de>, T: TryFrom<u64>>(
    deserializer: D,
) -> Result<T, D::Error> {
    deserializer.deserialize_any(Integer(PhantomData))
}

struct Optional<T>(PhantomData<T>);

impl<'de, T: TryFrom<u64>> Visitor<'de> for Optional<T> {
    type Value = Option<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a non-negative integer within range, or null")
    }

    fn visit_none<E: de::Error>(self) -> Result<Option<T>, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Option<T>, D::Error> {
        deserialize(deserializer).map(Some)
    }
}

/// `#[serde(default, deserialize_with = "crate::integer::option")]` for an optional integer member.
pub fn option<'de, D: Deserializer<'de>, T: TryFrom<u64>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    deserializer.deserialize_option(Optional(PhantomData))
}
