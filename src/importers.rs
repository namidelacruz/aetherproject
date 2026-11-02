//! Legacy settings importers: Photo Mechanic (`.pmx`, `.snap`, code
//! replacement `.tsv`/`.txt`) and Kyno (`.kynopreset`), plus native profiles.
//!
//! Precedence when several sources are combined
//! ([`SettingsImporter::combine`]): native profile < Kyno preset < PM ingest
//! settings < PM IPTC snapshot < explicit code replacement file. Naming comes
//! from Photo Mechanic, verification/structure/LUT handling from Kyno.

use crate::error::{AethrCraftError, Result};
use crate::profile::{ChecksumAlgorithm, UnifiedIngestProfile, UnifiedMetadataTemplate};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Photo Mechanic exported XML ingest / IPTC snapshot structure (`.pmx`).
#[derive(Debug, Deserialize)]
pub struct PhotoMechanicPmx {
    #[serde(rename = "IngestSettings", default)]
    pub ingest: Option<PmIngestBlock>,
    #[serde(rename = "IPTCStationery", default)]
    pub stationery: Option<PmIptcBlock>,
}

#[derive(Debug, Deserialize)]
pub struct PmIngestBlock {
    pub primary_destination: String,
    pub secondary_destination: Option<String>,
    pub folder_name: String,
    pub rename_string: String,
    pub sequence_num: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct PmIptcBlock {
    pub photographer: Option<String>,
    pub credit: Option<String>,
    pub copyright: Option<String>,
    pub caption: Option<String>,
    pub headline: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub keywords: Option<String>,
}

/// A `.snap` IPTC snapshot wrapping a stationery block.
#[derive(Debug, Deserialize)]
struct PmSnapshotWrapped {
    #[serde(rename = "IPTCStationery", default)]
    stationery: Option<PmIptcBlock>,
}

/// Kyno JSON preset structure (`.kynopreset`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KynoPresetJson {
    pub name: String,
    pub target_path: String,
    pub backup_paths: Option<Vec<String>>,
    pub naming_pattern: String,
    pub subfolder_pattern: Option<String>,
    pub verify_mode: Option<String>,
    pub write_mhl: Option<bool>,
    pub keep_card_structure: Option<bool>,
    pub lut_file: Option<String>,
    pub metadata: Option<KynoMetadataBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KynoMetadataBlock {
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub description: Option<String>,
    pub title: Option<String>,
    pub reel: Option<String>,
    pub scene: Option<String>,
    pub take: Option<String>,
    pub camera: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// Kind of settings file, detected from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsKind {
    PhotoMechanicPmx,
    PhotoMechanicSnapshot,
    CodeReplacements,
    KynoPreset,
    NativeProfile,
}

impl SettingsKind {
    pub fn detect(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "pmx" => SettingsKind::PhotoMechanicPmx,
            "snap" => SettingsKind::PhotoMechanicSnapshot,
            "tsv" | "txt" => SettingsKind::CodeReplacements,
            "kynopreset" => SettingsKind::KynoPreset,
            "toml" | "json" => SettingsKind::NativeProfile,
            _ => return None,
        })
    }
}

/// Inputs to [`SettingsImporter::combine`].
#[derive(Debug, Default, Clone)]
pub struct ImportSources {
    pub native: Option<PathBuf>,
    pub kyno: Option<PathBuf>,
    pub pmx: Option<PathBuf>,
    pub snapshot: Option<PathBuf>,
    pub codes: Option<PathBuf>,
}

pub struct SettingsImporter;

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl SettingsImporter {
    /// Imports a Photo Mechanic XML (`.pmx`) settings document plus an
    /// optional tab-delimited code replacement file.
    pub fn from_photo_mechanic(
        pmx_xml: &str,
        code_replacement_tsv: Option<&Path>,
    ) -> Result<UnifiedIngestProfile> {
        let pmx: PhotoMechanicPmx = quick_xml::de::from_str(pmx_xml)?;
        let mut profile = UnifiedIngestProfile {
            profile_name: "Imported Photo Mechanic Profile".to_string(),
            source_origin: "PhotoMechanic_PMX".to_string(),
            file_naming_pattern: "{date:YYYYMMDD}_{seq:4}".to_string(),
            // Upgrade PM workflows with automatic xxHash64 verification.
            verify_checksum: ChecksumAlgorithm::XxHash64,
            generate_asc_mhl: false,
            preserve_camera_card_structure: false,
            extract_video_subclips: false,
            ..UnifiedIngestProfile::default()
        };

        if let Some(ingest) = pmx.ingest {
            profile
                .destinations
                .push(PathBuf::from(ingest.primary_destination.trim()));
            if let Some(sec) = non_empty(ingest.secondary_destination) {
                profile.destinations.push(PathBuf::from(sec));
            }
            profile.folder_naming_pattern = Self::normalize_pm_tokens(&ingest.folder_name);
            profile.file_naming_pattern = Self::normalize_pm_tokens(&ingest.rename_string);
            profile.sequence_start = ingest.sequence_num;
        }
        if let Some(stat) = pmx.stationery {
            profile.iptc_template = Self::pm_iptc_to_template(stat);
        }
        if let Some(tsv_path) = code_replacement_tsv {
            profile.code_replacements = Self::parse_pm_code_replacements(tsv_path)?;
        }
        Ok(profile)
    }

    fn pm_iptc_to_template(stat: PmIptcBlock) -> UnifiedMetadataTemplate {
        let norm = |s: Option<String>| non_empty(s).map(|v| Self::normalize_pm_tokens(&v));
        UnifiedMetadataTemplate {
            creator: norm(stat.photographer),
            credit_line: norm(stat.credit),
            copyright_notice: norm(stat.copyright),
            caption_abstract: norm(stat.caption),
            headline: norm(stat.headline),
            city: non_empty(stat.city),
            province_state: non_empty(stat.state),
            country: non_empty(stat.country),
            keywords: stat
                .keywords
                .map(|kw| {
                    kw.split([',', ';'])
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    /// Imports a Photo Mechanic IPTC snapshot (`.snap`) as a metadata template.
    pub fn from_photo_mechanic_snapshot(xml: &str) -> Result<UnifiedMetadataTemplate> {
        // Either `<Root><IPTCStationery>...</IPTCStationery></Root>` or a bare
        // block whose root element holds the fields directly.
        let wrapped: PmSnapshotWrapped = quick_xml::de::from_str(xml)?;
        let block = match wrapped.stationery {
            Some(b) => b,
            None => quick_xml::de::from_str::<PmIptcBlock>(xml)?,
        };
        Ok(Self::pm_iptc_to_template(block))
    }

    /// Parses tab-delimited code replacement files
    /// (`key \t replacement1 \t replacement2 ...`). Later duplicates win.
    pub fn parse_pm_code_replacements(path: &Path) -> Result<HashMap<String, Vec<String>>> {
        let reader = BufReader::new(File::open(path)?);
        let mut map = HashMap::new();
        for line in reader.lines() {
            let line = line?;
            let line = line.trim_end_matches(['\r', '\n']);
            let trimmed = line.trim_start_matches('\u{feff}');
            if trimmed.trim().is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            let mut parts = trimmed.split('\t');
            if let Some(code) = parts.next() {
                let code = code.trim();
                let replacements: Vec<String> = parts.map(|s| s.trim().to_string()).collect();
                if !code.is_empty() && replacements.iter().any(|r| !r.is_empty()) {
                    map.insert(code.to_string(), replacements);
                }
            }
        }
        Ok(map)
    }

    /// Imports a Kyno (`.kynopreset`) JSON configuration.
    pub fn from_kyno_preset(kyno_json: &str) -> Result<UnifiedIngestProfile> {
        let kyno: KynoPresetJson = serde_json::from_str(kyno_json)?;

        let mut destinations = vec![PathBuf::from(&kyno.target_path)];
        destinations.extend(
            kyno.backup_paths
                .unwrap_or_default()
                .into_iter()
                .map(PathBuf::from),
        );

        let verify_checksum = match kyno.verify_mode.as_deref() {
            Some(m) => ChecksumAlgorithm::from_name(m).unwrap_or(ChecksumAlgorithm::XxHash64),
            None => ChecksumAlgorithm::XxHash64,
        };

        let mut iptc_template = UnifiedMetadataTemplate::default();
        if let Some(meta) = kyno.metadata {
            let norm = |s: Option<String>| non_empty(s).map(|v| Self::normalize_kyno_tokens(&v));
            iptc_template.creator = norm(meta.creator);
            iptc_template.copyright_notice = norm(meta.copyright);
            iptc_template.caption_abstract = norm(meta.description);
            iptc_template.headline = norm(meta.title);
            iptc_template.reel_name = norm(meta.reel);
            iptc_template.scene = norm(meta.scene);
            iptc_template.shot_take = norm(meta.take);
            iptc_template.camera_angle = norm(meta.camera);
            iptc_template.keywords = meta.tags.unwrap_or_default();
        }

        Ok(UnifiedIngestProfile {
            profile_name: kyno.name,
            source_origin: "KynoPremium_Preset".to_string(),
            destinations,
            folder_naming_pattern: Self::normalize_kyno_tokens(
                kyno.subfolder_pattern.as_deref().unwrap_or(""),
            ),
            file_naming_pattern: Self::normalize_kyno_tokens(&kyno.naming_pattern),
            verify_checksum,
            generate_asc_mhl: kyno.write_mhl.unwrap_or(true),
            preserve_camera_card_structure: kyno.keep_card_structure.unwrap_or(true),
            apply_3d_lut_path: kyno.lut_file.map(PathBuf::from),
            extract_video_subclips: true,
            iptc_template,
            ..UnifiedIngestProfile::default()
        })
    }

    /// Merges a Photo Mechanic profile with a Kyno preset: PM's IPTC, naming
    /// and code replacements combined with Kyno's checksum, LUT and
    /// video-structure handling.
    pub fn merge_pm_and_kyno(
        pm: UnifiedIngestProfile,
        kyno: UnifiedIngestProfile,
    ) -> UnifiedIngestProfile {
        let mut destinations = kyno.destinations.clone();
        for dest in &pm.destinations {
            if !destinations.contains(dest) {
                destinations.push(dest.clone());
            }
        }

        // Kyno provides the base metadata; PM's stationery overrides it.
        let mut iptc_template = kyno.iptc_template.clone();
        let mut pm_t = pm.iptc_template.clone();
        // Keyword order: PM first, then Kyno.
        let mut keywords = pm_t.keywords.clone();
        for kw in &kyno.iptc_template.keywords {
            if !keywords.contains(kw) {
                keywords.push(kw.clone());
            }
        }
        pm_t.keywords.clear();
        iptc_template.overlay(&pm_t);
        iptc_template.keywords = keywords;

        UnifiedIngestProfile {
            profile_name: format!("{} + {} (Unified)", pm.profile_name, kyno.profile_name),
            source_origin: "AethrCraft_Hybrid_PM_Kyno".to_string(),
            destinations,
            folder_naming_pattern: pm.folder_naming_pattern,
            file_naming_pattern: pm.file_naming_pattern,
            sequence_start: pm.sequence_start,
            sequence_padding: pm.sequence_padding,
            verify_checksum: kyno.verify_checksum,
            generate_asc_mhl: kyno.generate_asc_mhl,
            preserve_camera_card_structure: kyno.preserve_camera_card_structure,
            apply_3d_lut_path: kyno.apply_3d_lut_path,
            extract_video_subclips: kyno.extract_video_subclips,
            extract_embedded_raw_preview: true,
            code_replacement_delimiter: pm.code_replacement_delimiter,
            code_replacements: pm.code_replacements,
            iptc_template,
            ..kyno
        }
    }

    /// Loads any supported settings file into a profile (code replacement
    /// files and snapshots become a default profile carrying just that data).
    pub fn import_file(path: &Path) -> Result<UnifiedIngestProfile> {
        let kind = SettingsKind::detect(path).ok_or_else(|| {
            AethrCraftError::SchemaError(format!("unknown settings file type: {}", path.display()))
        })?;
        Ok(match kind {
            SettingsKind::PhotoMechanicPmx => {
                Self::from_photo_mechanic(&std::fs::read_to_string(path)?, None)?
            }
            SettingsKind::PhotoMechanicSnapshot => UnifiedIngestProfile {
                iptc_template: Self::from_photo_mechanic_snapshot(&std::fs::read_to_string(path)?)?,
                ..Default::default()
            },
            SettingsKind::CodeReplacements => UnifiedIngestProfile {
                code_replacements: Self::parse_pm_code_replacements(path)?,
                ..Default::default()
            },
            SettingsKind::KynoPreset => Self::from_kyno_preset(&std::fs::read_to_string(path)?)?,
            SettingsKind::NativeProfile => UnifiedIngestProfile::load(path)?,
        })
    }

    /// Combines any mix of settings sources with the documented precedence.
    pub fn combine(src: &ImportSources) -> Result<UnifiedIngestProfile> {
        let mut profile = match &src.native {
            Some(p) => UnifiedIngestProfile::load(p)?,
            None => UnifiedIngestProfile::default(),
        };

        let kyno = match &src.kyno {
            Some(p) => Some(Self::from_kyno_preset(&std::fs::read_to_string(p)?)?),
            None => None,
        };
        let pm = match &src.pmx {
            Some(p) => Some(Self::from_photo_mechanic(
                &std::fs::read_to_string(p)?,
                None,
            )?),
            None => None,
        };

        profile = match (pm, kyno) {
            (Some(pm), Some(kyno)) => Self::merge_pm_and_kyno(pm, kyno),
            (Some(mut pm), None) => {
                // Keep native verification settings when only PM is supplied.
                if src.native.is_some() {
                    pm.verify_checksum = profile.verify_checksum;
                    pm.generate_asc_mhl = profile.generate_asc_mhl;
                    pm.preserve_camera_card_structure = profile.preserve_camera_card_structure;
                    pm.apply_3d_lut_path = profile.apply_3d_lut_path.clone();
                    pm.code_replacements = profile.code_replacements.clone();
                    let mut t = profile.iptc_template.clone();
                    t.overlay(&pm.iptc_template);
                    pm.iptc_template = t;
                }
                pm
            }
            (None, Some(mut kyno)) => {
                if src.native.is_some() {
                    kyno.code_replacements = profile.code_replacements.clone();
                    let mut t = profile.iptc_template.clone();
                    t.overlay(&kyno.iptc_template);
                    kyno.iptc_template = t;
                }
                kyno
            }
            (None, None) => profile,
        };

        if let Some(snap) = &src.snapshot {
            let t = Self::from_photo_mechanic_snapshot(&std::fs::read_to_string(snap)?)?;
            profile.iptc_template.overlay(&t);
        }
        if let Some(codes) = &src.codes {
            for (k, v) in Self::parse_pm_code_replacements(codes)? {
                profile.code_replacements.insert(k, v);
            }
        }
        Ok(profile)
    }

    /// Translates Photo Mechanic `{variable}` tokens into unified tokens.
    /// Unknown variables (including already-unified ones) pass through.
    pub fn normalize_pm_tokens(input: &str) -> String {
        const MAP: &[(&str, &str)] = &[
            ("datesort", "{date:YYYYMMDD}"),
            ("year4", "{date:YYYY}"),
            ("year2", "{date:YY}"),
            ("month0", "{date:MM}"),
            ("day0", "{date:DD}"),
            ("hour", "{date:HH}"),
            ("minute", "{date:mm}"),
            ("second", "{date:ss}"),
            ("seqn", "{seq:4}"),
            ("make", "{camera:make}"),
            ("model", "{camera:model}"),
            ("snum", "{camera:serial}"),
            ("iso", "{exif:iso}"),
            ("lens", "{exif:lens}"),
            ("shutter", "{exif:shutter}"),
            ("aperture", "{exif:aperture}"),
            ("focallength", "{exif:focal}"),
            ("filenamebase", "{file:stem}"),
            ("filename", "{file:name}"),
            ("fileext", "{file:ext}"),
        ];
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close) => {
                    let name = &after[..close];
                    match MAP.iter().find(|(k, _)| *k == name) {
                        Some((_, v)) => out.push_str(v),
                        None => {
                            out.push('{');
                            out.push_str(name);
                            out.push('}');
                        }
                    }
                    rest = &after[close + 1..];
                }
                None => {
                    out.push_str(&rest[open..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Translates Kyno `$variable` / `${variable}` syntax into unified tokens
    /// using longest-match lookup; unknown variables pass through.
    pub fn normalize_kyno_tokens(input: &str) -> String {
        const MAP: &[(&str, &str)] = &[
            ("clip.creationDate", "{date:YYYYMMDD}"),
            ("clip.creationTime", "{date:HHmmss}"),
            ("clip.year", "{date:YYYY}"),
            ("clip.month", "{date:MM}"),
            ("clip.day", "{date:DD}"),
            ("clip.cameraModel", "{camera:model}"),
            ("clip.reel", "{video:reel}"),
            ("clip.scene", "{video:scene}"),
            ("clip.take", "{video:take}"),
            ("clip.camera", "{video:angle}"),
            ("clip.name", "{file:stem}"),
            ("clip.extension", "{file:ext}"),
            ("index:4", "{seq:4}"),
            ("index", "{seq:4}"),
        ];
        let lookup = |name: &str| MAP.iter().find(|(k, _)| *k == name).map(|(_, v)| *v);

        let mut out = String::with_capacity(input.len());
        let mut i = 0;
        let bytes = input.as_bytes();
        while i < input.len() {
            if bytes[i] != b'$' {
                let next = input[i..].find('$').map_or(input.len(), |p| i + p);
                out.push_str(&input[i..next]);
                i = next;
                continue;
            }
            // ${name}
            if input[i + 1..].starts_with('{') {
                if let Some(close) = input[i + 2..].find('}') {
                    let name = &input[i + 2..i + 2 + close];
                    if let Some(v) = lookup(name) {
                        out.push_str(v);
                        i += 3 + close;
                        continue;
                    }
                }
                out.push('$');
                i += 1;
                continue;
            }
            // $name — longest known prefix of the identifier run.
            let ident_len = input[i + 1..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
                .unwrap_or(input.len() - i - 1);
            let ident = &input[i + 1..i + 1 + ident_len];
            let best = MAP
                .iter()
                .filter(|(k, _)| !k.contains(':') && ident.starts_with(k))
                .max_by_key(|(k, _)| k.len());
            match best {
                Some((k, v)) => {
                    out.push_str(v);
                    i += 1 + k.len();
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            }
        }
        out
    }
}
