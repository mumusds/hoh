//! Immutable resource bytes and their identity (ADR-034). A running host owns one bundle; a Run pins it by role.
use crate::{context, objects};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub release: String,
    pub format: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub resource_bundle_version: String,
    pub bundle_sha256: String,
}

impl Bundle {
    pub fn current() -> Self {
        Self {
            release: env!("CARGO_PKG_VERSION").to_owned(),
            format: context::FORMAT.to_owned(),
            files: objects::RESOURCES
                .iter()
                .map(|(p, s)| ((*p).to_owned(), (*s).to_owned()))
                .collect(),
        }
    }

    pub fn version(&self) -> String {
        let files: Vec<_> = self
            .files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        format!(
            "{}+{}",
            self.release,
            objects::digest_of(&objects::with_format(&files, &self.format))
        )
    }

    pub fn text(&self, path: &str) -> Result<&str, String> {
        self.files
            .get(path)
            .map(String::as_str)
            .ok_or_else(|| format!("{path} missing from resource bundle"))
    }

    /// Store durable bytes before any transaction publishes a pin. Failed transactions leave only harmless objects.
    pub fn store(&self, project: &Path) -> Result<Pin, String> {
        self.store_io(project).map_err(|e| e.to_string())
    }

    /// Preserve the write's I/O kind until the persistence boundary classifies it.
    pub(crate) fn store_io(&self, project: &Path) -> std::io::Result<Pin> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        Ok(Pin {
            resource_bundle_version: self.version(),
            bundle_sha256: objects::put_io(project, &bytes)?,
        })
    }
}

impl Pin {
    pub fn load(&self, project: &Path) -> Result<Bundle, String> {
        let bundle: Bundle = serde_json::from_slice(&objects::get(project, &self.bundle_sha256)?)
            .map_err(|e| e.to_string())?;
        if bundle.version() != self.resource_bundle_version {
            return Err("resource bundle identity mismatch".to_owned());
        }
        if bundle.format != context::FORMAT {
            return Err("pinned context format cannot be rendered by this binary".to_owned());
        }
        Ok(bundle)
    }
}
