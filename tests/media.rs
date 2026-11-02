mod common;
use aethrcraft::markers::{format_timecode, parse_timecode};
use aethrcraft::metadata;
use aethrcraft::preview::{jpeg_extent, jpeg_is_displayable};
use aethrcraft::*;
use common::*;

#[test]
fn exif_from_tiff_and_jpeg() {
    let tiff = tiff_exif(
        "Nikon",
        "Z 9",
        1600,
        "2026:11:02 18:30:05",
        "NIKKOR Z 70-200mm",
    );
    let m = metadata::parse_bytes(&tiff);
    assert_eq!(m.make.as_deref(), Some("Nikon"));
    assert_eq!(m.model.as_deref(), Some("Z 9"));
    assert_eq!(m.iso, Some(1600));
    assert_eq!(m.lens.as_deref(), Some("NIKKOR Z 70-200mm"));
    assert_eq!(m.capture_time.unwrap().to_string(), "2026-11-02 18:30:05");
    assert_eq!(m.aperture_string(), "f2.8");
    assert_eq!(m.shutter_string(), "1/1000");

    let jpg = jpeg_with_exif(&jpeg(64, 48, [10, 20, 30]), &tiff);
    let m = metadata::parse_bytes(&jpg);
    assert_eq!(m.model.as_deref(), Some("Z 9"));
    assert_eq!((m.width, m.height), (Some(64), Some(48)));
}

#[test]
fn sony_xml_sidecar() {
    let xml = r#"<?xml version="1.0"?>
<NonRealTimeMeta xmlns="urn:schemas-professionalDisc:nonRealTimeMeta:ver.2.20">
  <Duration value="250"/>
  <LtcChangeTable tcFps="25"><LtcChange frameCount="0" value="05301210" status="increment"/></LtcChangeTable>
  <CreationDate value="2026-11-02T18:30:05+01:00"/>
  <VideoFormat><VideoFrame videoCodec="AVC_3840_2160_HP@L51" captureFps="25p"/></VideoFormat>
  <Device manufacturer="Sony" modelName="ILME-FX6" serialNo="994812"/>
</NonRealTimeMeta>"#;
    let m = metadata::parse_sony_nrt_xml(xml);
    assert_eq!(m.model.as_deref(), Some("ILME-FX6"));
    assert_eq!(m.frame_rate, Some(25.0));
    assert_eq!(m.duration_seconds, Some(10.0));
    assert_eq!(m.start_timecode.as_deref(), Some("10:12:30:05"));
    assert_eq!(m.capture_time.unwrap().to_string(), "2026-11-02 18:30:05");
}

#[test]
fn mp4_mvhd_creation_time_and_duration() {
    // ftyp + moov(mvhd v0)
    let mut mvhd = vec![0u8; 100];
    let secs_1904_to_2026: u32 = 3_844_800_000; // ~2025-11
    mvhd[4..8].copy_from_slice(&secs_1904_to_2026.to_be_bytes());
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&12_500u32.to_be_bytes());
    let mut mvhd_box = ((mvhd.len() + 8) as u32).to_be_bytes().to_vec();
    mvhd_box.extend(b"mvhd");
    mvhd_box.extend(&mvhd);
    let mut moov = ((mvhd_box.len() + 8) as u32).to_be_bytes().to_vec();
    moov.extend(b"moov");
    moov.extend(&mvhd_box);
    let mut file = vec![0, 0, 0, 16];
    file.extend(b"ftypisom\0\0\0\0");
    file.extend(&moov);
    let m = metadata::parse_bytes(&file);
    assert_eq!(m.duration_seconds, Some(12.5));
    assert!(m.capture_time.is_some());
}

#[test]
fn marker_walk_skips_nested_thumbnail() {
    let thumb = jpeg(8, 8, [255, 0, 0]);
    let big = jpeg(320, 240, [0, 255, 0]);
    // Large JPEG whose APP1 contains a complete thumbnail JPEG.
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(&thumb);
    let mut nested = vec![0xFF, 0xD8, 0xFF, 0xE1];
    nested.extend(((app1.len() + 2) as u16).to_be_bytes());
    nested.extend(&app1);
    nested.extend(&big[2..]);
    assert_eq!(jpeg_extent(&nested, 0), Some(nested.len()));
    assert!(jpeg_is_displayable(&nested));

    let mut raw = vec![0u8; 100];
    raw.extend(&nested);
    raw.extend(vec![7u8; 50]);
    raw.extend([0xFF, 0xD8, 0xFF, 0x00]); // truncated trailing SOI
    let found = ZeroDecodePreviewExtractor::find_largest_jpeg_in(&raw).unwrap();
    assert_eq!(found, nested.as_slice());

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("IMG_1.CR3");
    write(&p, &raw);
    let decoded = PreviewRenderer::decode(&p).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (320, 240));
    let empty = dir.path().join("empty.NEF");
    write(&empty, b"");
    assert!(
        ZeroDecodePreviewExtractor::find_largest_embedded_jpeg(&empty)
            .unwrap()
            .is_none()
    );
}

#[test]
fn tiff_preview_offsets_are_used() {
    // TIFF RAW stand-in whose IFD1 points at an embedded JPEG.
    let tiff = tiff_exif("Sony", "ILCE-1", 100, "2026:11:02 10:00:00", "FE 50mm");
    let preview = jpeg(200, 100, [1, 2, 3]);
    let mut file = tiff.clone();
    // Patch IFD0's next-IFD pointer to a new IFD1 appended at the end.
    let n0 = u16::from_le_bytes([file[8], file[9]]) as usize;
    let next_ptr = 8 + 2 + n0 * 12;
    let ifd1_off = file.len();
    file[next_ptr..next_ptr + 4].copy_from_slice(&(ifd1_off as u32).to_le_bytes());
    let jpeg_off = ifd1_off + 2 + 2 * 12 + 4;
    file.extend(2u16.to_le_bytes());
    for (tag, val) in [(0x0201u16, jpeg_off as u32), (0x0202, preview.len() as u32)] {
        file.extend(tag.to_le_bytes());
        file.extend(4u16.to_le_bytes());
        file.extend(1u32.to_le_bytes());
        file.extend(val.to_le_bytes());
    }
    file.extend(0u32.to_le_bytes());
    file.extend(&preview);
    let m = metadata::parse_bytes(&file);
    assert_eq!(m.preview_ranges, vec![(jpeg_off, jpeg_off + preview.len())]);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("DSC1.ARW");
    write(&p, &file);
    assert_eq!(
        ZeroDecodePreviewExtractor::find_largest_embedded_jpeg(&p)
            .unwrap()
            .unwrap(),
        preview
    );
}

#[test]
fn lut_identity_invert_and_shaper() {
    let id = CubeLut::identity(17);
    let v = id.apply_rgb([0.25, 0.5, 0.9]);
    for (a, b) in v.iter().zip([0.25, 0.5, 0.9]) {
        assert!((a - b).abs() < 1e-5);
    }
    let invert =
        "TITLE \"invert\"\nLUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";
    let lut = CubeLut::parse(invert).unwrap();
    assert_eq!(lut.title.as_deref(), Some("invert"));
    let out = lut.apply_rgb([0.2, 0.3, 0.4]);
    assert!(
        (out[0] - 0.8).abs() < 1e-5 && (out[1] - 0.7).abs() < 1e-5 && (out[2] - 0.6).abs() < 1e-5
    );

    let mut img = image::RgbImage::from_pixel(4, 3, image::Rgb([0, 128, 255]));
    lut.apply_to_image(&mut img);
    assert_eq!(img.get_pixel(3, 2).0, [255, 127, 0]);

    let shaper = "LUT_1D_SIZE 2\nLUT_3D_SIZE 2\n0 0 0\n0.5 0.5 0.5\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    let s = CubeLut::parse(shaper).unwrap();
    let o = s.apply_rgb([1.0, 1.0, 1.0]);
    assert!((o[0] - 0.5).abs() < 1e-5);

    assert!(CubeLut::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err());
}

#[test]
fn timecode_round_trips_including_drop_frame() {
    let r25 = FrameRate::parse("25").unwrap();
    assert_eq!(parse_timecode("01:00:00:00", r25).unwrap().0, 90_000);
    assert_eq!(format_timecode(90_001, r25, false), "01:00:00:01");
    assert_eq!(parse_timecode("2.5s", r25).unwrap().0, 63);

    let df = FrameRate::parse("29.97").unwrap();
    assert_eq!(
        df,
        FrameRate {
            num: 30000,
            den: 1001
        }
    );
    for f in [0u64, 1799, 1800, 17982, 107_892, 123_456] {
        let tc = format_timecode(f, df, true);
        assert_eq!(parse_timecode(&tc, df).unwrap().0, f, "{tc}");
    }
    assert_eq!(format_timecode(1800, df, true), "00:01:00;02");
    assert!(parse_timecode("00:01:00;00", df).is_err());
    assert!(parse_timecode("00:00:00;00", r25).is_err());
}

#[test]
fn xmp_round_trip_and_merge_rules() {
    let mut x = XmpSidecar {
        rating: Some(4),
        label: Some("Red".into()),
        tagged: true,
        ..Default::default()
    };
    x.meta.caption_abstract = Some("Goal & <celebration>".into());
    x.meta.keywords = vec!["hockey".into(), "goal".into()];
    x.meta.reel_name = Some("A001".into());
    x.meta.shot_take = Some("03".into());
    x.markers.rate = FrameRate::parse("29.97").unwrap();
    x.markers
        .add(Marker {
            name: "Goal".into(),
            note: "top shelf".into(),
            start: 300,
            duration: 0,
            color: Some("red".into()),
            subclip: false,
        })
        .unwrap();
    x.markers
        .add(Marker {
            name: "Play".into(),
            note: String::new(),
            start: 100,
            duration: 200,
            color: None,
            subclip: true,
        })
        .unwrap();
    x.checksum = Some(("xxh64".into(), "abc".into()));
    let back = XmpSidecar::parse(&x.to_xml());
    assert_eq!(back, x);

    // Lightroom-style element form.
    let lr = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description xmp:Rating="2"><xmp:Label>Green</xmp:Label>
      <dc:subject><rdf:Bag><rdf:li>goal</rdf:li><rdf:li>arena</rdf:li></rdf:Bag></dc:subject>
      <dc:description><rdf:Alt><rdf:li xml:lang="x-default">Template caption</rdf:li></rdf:Alt></dc:description>
      </rdf:Description></rdf:RDF></x:xmpmeta>"#;
    let other = XmpSidecar::parse(lr);
    assert_eq!(other.rating, Some(2));
    assert_eq!(other.label.as_deref(), Some("Green"));
    let mut merged = x.clone();
    merged.merge_from(&other);
    // Own values win, keywords union.
    assert_eq!(merged.rating, Some(4));
    assert_eq!(
        merged.meta.caption_abstract.as_deref(),
        Some("Goal & <celebration>")
    );
    assert_eq!(merged.meta.keywords, vec!["hockey", "goal", "arena"]);
}
