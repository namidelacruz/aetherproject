//! # AethrCraft
//!
//! Clean-room media offload, culling and metadata engine.
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`profile`] | Unified, versioned settings + conflict validation |
//! | [`importers`] | Photo Mechanic `.pmx`/`.snap`/`.tsv`, Kyno `.kynopreset` |
//! | [`variables`] | `{token}` engine and `\code\` replacements |
//! | [`metadata`] | Zero-decode EXIF/TIFF, CR3/MP4/MOV boxes, RAF, Sony XML |
//! | [`card`] | Camera-card detection and the virtual asset graph |
//! | [`hashing`] | Single-pass multi-algorithm hashing |
//! | [`ingest`] | Offload planning and execution |
//! | [`mhl`] | ASC-MHL v2 manifests, chain and verification |
//! | [`preview`] | Embedded JPEG extraction, preview rendering, frame export |
//! | [`lut`] | `.cube` 1D/3D LUTs |
//! | [`markers`] | Timecode, markers, subclips |
//! | [`xmp`] | XMP sidecars |
//! | [`catalog`] | Drill-down view, filters and culling |
//! | [`nle`] | FCPXML / ALE / CSV interchange |

pub mod card;
pub mod catalog;
pub mod error;
pub mod hashing;
pub mod importers;
pub mod ingest;
pub mod lut;
pub mod markers;
pub mod metadata;
pub mod mhl;
pub mod nle;
pub mod preview;
pub mod profile;
pub mod variables;
pub mod xmp;

pub use card::{Asset, AssetKind, CardFormat, CardScan};
pub use catalog::{Catalog, CatalogEntry, Filter};
pub use error::{AethrCraftError, Result};
pub use importers::{ImportSources, SettingsImporter};
pub use ingest::{
    IngestExecutor, IngestPlan, IngestPlanner, IngestReceipt, IngestReport, SinglePassIngestEngine,
};
pub use lut::CubeLut;
pub use markers::{FrameRate, Marker, MarkerSet};
pub use metadata::TechnicalMetadata;
pub use preview::{PreviewOptions, PreviewRenderer, ZeroDecodePreviewExtractor};
pub use profile::{
    ChecksumAlgorithm, CollisionPolicy, UnifiedIngestProfile, UnifiedMetadataTemplate,
};
pub use variables::{AssetContext, VariableEngine};
pub use xmp::XmpSidecar;
