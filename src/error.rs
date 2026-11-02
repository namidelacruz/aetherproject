use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AethrCraftError {
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("XML parse error (Photo Mechanic format): {0}")]
    XmlParse(#[from] quick_xml::DeError),
    #[error("JSON parse error (Kyno preset format): {0}")]
    JsonParse(#[from] serde_json::Error),
    #[error("Checksum verification mismatch on {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("Invalid settings schema: {0}")]
    SchemaError(String),
}
