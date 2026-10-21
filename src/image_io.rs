//! Chargement (RAW ou image classique) en RGBA linéaire f32, et export.

use anyhow::{Context, Result, anyhow, bail};
use image::metadata::Orientation;
use image::{DynamicImage, ImageBuffer, ImageDecoder, ImageReader, Rgb, Rgba};
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use std::path::Path;

/// Image en RGBA linéaire (primaires sRGB), une valeur f32 par canal.
pub struct LinearImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

impl LinearImage {
    pub fn to_half(&self) -> Vec<half::f16> {
        let src: &[f32] = bytemuck::cast_slice(&self.pixels);
        let mut out = vec![half::f16::ZERO; src.len()];
        par_chunks(src, &mut out, |s, d| {
            for (o, v) in d.iter_mut().zip(s) {
                *o = half::f16::from_f32(*v);
            }
        });
        out
    }
}

pub fn load(path: &Path) -> Result<LinearImage> {
    // Copie virtuelle : le fichier de la photo.
    let path = &crate::library::source(path);
    let img = match load_standard(path) {
        Ok(img) => img,
        Err(e_std) => load_raw(path).map_err(|e_raw| anyhow!("ni image classique ({e_std:#}) ni RAW ({e_raw:#})"))?,
    };
    let (width, height) = (img.width(), img.height());
    let buf = img.into_rgba32f();
    Ok(LinearImage { width, height, pixels: bytemuck::cast_slice(buf.as_raw()).to_vec() })
}

fn load_standard(path: &Path) -> Result<DynamicImage> {
    let mut dec = ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let orientation = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec)?;
    img.apply_orientation(orientation);
    // Les images classiques sont encodées en sRGB : on repasse en linéaire.
    let f = img.into_rgba32f();
    let (w, h) = f.dimensions();
    let src = f.into_raw();
    let mut lin = vec![0f32; src.len()];
    par_chunks(&src, &mut lin, |s, d| {
        for (i, (o, v)) in d.iter_mut().zip(s).enumerate() {
            // Les tranches font un multiple de 4 valeurs : i % 4 == 3 est l'alpha.
            *o = if i % 4 == 3 { *v } else { srgb_to_linear(*v) };
        }
    });
    Ok(DynamicImage::ImageRgba32F(ImageBuffer::from_raw(w, h, lin).unwrap()))
}

fn load_raw(path: &Path) -> Result<DynamicImage> {
    let raw = rawler::decode_file(path).context("décodage RAW")?;
    // Développement de base sans la courbe sRGB finale : on garde du linéaire en f32.
    let dev = RawDevelop::new_with(&[
        ProcessingStep::Rescale,
        ProcessingStep::Demosaic,
        ProcessingStep::FujiRotate,
        ProcessingStep::CropActiveArea,
        ProcessingStep::WhiteBalance,
        ProcessingStep::Calibrate,
        ProcessingStep::CropDefault,
    ]);
    let (w, h, data) = match dev.develop_intermediate(&raw).context("développement RAW")? {
        Intermediate::ThreeColor(px) => (px.dim().w, px.dim().h, px.flatten()),
        Intermediate::Monochrome(px) => (px.dim().w, px.dim().h, px.data.iter().flat_map(|v| [*v; 3]).collect()),
        Intermediate::FourColor(_) => bail!("capteur 4 couleurs non géré"),
    };
    let rgba: Vec<f32> = data.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 1.0]).collect();
    let buf = ImageBuffer::<Rgba<f32>, _>::from_raw(w as u32, h as u32, rgba).context("taille RAW incohérente")?;
    let mut img = DynamicImage::ImageRgba32F(buf);
    img.apply_orientation(raw_orientation(raw.orientation));
    Ok(img)
}

fn raw_orientation(o: rawler::Orientation) -> Orientation {
    use rawler::Orientation as R;
    match o {
        R::HorizontalFlip => Orientation::FlipHorizontal,
        R::Rotate180 => Orientation::Rotate180,
        R::VerticalFlip => Orientation::FlipVertical,
        R::Transpose => Orientation::Rotate90FlipH,
        R::Rotate90 => Orientation::Rotate90,
        R::Transverse => Orientation::Rotate270FlipH,
        R::Rotate270 => Orientation::Rotate270,
        R::Normal | R::Unknown => Orientation::NoTransforms,
    }
}

/// Applique `f` sur des tranches parallèles de `src` / `dst` (même longueur, découpage multiple de 4).
fn par_chunks<A: Sync, B: Send>(src: &[A], dst: &mut [B], f: impl Fn(&[A], &mut [B]) + Sync) {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let chunk = (src.len().div_ceil(n) + 3) / 4 * 4;
    if chunk == 0 {
        return;
    }
    std::thread::scope(|sc| {
        for (s, d) in src.chunks(chunk).zip(dst.chunks_mut(chunk)) {
            let f = &f;
            sc.spawn(move || f(s, d));
        }
    });
}

pub fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

/// Écrit une image déjà développée (RGB encodé sRGB, valeurs 0..1).
/// Le format suit l'extension : JPEG 8 bits, PNG et TIFF 16 bits.
/// PNG 16 bits avec transparence (détourage).
pub fn export_rgba(path: &Path, width: u32, height: u32, rgba: &[[f32; 4]]) -> Result<()> {
    let data: Vec<u16> = rgba.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)).collect();
    ImageBuffer::<image::Rgba<u16>, _>::from_raw(width, height, data).unwrap().save_with_format(path, image::ImageFormat::Png)?;
    Ok(())
}

pub fn export(path: &Path, width: u32, height: u32, rgba: &[[f32; 4]]) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => {
            let data: Vec<u8> = rgba.iter().flat_map(|p| [0, 1, 2].map(|k| (p[k].clamp(0.0, 1.0) * 255.0).round() as u8)).collect();
            let img = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, data).unwrap();
            let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92).encode_image(&img)?;
        }
        "png" | "tif" | "tiff" => {
            let data: Vec<u16> = rgba.iter().flat_map(|p| [0, 1, 2].map(|k| (p[k].clamp(0.0, 1.0) * 65535.0).round() as u16)).collect();
            ImageBuffer::<Rgb<u16>, _>::from_raw(width, height, data).unwrap().save(path)?;
        }
        _ => bail!("extension « {ext} » non gérée (jpg, png, tif)"),
    }
    Ok(())
}
