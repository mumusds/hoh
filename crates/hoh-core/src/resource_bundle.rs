//! Immutable resource bytes and their identity (ADR-034). A running host owns one bundle; a Run pins it by role.
use crate::{context, objects};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub release: String,
    pub format: String,
    pub files: BTreeMap<String, String>,
    /// Filled on the first [`Bundle::version`] call. Not part of the stored bytes or of equality.
    /// Do not change `files` after that call: the cached digest would no longer match them (M3G-101).
    #[serde(skip, default)]
    version_cache: std::sync::OnceLock<String>,
}

impl Clone for Bundle {
    fn clone(&self) -> Self {
        let version_cache = std::sync::OnceLock::new();
        if let Some(version) = self.version_cache.get() {
            let _ = version_cache.set(version.clone());
        }
        Self {
            release: self.release.clone(),
            format: self.format.clone(),
            files: self.files.clone(),
            version_cache,
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
            version_cache: std::sync::OnceLock::new(),
        }
    }

    pub fn version(&self) -> String {
        self.version_cache
            .get_or_init(|| {
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
            })
            .clone()
    }

    /// Whether this binary can render `role` from these bytes (M3G-52): the context format matches,
    /// the role's output schemas resolve, and its contract template parses when the bundle has one.
    /// A prompt with no placeholders is compatible. This does not reject an old prompt's wording.
    pub fn renderer_compatible(&self, role: &str) -> Result<(), String> {
        if self.format != context::FORMAT {
            return Err("pinned context format cannot be rendered by this binary".to_owned());
        }
        context::output_schemas_with(role, |path| {
            serde_json::from_str(self.text(path)?).map_err(|error| error.to_string())
        })?;
        let prompt = format!("resources/prompts/{role}.md");
        if let Ok(text) = self.text(&prompt) {
            context::contract_template_ok(text)?;
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
            version_cache: std::sync::OnceLock::new(),
        }
    }

    /// The cached digest is not part of the stored object or of equality (M3G-101).
    /// Callers must not change `files` after the first `version` call.
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
    }
}
