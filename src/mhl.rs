//! ASC-MHL v2 manifests (`ascmhl/NNNN_<root>_<date>.mhl` + `ascmhl_chain.xml`).
//!
//! Each ingest writes a new *generation* in every destination root listing the
//! files that run produced. Verification takes the latest recorded hash of
//! every path across all generations, re-hashes the file, and (optionally)
//! records the result as another generation.

use crate::card::MHL_DIR;
use crate::error::{AethrCraftError, Result};
use crate::hashing;
use crate::profile::ChecksumAlgorithm;
use chrono::{DateTime, SecondsFormat, Utc};
use quick_xml::events::Event;
use serde::Serialize;
use sha2::{Digest, Sha512};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HashAction {
    Original,
    Verified,
    Failed,
}

impl HashAction {
    fn as_str(self) -> &'static str {
        match self {
            HashAction::Original => "original",
            HashAction::Verified => "verified",
            HashAction::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MhlEntry {
    /// Relative to the MHL root, with `/`.
    pub path: String,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub algorithm: ChecksumAlgorithm,
    pub digest: String,
    pub action: HashAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MhlProcess {
    Transfer,
    InPlace,
}

const C4_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// C4 ID (SMPTE ST 2114): `c4` + base58(SHA-512), left-padded to 90 chars.
pub fn c4_id(data: &[u8]) -> String {
    let mut num: Vec<u8> = Sha512::digest(data).to_vec();
    let mut digits = Vec::new();
    while num.iter().any(|&b| b != 0) {
        let mut rem = 0u32;
        for b in num.iter_mut() {
            let acc = (rem << 8) | *b as u32;
            *b = (acc / 58) as u8;
            rem = acc % 58;
        }
        digits.push(C4_ALPHABET[rem as usize]);
    }
    while digits.len() < 88 {
        digits.push(b'1');
    }
    digits.reverse();
    format!("c4{}", String::from_utf8(digits).unwrap())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

fn esc(s: &str) -> String {
    quick_xml::escape::escape(s).to_string()
}

/// Renders a manifest document.
pub fn render_manifest(
    entries: &[MhlEntry],
    process: MhlProcess,
    created: DateTime<Utc>,
) -> String {
    let date = created.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<hashlist version=\"2.0\" xmlns=\"urn:ASC:MHL:v2.0\">\n");
    out.push_str("  <creatorinfo>\n");
    out.push_str(&format!("    <creationdate>{date}</creationdate>\n"));
    out.push_str(&format!("    <hostname>{}</hostname>\n", esc(&hostname())));
    out.push_str(&format!(
        "    <tool version=\"{}\">AethrCraft</tool>\n",
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str("  </creatorinfo>\n");
    out.push_str("  <processinfo>\n");
    out.push_str(&format!(
        "    <process>{}</process>\n",
        match process {
            MhlProcess::Transfer => "transfer",
            MhlProcess::InPlace => "in-place",
        }
    ));
    out.push_str("    <ignore>\n");
    for p in [
        ".DS_Store",
        "ascmhl",
        "ascmhl/",
        crate::card::SIDECAR_DIR,
        "*.xmp",
    ] {
        out.push_str(&format!("      <pattern>{}</pattern>\n", esc(p)));
    }
    out.push_str("    </ignore>\n");
    out.push_str("  </processinfo>\n");
    out.push_str("  <hashes>\n");
    for e in entries {
        let name = e.algorithm.mhl_name().unwrap_or("xxh64");
        let mtime = e
            .modified
            .map(|m| {
                format!(
                    " lastmodificationdate=\"{}\"",
                    m.to_rfc3339_opts(SecondsFormat::Secs, true)
                )
            })
            .unwrap_or_default();
        out.push_str("    <hash>\n");
        out.push_str(&format!(
            "      <path size=\"{}\"{}>{}</path>\n",
            e.size,
            mtime,
            esc(&e.path)
        ));
        out.push_str(&format!(
            "      <{name} action=\"{}\" hashdate=\"{date}\">{}</{name}>\n",
            e.action.as_str(),
            esc(&e.digest)
        ));
        out.push_str("    </hash>\n");
    }
    out.push_str("  </hashes>\n</hashlist>\n");
    out
}

/// One generation listed in `ascmhl_chain.xml`.
#[derive(Debug, Clone)]
pub struct Generation {
    pub sequence: u32,
    pub file: String,
    pub c4: String,
}

pub fn read_chain(root: &Path) -> Result<Vec<Generation>> {
    let chain = root.join(MHL_DIR).join("ascmhl_chain.xml");
    let text = match std::fs::read_to_string(&chain) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut gens = Vec::new();
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut cur: Option<Generation> = None;
    let mut field = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == "hashlist" {
                    let seq = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"sequencenr")
                        .and_then(|a| a.unescape_value().ok()?.parse().ok())
                        .unwrap_or(0);
                    cur = Some(Generation {
                        sequence: seq,
                        file: String::new(),
                        c4: String::new(),
                    });
                }
                field = name;
            }
            Event::Text(t) => {
                if let Some(g) = cur.as_mut() {
                    let v = t
                        .unescape()
                        .map(|v| v.trim().to_string())
                        .unwrap_or_default();
                    match field.as_str() {
                        "path" => g.file = v,
                        "c4" => g.c4 = v,
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                if e.local_name().as_ref() == b"hashlist" {
                    if let Some(g) = cur.take() {
                        gens.push(g);
                    }
                }
                field.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    gens.sort_by_key(|g| g.sequence);
    Ok(gens)
}

fn write_chain(root: &Path, gens: &[Generation]) -> Result<()> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ascmhldirectory xmlns=\"urn:ASC:MHL:DIRECTORY:v2.0\">\n",
    );
    for g in gens {
        out.push_str(&format!(
            "  <hashlist sequencenr=\"{}\">\n    <path>{}</path>\n    <c4>{}</c4>\n  </hashlist>\n",
            g.sequence,
            esc(&g.file),
            g.c4
        ));
    }
    out.push_str("</ascmhldirectory>\n");
    std::fs::write(root.join(MHL_DIR).join("ascmhl_chain.xml"), out)?;
    Ok(())
}

/// Writes a new generation under `root/ascmhl` and appends it to the chain.
pub fn write_generation(root: &Path, entries: &[MhlEntry], process: MhlProcess) -> Result<PathBuf> {
    let dir = root.join(MHL_DIR);
    std::fs::create_dir_all(&dir)?;
    let mut gens = read_chain(root)?;
    let seq = gens.last().map_or(1, |g| g.sequence + 1);
    let now = Utc::now();
    let root_name = root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "root".into())
        .replace(
            |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_',
            "_",
        );
    let file = format!(
        "{seq:04}_{root_name}_{}.mhl",
        now.format("%Y-%m-%d_%H%M%SZ")
    );
    let body = render_manifest(entries, process, now);
    std::fs::write(dir.join(&file), &body)?;
    gens.push(Generation {
        sequence: seq,
        file: file.clone(),
        c4: c4_id(body.as_bytes()),
    });
    write_chain(root, &gens)?;
    Ok(dir.join(file))
}

/// Latest recorded `(algorithm, digest, size)` per path across all generations.
/// Fails if a manifest's C4 no longer matches the chain (tampering/corruption).
pub fn load_history(root: &Path) -> Result<BTreeMap<String, (ChecksumAlgorithm, String, u64)>> {
    let mut map = BTreeMap::new();
    for g in read_chain(root)? {
        let path = root.join(MHL_DIR).join(&g.file);
        let bytes = std::fs::read(&path)?;
        if !g.c4.is_empty() && c4_id(&bytes) != g.c4 {
            return Err(AethrCraftError::Mhl(format!(
                "{} does not match its chain C4 id",
                g.file
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut reader = quick_xml::Reader::from_str(&text);
        let mut cur_path: Option<(String, u64)> = None;
        let mut field = String::new();
        let mut action = String::new();
        loop {
            match reader.read_event()? {
                Event::Start(e) => {
                    field = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                    action.clear();
                    for a in e.attributes().flatten() {
                        let v = a
                            .unescape_value()
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        match a.key.as_ref() {
                            b"size" => cur_path = Some((String::new(), v.parse().unwrap_or(0))),
                            b"action" => action = v,
                            _ => {}
                        }
                    }
                }
                Event::Text(t) => {
                    let v = t
                        .unescape()
                        .map(|v| v.trim().to_string())
                        .unwrap_or_default();
                    if field == "path" {
                        if let Some(p) = cur_path.as_mut() {
                            p.0 = v;
                        }
                    } else if let Some(algo) = ChecksumAlgorithm::from_mhl_name(&field) {
                        if let Some((p, size)) = &cur_path {
                            if action != "failed" {
                                map.insert(p.clone(), (algo, v, *size));
                            }
                        }
                    }
                }
                Event::End(e) => {
                    if e.local_name().as_ref() == b"hash" {
                        cur_path = None;
                    }
                    field.clear();
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    Ok(map)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VerifyReport {
    pub verified: Vec<String>,
    pub failed: Vec<(String, String, String)>,
    pub missing: Vec<String>,
    pub generation: Option<PathBuf>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.failed.is_empty() && self.missing.is_empty()
    }
}

/// Re-hashes every file recorded in `root`'s MHL history.
pub async fn verify(root: &Path, record_generation: bool) -> Result<VerifyReport> {
    let history = load_history(root)?;
    if history.is_empty() {
        return Err(AethrCraftError::Mhl(format!(
            "no ASC-MHL history in {}",
            root.display()
        )));
    }
    let mut report = VerifyReport::default();
    let mut entries = Vec::new();
    for (rel, (algo, expected, _size)) in &history {
        let path = root.join(rel);
        if !path.exists() {
            report.missing.push(rel.clone());
            continue;
        }
        let actual = hashing::hash_file(&path, &[*algo])
            .await?
            .remove(algo)
            .unwrap_or_default();
        let md = std::fs::metadata(&path)?;
        let ok = &actual == expected;
        if ok {
            report.verified.push(rel.clone());
        } else {
            report
                .failed
                .push((rel.clone(), expected.clone(), actual.clone()));
        }
        entries.push(MhlEntry {
            path: rel.clone(),
            size: md.len(),
            modified: md.modified().ok().map(DateTime::<Utc>::from),
            algorithm: *algo,
            digest: actual,
            action: if ok {
                HashAction::Verified
            } else {
                HashAction::Failed
            },
        });
    }
    if record_generation && !entries.is_empty() {
        report.generation = Some(write_generation(root, &entries, MhlProcess::InPlace)?);
    }
    Ok(report)
}
