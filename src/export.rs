//! Export final : taille, netteté de sortie, espace couleur avec profil ICC intégré, métadonnées EXIF.

use std::io::{Cursor, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::{ImageBuffer, ImageEncoder, Rgb};
use moxcms::{ColorProfile, Layout, TransformOptions};
use serde::{Deserialize, Serialize};

use crate::image_io::srgb_to_linear;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Space {
    Srgb,
    DisplayP3,
    AdobeRgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sharpen {
    None,
    Screen,
    Print,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub space: Space,
    /// Bord long en pixels ; 0 : taille d'origine (jamais agrandie).
    pub long_edge: u32,
    pub sharpen: Sharpen,
    /// Qualité JPEG 1..100.
    pub quality: u8,
    /// Recopier les EXIF de la photo (sans la position GPS si `gps` est faux).
    pub metadata: bool,
    pub gps: bool,
    /// Auteur et copyright écrits dans les EXIF de chaque export (vides : rien).
    pub author: String,
    pub copyright: String,
    pub watermark: crate::watermark::Watermark,
    /// PNG et TIFF en 16 bits par couche (sinon 8) : utile pour un RAW ou une retouche ailleurs,
    /// mais le fichier pèse le double.
    pub deep: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { space: Space::Srgb, long_edge: 0, sharpen: Sharpen::None, quality: 92, metadata: true, gps: true, author: String::new(), copyright: String::new(), watermark: Default::default(), deep: false }
    }
}

/// Profil ICC v2 de l'espace (matrice et courbes) : la version lue par tous les logiciels, y compris
/// ceux qui ignorent les profils v4 ou refusent un illuminant approché.
pub fn icc(space: Space) -> Vec<u8> {
    // Primaires (x, y) rouge, vert, bleu ; blanc D65 pour les trois.
    let (prim, name, gamma) = match space {
        Space::Srgb => ([[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]], "sRGB (Luminux IA)", None),
        Space::DisplayP3 => ([[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]], "Display P3 (Luminux IA)", None),
        Space::AdobeRgb => ([[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]], "Adobe RGB (1998) compatible (Luminux IA)", Some(563u16)),
    };
    let xyz = |x: f64, y: f64| [x / y, 1.0, (1.0 - x - y) / y];
    let wp = xyz(0.3127, 0.3290);
    let d50 = [0.9642, 1.0, 0.8249];
    let p: Vec<[f64; 3]> = prim.iter().map(|c| xyz(c[0], c[1])).collect();
    // Gains des primaires pour que R = G = B = 1 donne le blanc.
    let m = [[p[0][0], p[1][0], p[2][0]], [p[0][1], p[1][1], p[2][1]], [p[0][2], p[1][2], p[2][2]]];
    let gains = solve3(m, wp);
    // Adaptation Bradford D65 -> D50 (connexion des profils ICC).
    let b = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let mul = |a: &[[f64; 3]; 3], v: [f64; 3]| [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2]);
    let (ls, ld) = (mul(&b, wp), mul(&b, d50));
    let adapt = |v: [f64; 3]| {
        let c = mul(&b, v);
        solve3(b, [0, 1, 2].map(|i| c[i] * ld[i] / ls[i]))
    };
    let cols: Vec<[f64; 3]> = (0..3).map(|i| adapt(p[i].map(|v| v * gains[i]))).collect();

    let s15 = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
    let xyz_tag = |v: [f64; 3]| [&b"XYZ \0\0\0\0"[..], &s15(v[0]), &s15(v[1]), &s15(v[2])].concat();
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend((name.len() as u32 + 1).to_be_bytes());
    desc.extend(name.as_bytes());
    desc.extend([0u8; 1 + 8 + 3 + 67]);
    let mut cprt = b"text\0\0\0\0No copyright, use freely".to_vec();
    cprt.push(0);
    let mut trc = b"curv\0\0\0\0".to_vec();
    match gamma {
        Some(g) => {
            trc.extend(1u32.to_be_bytes());
            trc.extend(g.to_be_bytes());
        }
        None => {
            trc.extend(1024u32.to_be_bytes());
            for i in 0..1024 {
                trc.extend(((srgb_to_linear(i as f32 / 1023.0) as f64 * 65535.0).round() as u16).to_be_bytes());
            }
        }
    }
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc),
        (b"cprt", cprt),
        (b"wtpt", xyz_tag(d50)),
        (b"rXYZ", xyz_tag(cols[0])),
        (b"gXYZ", xyz_tag(cols[1])),
        (b"bXYZ", xyz_tag(cols[2])),
        (b"rTRC", trc),
    ];
    // Données des balises alignées sur 4 octets ; les trois courbes partagent la même.
    let table = 4 + 12 * (tags.len() + 2);
    let mut data = Vec::new();
    let mut entries = Vec::new();
    for (sig, t) in &tags {
        let off = 128 + table + data.len();
        entries.push((**sig, off as u32, t.len() as u32));
        data.extend(t);
        data.resize(data.len().div_ceil(4) * 4, 0);
    }
    let trc = entries[6];
    entries.push((*b"gTRC", trc.1, trc.2));
    entries.push((*b"bTRC", trc.1, trc.2));
    let size = 128 + table + data.len();
    let mut out = Vec::with_capacity(size);
    out.extend((size as u32).to_be_bytes());
    out.extend([0u8; 4]); // CMM
    out.extend(0x0210_0000u32.to_be_bytes());
    out.extend(b"mntrRGB XYZ ");
    out.extend([0u8; 12]); // date
    out.extend(b"acsp");
    out.extend([0u8; 4 + 4 + 4 + 4 + 8 + 4]); // plateforme, drapeaux, fabricant, modèle, attributs, intention
    out.extend([0x0000_F6D6u32, 0x0001_0000, 0x0000_D32D].iter().flat_map(|v| v.to_be_bytes()));
    out.extend([0u8; 4 + 16 + 28]);
    out.extend((entries.len() as u32).to_be_bytes());
    for (sig, off, len) in entries {
        out.extend(sig);
        out.extend(off.to_be_bytes());
        out.extend(len.to_be_bytes());
    }
    out.extend(data);
    out
}

/// Résout `m · x = v` (Cramer).
fn solve3(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let det = |a: [[f64; 3]; 3]| a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0]) + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let d = det(m);
    [0, 1, 2].map(|c| {
        let mut a = m;
        for r in 0..3 {
            a[r][c] = v[r];
        }
        det(a) / d
    })
}

fn profile(space: Space) -> Result<ColorProfile> {
    ColorProfile::new_from_slice(&icc(space)).map_err(|e| anyhow::anyhow!("profil ICC : {e:?}"))
}

fn linear_to_srgb(x: f32) -> f32 {
    if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// Réduit l'image pour que son bord long fasse `long_edge` (en lumière linéaire, Lanczos).
fn resize(w: u32, h: u32, rgb: Vec<f32>, long_edge: u32) -> (u32, u32, Vec<f32>) {
    if long_edge == 0 || w.max(h) <= long_edge {
        return (w, h, rgb);
    }
    let k = long_edge as f64 / w.max(h) as f64;
    let (nw, nh) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
    let lin: Vec<f32> = rgb.iter().map(|&v| srgb_to_linear(v.clamp(0.0, 1.0))).collect();
    let img = ImageBuffer::<Rgb<f32>, _>::from_raw(w, h, lin).unwrap();
    let out = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
    (nw, nh, out.into_raw().into_iter().map(|v| linear_to_srgb(v.clamp(0.0, 1.0))).collect())
}

/// Netteté de sortie (masque flou) : légère pour l'écran, plus marquée pour l'impression.
fn sharpen(w: u32, h: u32, rgb: &mut [f32], kind: Sharpen) {
    let (sigma, amount) = match kind {
        Sharpen::None => return,
        Sharpen::Screen => (0.6, 0.5),
        Sharpen::Print => (1.0, 1.0),
    };
    let img = ImageBuffer::<Rgb<f32>, _>::from_raw(w, h, rgb.to_vec()).unwrap();
    let blur = image::imageops::blur(&img, sigma);
    for (v, b) in rgb.iter_mut().zip(blur.into_raw()) {
        *v = (*v + amount * (*v - b)).clamp(0.0, 1.0);
    }
}

/// sRGB -> espace de sortie.
fn convert(rgb: &mut Vec<f32>, space: Space) -> Result<()> {
    if space == Space::Srgb {
        return Ok(());
    }
    let t = profile(Space::Srgb)?
        .create_transform_f32(Layout::Rgb, &profile(space)?, Layout::Rgb, TransformOptions::default())
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut out = vec![0.0f32; rgb.len()];
    t.transform(rgb, &mut out).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    *rgb = out;
    Ok(())
}

/// EXIF de la photo d'origine, adaptés à l'image exportée (taille, orientation, espace couleur), en TIFF brut.
/// `src` absent : seulement l'auteur et le copyright, s'ils sont remplis.
pub fn exif_for(src: Option<&Path>, w: u32, h: u32, space: Space, gps: bool, author: &str, copyright: &str) -> Option<Vec<u8>> {
    use exif::{Field, In, Tag, Value};
    let src = src.map(crate::library::source);
    let read = src.as_ref().and_then(|src| std::fs::File::open(src).ok()).and_then(|f| exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)).ok());
    // Champs qui décrivent le fichier d'origine (structure, aperçus) ou que l'export remplace.
    const SKIP: &[Tag] = &[
        Tag::ImageWidth,
        Tag::ImageLength,
        Tag::BitsPerSample,
        Tag::Compression,
        Tag::PhotometricInterpretation,
        Tag::SamplesPerPixel,
        Tag::RowsPerStrip,
        Tag::PlanarConfiguration,
        Tag::Orientation,
        Tag::MakerNote,
        Tag::PixelXDimension,
        Tag::PixelYDimension,
        Tag::ColorSpace,
        Tag::YCbCrPositioning,
        Tag::YCbCrSubSampling,
        Tag::YCbCrCoefficients,
        Tag::ComponentsConfiguration,
        Tag::CompressedBitsPerPixel,
        Tag::InteroperabilityIndex,
    ];
    let mut fields: Vec<Field> = match &read {
        Some(e) => e
            .fields()
            .filter(|f| f.ifd_num == In::PRIMARY && !SKIP.contains(&f.tag) && !matches!(f.value, Value::Unknown(..)))
            .filter(|f| f.tag.context() != exif::Context::Interop && (gps || f.tag.context() != exif::Context::Gps))
            .cloned()
            .collect(),
        None => src.as_deref().map(raw_fields).unwrap_or_default(),
    };
    let field = |tag, value| Field { tag, ifd_num: In::PRIMARY, value };
    for (tag, v) in [(Tag::Artist, author.trim()), (Tag::Copyright, copyright.trim())] {
        if !v.is_empty() {
            fields.retain(|f| f.tag != tag);
            fields.push(field(tag, Value::Ascii(vec![v.as_bytes().to_vec()])));
        }
    }
    if fields.is_empty() {
        return None;
    }
    fields.push(field(Tag::Orientation, Value::Short(vec![1])));
    fields.push(field(Tag::PixelXDimension, Value::Long(vec![w])));
    fields.push(field(Tag::PixelYDimension, Value::Long(vec![h])));
    // 1 = sRGB, 0xFFFF = autre (le profil ICC fait foi).
    fields.push(field(Tag::ColorSpace, Value::Short(vec![if space == Space::Srgb { 1 } else { 0xFFFF }])));
    fields.retain(|f| f.tag != Tag::Software);
    fields.push(field(Tag::Software, Value::Ascii(vec![format!("Luminux IA {}", env!("CARGO_PKG_VERSION")).into_bytes()])));
    let write = |fields: &[Field]| -> Option<Vec<u8>> {
        let mut w = exif::experimental::Writer::new();
        for f in fields {
            w.push_field(f);
        }
        let mut out = Cursor::new(Vec::new());
        w.write(&mut out, true).ok()?;
        Some(out.into_inner())
    };
    write(&fields).or_else(|| {
        // Un champ exotique refusé : on garde l'essentiel.
        const KEEP: &[Tag] = &[
            Tag::Make,
            Tag::Model,
            Tag::DateTime,
            Tag::DateTimeOriginal,
            Tag::DateTimeDigitized,
            Tag::ExposureTime,
            Tag::FNumber,
            Tag::PhotographicSensitivity,
            Tag::FocalLength,
            Tag::FocalLengthIn35mmFilm,
            Tag::LensModel,
            Tag::LensMake,
            Tag::ExposureBiasValue,
            Tag::Flash,
            Tag::Artist,
            Tag::Copyright,
            Tag::Orientation,
            Tag::PixelXDimension,
            Tag::PixelYDimension,
            Tag::ColorSpace,
            Tag::Software,
        ];
        let base: Vec<Field> = fields.iter().filter(|f| KEEP.contains(&f.tag)).cloned().collect();
        write(&base)
    })
}

/// EXIF essentiels d'un RAW que kamadak-exif ne lit pas (CR3, RAF…), d'après rawler.
fn raw_fields(src: &Path) -> Vec<exif::Field> {
    use exif::{Field, In, Rational, SRational, Tag, Value};
    if !crate::library::is_raw(src) {
        return Vec::new();
    }
    let meta = (|| {
        let s = rawler::rawsource::RawSource::new(src).ok()?;
        let dec = rawler::get_decoder(&s).ok()?;
        dec.raw_metadata(&s, &Default::default()).ok()
    })();
    let Some(m) = meta else { return Vec::new() };
    let mut f = Vec::new();
    let mut push = |tag, value| f.push(Field { tag, ifd_num: In::PRIMARY, value });
    let ascii = |s: &str| Value::Ascii(vec![s.as_bytes().to_vec()]);
    if !m.make.is_empty() {
        push(Tag::Make, ascii(&m.make));
    }
    if !m.model.is_empty() {
        push(Tag::Model, ascii(&m.model));
    }
    let e = &m.exif;
    if let Some(d) = &e.date_time_original {
        push(Tag::DateTimeOriginal, ascii(d));
    }
    if let Some(r) = e.exposure_time {
        push(Tag::ExposureTime, Value::Rational(vec![Rational { num: r.n, denom: r.d }]));
    }
    if let Some(r) = e.fnumber {
        push(Tag::FNumber, Value::Rational(vec![Rational { num: r.n, denom: r.d }]));
    }
    if let Some(r) = e.focal_length {
        push(Tag::FocalLength, Value::Rational(vec![Rational { num: r.n, denom: r.d }]));
    }
    if let Some(r) = e.exposure_bias {
        push(Tag::ExposureBiasValue, Value::SRational(vec![SRational { num: r.n, denom: r.d }]));
    }
    if let Some(iso) = e.iso_speed_ratings {
        push(Tag::PhotographicSensitivity, Value::Short(vec![iso]));
    }
    if let Some(l) = &e.lens_model {
        push(Tag::LensModel, ascii(l));
    }
    if let Some(l) = &e.lens_make {
        push(Tag::LensMake, ascii(l));
    }
    if let Some(a) = &e.artist {
        push(Tag::Artist, ascii(a));
    }
    if let Some(c) = &e.copyright {
        push(Tag::Copyright, ascii(c));
    }
    f
}

/// Écrit l'image développée (RGB encodé sRGB, 0..1) selon les options ; `src` : photo d'origine (EXIF).
/// Le format suit l'extension : JPEG, WebP et AVIF 8 bits, PNG et TIFF 8 ou 16 bits (`deep`).
pub fn write(out: &Path, src: Option<&Path>, width: u32, height: u32, rgba: &[[f32; 4]], opts: &Options) -> Result<()> {
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "avif") {
        bail!("extension « {ext} » non gérée (jpg, png, tif, webp, avif)");
    }
    // L'encodeur AVIF n'intègre pas de profil ICC : le fichier est lu comme du sRGB.
    let space = if ext == "avif" { Space::Srgb } else { opts.space };
    let rgb: Vec<f32> = rgba.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let (w, h, mut rgb) = resize(width, height, rgb, opts.long_edge);
    sharpen(w, h, &mut rgb, opts.sharpen);
    convert(&mut rgb, space).context("conversion de l'espace couleur")?;
    let icc = icc(space);
    crate::watermark::apply(&opts.watermark, w, h, &mut rgb);
    let exif = exif_for(if opts.metadata { src } else { None }, w, h, space, opts.gps, &opts.author, &opts.copyright);
    let file = std::io::BufWriter::new(std::fs::File::create(out).with_context(|| format!("création de {}", out.display()))?);
    match ext.as_str() {
        "jpg" | "jpeg" => {
            let data: Vec<u8> = rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(file, opts.quality.clamp(1, 100));
            let _ = enc.set_icc_profile(icc);
            if let Some(x) = exif {
                let _ = enc.set_exif_metadata(x);
            }
            enc.write_image(&data, w, h, image::ExtendedColorType::Rgb8)?;
        }
        "png" => {
            let (data, ty) = samples(&rgb, opts.deep);
            let mut enc = image::codecs::png::PngEncoder::new(file);
            let _ = enc.set_icc_profile(icc);
            if let Some(x) = exif {
                let _ = enc.set_exif_metadata(x);
            }
            enc.write_image(&data, w, h, ty)?;
        }
        "webp" => {
            let data: Vec<u8> = rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
            let vp8 = webp::Encoder::from_rgb(&data, w, h).encode(opts.quality.clamp(1, 100) as f32);
            let mut file = file;
            file.write_all(&webp_container(&vp8, w, h, &icc, exif.as_deref())?)?;
        }
        "avif" => {
            let data: Vec<u8> = rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
            let mut enc = image::codecs::avif::AvifEncoder::new_with_speed_quality(file, 6, opts.quality.clamp(1, 100));
            if let Some(x) = exif {
                let _ = enc.set_exif_metadata(x);
            }
            enc.write_image(&data, w, h, image::ExtendedColorType::Rgb8)?;
        }
        _ => {
            // TIFF : profil ICC intégré ; l'encodeur d'image ne sait pas écrire les EXIF.
            let (data, ty) = samples(&rgb, opts.deep);
            let mut enc = image::codecs::tiff::TiffEncoder::new(file);
            let _ = enc.set_icc_profile(icc);
            enc.write_image(&data, w, h, ty)?;
        }
    }
    Ok(())
}

/// Valeurs 0..1 en octets de 8 ou 16 bits par couche, avec le type d'image à écrire.
fn samples(rgb: &[f32], deep: bool) -> (Vec<u8>, image::ExtendedColorType) {
    if deep {
        (rgb.iter().flat_map(|v| ((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_ne_bytes()).collect(), image::ExtendedColorType::Rgb16)
    } else {
        (rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect(), image::ExtendedColorType::Rgb8)
    }
}

/// Fichier WebP étendu (VP8X) : profil ICC, image de libwebp, EXIF. libwebp seul n'écrit que l'image.
fn webp_container(encoded: &[u8], w: u32, h: u32, icc: &[u8], exif: Option<&[u8]>) -> Result<Vec<u8>> {
    if encoded.len() < 12 || &encoded[0..4] != b"RIFF" || &encoded[8..12] != b"WEBP" {
        bail!("sortie WebP inattendue");
    }
    let image = &encoded[12..];
    let chunk = |out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
    };
    let mut body = b"WEBP".to_vec();
    let mut vp8x = vec![0x20 | if exif.is_some() { 0x08 } else { 0 }, 0, 0, 0];
    vp8x.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
    chunk(&mut body, b"VP8X", &vp8x);
    chunk(&mut body, b"ICCP", icc);
    body.extend_from_slice(image);
    if let Some(x) = exif {
        chunk(&mut body, b"EXIF", x);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Vec<[f32; 4]> {
        (0..w * h).map(|i| [(i % w) as f32 / w as f32, (i / w) as f32 / h as f32, 0.5, 1.0]).collect()
    }

    #[test]
    fn webp_keeps_icc_and_avif_is_written() {
        let dir = std::env::temp_dir().join(format!("luminux_webp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let opts = Options { space: Space::DisplayP3, ..Default::default() };
        let out = dir.join("t.webp");
        write(&out, None, 64, 32, &gradient(64, 32), &opts).unwrap();
        let mut dec = image::ImageReader::open(&out).unwrap().into_decoder().unwrap();
        use image::ImageDecoder;
        assert_eq!(dec.dimensions(), (64, 32));
        assert_eq!(dec.icc_profile().unwrap().expect("profil ICC"), icc(Space::DisplayP3));
        let img = image::open(&out).unwrap().to_rgb8();
        assert!(img.get_pixel(63, 0)[0] > 200 && img.get_pixel(0, 0)[0] < 40);
        let out = dir.join("t.avif");
        write(&out, None, 64, 32, &gradient(64, 32), &opts).unwrap();
        let data = std::fs::read(&out).unwrap();
        assert_eq!(&data[4..12], b"ftypavif");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resizes_long_edge_and_embeds_icc() {
        let dir = std::env::temp_dir().join(format!("luminux_export_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.jpg");
        let opts = Options { space: Space::DisplayP3, long_edge: 100, sharpen: Sharpen::Screen, ..Default::default() };
        write(&out, None, 400, 200, &gradient(400, 200), &opts).unwrap();
        let mut dec = image::ImageReader::open(&out).unwrap().into_decoder().unwrap();
        use image::ImageDecoder;
        assert_eq!(dec.dimensions(), (100, 50));
        let icc = dec.icc_profile().unwrap().expect("profil ICC intégré");
        assert_eq!(icc, super::icc(Space::DisplayP3));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn icc_v2_profiles_are_valid_and_match_the_standard_spaces() {
        let reference = [(Space::Srgb, ColorProfile::new_srgb()), (Space::DisplayP3, ColorProfile::new_display_p3()), (Space::AdobeRgb, ColorProfile::new_adobe_rgb())];
        for (space, std_p) in reference {
            let icc = icc(space);
            assert_eq!(u32::from_be_bytes(icc[..4].try_into().unwrap()) as usize, icc.len());
            assert_eq!(&icc[36..40], b"acsp");
            // Illuminant D50 exact, données des balises alignées sur 4 octets.
            assert_eq!(&icc[68..80], &[0, 0, 0xF6, 0xD6, 0, 1, 0, 0, 0, 0, 0xD3, 0x2D]);
            let n = u32::from_be_bytes(icc[128..132].try_into().unwrap()) as usize;
            for i in 0..n {
                let e = &icc[132 + 12 * i..144 + 12 * i];
                let (off, len) = (u32::from_be_bytes(e[4..8].try_into().unwrap()) as usize, u32::from_be_bytes(e[8..12].try_into().unwrap()) as usize);
                assert!(off % 4 == 0 && off + len <= icc.len());
            }
            // Même conversion que les profils de référence de moxcms.
            let mine = profile(space).unwrap();
            let src = ColorProfile::new_srgb();
            let a = src.create_transform_f32(Layout::Rgb, &mine, Layout::Rgb, TransformOptions::default()).unwrap();
            let b = src.create_transform_f32(Layout::Rgb, &std_p, Layout::Rgb, TransformOptions::default()).unwrap();
            let px = [0.9f32, 0.2, 0.1, 0.2, 0.7, 0.3, 0.5, 0.5, 0.5, 0.1, 0.2, 0.8];
            let (mut oa, mut ob) = (vec![0f32; 12], vec![0f32; 12]);
            a.transform(&px, &mut oa).unwrap();
            b.transform(&px, &mut ob).unwrap();
            for (x, y) in oa.iter().zip(&ob) {
                assert!((x - y).abs() < 0.004, "{space:?} {oa:?} {ob:?}");
            }
        }
    }

    #[test]
    fn png_16_bit_keeps_colors() {
        let dir = std::env::temp_dir().join(format!("luminux_png_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.png");
        let px = vec![[0.2f32, 0.5, 0.8, 1.0]; 4];
        write(&out, None, 2, 2, &px, &Options { deep: true, ..Default::default() }).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!(img.color(), image::ColorType::Rgb16);
        assert_eq!(img.to_rgb16().get_pixel(1, 1).0, [13107, 32768, 52428]);
        // Par défaut : 8 bits, fichier deux fois plus léger.
        write(&out, None, 2, 2, &px, &Options::default()).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!(img.color(), image::ColorType::Rgb8);
        assert_eq!(img.to_rgb8().get_pixel(1, 1).0, [51, 128, 204]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exif_copied_with_new_size() {
        let dir = std::env::temp_dir().join(format!("luminux_exif_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Source : JPEG avec EXIF (marque, modèle, orientation 6).
        let src = dir.join("src.jpg");
        let fields = [
            exif::Field { tag: exif::Tag::Make, ifd_num: exif::In::PRIMARY, value: exif::Value::Ascii(vec![b"Canon".to_vec()]) },
            exif::Field { tag: exif::Tag::Model, ifd_num: exif::In::PRIMARY, value: exif::Value::Ascii(vec![b"Canon EOS 600D".to_vec()]) },
            exif::Field { tag: exif::Tag::Orientation, ifd_num: exif::In::PRIMARY, value: exif::Value::Short(vec![6]) },
            exif::Field { tag: exif::Tag::FNumber, ifd_num: exif::In::PRIMARY, value: exif::Value::Rational(vec![(56, 10).into()]) },
        ];
        let mut wr = exif::experimental::Writer::new();
        for f in &fields {
            wr.push_field(f);
        }
        let mut blob = Cursor::new(Vec::new());
        wr.write(&mut blob, false).unwrap();
        let mut enc = image::codecs::jpeg::JpegEncoder::new(std::fs::File::create(&src).unwrap());
        enc.set_exif_metadata(blob.into_inner()).unwrap();
        enc.write_image(&[128u8; 8 * 8 * 3], 8, 8, image::ExtendedColorType::Rgb8).unwrap();

        let out = dir.join("out.jpg");
        write(&out, Some(&src), 64, 32, &gradient(64, 32), &Options::default()).unwrap();
        let e = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(std::fs::File::open(&out).unwrap())).unwrap();
        let get = |t| e.get_field(t, exif::In::PRIMARY).map(|f| f.display_value().to_string());
        assert_eq!(get(exif::Tag::Model).as_deref(), Some("\"Canon EOS 600D\""));
        assert_eq!(e.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)), Some(1));
        assert_eq!(e.get_field(exif::Tag::PixelXDimension, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)), Some(64));
        assert!(get(exif::Tag::FNumber).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
