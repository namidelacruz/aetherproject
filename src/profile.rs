//! Unified, versioned ingest/culling profile.
//!
//! Merges Photo Mechanic concepts (IPTC stationery, code replacements, rename
//! strings) with Kyno concepts (checksums, MHL, LUTs, card-structure lock),
//! and owns the rules that resolve conflicts between overlapping features
//! (see [`UnifiedIngestProfile::validate`]).

use crate::error::{AethrCraftError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Bumped whenever the on-disk profile schema changes incompatibly.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChecksumAlgorithm {
    None,
    /// Fast non-cryptographic hash, standard for camera offloads.
    XxHash64,
    /// SIMD-accelerated and cryptographic.
    Blake3,
    /// Archival / broadcast requirement.
    Sha256,
    Md5,
    Sha1,
}

impl ChecksumAlgorithm {
    pub const ALL: [ChecksumAlgorithm; 5] = [
        ChecksumAlgorithm::XxHash64,
        ChecksumAlgorithm::Blake3,
        ChecksumAlgorithm::Sha256,
        ChecksumAlgorithm::Md5,
        ChecksumAlgorithm::Sha1,
    ];

    /// Element name used in ASC-MHL v2 manifests, if the format supports it.
    pub fn mhl_name(self) -> Option<&'static str> {
        match self {
            ChecksumAlgorithm::XxHash64 => Some("xxh64"),
            ChecksumAlgorithm::Md5 => Some("md5"),
            ChecksumAlgorithm::Sha1 => Some("sha1"),
            _ => None,
        }
    }

    pub fn from_mhl_name(name: &str) -> Option<Self> {
        match name {
            "xxh64" => Some(ChecksumAlgorithm::XxHash64),
            "md5" => Some(ChecksumAlgorithm::Md5),
            "sha1" => Some(ChecksumAlgorithm::Sha1),
            _ => None,
        }
    }

    /// Lenient parser for user input and legacy presets.
    pub fn from_name(name: &str) -> Option<Self> {
        let n: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_uppercase();
        match n.as_str() {
            "NONE" | "OFF" => Some(ChecksumAlgorithm::None),
            "XXHASH64" | "XXHASH" | "XXH64" => Some(ChecksumAlgorithm::XxHash64),
            "BLAKE3" => Some(ChecksumAlgorithm::Blake3),
            "SHA256" => Some(ChecksumAlgorithm::Sha256),
            "MD5" => Some(ChecksumAlgorithm::Md5),
            "SHA1" => Some(ChecksumAlgorithm::Sha1),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChecksumAlgorithm::None => "none",
            ChecksumAlgorithm::XxHash64 => "xxh64",
            ChecksumAlgorithm::Blake3 => "blake3",
            ChecksumAlgorithm::Sha256 => "sha256",
            ChecksumAlgorithm::Md5 => "md5",
            ChecksumAlgorithm::Sha1 => "sha1",
        }
    }
}

/// What to do when a planned destination path already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CollisionPolicy {
    /// Keep both: renamable assets get a `_N` stem suffix (applied to every
    /// member so RAW+JPEG pairs stay paired); locked camera structures are
    /// placed in a `<card>_N` directory instead, never renamed.
    #[default]
    Suffix,
    /// Leave the existing file; it is re-hashed and must match the source.
    Skip,
    /// Replace the existing file.
    Overwrite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UnifiedIngestProfile {
    pub schema_version: u32,
    pub profile_name: String,
    pub source_origin: String,

    // Multi-destination routing
    pub destinations: Vec<PathBuf>,
    pub folder_naming_pattern: String,
    /// Empty pattern keeps original file names.
    pub file_naming_pattern: String,
    pub sequence_start: u64,
    pub sequence_padding: usize,
    pub collision_policy: CollisionPolicy,
    pub max_concurrent_files: usize,

    // Verification & camera-structure features
    pub verify_checksum: ChecksumAlgorithm,
    pub generate_asc_mhl: bool,
    /// Protects BDMV / M4ROOT / RDC spanned clips by copying the whole camera
    /// structure verbatim.
    pub preserve_camera_card_structure: bool,
    /// Applied to previews and frame exports only — never to originals.
    pub apply_3d_lut_path: Option<PathBuf>,
    pub extract_video_subclips: bool,

    // Metadata & culling-speed features
    pub extract_embedded_raw_preview: bool,
    pub write_xmp_sidecars: bool,
    pub code_replacement_delimiter: char,
    pub code_replacements: HashMap<String, Vec<String>>,
    pub iptc_template: UnifiedMetadataTemplate,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnifiedMetadataTemplate {
    pub creator: Option<String>,
    pub credit_line: Option<String>,
    pub copyright_notice: Option<String>,
    pub caption_abstract: Option<String>,
    pub headline: Option<String>,
    pub city: Option<String>,
    pub province_state: Option<String>,
    pub country: Option<String>,
    pub keywords: Vec<String>,
    // Video production extensions
    pub reel_name: Option<String>,
    pub scene: Option<String>,
    pub shot_take: Option<String>,
    pub camera_angle: Option<String>,
}

impl UnifiedMetadataTemplate {
    /// Overlays `other` onto `self`: non-empty fields of `other` win, keywords
    /// are unioned in order (no duplicates).
    pub fn overlay(&mut self, other: &UnifiedMetadataTemplate) {
        fn pick(dst: &mut Option<String>, src: &Option<String>) {
            if let Some(v) = src {
                if !v.trim().is_empty() {
                    *dst = Some(v.clone());
                }
            }
        }
        pick(&mut self.creator, &other.creator);
        pick(&mut self.credit_line, &other.credit_line);
        pick(&mut self.copyright_notice, &other.copyright_notice);
        pick(&mut self.caption_abstract, &other.caption_abstract);
        pick(&mut self.headline, &other.headline);
        pick(&mut self.city, &other.city);
        pick(&mut self.province_state, &other.province_state);
        pick(&mut self.country, &other.country);
        pick(&mut self.reel_name, &other.reel_name);
        pick(&mut self.scene, &other.scene);
        pick(&mut self.shot_take, &other.shot_take);
        pick(&mut self.camera_angle, &other.camera_angle);
        for kw in &other.keywords {
            if !self.keywords.iter().any(|k| k.eq_ignore_ascii_case(kw)) {
                self.keywords.push(kw.clone());
            }
        }
    }
}

impl Default for UnifiedIngestProfile {
    fn default() -> Self {
        UnifiedIngestProfile {
            schema_version: PROFILE_SCHEMA_VERSION,
            profile_name: "Default".to_string(),
            source_origin: "AethrCraft".to_string(),
            destinations: Vec::new(),
            folder_naming_pattern: "{date:YYYY}-{date:MM}-{date:DD}".to_string(),
            file_naming_pattern: "{date:YYYYMMDD}_{seq}".to_string(),
            sequence_start: 1,
            sequence_padding: 4,
            collision_policy: CollisionPolicy::Suffix,
            max_concurrent_files: 2,
            verify_checksum: ChecksumAlgorithm::XxHash64,
            generate_asc_mhl: true,
            preserve_camera_card_structure: true,
            apply_3d_lut_path: None,
            extract_video_subclips: true,
            extract_embedded_raw_preview: true,
            write_xmp_sidecars: true,
            code_replacement_delimiter: '\\',
            code_replacements: HashMap::new(),
            iptc_template: UnifiedMetadataTemplate::default(),
        }
    }
}

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl UnifiedIngestProfile {
    /// Normalizes the profile and resolves feature conflicts. Returns
    /// human-readable warnings for every adjustment; hard conflicts are errors.
    pub fn validate(&mut self) -> Result<Vec<String>> {
        let mut warnings = Vec::new();

        if self.schema_version > PROFILE_SCHEMA_VERSION {
            return Err(AethrCraftError::SchemaError(format!(
                "profile schema v{} is newer than supported v{}",
                self.schema_version, PROFILE_SCHEMA_VERSION
            )));
        }
        self.schema_version = PROFILE_SCHEMA_VERSION;

        let d = self.code_replacement_delimiter;
        if d.is_alphanumeric() || d.is_whitespace() || matches!(d, '{' | '}' | '/' | '#' | '$') {
            return Err(AethrCraftError::SchemaError(format!(
                "code replacement delimiter {d:?} collides with token or path syntax"
            )));
        }

        // Duplicate / nested destinations.
        let mut seen: Vec<PathBuf> = Vec::new();
        let mut unique = Vec::new();
        for dest in &self.destinations {
            let norm = lexical_normalize(dest);
            if seen.contains(&norm) {
                warnings.push(format!("duplicate destination {} removed", dest.display()));
                continue;
            }
            seen.push(norm);
            unique.push(dest.clone());
        }
        for (i, a) in seen.iter().enumerate() {
            for (j, b) in seen.iter().enumerate() {
                if i != j && b.starts_with(a) {
                    return Err(AethrCraftError::SchemaError(format!(
                        "destination {} is nested inside {}",
                        b.display(),
                        a.display()
                    )));
                }
            }
        }
        self.destinations = unique;

        for (name, pat) in [
            ("folder_naming_pattern", &self.folder_naming_pattern),
            ("file_naming_pattern", &self.file_naming_pattern),
        ] {
            let p = Path::new(pat);
            if p.is_absolute() || pat.starts_with('/') {
                return Err(AethrCraftError::SchemaError(format!(
                    "{name} must be relative to the destination"
                )));
            }
            if pat.split(['/', '\\']).any(|c| c == "..") {
                return Err(AethrCraftError::SchemaError(format!(
                    "{name} must not contain '..'"
                )));
            }
        }
        if self.file_naming_pattern.contains('/') {
            warnings.push(
                "file_naming_pattern contains '/'; separators become '_' (use folder_naming_pattern for folders)"
                    .into(),
            );
        }

        if self.sequence_padding > 12 {
            warnings.push("sequence_padding clamped to 12".into());
            self.sequence_padding = 12;
        }
        if self.max_concurrent_files == 0 {
            self.max_concurrent_files = 1;
        }

        if self.generate_asc_mhl {
            match self.verify_checksum {
                ChecksumAlgorithm::None => warnings.push(
                    "ASC-MHL needs hashes: xxh64 will be computed even though verification is off"
                        .into(),
                ),
                a if a.mhl_name().is_none() => warnings.push(format!(
                    "{} is not an ASC-MHL hash format: xxh64 is computed in the same pass for the manifest",
                    a.label()
                )),
                _ => {}
            }
        }

        if let Some(lut) = &self.apply_3d_lut_path {
            if !lut.exists() {
                warnings.push(format!(
                    "LUT {} not found; previews will be ungraded",
                    lut.display()
                ));
            }
        }
        Ok(warnings)
    }

    /// Hash format written to ASC-MHL manifests, if enabled.
    pub fn mhl_algorithm(&self) -> Option<ChecksumAlgorithm> {
        if !self.generate_asc_mhl {
            return None;
        }
        Some(if self.verify_checksum.mhl_name().is_some() {
            self.verify_checksum
        } else {
            ChecksumAlgorithm::XxHash64
        })
    }

    /// Every algorithm computed during the single read pass.
    pub fn hash_algorithms(&self) -> Vec<ChecksumAlgorithm> {
        let mut v = Vec::new();
        if self.verify_checksum != ChecksumAlgorithm::None {
            v.push(self.verify_checksum);
        }
        if let Some(m) = self.mhl_algorithm() {
            if !v.contains(&m) {
                v.push(m);
            }
        }
        v
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let profile = if Self::is_json(path) {
            serde_json::from_str(&text)?
        } else {
            toml::from_str(&text)?
        };
        Ok(profile)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = if Self::is_json(path) {
            serde_json::to_string_pretty(self)?
        } else {
            toml::to_string_pretty(self)?
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    fn is_json(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    }
}
