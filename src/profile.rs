//! Unified configuration schema: merges Photo Mechanic (IPTC, code
//! replacements, snapshots) with Kyno (checksums, MHL, video subclips, LUTs,
//! card-structure lock).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChecksumAlgorithm {
    None,
    /// Fast non-cryptographic hash, standard for camera offloads.
    XxHash64,
    /// SIMD-accelerated and cryptographic.
    Blake3,
    /// Archival / broadcast requirement.
    Sha256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedIngestProfile {
    pub profile_name: String,
    pub source_origin: String,

    // Multi-destination routing
    pub destinations: Vec<PathBuf>,
    pub folder_naming_pattern: String,
    pub file_naming_pattern: String,
    pub sequence_start: u64,
    pub sequence_padding: usize,

    // Verification & camera-structure features
    pub verify_checksum: ChecksumAlgorithm,
    pub generate_asc_mhl: bool,
    /// Protects BDMV / M4ROOT / RDC spanned clips.
    pub preserve_camera_card_structure: bool,
    pub apply_3d_lut_path: Option<PathBuf>,
    pub extract_video_subclips: bool,

    // Metadata & culling-speed features
    pub extract_embedded_raw_preview: bool,
    pub code_replacement_delimiter: char,
    pub code_replacements: HashMap<String, Vec<String>>,
    pub iptc_template: UnifiedMetadataTemplate,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
