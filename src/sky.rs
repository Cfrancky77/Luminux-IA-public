//! Ciels de remplacement : images rangées dans ~/.local/share/luminux/skies,
//! plus quelques ciels générés au premier lancement pour commencer.

use anyhow::{Context, Result};
use half::f16;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Côté maximal d'un ciel envoyé au GPU (taille de sa texture).
pub const SKY_MAX: u32 = 2048;
pub const SKY_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp"];

pub struct SkyImage {
    pub w: u32,
    pub h: u32,
    /// RGBA linéaire.
    pub half: Vec<f16>,
    /// Luminance linéaire moyenne et couleur moyenne (normalisée) de la partie haute.
    pub mean_luma: f32,
    pub avg: [f32; 3],
}

pub fn skies_dir() -> PathBuf {
    crate::library::data_dir().join("skies")
}

/// Chemin d'un ciel : nom de fichier dans le dossier des ciels, ou chemin absolu.
pub fn resolve(name: &str) -> PathBuf {
    let p = Path::new(name);
    if p.is_absolute() { p.to_path_buf() } else { skies_dir().join(p) }
}

pub fn is_sky_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| SKY_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Ciels disponibles (noms de fichiers), triés.
pub fn list() -> Vec<String> {
    ensure_builtin();
    let mut v: Vec<String> = std::fs::read_dir(skies_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_sky_file(p))
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
        .collect();
    v.sort_by_key(|s| s.to_lowercase());
    v
}

/// Copie une image dans le dossier des ciels ; renvoie son nom.
pub fn import(src: &Path) -> Result<String> {
    std::fs::create_dir_all(skies_dir())?;
    let name = src.file_name().and_then(|n| n.to_str()).context("nom de fichier invalide")?.to_string();
    image::image_dimensions(src).with_context(|| format!("{} n'est pas une image lisible", src.display()))?;
    std::fs::copy(src, skies_dir().join(&name))?;
    Ok(name)
}

pub fn load(name: &str) -> Option<Arc<SkyImage>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<SkyImage>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(s) = cache.lock().unwrap().get(name) {
        return s.clone();
    }
    ensure_builtin();
    let path = resolve(name);
    let sky = decode(&path).map_err(|e| eprintln!("ciel {} : {e:#}", path.display())).ok().map(Arc::new);
    cache.lock().unwrap().insert(name.to_string(), sky.clone());
    sky
}

fn decode(path: &Path) -> Result<SkyImage> {
    let mut img = image::open(path)?;
    if img.width().max(img.height()) > SKY_MAX {
        img = img.resize(SKY_MAX, SKY_MAX, image::imageops::FilterType::Triangle);
    }
    let rgb = img.to_rgb32f();
    let (w, h) = rgb.dimensions();
    let mut half = Vec::with_capacity((w * h * 4) as usize);
    let (mut sum, mut col, mut n) = (0f64, [0f64; 3], 0f64);
    for (y, row) in rgb.rows().enumerate() {
        for p in row {
            let l = p.0.map(crate::image_io::srgb_to_linear);
            half.extend([f16::from_f32(l[0]), f16::from_f32(l[1]), f16::from_f32(l[2]), f16::ONE]);
            if (y as u32) < h * 7 / 10 {
                let y = 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2];
                sum += y as f64;
                for c in 0..3 {
                    col[c] += l[c] as f64;
                }
                n += 1.0;
            }
        }
    }
    let mean_luma = (sum / n.max(1.0)) as f32;
    let avg = col.map(|c| (c / n.max(1.0)) as f32 / mean_luma.max(1e-4));
    Ok(SkyImage { w, h, half, mean_luma, avg })
}

// ---------- Ciels générés ----------

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263) ^ seed.wrapping_mul(2246822519);
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    ((h ^ (h >> 16)) & 0xffff) as f32 / 65535.0
}

fn noise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let a = hash(xi, yi, seed) + (hash(xi + 1, yi, seed) - hash(xi, yi, seed)) * sx;
    let b = hash(xi, yi + 1, seed) + (hash(xi + 1, yi + 1, seed) - hash(xi, yi + 1, seed)) * sx;
    a + (b - a) * sy
}

fn fbm(x: f32, y: f32, seed: u32) -> f32 {
    let (mut v, mut amp, mut f) = (0.0, 0.5, 1.0);
    for o in 0..6 {
        v += amp * noise(x * f, y * f, seed + o);
        amp *= 0.5;
        f *= 2.03;
    }
    v
}

struct Builtin {
    name: &'static str,
    zenith: [f32; 3],
    horizon: [f32; 3],
    cloud: [f32; 3],
    /// Couverture nuageuse 0..1, étirement horizontal des nuages.
    cover: f32,
    stretch: f32,
    /// Soleil (x, y) près de l'horizon, couleur.
    sun: Option<([f32; 2], [f32; 3])>,
}

const BUILTIN: &[Builtin] = &[
    Builtin { name: "Ciel bleu.png", zenith: [0.10, 0.28, 0.62], horizon: [0.62, 0.76, 0.90], cloud: [0.97, 0.97, 0.98], cover: 0.35, stretch: 2.0, sun: None },
    Builtin { name: "Coucher de soleil.png", zenith: [0.16, 0.18, 0.40], horizon: [1.0, 0.55, 0.25], cloud: [0.95, 0.45, 0.40], cover: 0.30, stretch: 4.0, sun: Some(([0.62, 0.95], [1.0, 0.75, 0.45])) },
    Builtin { name: "Heure bleue.png", zenith: [0.05, 0.08, 0.25], horizon: [0.45, 0.42, 0.62], cloud: [0.35, 0.33, 0.50], cover: 0.25, stretch: 5.0, sun: None },
    Builtin { name: "Ciel d'orage.png", zenith: [0.22, 0.24, 0.28], horizon: [0.55, 0.57, 0.60], cloud: [0.30, 0.31, 0.34], cover: 0.75, stretch: 1.5, sun: None },
];

/// Écrit les ciels générés manquants (une seule fois par nom).
fn ensure_builtin() {
    let dir = skies_dir();
    let marker = dir.join(".generes");
    if marker.exists() {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let (w, h) = (1600u32, 900u32);
    for (k, b) in BUILTIN.iter().enumerate() {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let t = v.powf(1.6);
            let mut c: [f32; 3] = std::array::from_fn(|i| b.zenith[i] + (b.horizon[i] - b.zenith[i]) * t);
            if let Some((p, sc)) = b.sun {
                let d = (((u - p[0]) * 1.6).powi(2) + (v - p[1]).powi(2)).sqrt();
                let glow = (-d * 6.0).exp() * 0.8 + (-d * 40.0).exp();
                for i in 0..3 {
                    c[i] += sc[i] * glow;
                }
            }
            // Nuages : bruit fractal sur un plan vu en perspective (plus fins vers l'horizon).
            let d = 1.15 - v;
            let n = fbm((u - 0.5) * 3.0 / (d * b.stretch) + 10.0, 2.0 / d, k as u32 * 17 + 1);
            let a = ((n - (0.62 - 0.3 * b.cover)) / 0.18).clamp(0.0, 1.0) * (0.5 + 0.5 * (1.0 - v));
            let shade = 0.75 + 0.25 * fbm(u * 9.0, v * 9.0, k as u32 * 31 + 7);
            for i in 0..3 {
                c[i] += (b.cloud[i] * shade - c[i]) * a;
            }
            image::Rgb(c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8))
        });
        let _ = img.save(dir.join(b.name));
    }
    let _ = std::fs::write(marker, b"");
}
