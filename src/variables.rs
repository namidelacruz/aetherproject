//! Unified `{namespace:arg}` token engine and `\code\` replacement scanner.
//!
//! Evaluation order (fixed, to avoid ambiguity when features overlap):
//! 1. Code replacements are expanded on the raw template. Replacement text may
//!    itself contain `{tokens}`, which is how PM stationery commonly works.
//! 2. The result is parsed into literal and token segments; each token is
//!    resolved exactly once. Token *values* are never re-scanned, so metadata
//!    that happens to contain `\` or `{` cannot trigger further expansion.
//! 3. In path mode every token value is sanitized, so camera names such as
//!    `EOS R5/R6` can never create extra directories or escape the destination.

use crate::profile::UnifiedIngestProfile;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct AssetContext {
    pub original_filename: String,
    pub capture_timestamp: DateTime<Utc>,
    pub camera_make: String,
    pub camera_model: String,
    pub camera_serial: String,
    /// 0 = unknown.
    pub iso: u32,
    pub lens: String,
    pub shutter: String,
    pub aperture: String,
    pub focal_length: String,
    pub video_reel: String,
    pub video_scene: String,
    pub video_take: String,
    pub camera_angle: String,
    pub sequence_number: u64,
    pub card_name: String,
    pub ingest_timestamp: DateTime<Utc>,
    pub extra: HashMap<String, String>,
}

/// One parsed piece of a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment<'a> {
    Literal(&'a str),
    Token {
        raw: &'a str,
        namespace: &'a str,
        arg: Option<&'a str>,
    },
}

/// Parses a template into segments. `{` without a matching `}` or containing
/// whitespace/nesting is treated as literal text.
pub fn parse_template(template: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut lit_start = 0;
    let mut i = 0;
    let b = template.as_bytes();
    while i < b.len() {
        if b[i] == b'{' {
            if let Some(rel) = template[i + 1..].find(['}', '{']) {
                let close = i + 1 + rel;
                let inner = &template[i + 1..close];
                if b[close] == b'}' && !inner.is_empty() && !inner.chars().any(char::is_whitespace)
                {
                    if lit_start < i {
                        out.push(Segment::Literal(&template[lit_start..i]));
                    }
                    let (namespace, arg) = match inner.split_once(':') {
                        Some((n, a)) => (n, Some(a)),
                        None => (inner, None),
                    };
                    out.push(Segment::Token {
                        raw: &template[i..=close],
                        namespace,
                        arg,
                    });
                    i = close + 1;
                    lit_start = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    if lit_start < template.len() {
        out.push(Segment::Literal(&template[lit_start..]));
    }
    out
}

/// Converts `YYYY MM DD HH mm ss YY` patterns into a chrono format string.
fn date_pattern_to_chrono(pattern: &str) -> String {
    const MAP: &[(&str, &str)] = &[
        ("YYYY", "%Y"),
        ("YY", "%y"),
        ("MM", "%m"),
        ("DD", "%d"),
        ("HH", "%H"),
        ("mm", "%M"),
        ("ss", "%S"),
    ];
    let mut out = String::new();
    let mut rest = pattern;
    'outer: while !rest.is_empty() {
        for (k, v) in MAP {
            if let Some(r) = rest.strip_prefix(k) {
                out.push_str(v);
                rest = r;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap();
        if c == '%' {
            out.push_str("%%");
        } else {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Characters never allowed inside a single path component.
fn sanitize_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == ' ' || c == '.');
    if trimmed.is_empty() {
        String::new()
    } else {
        trimmed.to_string()
    }
}

pub struct VariableEngine;

impl VariableEngine {
    /// Expands codes and tokens for metadata fields (captions, credits...).
    pub fn evaluate(template: &str, ctx: &AssetContext, profile: &UnifiedIngestProfile) -> String {
        let coded = Self::expand_codes(template, profile);
        Self::render(&coded, ctx, profile, false)
    }

    /// Expands a folder pattern into a safe relative path using `/`.
    /// Empty and `..` components are removed.
    pub fn evaluate_path(
        template: &str,
        ctx: &AssetContext,
        profile: &UnifiedIngestProfile,
    ) -> String {
        let coded = Self::expand_codes(template, profile).replace('\\', "/");
        let rendered = Self::render(&coded, ctx, profile, true);
        rendered
            .split('/')
            .map(sanitize_component)
            .filter(|c| !c.is_empty() && c != "..")
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Expands a file-name pattern into a single safe file-name component.
    pub fn evaluate_file_name(
        template: &str,
        ctx: &AssetContext,
        profile: &UnifiedIngestProfile,
    ) -> String {
        let coded = Self::expand_codes(template, profile);
        sanitize_component(&Self::render(&coded, ctx, profile, true))
    }

    /// Returns tokens in `template` that the engine cannot resolve.
    pub fn unknown_tokens(template: &str) -> Vec<String> {
        let probe = AssetContext::default();
        let profile = UnifiedIngestProfile::default();
        parse_template(template)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Token {
                    raw,
                    namespace,
                    arg,
                } => Self::resolve(namespace, arg, &probe, &profile)
                    .is_none()
                    .then(|| raw.to_string()),
                _ => None,
            })
            .collect()
    }

    fn render(
        template: &str,
        ctx: &AssetContext,
        profile: &UnifiedIngestProfile,
        path: bool,
    ) -> String {
        let mut out = String::with_capacity(template.len() + 16);
        for seg in parse_template(template) {
            match seg {
                Segment::Literal(l) => out.push_str(l),
                Segment::Token {
                    raw,
                    namespace,
                    arg,
                } => match Self::resolve(namespace, arg, ctx, profile) {
                    Some(v) if path => out.push_str(&sanitize_component(&v)),
                    Some(v) => out.push_str(&v),
                    // Unknown tokens are left intact (visible to the user);
                    // in path mode they are sanitized like any other text.
                    None => out.push_str(raw),
                },
            }
        }
        out
    }

    /// Resolves a single token. `None` means the token is unknown; a known
    /// token with missing data resolves to an empty string.
    pub fn resolve(
        namespace: &str,
        arg: Option<&str>,
        ctx: &AssetContext,
        profile: &UnifiedIngestProfile,
    ) -> Option<String> {
        let file = Path::new(&ctx.original_filename);
        let s = |v: &str| Some(v.to_string());
        match (namespace, arg) {
            ("date", Some(fmt)) => Some(
                ctx.capture_timestamp
                    .format(&date_pattern_to_chrono(fmt))
                    .to_string(),
            ),
            ("date", None) => Some(ctx.capture_timestamp.format("%Y%m%d").to_string()),
            ("ingest", Some(fmt)) => Some(
                ctx.ingest_timestamp
                    .format(&date_pattern_to_chrono(fmt))
                    .to_string(),
            ),
            ("ingest", None) => Some(ctx.ingest_timestamp.format("%Y%m%d").to_string()),
            ("seq", arg) => {
                let width = match arg {
                    Some(a) => a.parse::<usize>().ok()?.min(12),
                    None => profile.sequence_padding,
                };
                Some(format!("{:0width$}", ctx.sequence_number, width = width))
            }
            ("camera", Some("make")) => s(&ctx.camera_make),
            ("camera", Some("model")) => s(&ctx.camera_model),
            ("camera", Some("serial")) => s(&ctx.camera_serial),
            ("exif", Some("iso")) => Some(if ctx.iso == 0 {
                String::new()
            } else {
                ctx.iso.to_string()
            }),
            ("exif", Some("lens")) => s(&ctx.lens),
            ("exif", Some("shutter")) => s(&ctx.shutter),
            ("exif", Some("aperture")) => s(&ctx.aperture),
            ("exif", Some("focal")) => s(&ctx.focal_length),
            ("video", Some("reel")) => s(&ctx.video_reel),
            ("video", Some("scene")) => s(&ctx.video_scene),
            ("video", Some("take")) => s(&ctx.video_take),
            ("video", Some("angle")) => s(&ctx.camera_angle),
            ("file", Some("stem")) => Some(
                file.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("clip")
                    .to_string(),
            ),
            ("file", Some("ext")) => Some(
                file.extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            ("file", Some("name")) => s(&ctx.original_filename),
            ("card", Some("name")) => s(&ctx.card_name),
            ("meta", Some(field)) => {
                let t = &profile.iptc_template;
                let v = match field {
                    "creator" => t.creator.as_deref(),
                    "credit" => t.credit_line.as_deref(),
                    "copyright" => t.copyright_notice.as_deref(),
                    "headline" => t.headline.as_deref(),
                    "city" => t.city.as_deref(),
                    "state" => t.province_state.as_deref(),
                    "country" => t.country.as_deref(),
                    _ => return None,
                };
                Some(v.unwrap_or("").to_string())
            }
            ("extra", Some(key)) => Some(ctx.extra.get(key).cloned().unwrap_or_default()),
            _ => None,
        }
    }

    /// Expands `\code\` and `\code#N\` replacements (N is 1-based; out of
    /// range falls back to column 1). Keys must be non-empty and contain no
    /// whitespace. Unknown codes are kept verbatim and scanning resumes at
    /// their closing delimiter, so `C:\Users\p14\` still expands `\p14\`.
    pub fn expand_codes(text: &str, profile: &UnifiedIngestProfile) -> String {
        let delim = profile.code_replacement_delimiter;
        if profile.code_replacements.is_empty() || !text.contains(delim) {
            return text.to_string();
        }
        let dl = delim.len_utf8();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while let Some(rel) = text[i..].find(delim) {
            let open = i + rel;
            out.push_str(&text[i..open]);
            let body_start = open + dl;
            let Some(rel_close) = text[body_start..].find(delim) else {
                out.push_str(&text[open..]);
                return out;
            };
            let close = body_start + rel_close;
            let body = &text[body_start..close];
            let (key, col) = match body.split_once('#') {
                Some((k, n)) => (k, n.parse::<usize>().ok()),
                None => (body, Some(1)),
            };
            let valid = !key.is_empty() && !key.chars().any(char::is_whitespace) && col.is_some();
            match profile.code_replacements.get(key).filter(|_| valid) {
                Some(cols) if !cols.is_empty() => {
                    let idx = col.unwrap_or(1).saturating_sub(1);
                    out.push_str(cols.get(idx).unwrap_or(&cols[0]));
                    i = close + dl;
                }
                _ => {
                    // Not a code: emit the opening delimiter and the body,
                    // and let the closing delimiter start the next match.
                    out.push_str(&text[open..close]);
                    i = close;
                }
            }
        }
        out.push_str(&text[i..]);
        out
    }
}
