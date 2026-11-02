//! XMP sidecar reading and writing.
//!
//! Standard properties are used wherever they exist (xmp:Rating, xmp:Label,
//! dc:*, photoshop:*, xmpDM:*), so Lightroom, Bridge and Photo Mechanic read
//! AethrCraft ratings/captions. AethrCraft-only data (take, tag flag, markers,
//! checksums) lives in the `aethr:` namespace. Unknown properties of an
//! existing sidecar are not preserved on rewrite; the merge rules in
//! [`XmpSidecar::merge_from`] decide which values survive.

use crate::error::Result;
use crate::markers::{FrameRate, Marker, MarkerSet};
use crate::profile::UnifiedMetadataTemplate;
use quick_xml::events::{BytesStart, Event};
use std::path::Path;

pub const AETHR_NS: &str = "https://aethrcraft.dev/ns/1.0/";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct XmpSidecar {
    /// -1 = reject, 0 = unrated, 1..=5 stars.
    pub rating: Option<i8>,
    pub label: Option<String>,
    pub tagged: bool,
    pub meta: UnifiedMetadataTemplate,
    pub preserved_file_name: Option<String>,
    pub checksum: Option<(String, String)>,
    pub markers: MarkerSet,
}

fn esc(s: &str) -> String {
    quick_xml::escape::escape(s).to_string()
}

impl XmpSidecar {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(t) => Ok(Some(Self::parse(&t))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        // Write-then-rename so a crash never leaves a half-written sidecar.
        let tmp = path.with_extension("xmp.tmp");
        std::fs::write(&tmp, self.to_xml())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Merge rule used when an asset has several metadata sources:
    /// per-asset values in `self` (culling/caption edits) win over `other`
    /// (template or older sidecar); keywords are unioned; rating/label/markers
    /// from `other` only fill gaps.
    pub fn merge_from(&mut self, other: &XmpSidecar) {
        let mut base = other.meta.clone();
        let mut mine = self.meta.clone();
        let mut kws = mine.keywords.clone();
        for k in &base.keywords {
            if !kws.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                kws.push(k.clone());
            }
        }
        mine.keywords.clear();
        base.keywords.clear();
        base.overlay(&mine);
        base.keywords = kws;
        self.meta = base;
        if self.rating.is_none() {
            self.rating = other.rating;
        }
        if self.label.is_none() {
            self.label = other.label.clone();
        }
        self.tagged |= other.tagged;
        if self.preserved_file_name.is_none() {
            self.preserved_file_name = other.preserved_file_name.clone();
        }
        if self.checksum.is_none() {
            self.checksum = other.checksum.clone();
        }
        if self.markers.markers.is_empty() {
            self.markers = other.markers.clone();
        }
    }

    pub fn to_xml(&self) -> String {
        let m = &self.meta;
        let mut attrs = Vec::new();
        let mut push_attr = |k: &str, v: &Option<String>| {
            if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
                attrs.push(format!("{k}=\"{}\"", esc(v)));
            }
        };
        push_attr("xmp:Rating", &self.rating.map(|r| r.to_string()));
        push_attr("xmp:Label", &self.label);
        push_attr("photoshop:Credit", &m.credit_line);
        push_attr("photoshop:Headline", &m.headline);
        push_attr("photoshop:City", &m.city);
        push_attr("photoshop:State", &m.province_state);
        push_attr("photoshop:Country", &m.country);
        push_attr("xmpDM:tapeName", &m.reel_name);
        push_attr("xmpDM:scene", &m.scene);
        push_attr("xmpDM:cameraAngle", &m.camera_angle);
        push_attr("aethr:Take", &m.shot_take);
        push_attr("xmpMM:PreservedFileName", &self.preserved_file_name);
        if self.tagged {
            attrs.push("aethr:Tagged=\"True\"".into());
        }
        if let Some((algo, hex)) = &self.checksum {
            attrs.push(format!("aethr:ChecksumType=\"{}\"", esc(algo)));
            attrs.push(format!("aethr:Checksum=\"{}\"", esc(hex)));
        }
        if !self.markers.markers.is_empty() {
            attrs.push(format!(
                "aethr:FrameRate=\"{}/{}\"",
                self.markers.rate.num, self.markers.rate.den
            ));
        }

        let mut body = String::new();
        let alt = |name: &str, v: &Option<String>| {
            match v.as_ref().filter(|v| !v.is_empty()) {
            Some(v) => format!(
                "   <{name}>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </{name}>\n",
                esc(v)
            ),
            None => String::new(),
        }
        };
        if let Some(c) = m.creator.as_ref().filter(|v| !v.is_empty()) {
            body.push_str(&format!(
                "   <dc:creator>\n    <rdf:Seq>\n     <rdf:li>{}</rdf:li>\n    </rdf:Seq>\n   </dc:creator>\n",
                esc(c)
            ));
        }
        body.push_str(&alt("dc:rights", &m.copyright_notice));
        body.push_str(&alt("dc:description", &m.caption_abstract));
        if !m.keywords.is_empty() {
            body.push_str("   <dc:subject>\n    <rdf:Bag>\n");
            for k in &m.keywords {
                body.push_str(&format!("     <rdf:li>{}</rdf:li>\n", esc(k)));
            }
            body.push_str("    </rdf:Bag>\n   </dc:subject>\n");
        }
        if !self.markers.markers.is_empty() {
            body.push_str("   <aethr:Markers>\n    <rdf:Seq>\n");
            for mk in &self.markers.markers {
                body.push_str(&format!(
                    "     <rdf:li rdf:parseType=\"Resource\" aethr:name=\"{}\" aethr:note=\"{}\" aethr:start=\"{}\" aethr:duration=\"{}\" aethr:color=\"{}\" aethr:subclip=\"{}\"/>\n",
                    esc(&mk.name),
                    esc(&mk.note),
                    mk.start,
                    mk.duration,
                    esc(mk.color.as_deref().unwrap_or("")),
                    mk.subclip
                ));
            }
            body.push_str("    </rdf:Seq>\n   </aethr:Markers>\n");
        }

        format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="AethrCraft {ver}">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    xmlns:xmpDM="http://ns.adobe.com/xmp/1.0/DynamicMedia/"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmlns:aethr="{ns}"
    {attrs}>
{body}  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#,
            ver = env!("CARGO_PKG_VERSION"),
            ns = AETHR_NS,
            attrs = attrs.join("\n    "),
            body = body
        )
    }

    /// Lenient parser: accepts attribute and element forms of every property
    /// written by [`to_xml`](Self::to_xml), plus typical Lightroom/Bridge output.
    pub fn parse(text: &str) -> Self {
        let mut x = XmpSidecar::default();
        let mut reader = quick_xml::Reader::from_str(text);
        // Stack of open property element names (qualified).
        let mut stack: Vec<String> = Vec::new();
        let mut text_buf = String::new();
        let mut rate: Option<FrameRate> = None;

        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    x.read_attrs(&e, &mut rate);
                    stack.push(name);
                    text_buf.clear();
                }
                Ok(Event::Empty(e)) => {
                    x.read_attrs(&e, &mut rate);
                }
                Ok(Event::Text(t)) => {
                    if let Ok(s) = t.unescape() {
                        text_buf.push_str(&s);
                    }
                }
                Ok(Event::End(_)) => {
                    let closed = stack.pop().unwrap_or_default();
                    let value = text_buf.trim().to_string();
                    text_buf.clear();
                    if value.is_empty() {
                        continue;
                    }
                    // Property = nearest enclosing element that is not RDF.
                    let prop = if closed.starts_with("rdf:") {
                        stack
                            .iter()
                            .rev()
                            .find(|n| !n.starts_with("rdf:"))
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        closed
                    };
                    x.set_property(&prop, value);
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        if let Some(r) = rate {
            x.markers.rate = r;
        }
        x
    }

    fn read_attrs(&mut self, e: &BytesStart, rate: &mut Option<FrameRate>) {
        let name = e.name();
        let is_marker = name.as_ref() == b"rdf:li"
            && e.attributes()
                .flatten()
                .any(|a| a.key.as_ref() == b"aethr:start");
        if is_marker {
            let mut mk = Marker {
                name: String::new(),
                note: String::new(),
                start: 0,
                duration: 0,
                color: None,
                subclip: false,
            };
            for a in e.attributes().flatten() {
                let v = a
                    .unescape_value()
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                match a.key.as_ref() {
                    b"aethr:name" => mk.name = v,
                    b"aethr:note" => mk.note = v,
                    b"aethr:start" => mk.start = v.parse().unwrap_or(0),
                    b"aethr:duration" => mk.duration = v.parse().unwrap_or(0),
                    b"aethr:color" => mk.color = (!v.is_empty()).then_some(v),
                    b"aethr:subclip" => mk.subclip = v == "true",
                    _ => {}
                }
            }
            self.markers.markers.push(mk);
            return;
        }
        for a in e.attributes().flatten() {
            let key = String::from_utf8_lossy(a.key.as_ref()).to_string();
            let v = a
                .unescape_value()
                .map(|v| v.to_string())
                .unwrap_or_default();
            if key == "aethr:FrameRate" {
                *rate = FrameRate::parse(&v).ok();
                continue;
            }
            if !v.is_empty() {
                self.set_property(&key, v);
            }
        }
    }

    fn set_property(&mut self, key: &str, v: String) {
        let m = &mut self.meta;
        match key {
            "xmp:Rating" => {
                self.rating = v
                    .parse::<f32>()
                    .ok()
                    .map(|r| r.round().clamp(-1.0, 5.0) as i8)
            }
            "xmp:Label" => self.label = Some(v),
            "aethr:Tagged" | "photomechanic:Tagged" => self.tagged = v.eq_ignore_ascii_case("true"),
            "photoshop:Credit" => m.credit_line = Some(v),
            "photoshop:Headline" => m.headline = Some(v),
            "photoshop:City" => m.city = Some(v),
            "photoshop:State" => m.province_state = Some(v),
            "photoshop:Country" => m.country = Some(v),
            "xmpDM:tapeName" => m.reel_name = Some(v),
            "xmpDM:scene" => m.scene = Some(v),
            "xmpDM:cameraAngle" => m.camera_angle = Some(v),
            "aethr:Take" | "xmpDM:takeNumber" => m.shot_take = Some(v),
            "xmpMM:PreservedFileName" => self.preserved_file_name = Some(v),
            "aethr:ChecksumType" => {
                let hex = self.checksum.take().map(|c| c.1).unwrap_or_default();
                self.checksum = Some((v, hex));
            }
            "aethr:Checksum" => {
                let algo = self.checksum.take().map(|c| c.0).unwrap_or_default();
                self.checksum = Some((algo, v));
            }
            "dc:creator" => m.creator = Some(v),
            "dc:rights" => m.copyright_notice = Some(v),
            "dc:description" => m.caption_abstract = Some(v),
            "dc:subject" if !m.keywords.iter().any(|k| k == &v) => m.keywords.push(v),
            _ => {}
        }
    }
}
