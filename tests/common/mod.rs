#![allow(dead_code)]
use std::path::Path;

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Little-endian TIFF with IFD0 (Make, Model, ExifIFD) and an Exif IFD
/// (ISO, DateTimeOriginal, LensModel, FNumber, ExposureTime).
pub fn tiff_exif(make: &str, model: &str, iso: u16, datetime: &str, lens: &str) -> Vec<u8> {
    struct E {
        tag: u16,
        typ: u16,
        count: u32,
        data: Vec<u8>,
    }
    fn ascii(tag: u16, s: &str) -> E {
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        E {
            tag,
            typ: 2,
            count: d.len() as u32,
            data: d,
        }
    }
    fn build_ifd(entries: &mut [E], start: usize, out: &mut Vec<u8>) {
        entries.sort_by_key(|e| e.tag);
        let n = entries.len();
        let mut data_off = start + 2 + n * 12 + 4;
        let mut table: Vec<u8> = Vec::new();
        let mut blob: Vec<u8> = Vec::new();
        table.extend((n as u16).to_le_bytes());
        for e in entries.iter() {
            table.extend(e.tag.to_le_bytes());
            table.extend(e.typ.to_le_bytes());
            table.extend(e.count.to_le_bytes());
            if e.data.len() <= 4 {
                let mut v = e.data.clone();
                v.resize(4, 0);
                table.extend(v);
            } else {
                table.extend((data_off as u32).to_le_bytes());
                blob.extend(&e.data);
                data_off += e.data.len();
            }
        }
        table.extend(0u32.to_le_bytes());
        out.extend(table);
        out.extend(blob);
    }

    let mut exif = vec![
        E {
            tag: 0x8827,
            typ: 3,
            count: 1,
            data: iso.to_le_bytes().to_vec(),
        },
        ascii(0x9003, datetime),
        ascii(0xA434, lens),
        E {
            tag: 0x829D,
            typ: 5,
            count: 1,
            data: [28u32.to_le_bytes(), 10u32.to_le_bytes()].concat(),
        },
        E {
            tag: 0x829A,
            typ: 5,
            count: 1,
            data: [1u32.to_le_bytes(), 1000u32.to_le_bytes()].concat(),
        },
    ];
    // Lay out IFD0 first to know where the Exif IFD will start.
    let mut ifd0 = vec![
        ascii(0x010F, make),
        ascii(0x0110, model),
        E {
            tag: 0x8769,
            typ: 4,
            count: 1,
            data: vec![0; 4],
        },
    ];
    let mut probe = Vec::new();
    build_ifd(&mut ifd0, 8, &mut probe);
    let exif_off = 8 + probe.len();
    for e in ifd0.iter_mut() {
        if e.tag == 0x8769 {
            e.data = (exif_off as u32).to_le_bytes().to_vec();
        }
    }
    let mut out = b"II*\0".to_vec();
    out.extend(8u32.to_le_bytes());
    build_ifd(&mut ifd0, 8, &mut out);
    assert_eq!(out.len(), exif_off);
    build_ifd(&mut exif, exif_off, &mut out);
    out
}

/// A real decodable JPEG of the given size and colour.
pub fn jpeg(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb(rgb));
    aethrcraft::preview::encode_jpeg(&img, 90).unwrap()
}

/// Inserts an APP1 Exif segment (with the given TIFF) after the SOI.
pub fn jpeg_with_exif(jpeg: &[u8], tiff: &[u8]) -> Vec<u8> {
    let mut seg = b"Exif\0\0".to_vec();
    seg.extend(tiff);
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
    out.extend(((seg.len() + 2) as u16).to_be_bytes());
    out.extend(seg);
    out.extend(&jpeg[2..]);
    out
}
