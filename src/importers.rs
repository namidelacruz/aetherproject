//! Legacy settings importers: Photo Mechanic (`.pmx` / `.tsv`) and Kyno
//! (`.kynopreset`).

use crate::error::AethrCraftError;
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

#[derive(Debug, Deserialize)]
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

pub struct SettingsImporter;

impl SettingsImporter {
    /// Imports a Photo Mechanic XML (`.pmx`) settings document plus an
    /// optional tab-delimited code replacement file.
    pub fn from_photo_mechanic(
        pmx_xml: &str,
        code_replacement_tsv: Option<&Path>,
    ) -> Result<UnifiedIngestProfile, AethrCraftError> {
        let pmx: PhotoMechanicPmx = quick_xml::de::from_str(pmx_xml)?;

        let mut destinations = Vec::new();
        let mut folder_pattern = String::from("{date:YYYY}-{date:MM}-{date:DD}");
        let mut file_pattern = String::from("{date:YYYYMMDD}_{seq:4}");
        let mut seq_start = 1;

        if let Some(ingest) = pmx.ingest {
            destinations.push(PathBuf::from(ingest.primary_destination));
            if let Some(sec) = ingest.secondary_destination {
                if !sec.trim().is_empty() {
                    destinations.push(PathBuf::from(sec));
                }
            }
            folder_pattern = Self::normalize_pm_tokens(&ingest.folder_name);
            file_pattern = Self::normalize_pm_tokens(&ingest.rename_string);
            seq_start = ingest.sequence_num;
        }

        let mut iptc_template = UnifiedMetadataTemplate::default();
        if let Some(stat) = pmx.stationery {
            let norm = |s: String| Self::normalize_pm_tokens(&s);
            iptc_template.creator = stat.photographer.map(norm);
            iptc_template.credit_line = stat.credit.map(norm);
            iptc_template.copyright_notice = stat.copyright.map(norm);
            iptc_template.caption_abstract = stat.caption.map(norm);
            iptc_template.headline = stat.headline.map(norm);
            iptc_template.city = stat.city;
            iptc_template.province_state = stat.state;
            iptc_template.country = stat.country;
            if let Some(kw) = stat.keywords {
                iptc_template.keywords = kw
                    .split(',')
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect();
            }
        }

        let code_replacements = match code_replacement_tsv {
            Some(tsv_path) => Self::parse_pm_code_replacements(tsv_path)?,
            None => HashMap::new(),
        };

        Ok(UnifiedIngestProfile {
            profile_name: "Imported Photo Mechanic Profile".to_string(),
            source_origin: "PhotoMechanic_PMX".to_string(),
            destinations,
            folder_naming_pattern: folder_pattern,
            file_naming_pattern: file_pattern,
            sequence_start: seq_start,
            sequence_padding: 4,
            // Upgrade PM workflows with automatic xxHash64 verification.
            verify_checksum: ChecksumAlgorithm::XxHash64,
            generate_asc_mhl: false,
            preserve_camera_card_structure: false,
            apply_3d_lut_path: None,
            extract_video_subclips: false,
            extract_embedded_raw_preview: true,
            code_replacement_delimiter: '\\',
            code_replacements,
            iptc_template,
        })
    }

    /// Parses tab-delimited code replacement files
    /// (`key \t replacement1 \t replacement2 ...`).
    pub fn parse_pm_code_replacements(
        path: &Path,
    ) -> Result<HashMap<String, Vec<String>>, AethrCraftError> {
        let reader = BufReader::new(File::open(path)?);
        let mut map = HashMap::new();

        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            let mut parts = trimmed.split('\t');
            if let Some(code) = parts.next() {
                let replacements: Vec<String> = parts.map(|s| s.trim().to_string()).collect();
                if !replacements.is_empty() {
                    map.insert(code.trim().to_string(), replacements);
                }
            }
        }
        Ok(map)
    }

    /// Imports a Kyno (`.kynopreset`) JSON configuration.
    pub fn from_kyno_preset(kyno_json: &str) -> Result<UnifiedIngestProfile, AethrCraftError> {
        let kyno: KynoPresetJson = serde_json::from_str(kyno_json)?;

        let mut destinations = vec![PathBuf::from(&kyno.target_path)];
        destinations.extend(
            kyno.backup_paths
                .unwrap_or_default()
                .into_iter()
                .map(PathBuf::from),
        );

        let verify_checksum = match kyno
            .verify_mode
            .as_deref()
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("XXHASH64") | Some("XXHASH") => ChecksumAlgorithm::XxHash64,
            Some("SHA256") | Some("SHA-256") => ChecksumAlgorithm::Sha256,
            Some("BLAKE3") => ChecksumAlgorithm::Blake3,
            Some("NONE") => ChecksumAlgorithm::None,
            _ => ChecksumAlgorithm::XxHash64,
        };

        let mut iptc_template = UnifiedMetadataTemplate::default();
        if let Some(meta) = kyno.metadata {
            let norm = |s: String| Self::normalize_kyno_tokens(&s);
            iptc_template.creator = meta.creator.map(norm);
            iptc_template.copyright_notice = meta.copyright.map(norm);
            iptc_template.caption_abstract = meta.description.map(norm);
            iptc_template.headline = meta.title.map(norm);
            iptc_template.reel_name = meta.reel.map(norm);
            iptc_template.scene = meta.scene.map(norm);
            iptc_template.shot_take = meta.take.map(norm);
            iptc_template.camera_angle = meta.camera.map(norm);
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
            sequence_start: 1,
            sequence_padding: 4,
            verify_checksum,
            generate_asc_mhl: kyno.write_mhl.unwrap_or(true),
            preserve_camera_card_structure: kyno.keep_card_structure.unwrap_or(true),
            apply_3d_lut_path: kyno.lut_file.map(PathBuf::from),
            extract_video_subclips: true,
            extract_embedded_raw_preview: true,
            code_replacement_delimiter: '\\',
            code_replacements: HashMap::new(),
            iptc_template,
        })
    }

    /// Merges a Photo Mechanic profile with a Kyno preset: PM's IPTC, naming
    /// and code replacements combined with Kyno's checksum, LUT and
    /// video-structure handling.
    pub fn merge_pm_and_kyno(
        pm: UnifiedIngestProfile,
        kyno: UnifiedIngestProfile,
    ) -> UnifiedIngestProfile {
        let pm_t = pm.iptc_template;
        let ky_t = kyno.iptc_template;

        let mut destinations = kyno.destinations;
        for dest in pm.destinations {
            if !destinations.contains(&dest) {
                destinations.push(dest);
            }
        }

        let mut keywords = pm_t.keywords;
        for kw in ky_t.keywords {
            if !keywords.contains(&kw) {
                keywords.push(kw);
            }
        }

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
            iptc_template: UnifiedMetadataTemplate {
                creator: pm_t.creator.or(ky_t.creator),
                credit_line: pm_t.credit_line,
                copyright_notice: pm_t.copyright_notice.or(ky_t.copyright_notice),
                caption_abstract: pm_t.caption_abstract.or(ky_t.caption_abstract),
                headline: pm_t.headline.or(ky_t.headline),
                city: pm_t.city,
                province_state: pm_t.province_state,
                country: pm_t.country,
                keywords,
                reel_name: ky_t.reel_name,
                scene: ky_t.scene,
                shot_take: ky_t.shot_take,
                camera_angle: ky_t.camera_angle,
            },
        }
    }

    /// Translates Photo Mechanic `{variable}` syntax into unified tokens.
    pub fn normalize_pm_tokens(input: &str) -> String {
        input
            .replace("{datesort}", "{date:YYYYMMDD}")
            .replace("{year4}", "{date:YYYY}")
            .replace("{month0}", "{date:MM}")
            .replace("{day0}", "{date:DD}")
            .replace("{seqn}", "{seq:4}")
            .replace("{model}", "{camera:model}")
            .replace("{snum}", "{camera:serial}")
            .replace("{iso}", "{exif:iso}")
            .replace("{lens}", "{exif:lens}")
            .replace("{filenamebase}", "{file:stem}")
    }

    /// Translates Kyno `$variable` / `${variable}` syntax into unified tokens.
    pub fn normalize_kyno_tokens(input: &str) -> String {
        input
            .replace("${index:4}", "{seq:4}")
            .replace("$clip.creationDate", "{date:YYYYMMDD}")
            .replace("${clip.year}", "{date:YYYY}")
            .replace("${clip.month}", "{date:MM}")
            .replace("${clip.day}", "{date:DD}")
            .replace("$index", "{seq:4}")
            .replace("$clip.cameraModel", "{camera:model}")
            .replace("$clip.reel", "{video:reel}")
            .replace("$clip.scene", "{video:scene}")
            .replace("$clip.take", "{video:take}")
            .replace("$clip.name", "{file:stem}")
    }
}
