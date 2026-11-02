use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AethrCraftError {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("XML parse error: {0}")]
    XmlParse(#[from] quick_xml::DeError),
    #[error("XML read error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
    #[error("TOML write error: {0}")]
    TomlWrite(#[from] toml::ser::Error),
    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("Checksum verification mismatch on {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("Invalid settings schema: {0}")]
    SchemaError(String),
    #[error("Invalid LUT: {0}")]
    Lut(String),
    #[error("Invalid timecode: {0}")]
    Timecode(String),
    #[error("MHL error: {0}")]
    Mhl(String),
    #[error("Ingest conflict: {0}")]
    Conflict(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("External tool failed: {0}")]
    External(String),
}

pub type Result<T> = std::result::Result<T, AethrCraftError>;
