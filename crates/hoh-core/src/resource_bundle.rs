//! Immutable resource bytes and their identity (ADR-034). A running host owns one bundle; a Run pins it by role.
use crate::{context, objects};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Serialize, Deserialize)]
pub struct Bundle {
    release: String,
    format: String,
    files: BTreeMap<String, String>,
    /// The digest of release, format and files, filled by [`Bundle::version`]. All mutation
    /// accessors invalidate it. The cache is not part of stored bytes or equality.
    #[serde(skip, default)]
    version_cache: std::sync::Mutex<Option<String>>,
}

impl Clone for Bundle {
    fn clone(&self) -> Self {
        let version_cache = self
            .version_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Self {
            release: self.release.clone(),
            format: self.format.clone(),
            files: self.files.clone(),
            version_cache: std::sync::Mutex::new(version_cache),
        }
    }
}

impl PartialEq for Bundle {
    fn eq(&self, other: &Self) -> bool {
        self.release == other.release && self.format == other.format && self.files == other.files
    }
}

impl Eq for Bundle {}

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
            version_cache: std::sync::Mutex::new(None),
        }
    }

    pub fn release(&self) -> &str {
        &self.release
    }

    pub fn set_release(&mut self, release: impl Into<String>) {
        self.clear_version();
        self.release = release.into();
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub fn set_format(&mut self, format: impl Into<String>) {
        self.clear_version();
        self.format = format.into();
    }

    /// The bundle's files. Mutate them with [`Bundle::insert_file`] or [`Bundle::retain_files`]
    /// so a later [`Bundle::version`] hashes the bytes that will be stored.
    pub fn files(&self) -> &BTreeMap<String, String> {
        &self.files
    }

    /// Replace one file and drop a cached digest (M3G-101).
    pub fn insert_file(&mut self, path: impl Into<String>, text: impl Into<String>) {
        self.clear_version();
        self.files.insert(path.into(), text.into());
    }

    /// Keep the files `keep` accepts and drop a cached digest (M3G-101).
    pub fn retain_files(&mut self, mut keep: impl FnMut(&String, &mut String) -> bool) {
        self.clear_version();
        self.files.retain(|path, text| keep(path, text));
    }

    fn clear_version(&self) {
        *self
            .version_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    fn compute_version(&self) -> String {
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

    pub fn version(&self) -> String {
        if let Some(hit) = self
            .version_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return hit;
        }
        let computed = self.compute_version();
        let mut slot = self
            .version_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(computed.clone());
        }
        computed
    }

    /// Check the output schemas and the actual builtin selected by the override chain.
    /// A user override is validated separately and never becomes a transient bundle error.
    pub fn renderer_compatible(
        &self,
        role: &str,
        builtin_path: Option<&str>,
    ) -> Result<(), String> {
        if self.format != context::FORMAT {
            return Err("pinned context format cannot be rendered by this binary".to_owned());
        }
        context::output_schemas_with(role, |path| {
            serde_json::from_str(self.text(path)?).map_err(|error| error.to_string())
        })?;
        if let Some(path) = builtin_path {
            context::contract_template_ok(self.text(path)?)?;
        }
        Ok(())
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

/// Loading failures describe immutable input integrity, not renderer compatibility.
#[derive(Debug)]
pub enum LoadError {
    Object(String),
    Decode(serde_json::Error),
    IdentityMismatch,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(why) => f.write_str(why),
            Self::Decode(error) => error.fmt(f),
            Self::IdentityMismatch => f.write_str("resource bundle identity mismatch"),
        }
    }
}

impl std::error::Error for LoadError {}

impl Pin {
    /// Load and verify identity; the executor checks compatibility only after this succeeds.
    pub fn load(&self, project: &Path) -> Result<Bundle, LoadError> {
        let bytes = objects::get(project, &self.bundle_sha256).map_err(LoadError::Object)?;
        let bundle: Bundle = serde_json::from_slice(&bytes).map_err(LoadError::Decode)?;
        if bundle.version() != self.resource_bundle_version {
            return Err(LoadError::IdentityMismatch);
        }
        Ok(bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bundle {
        let mut files = BTreeMap::new();
        files.insert(
            "resources/prompts/developer.md".to_owned(),
            "body\n".to_owned(),
        );
        Bundle {
            release: "0.0.0".to_owned(),
            format: context::FORMAT.to_owned(),
            files,
            version_cache: std::sync::Mutex::new(None),
        }
    }

    #[test]
    fn test_047_03_release_change_after_version_round_trips() {
        let dir = tempfile::Builder::new()
            .prefix("m3low-c-release-")
            .tempdir()
            .unwrap();
        let mut bundle = sample();
        let before = bundle.version();
        bundle.set_release("0.0.1");
        let pin = bundle.store(dir.path()).unwrap();
        let loaded = pin.load(dir.path()).unwrap();
        assert_ne!(before, pin.resource_bundle_version);
        assert_eq!(loaded, bundle);
        assert_eq!(loaded.version(), pin.resource_bundle_version);
    }

    #[test]
    fn test_047_03_format_change_after_version_round_trips() {
        let dir = tempfile::Builder::new()
            .prefix("m3low-c-format-")
            .tempdir()
            .unwrap();
        let mut bundle = sample();
        bundle.set_format("legacy-format");
        let before = bundle.version();
        bundle.set_format(context::FORMAT);
        let pin = bundle.store(dir.path()).unwrap();
        let loaded = pin.load(dir.path()).unwrap();
        assert_ne!(before, pin.resource_bundle_version);
        assert_eq!(loaded, bundle);
        assert_eq!(loaded.version(), pin.resource_bundle_version);
    }

    /// The cached digest is not part of the stored object or of equality (M3G-101).
    /// Replacing a file drops the cache, so a later store pins the new bytes.
    #[test]
    fn version_cache_does_not_change_identity_bytes_or_equality() {
        let fresh = sample();
        let cached = sample();
        let once = cached.version();
        assert_eq!(cached.version(), once);
        assert_eq!(fresh.version(), once);
        assert_eq!(fresh, cached);
        assert_eq!(
            serde_json::to_vec(&fresh).unwrap(),
            serde_json::to_vec(&cached).unwrap()
        );
        let restored: Bundle =
            serde_json::from_slice(&serde_json::to_vec(&cached).unwrap()).unwrap();
        assert_eq!(restored, cached);
        assert_eq!(restored.version(), once);
        assert_eq!(cached.clone().version(), once);
        let mut changed = cached.clone();
        changed.insert_file("resources/prompts/developer.md", "other\n");
        assert_ne!(changed.version(), once);
        assert_eq!(changed.version(), changed.version());
    }
}
