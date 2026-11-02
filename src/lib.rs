//! # AethrCraft
//!
//! Clean-room media offload, culling & metadata engine.
//!
//! * [`importers`] — normalizes Photo Mechanic (`.pmx` / `.tsv`) and Kyno
//!   (`.kynopreset`) settings into one [`UnifiedIngestProfile`].
//! * [`variables`] — expands `{token}` variables and `\code\` replacements.
//! * [`ingest`] — single-pass read, in-flight hashing, multi-destination write
//!   and post-write verification.
//! * [`preview`] — zero-decode embedded JPEG preview extraction from RAW files.

pub mod error;
pub mod importers;
pub mod ingest;
pub mod preview;
pub mod profile;
pub mod variables;

pub use error::AethrCraftError;
pub use importers::SettingsImporter;
pub use ingest::{IngestReceipt, SinglePassIngestEngine};
pub use preview::ZeroDecodePreviewExtractor;
pub use profile::{ChecksumAlgorithm, UnifiedIngestProfile, UnifiedMetadataTemplate};
pub use variables::{AssetContext, VariableEngine};
