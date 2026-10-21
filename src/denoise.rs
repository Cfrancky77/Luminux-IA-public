//! Débruitage IA : la photo source passe dans un modèle de restauration de ComfyUI (facteur 1, SCUNet…),
//! par tuiles ; le résultat est gardé en cache et mêlé à la source avant les retouches et le développement.

use crate::adjust::AiDenoise;
use crate::retouch::{to_srgb, Canvas};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Résultat en cache pour cette photo (chemin et date) et ce modèle.
pub fn cache_file(photo: &Path, model: &str) -> Option<PathBuf> {
    let photo = &crate::library::source(photo);
    use std::hash::{Hash, Hasher};
    let mtime = std::fs::metadata(photo).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    std::fs::canonicalize(photo).unwrap_or_else(|_| photo.to_path_buf()).hash(&mut hs);
    mtime.hash(&mut hs);
    model.hash(&mut hs);
    Some(crate::library::data_dir().join("denoise").join(format!("{:016x}.png", hs.finish())))
}

pub fn load(f: &Path) -> Option<image::RgbImage> {
    image::open(f).ok().map(|i| i.to_rgb8())
}

/// Source en sRGB 8 bits, telle qu'envoyée au modèle.
fn source_srgb(img: &impl Canvas) -> image::RgbImage {
    let (w, h) = img.size();
    image::RgbImage::from_fn(w, h, |x, y| image::Rgb(img.get(x as i32, y as i32).map(|c| (to_srgb(c) * 255.0).round() as u8)))
}

/// Calcule le débruitage de `img` dans ComfyUI et l'enregistre dans `file`.
pub fn compute(img: &impl Canvas, model: &str, file: &Path, progress: impl Fn(usize, usize)) -> Result<()> {
    let set = crate::settings::Settings::load();
    let ollama = set.free_vram.then_some(set.ollama_url.as_str());
    let out = crate::comfy::upscale(&set.comfy_url, &source_srgb(img), model, 1.0, ollama, progress)?;
    std::fs::create_dir_all(file.parent().unwrap())?;
    out.save(file).context("enregistrement du débruitage")?;
    Ok(())
}

/// Mêle le résultat à la source. Seul l'écart entre le résultat et la source quantifiée sur 8 bits est
/// appliqué : la source garde sa précision (pas de paliers dans les ombres relevées) et les hautes
/// lumières au-delà de 1 restent intactes.
pub fn blend(img: &mut impl Canvas, den: &image::RgbImage, amount: f32) {
    let (w, h) = img.size();
    if den.dimensions() != (w, h) || amount <= 0.0 {
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let v = img.get(x as i32, y as i32);
            let d = den.get_pixel(x, y);
            let out = std::array::from_fn(|c| {
                if !(0.0..=1.0).contains(&v[c]) {
                    return v[c];
                }
                let s = to_srgb(v[c]);
                let q = (s * 255.0).round() / 255.0;
                crate::image_io::srgb_to_linear((s + amount * (d[c] as f32 / 255.0 - q)).clamp(0.0, 1.0))
            });
            img.set(x as i32, y as i32, out);
        }
    }
}

/// Applique le débruitage de l'ajustement ; sans cache, il est calculé si `compute`.
/// Renvoie false s'il manque (non calculé ou en échec).
pub fn apply(img: &mut impl Canvas, photo: Option<&Path>, d: &AiDenoise, compute_missing: bool) -> bool {
    if !d.active() {
        return true;
    }
    let Some(file) = photo.and_then(|p| cache_file(p, &d.model)) else { return false };
    if !file.exists() && compute_missing {
        if let Err(e) = compute(img, &d.model, &file, |_, _| {}) {
            eprintln!("Débruitage IA : {e:#}");
            return false;
        }
    }
    match load(&file) {
        Some(den) => {
            blend(img, &den, d.amount);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::retouch::{Canvas, F32Canvas};

    #[test]
    fn blend_moves_toward_result_and_keeps_highlights() {
        let mut px = vec![[0.2f32, 0.5, 1.5, 1.0]; 4];
        let den = image::RgbImage::from_pixel(2, 2, image::Rgb([0, 255, 0]));
        let mut c = F32Canvas { data: &mut px, w: 2, h: 2 };
        super::blend(&mut c, &den, 1.0);
        let v = c.get(0, 0);
        assert!(v[0] < 1e-2 && (v[1] - 1.0).abs() < 1e-2, "{v:?}");
        assert_eq!(v[2], 1.5);
        // Intensité nulle : rien ne change.
        let mut px2 = vec![[0.2f32, 0.5, 0.7, 1.0]; 4];
        let mut c2 = F32Canvas { data: &mut px2, w: 2, h: 2 };
        super::blend(&mut c2, &den, 0.0);
        assert_eq!(c2.get(1, 1), [0.2, 0.5, 0.7]);
    }
}
