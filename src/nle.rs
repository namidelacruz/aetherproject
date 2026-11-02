//! NLE interchange: FCPXML 1.10 (Final Cut Pro, DaVinci Resolve, Premiere via
//! import), Avid Log Exchange (ALE — Avid, Resolve, Premiere) and CSV.
//!
//! Export reads catalog sidecars; import writes back into sidecars, so
//! metadata round-trips: XMP <-> FCPXML / ALE.

use crate::catalog::{Catalog, CatalogEntry};
use crate::error::Result;
use crate::markers::{format_timecode, parse_timecode, FrameRate, Marker};
use quick_xml::events::Event;
use std::collections::HashMap;
use std::path::Path;

fn esc(s: &str) -> String {
    quick_xml::escape::escape(s).to_string()
}

fn file_url(p: &Path) -> String {
    let abs = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_to_path(url: &str) -> String {
    let s = url.strip_prefix("file://").unwrap_or(url);
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let p = String::from_utf8_lossy(&out).to_string();
    // file:///C:/x -> C:/x on Windows paths.
    if p.len() > 3 && p.as_bytes()[0] == b'/' && p.as_bytes()[2] == b':' {
        p[1..].to_string()
    } else {
        p
    }
}

/// Rational FCPXML time for a frame count.
fn fcp_time(frames: u64, rate: FrameRate) -> String {
    if frames == 0 {
        return "0s".into();
    }
    let (mut n, mut d) = (frames * rate.den as u64, rate.num as u64);
    let g = gcd(n, d);
    n /= g;
    d /= g;
    if d == 1 {
        format!("{n}s")
    } else {
        format!("{n}/{d}s")
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

fn parse_fcp_time(s: &str, rate: FrameRate) -> u64 {
    let s = s.trim_end_matches('s');
    let secs = match s.split_once('/') {
        Some((n, d)) => n.parse::<f64>().unwrap_or(0.0) / d.parse::<f64>().unwrap_or(1.0),
        None => s.parse::<f64>().unwrap_or(0.0),
    };
    rate.seconds_to_frames(secs)
}

fn entry_rate(e: &CatalogEntry, fallback: FrameRate) -> FrameRate {
    if !e.xmp.markers.markers.is_empty() {
        return e.xmp.markers.rate;
    }
    e.metadata
        .frame_rate
        .and_then(|f| FrameRate::from_f64(f).ok())
        .unwrap_or(fallback)
}

fn entry_duration_frames(e: &CatalogEntry, rate: FrameRate) -> u64 {
    let from_meta = e
        .metadata
        .duration_seconds
        .map(|s| rate.seconds_to_frames(s))
        .unwrap_or(0);
    let from_markers = e
        .xmp
        .markers
        .markers
        .iter()
        .map(Marker::end)
        .max()
        .unwrap_or(0);
    from_meta.max(from_markers).max(rate.timebase() as u64) // at least 1s
}

/// Exports media assets of a catalog as an FCPXML 1.10 event.
pub fn export_fcpxml(
    cat: &Catalog,
    entries: &[&CatalogEntry],
    event_name: &str,
    fallback: FrameRate,
) -> String {
    let mut formats: Vec<FrameRate> = Vec::new();
    let mut out = String::new();
    let mut resources = String::new();
    let mut clips = String::new();
    for (i, e) in entries.iter().enumerate() {
        let rate = entry_rate(e, fallback);
        if !formats.contains(&rate) {
            formats.push(rate);
        }
        let fid = formats.iter().position(|r| *r == rate).unwrap() + 1;
        let dur = entry_duration_frames(e, rate);
        let id = format!("r{}", 100 + i);
        let name = e.asset.display_name();
        let is_video = e.asset.kind == crate::card::AssetKind::Video;
        resources.push_str(&format!(
            "    <asset id=\"{id}\" name=\"{}\" start=\"0s\" duration=\"{}\" hasVideo=\"1\" hasAudio=\"{}\" format=\"f{fid}\">\n      <media-rep kind=\"original-media\" src=\"{}\"/>\n    </asset>\n",
            esc(&name),
            fcp_time(dur, rate),
            if is_video { 1 } else { 0 },
            esc(&file_url(&cat.scan.root.join(&e.asset.primary)))
        ));
        let m = &e.xmp.meta;
        let mut inner = String::new();
        if let Some(c) = &m.caption_abstract {
            inner.push_str(&format!("        <note>{}</note>\n", esc(c)));
        }
        for mk in e.xmp.markers.markers.iter().filter(|m| !m.subclip) {
            inner.push_str(&format!(
                "        <marker start=\"{}\" duration=\"{}\" value=\"{}\"{}/>\n",
                fcp_time(mk.start, rate),
                fcp_time(mk.duration.max(1), rate),
                esc(&mk.name),
                if mk.note.is_empty() {
                    String::new()
                } else {
                    format!(" note=\"{}\"", esc(&mk.note))
                }
            ));
        }
        if !m.keywords.is_empty() {
            inner.push_str(&format!(
                "        <keyword start=\"0s\" duration=\"{}\" value=\"{}\"/>\n",
                fcp_time(dur, rate),
                esc(&m.keywords.join(", "))
            ));
        }
        match e.xmp.rating {
            Some(r) if r >= 4 => inner.push_str(&format!(
                "        <rating start=\"0s\" duration=\"{}\" value=\"favorite\"/>\n",
                fcp_time(dur, rate)
            )),
            Some(-1) => inner.push_str(&format!(
                "        <rating start=\"0s\" duration=\"{}\" value=\"reject\"/>\n",
                fcp_time(dur, rate)
            )),
            _ => {}
        }
        let mut md = String::new();
        for (k, v) in [
            ("com.apple.proapps.studio.reel", &m.reel_name),
            ("com.apple.proapps.studio.scene", &m.scene),
            ("com.apple.proapps.studio.shot", &m.shot_take),
            ("com.apple.proapps.studio.angle", &m.camera_angle),
        ] {
            if let Some(v) = v {
                md.push_str(&format!(
                    "          <md key=\"{k}\" value=\"{}\"/>\n",
                    esc(v)
                ));
            }
        }
        if !md.is_empty() {
            inner.push_str(&format!("        <metadata>\n{md}        </metadata>\n"));
        }
        clips.push_str(&format!(
            "      <asset-clip ref=\"{id}\" name=\"{}\" start=\"0s\" duration=\"{}\" format=\"f{fid}\">\n{inner}      </asset-clip>\n",
            esc(&name),
            fcp_time(dur, rate)
        ));
        for sc in e.xmp.markers.subclips() {
            clips.push_str(&format!(
                "      <asset-clip ref=\"{id}\" name=\"{}\" start=\"{}\" duration=\"{}\" format=\"f{fid}\"/>\n",
                esc(&format!("{name} - {}", sc.name)),
                fcp_time(sc.start, rate),
                fcp_time(sc.duration, rate)
            ));
        }
    }
    let mut fmt = String::new();
    for (i, r) in formats.iter().enumerate() {
        fmt.push_str(&format!(
            "    <format id=\"f{}\" name=\"AethrCraft{}\" frameDuration=\"{}/{}s\"/>\n",
            i + 1,
            r.label().replace('.', "_"),
            r.den,
            r.num
        ));
    }
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.10\">\n  <resources>\n");
    out.push_str(&fmt);
    out.push_str(&resources);
    out.push_str("  </resources>\n  <library>\n");
    out.push_str(&format!("    <event name=\"{}\">\n", esc(event_name)));
    out.push_str(&clips);
    out.push_str("    </event>\n  </library>\n</fcpxml>\n");
    out
}

const ALE_COLUMNS: &[&str] = &[
    "Name",
    "Tape",
    "Start",
    "End",
    "Duration",
    "Source File",
    "Scene",
    "Take",
    "Camera",
    "Comments",
    "Keywords",
    "Rating",
    "Label",
];

fn ale_clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Exports an Avid Log Exchange file.
pub fn export_ale(cat: &Catalog, entries: &[&CatalogEntry], rate: FrameRate) -> String {
    let mut out = format!(
        "Heading\nFIELD_DELIM\tTABS\nVIDEO_FORMAT\t1080\nAUDIO_FORMAT\t48khz\nFPS\t{}\n\nColumn\n{}\n\nData\n",
        rate.label(),
        ALE_COLUMNS.join("\t")
    );
    for e in entries {
        let r = entry_rate(e, rate);
        let dur = entry_duration_frames(e, r);
        let start = e
            .metadata
            .start_timecode
            .as_deref()
            .and_then(|tc| parse_timecode(tc, r).ok())
            .map(|x| x.0)
            .unwrap_or(0);
        let m = &e.xmp.meta;
        let row = [
            e.asset.display_name(),
            m.reel_name.clone().unwrap_or_default(),
            format_timecode(start, r, false),
            format_timecode(start + dur, r, false),
            format_timecode(dur, r, false),
            cat.scan
                .root
                .join(&e.asset.primary)
                .to_string_lossy()
                .to_string(),
            m.scene.clone().unwrap_or_default(),
            m.shot_take.clone().unwrap_or_default(),
            m.camera_angle.clone().unwrap_or_default(),
            m.caption_abstract.clone().unwrap_or_default(),
            m.keywords.join(", "),
            e.xmp.rating.map(|r| r.to_string()).unwrap_or_default(),
            e.xmp.label.clone().unwrap_or_default(),
        ];
        out.push_str(
            &row.iter()
                .map(|c| ale_clean(c))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        out.push('\n');
    }
    out
}

/// One row of imported NLE metadata, keyed by clip name / source file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportedClip {
    pub name: String,
    pub source_file: Option<String>,
    pub fields: HashMap<String, String>,
    pub markers: Vec<Marker>,
    pub keywords: Vec<String>,
}

/// Parses an ALE file (any column order; unknown columns are kept).
pub fn parse_ale(text: &str) -> Vec<ImportedClip> {
    let mut section = "";
    let mut columns: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim_end_matches('\r');
        match l.trim() {
            "Heading" | "Column" | "Data" => {
                section = match l.trim() {
                    "Heading" => "h",
                    "Column" => "c",
                    _ => "d",
                };
                continue;
            }
            "" => continue,
            _ => {}
        }
        match section {
            "c" => columns = l.split('\t').map(|s| s.trim().to_string()).collect(),
            "d" => {
                let vals: Vec<&str> = l.split('\t').collect();
                let mut clip = ImportedClip::default();
                for (i, c) in columns.iter().enumerate() {
                    let v = vals.get(i).map(|s| s.trim()).unwrap_or("");
                    if v.is_empty() {
                        continue;
                    }
                    match c.as_str() {
                        "Name" => clip.name = v.to_string(),
                        "Source File" => clip.source_file = Some(v.to_string()),
                        "Keywords" => {
                            clip.keywords = v
                                .split(',')
                                .map(|k| k.trim().to_string())
                                .filter(|k| !k.is_empty())
                                .collect()
                        }
                        _ => {
                            clip.fields.insert(c.clone(), v.to_string());
                        }
                    }
                }
                if !clip.name.is_empty() || clip.source_file.is_some() {
                    out.push(clip);
                }
            }
            _ => {}
        }
    }
    out
}

/// Parses FCPXML asset-clips: names, source files, markers, keywords, notes
/// and ratings.
pub fn parse_fcpxml(text: &str, rate: FrameRate) -> Vec<ImportedClip> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut assets: HashMap<String, (String, String)> = HashMap::new(); // id -> (name, src)
    let mut current_asset: Option<String> = None;
    let mut clips: Vec<ImportedClip> = Vec::new();
    let mut in_clip = false;
    let mut in_note = false;
    loop {
        let ev = reader.read_event();
        let (e, is_empty) = match &ev {
            Ok(Event::Start(e)) => (Some(e.clone()), false),
            Ok(Event::Empty(e)) => (Some(e.clone()), true),
            _ => (None, false),
        };
        if let Some(e) = e {
            let attrs: HashMap<String, String> = e
                .attributes()
                .flatten()
                .map(|a| {
                    (
                        String::from_utf8_lossy(a.key.as_ref()).to_string(),
                        a.unescape_value()
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            let get = |k: &str| attrs.get(k).cloned().unwrap_or_default();
            match e.name().as_ref() {
                b"asset" => {
                    let id = get("id");
                    assets.insert(
                        id.clone(),
                        (get("name"), attrs.get("src").cloned().unwrap_or_default()),
                    );
                    current_asset = (!is_empty).then_some(id);
                }
                b"media-rep" => {
                    if let Some(id) = &current_asset {
                        if let Some(a) = assets.get_mut(id) {
                            a.1 = get("src");
                        }
                    }
                }
                b"asset-clip" => {
                    let (aname, src) = assets.get(&get("ref")).cloned().unwrap_or_default();
                    let start = parse_fcp_time(&get("start"), rate);
                    // Range clips (non-zero start) are subclips of the asset.
                    let name = if aname.is_empty() { get("name") } else { aname };
                    let mut clip = ImportedClip {
                        name: name.clone(),
                        source_file: (!src.is_empty()).then(|| url_to_path(&src)),
                        ..Default::default()
                    };
                    let clip_name = get("name");
                    if start > 0 || clip_name.starts_with(&format!("{name} - ")) {
                        let sub = clip_name
                            .strip_prefix(&format!("{name} - "))
                            .unwrap_or(&clip_name)
                            .to_string();
                        clip.markers.push(Marker {
                            name: sub,
                            note: String::new(),
                            start,
                            duration: parse_fcp_time(&get("duration"), rate),
                            color: None,
                            subclip: true,
                        });
                    }
                    clips.push(clip);
                    in_clip = !is_empty;
                }
                b"marker" | b"chapter-marker" if in_clip => {
                    if let Some(c) = clips.last_mut() {
                        c.markers.push(Marker {
                            name: get("value"),
                            note: get("note"),
                            start: parse_fcp_time(&get("start"), rate),
                            duration: {
                                let d = parse_fcp_time(&get("duration"), rate);
                                if d <= 1 {
                                    0
                                } else {
                                    d
                                }
                            },
                            color: None,
                            subclip: false,
                        });
                    }
                }
                b"keyword" if in_clip => {
                    if let Some(c) = clips.last_mut() {
                        c.keywords.extend(
                            get("value")
                                .split(',')
                                .map(|k| k.trim().to_string())
                                .filter(|k| !k.is_empty()),
                        );
                    }
                }
                b"rating" if in_clip => {
                    if let Some(c) = clips.last_mut() {
                        let v = match get("value").as_str() {
                            "favorite" => "4",
                            "reject" => "-1",
                            _ => "",
                        };
                        if !v.is_empty() {
                            c.fields.insert("Rating".into(), v.into());
                        }
                    }
                }
                b"md" if in_clip => {
                    if let Some(c) = clips.last_mut() {
                        let field = match get("key").as_str() {
                            "com.apple.proapps.studio.reel" => "Tape",
                            "com.apple.proapps.studio.scene" => "Scene",
                            "com.apple.proapps.studio.shot" => "Take",
                            "com.apple.proapps.studio.angle" => "Camera",
                            _ => "",
                        };
                        if !field.is_empty() {
                            c.fields.insert(field.into(), get("value"));
                        }
                    }
                }
                b"note" if in_clip => in_note = true,
                _ => {}
            }
            continue;
        }
        match ev {
            Ok(Event::Text(t)) if in_note => {
                if let (Some(c), Ok(v)) = (clips.last_mut(), t.unescape()) {
                    c.fields.insert("Comments".into(), v.trim().to_string());
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"asset" => current_asset = None,
                b"asset-clip" => in_clip = false,
                b"note" => in_note = false,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    // Merge subclip-only rows into their parent clip rows.
    let mut merged: Vec<ImportedClip> = Vec::new();
    for c in clips {
        match merged
            .iter_mut()
            .find(|m| m.name == c.name && m.source_file == c.source_file)
        {
            Some(m) => {
                m.markers.extend(c.markers);
                for k in c.keywords {
                    if !m.keywords.contains(&k) {
                        m.keywords.push(k);
                    }
                }
                m.fields.extend(c.fields);
            }
            None => merged.push(c),
        }
    }
    merged
}

/// Applies imported clips to catalog sidecars. Clips are matched by source
/// file path first, then by name (asset display name or primary file name).
/// Imported values overwrite the fields they carry; keywords are unioned;
/// markers with the same name and start are replaced. Returns updated count.
pub fn apply_import(cat: &mut Catalog, clips: &[ImportedClip], rate: FrameRate) -> Result<usize> {
    let root = cat.scan.root.clone();
    let mut updated = 0;
    for clip in clips {
        let pos = cat.entries.iter().position(|e| {
            let full = root.join(&e.asset.primary);
            clip.source_file.as_deref().is_some_and(|s| {
                Path::new(s) == full
                    || full.canonicalize().ok().is_some_and(|c| Path::new(s) == c)
                    || s.replace('\\', "/").ends_with(&e.asset.id)
            })
        });
        let pos = pos.or_else(|| {
            cat.entries.iter().position(|e| {
                e.asset.is_media()
                    && (e.asset.display_name() == clip.name
                        || e.asset
                            .primary
                            .file_stem()
                            .is_some_and(|s| s.to_string_lossy() == clip.name))
            })
        });
        let Some(pos) = pos else { continue };
        let e = &mut cat.entries[pos];
        let m = &mut e.xmp.meta;
        for (k, v) in &clip.fields {
            match k.as_str() {
                "Tape" => m.reel_name = Some(v.clone()),
                "Scene" => m.scene = Some(v.clone()),
                "Take" => m.shot_take = Some(v.clone()),
                "Camera" => m.camera_angle = Some(v.clone()),
                "Comments" => m.caption_abstract = Some(v.clone()),
                "Rating" => e.xmp.rating = v.parse().ok(),
                "Label" => e.xmp.label = Some(v.clone()),
                _ => {}
            }
        }
        for k in &clip.keywords {
            if !m.keywords.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                m.keywords.push(k.clone());
            }
        }
        if !clip.markers.is_empty() {
            if e.xmp.markers.markers.is_empty() {
                e.xmp.markers.rate = rate;
            }
            for mk in &clip.markers {
                e.xmp.markers.add(mk.clone())?;
            }
        }
        e.save()?;
        updated += 1;
    }
    Ok(updated)
}

/// Simple CSV export for spreadsheets / shot logs.
pub fn export_csv(entries: &[&CatalogEntry]) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::from("id,kind,camera,capture_time,rating,label,tagged,reel,scene,take,caption,keywords,markers\n");
    for e in entries {
        let m = &e.xmp.meta;
        let row = [
            q(&e.asset.id),
            q(&format!("{:?}", e.asset.kind)),
            q(&e.metadata.model.clone().unwrap_or_default()),
            q(&e.metadata
                .capture_time
                .map(|t| t.to_string())
                .unwrap_or_default()),
            q(&e.xmp.rating.map(|r| r.to_string()).unwrap_or_default()),
            q(e.xmp.label.as_deref().unwrap_or("")),
            q(if e.xmp.tagged { "yes" } else { "" }),
            q(m.reel_name.as_deref().unwrap_or("")),
            q(m.scene.as_deref().unwrap_or("")),
            q(m.shot_take.as_deref().unwrap_or("")),
            q(m.caption_abstract.as_deref().unwrap_or("")),
            q(&m.keywords.join("; ")),
            q(&e.xmp.markers.markers.len().to_string()),
        ];
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}
