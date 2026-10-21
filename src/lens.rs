//! Correction d'objectif à partir de la base Lensfun (CC BY-SA 3.0), embarquée dans le programme :
//! distorsion (ptlens, poly3, poly5), aberration chromatique latérale (poly3, linéaire) et vignetage (pa).
//! Conventions reprises de lensfun (modifier.cpp, mod-coord.cpp, mod-subpix.cpp, mod-color.cpp) :
//! distorsion et aberration en unités « Hugin » (r = 1 à mi-hauteur du capteur de calibration),
//! vignetage avec r = 1 au coin du capteur de calibration.

use crate::adjust::LensFix;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

/// Diagonale d'un capteur 24x36, en mm.
const FF_DIAG: f32 = 43.266_6;

#[derive(Clone, Debug)]
pub struct Camera {
    pub maker: String,
    pub model: String,
    pub crop: f32,
}

/// Distorsion sous la forme r_source = r (1 + c1 r + c2 r² + c3 r³ + c4 r⁴), en unités Hugin,
/// déjà normalisée comme dans lensfun (le centre de l'image garde son échelle).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dist(pub [f32; 4]);

/// Aberration : r_canal = r (v + c r + b r²) pour le rouge et le bleu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tca {
    pub red: [f32; 3],
    pub blue: [f32; 3],
}

#[derive(Clone, Copy, Debug)]
struct Vig {
    focal: f32,
    aperture: f32,
    distance: f32,
    k: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct Lens {
    pub maker: String,
    pub model: String,
    pub crop: f32,
    pub aspect: f32,
    dist: Vec<(f32, Dist)>,
    tca: Vec<(f32, Tca)>,
    vig: Vec<Vig>,
}

impl Lens {
    /// Nom affiché, unique même quand l'objectif a plusieurs calibrations (facteurs de recadrage).
    pub fn key(&self) -> String {
        if (self.crop - 1.0).abs() < 0.01 { self.model.clone() } else { format!("{} (×{:.2})", self.model, self.crop) }
    }

    pub fn has_data(&self) -> bool {
        !self.dist.is_empty() || !self.tca.is_empty() || !self.vig.is_empty()
    }
}

pub struct Db {
    pub cameras: Vec<Camera>,
    pub lenses: Vec<Lens>,
}

fn attr(n: &roxmltree::Node, k: &str) -> f32 {
    n.attribute(k).and_then(|v| v.trim().parse().ok()).unwrap_or(0.0)
}

fn child_text<'a>(n: &'a roxmltree::Node, tag: &str) -> Option<&'a str> {
    n.children().find(|c| c.has_tag_name(tag) && c.attribute("lang").is_none()).and_then(|c| c.text()).map(str::trim)
}

fn parse_aspect(s: &str) -> Option<f32> {
    match s.split_once(':') {
        Some((a, b)) => Some(a.trim().parse::<f32>().ok()? / b.trim().parse::<f32>().ok()?),
        None => s.trim().parse().ok(),
    }
}

/// Coefficients de distorsion lensfun -> forme commune, avec la renormalisation de lensfun
/// (a/d⁴, b/d³, c/d² pour ptlens ; k1/d³ pour poly3).
fn dist_of(n: &roxmltree::Node) -> Option<Dist> {
    match n.attribute("model")? {
        "ptlens" => {
            let (a, b, c) = (attr(n, "a"), attr(n, "b"), attr(n, "c"));
            let d = 1.0 - a - b - c;
            Some(Dist([c / d.powi(2), b / d.powi(3), a / d.powi(4), 0.0]))
        }
        "poly3" => {
            let k1 = attr(n, "k1");
            let d = 1.0 - k1;
            Some(Dist([0.0, k1 / d.powi(3), 0.0, 0.0]))
        }
        "poly5" => Some(Dist([0.0, attr(n, "k1"), 0.0, attr(n, "k2")])),
        _ => None,
    }
}

fn tca_of(n: &roxmltree::Node) -> Option<Tca> {
    match n.attribute("model")? {
        "poly3" => Some(Tca {
            red: [n.attribute("vr").map_or(1.0, |_| attr(n, "vr")), attr(n, "cr"), attr(n, "br")],
            blue: [n.attribute("vb").map_or(1.0, |_| attr(n, "vb")), attr(n, "cb"), attr(n, "bb")],
        }),
        "linear" => Some(Tca { red: [attr(n, "kr"), 0.0, 0.0], blue: [attr(n, "kb"), 0.0, 0.0] }),
        _ => None,
    }
}

fn parse_file(text: &str, db: &mut Db) {
    let Ok(doc) = roxmltree::Document::parse(text) else { return };
    for n in doc.root_element().children().filter(|n| n.is_element()) {
        if n.has_tag_name("camera") {
            if let (Some(maker), Some(model)) = (child_text(&n, "maker"), child_text(&n, "model")) {
                let crop = child_text(&n, "cropfactor").and_then(|c| c.parse().ok()).unwrap_or(1.0);
                db.cameras.push(Camera { maker: maker.into(), model: model.into(), crop });
            }
        } else if n.has_tag_name("lens") {
            let (Some(maker), Some(model)) = (child_text(&n, "maker"), child_text(&n, "model")) else { continue };
            // Objectifs fisheye : la correction rectilinéaire ne s'applique pas.
            if child_text(&n, "type").is_some_and(|t| t != "rectilinear") {
                continue;
            }
            let mut lens = Lens {
                maker: maker.into(),
                model: model.into(),
                crop: child_text(&n, "cropfactor").and_then(|c| c.parse().ok()).unwrap_or(1.0),
                aspect: child_text(&n, "aspect-ratio").and_then(parse_aspect).unwrap_or(1.5),
                dist: Vec::new(),
                tca: Vec::new(),
                vig: Vec::new(),
            };
            for cal in n.children().filter(|c| c.has_tag_name("calibration")) {
                for e in cal.children().filter(|c| c.is_element()) {
                    let focal = attr(&e, "focal");
                    match e.tag_name().name() {
                        "distortion" => {
                            if let Some(d) = dist_of(&e) {
                                lens.dist.push((focal, d));
                            }
                        }
                        "tca" => {
                            if let Some(t) = tca_of(&e) {
                                lens.tca.push((focal, t));
                            }
                        }
                        "vignetting" if e.attribute("model") == Some("pa") => lens.vig.push(Vig {
                            focal,
                            aperture: attr(&e, "aperture"),
                            distance: attr(&e, "distance"),
                            k: [attr(&e, "k1"), attr(&e, "k2"), attr(&e, "k3")],
                        }),
                        _ => {}
                    }
                }
            }
            lens.dist.sort_by(|a, b| a.0.total_cmp(&b.0));
            lens.tca.sort_by(|a, b| a.0.total_cmp(&b.0));
            if lens.has_data() {
                db.lenses.push(lens);
            }
        }
    }
}

pub fn db() -> &'static Db {
    static DB: OnceLock<Db> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = Db { cameras: Vec::new(), lenses: Vec::new() };
        let bytes: &[u8] = include_bytes!("../data/lensfun-db.zip");
        if let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
            for i in 0..zip.len() {
                let mut text = String::new();
                if let Ok(mut f) = zip.by_index(i) {
                    if f.read_to_string(&mut text).is_ok() {
                        parse_file(&text, &mut db);
                    }
                }
            }
        }
        db.lenses.sort_by_key(|l| l.key().to_lowercase());
        db
    })
}

// ---------- Recherche ----------

/// Mots normalisés : minuscules, lettres et chiffres séparés ("EF24-105mm" -> ef 24 105 mm).
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digit = false;
    for c in s.to_lowercase().chars() {
        let is_d = c.is_ascii_digit() || (c == '.' && digit && !cur.is_empty());
        if c.is_alphanumeric() || is_d {
            if !cur.is_empty() && is_d != digit {
                out.push(std::mem::take(&mut cur));
            }
            digit = is_d;
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Similarité entre deux noms : part des mots de `db_name` présents dans `exif` (la marque peut manquer).
fn score(exif: &[String], db_name: &str, maker: &str) -> f32 {
    let maker = maker.to_lowercase();
    let want: Vec<String> = tokens(db_name).into_iter().filter(|t| *t != maker).collect();
    if want.is_empty() {
        return 0.0;
    }
    let hit = want.iter().filter(|t| exif.contains(t)).count() as f32;
    let extra = exif.iter().filter(|t| **t != maker && !want.contains(t)).count() as f32;
    hit / want.len() as f32 - 0.05 * extra
}

pub fn find_camera(make: &str, model: &str) -> Option<&'static Camera> {
    let t = tokens(&format!("{make} {model}"));
    db().cameras
        .iter()
        .filter(|c| make.to_lowercase().contains(&c.maker.to_lowercase()) || c.maker.to_lowercase().contains(&make.to_lowercase()))
        .map(|c| (score(&t, &c.model, &c.maker), c))
        .filter(|(s, _)| *s >= 0.99)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| c)
}

/// Objectif le plus proche du nom EXIF, calibré pour un capteur au plus aussi grand que l'appareil.
pub fn find_lens(name: &str, cam_crop: Option<f32>) -> Option<&'static Lens> {
    let t = tokens(name);
    let mut best: Option<(f32, &Lens)> = None;
    for l in &db().lenses {
        let mut s = score(&t, &l.model, &l.maker);
        if s < 0.85 {
            continue;
        }
        if let Some(c) = cam_crop {
            if l.crop > c * 1.02 {
                continue;
            }
            // Calibration au facteur le plus proche de l'appareil.
            s -= (c - l.crop).abs() * 0.01;
        }
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, l));
        }
    }
    best.map(|(_, l)| l)
}

pub fn lens_by_key(key: &str) -> Option<&'static Lens> {
    db().lenses.iter().find(|l| l.key() == key)
}

// ---------- Infos de prise de vue ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shot {
    pub make: String,
    pub model: String,
    pub lens: Option<String>,
    pub focal: Option<f32>,
    pub aperture: Option<f32>,
}

pub fn read_shot(p: &Path) -> Shot {
    let p = &crate::library::source(p);
    if crate::library::is_raw(p) {
        let meta = (|| {
            let src = rawler::rawsource::RawSource::new(p).ok()?;
            let dec = rawler::get_decoder(&src).ok()?;
            dec.raw_metadata(&src, &Default::default()).ok()
        })();
        if let Some(m) = meta {
            let r = |v: Option<rawler::formats::tiff::Rational>| v.filter(|r| r.d != 0).map(|r| r.n as f32 / r.d as f32);
            let lens = m.lens.as_ref().map(|l| l.lens_model.clone()).or(m.exif.lens_model.clone());
            return Shot { make: m.make, model: m.model, lens, focal: r(m.exif.focal_length), aperture: r(m.exif.fnumber) };
        }
        return Shot::default();
    }
    let exif = std::fs::File::open(p).ok().and_then(|f| exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)).ok());
    let Some(exif) = exif else { return Shot::default() };
    let text = |t| exif.get_field(t, exif::In::PRIMARY).map(|f| f.display_value().to_string().trim_matches('"').trim().to_string()).filter(|s| !s.is_empty());
    let num = |t| {
        exif.get_field(t, exif::In::PRIMARY).and_then(|f| match &f.value {
            exif::Value::Rational(v) => v.first().map(|r| r.to_f32()),
            _ => None,
        })
    };
    Shot {
        make: text(exif::Tag::Make).unwrap_or_default(),
        model: text(exif::Tag::Model).unwrap_or_default(),
        lens: text(exif::Tag::LensModel),
        focal: num(exif::Tag::FocalLength),
        aperture: num(exif::Tag::FNumber),
    }
}

// ---------- Profil de correction ----------

/// Correction calculée pour une photo : coefficients interpolés à sa focale et son ouverture.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub lens_name: String,
    /// Facteurs de recadrage de l'appareil et de la calibration, rapport du capteur de calibration.
    pub cam_crop: f32,
    pub lens_crop: f32,
    pub aspect: f32,
    pub dist: Option<Dist>,
    pub tca: Option<Tca>,
    pub vig: Option<[f32; 3]>,
}

fn lerp_by<T: Copy>(v: &[(f32, T)], f: f32, mix: impl Fn(T, T, f32) -> T) -> Option<T> {
    let first = v.first()?;
    if f <= first.0 || v.len() == 1 {
        return Some(first.1);
    }
    for w in v.windows(2) {
        if f <= w[1].0 {
            let t = (f - w[0].0) / (w[1].0 - w[0].0).max(1e-6);
            return Some(mix(w[0].1, w[1].1, t));
        }
    }
    Some(v.last().unwrap().1)
}

/// Vignetage : pondération inverse à la distance en (focale, ouverture), à la plus grande distance de mise au point.
fn vig_at(v: &[Vig], focal: f32, aperture: f32) -> Option<[f32; 3]> {
    let far = v.iter().map(|x| x.distance).fold(0.0f32, f32::max);
    let pts: Vec<&Vig> = v.iter().filter(|x| (x.distance - far).abs() < 1e-3).collect();
    if pts.is_empty() {
        return None;
    }
    let (mut k, mut wsum) = ([0f32; 3], 0f32);
    for p in pts {
        // Distances relatives (échelles logarithmiques, comme les crans de focale et de diaphragme).
        let df = (p.focal.max(1.0) / focal.max(1.0)).ln();
        let da = (p.aperture.max(0.5) / aperture.max(0.5)).ln();
        let d2 = df * df + da * da;
        if d2 < 1e-8 {
            return Some(p.k);
        }
        let w = 1.0 / (d2 * d2);
        for i in 0..3 {
            k[i] += p.k[i] * w;
        }
        wsum += w;
    }
    Some(k.map(|x| x / wsum))
}

pub fn profile(lens: &Lens, cam_crop: Option<f32>, shot: &Shot) -> Profile {
    // Focale inconnue : la plus courte calibrée (là où la distorsion est souvent la plus forte).
    let focal = shot.focal.or(lens.dist.first().map(|d| d.0)).unwrap_or(50.0);
    let aperture = shot.aperture.unwrap_or(8.0);
    Profile {
        lens_name: lens.key(),
        cam_crop: cam_crop.unwrap_or(lens.crop),
        lens_crop: lens.crop,
        aspect: lens.aspect,
        dist: lerp_by(&lens.dist, focal, |a, b, t| Dist(std::array::from_fn(|i| a.0[i] + (b.0[i] - a.0[i]) * t))),
        tca: lerp_by(&lens.tca, focal, |a, b, t| Tca {
            red: std::array::from_fn(|i| a.red[i] + (b.red[i] - a.red[i]) * t),
            blue: std::array::from_fn(|i| a.blue[i] + (b.blue[i] - a.blue[i]) * t),
        }),
        vig: vig_at(&lens.vig, focal, aperture),
    }
}

/// Profil d'une photo : objectif choisi à la main, sinon détecté dans les EXIF.
pub fn profile_for(shot: &Shot, fix: &LensFix) -> Option<Profile> {
    if !fix.enabled {
        return None;
    }
    let cam = find_camera(&shot.make, &shot.model);
    let lens = match &fix.lens {
        Some(k) => lens_by_key(k)?,
        None => find_lens(shot.lens.as_deref()?, cam.map(|c| c.crop))?,
    };
    Some(profile(lens, cam.map(|c| c.crop), shot))
}

/// Profils mémorisés par photo (la lecture des métadonnées RAW coûte quelques ms).
pub fn shot_cached(p: &Path) -> Shot {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<std::path::PathBuf, Shot>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(s) = cache.lock().unwrap().get(p) {
        return s.clone();
    }
    let s = read_shot(p);
    cache.lock().unwrap().insert(p.to_path_buf(), s.clone());
    s
}

/// Prépare la correction d'objectif d'une photo sur un Developer.
pub fn setup(dev: &mut crate::gpu::Developer, photo: &Path, fix: &LensFix) {
    let profile = if fix.enabled { profile_for(&shot_cached(photo), fix) } else { None };
    dev.set_lens(profile.as_ref(), fix);
}

/// Uniformes de la correction pour une image `w`x`h` : (coefficients, échelles).
/// `amounts` = (distorsion 0..1, aberration 0/1, vignetage 0..1, recadrage automatique 0/1).
#[derive(Clone, Copy)]
pub struct LensUniforms {
    pub info: [f32; 4],
    pub dist: [f32; 4],
    pub tca_r: [f32; 4],
    pub tca_b: [f32; 4],
    pub vig: [f32; 4],
}

impl LensUniforms {
    /// Position dans l'image corrigée (0..1) -> position dans la source (0..1) : le calcul du shader.
    /// `dims` : taille de la source en pixels.
    pub fn to_source(&self, uv: [f32; 2], dims: [f32; 2]) -> [f32; 2] {
        let q = [(uv[0] - 0.5) * dims[0] / self.info[2], (uv[1] - 0.5) * dims[1] / self.info[2]];
        let r = (q[0] * q[0] + q[1] * q[1]).sqrt() * self.info[1];
        let c = self.dist;
        let g = 1.0 + r * (c[0] + r * (c[1] + r * (c[2] + r * c[3])));
        [q[0] * g / dims[0] + 0.5, q[1] * g / dims[1] + 0.5]
    }

    /// Inverse de `to_source` (point fixe : la distorsion reste faible devant 1).
    pub fn to_corrected(&self, src: [f32; 2], dims: [f32; 2]) -> [f32; 2] {
        let mut uv = src;
        for _ in 0..30 {
            let m = self.to_source(uv, dims);
            let d = [src[0] - m[0], src[1] - m[1]];
            uv = [uv[0] + d[0], uv[1] + d[1]];
            if d[0].abs() + d[1].abs() < 1e-7 {
                break;
            }
        }
        uv
    }
}

impl Profile {
    /// Pixels (depuis le centre) -> unités Hugin, et -> unités du vignetage (r = 1 au coin).
    fn scales(&self, w: f32, h: f32) -> (f32, f32) {
        let mm_per_px = FF_DIAG / self.cam_crop / (w * w + h * h).sqrt();
        let hugin_mm = FF_DIAG / self.lens_crop / (self.aspect * self.aspect + 1.0).sqrt() / 2.0;
        let pa_mm = FF_DIAG / self.lens_crop / 2.0;
        (mm_per_px / hugin_mm, mm_per_px / pa_mm)
    }

    /// Zoom minimal pour que l'image corrigée n'ait pas de bords vides.
    fn auto_scale(c: [f32; 4], kh: f32, w: f32, h: f32) -> f32 {
        let map = |q: [f32; 2], s: f32| {
            let p = [q[0] / s, q[1] / s];
            let r = (p[0] * p[0] + p[1] * p[1]).sqrt() * kh;
            let g = 1.0 + c[0] * r + c[1] * r * r + c[2] * r * r * r + c[3] * r * r * r * r;
            [p[0] * g, p[1] * g]
        };
        let (hw, hh) = (w / 2.0, h / 2.0);
        let mut pts = Vec::new();
        for k in 0..=16 {
            let t = k as f32 / 16.0 * 2.0 - 1.0;
            pts.extend([[t * hw, hh], [t * hw, -hh], [hw, t * hh], [-hw, t * hh]]);
        }
        let inside = |s: f32| pts.iter().all(|q| {
            let p = map(*q, s);
            p[0].abs() <= hw * 1.0005 && p[1].abs() <= hh * 1.0005
        });
        let (mut lo, mut hi) = (0.5f32, 2.0f32);
        if inside(lo) {
            return lo;
        }
        if !inside(hi) {
            return 1.0;
        }
        for _ in 0..30 {
            let m = 0.5 * (lo + hi);
            if inside(m) { hi = m } else { lo = m }
        }
        hi
    }

    pub fn uniforms(&self, fix: &LensFix, w: u32, h: u32) -> LensUniforms {
        let (w, h) = (w as f32, h as f32);
        let (kh, kv) = self.scales(w, h);
        let amount = fix.distortion.clamp(0.0, 1.0);
        let c = self.dist.map_or([0.0; 4], |d| d.0.map(|x| x * amount));
        let scale = if fix.autoscale && self.dist.is_some() { Self::auto_scale(c, kh, w, h) } else { 1.0 };
        let (tr, tb) = match (fix.tca, self.tca) {
            (true, Some(t)) => (t.red, t.blue),
            _ => ([1.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
        };
        let v = self.vig.map_or([0.0; 3], |k| k.map(|x| x * fix.vignetting.clamp(0.0, 1.0)));
        LensUniforms {
            info: [1.0, kh, scale, kv],
            dist: c,
            tca_r: [tr[0], tr[1], tr[2], 0.0],
            tca_b: [tb[0], tb[1], tb[2], 0.0],
            vig: [v[0], v[1], v[2], 0.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_loads_and_finds_canon_lens() {
        let d = db();
        assert!(d.lenses.len() > 500, "{} objectifs", d.lenses.len());
        let cam = find_camera("Canon", "Canon EOS 5D Mark III").expect("boîtier");
        assert_eq!(cam.crop, 1.0);
        let l = find_lens("EF24-105mm f/4L IS USM", Some(cam.crop)).expect("objectif");
        assert_eq!(l.model, "Canon EF 24-105mm f/4L IS USM");
        assert_eq!(l.crop, 1.0);
        // Sur APS-C, la calibration APS-C est préférée.
        let l = find_lens("EF24-105mm f/4L IS USM", Some(1.6)).unwrap();
        assert!((l.crop - 1.611).abs() < 1e-3);
    }

    #[test]
    fn finds_canon_600d_and_kit_lens() {
        for (make, model) in [("Canon", "Canon EOS 600D"), ("Canon", "Canon EOS REBEL T3i")] {
            let c = find_camera(make, model).unwrap_or_else(|| panic!("{model} introuvable"));
            assert!((c.crop - 1.613).abs() < 0.01);
        }
        let l = find_lens("EF-S18-55mm f/3.5-5.6 IS II", Some(1.613)).expect("18-55 IS II introuvable");
        assert!(l.model.contains("18-55"), "{}", l.model);
        eprintln!("600D + kit : {} (facteur {}, distorsion {}, vignetage {})", l.model, l.crop, !l.dist.is_empty(), !l.vig.is_empty());
    }

    #[test]
    fn ptlens_interpolation_and_autoscale() {
        let l = find_lens("Canon EF 24-105mm f/4L IS USM", Some(1.0)).unwrap();
        let shot = Shot { focal: Some(24.0), aperture: Some(4.0), ..Default::default() };
        let p = profile(l, Some(1.0), &shot);
        // a=0.017263, b=-0.049244 à 24 mm (base lensfun).
        let d: f32 = 1.0 - 0.017263 + 0.049244;
        let c = p.dist.unwrap().0;
        assert!((c[1] - (-0.049244 / d.powi(3))).abs() < 1e-6);
        assert!((c[2] - 0.017263 / d.powi(4)).abs() < 1e-6);
        // Le zoom automatique garde le cadre rempli : les bords de sortie tombent dans la source.
        let fix = LensFix { enabled: true, ..Default::default() };
        let u = p.uniforms(&fix, 5760, 3840);
        // Aller-retour image corrigée <-> source, coins compris.
        for uv in [[0.5, 0.5], [0.02, 0.03], [0.97, 0.9], [0.3, 0.7]] {
            let back = u.to_corrected(u.to_source(uv, [5760.0, 3840.0]), [5760.0, 3840.0]);
            assert!((back[0] - uv[0]).abs() < 1e-4 && (back[1] - uv[1]).abs() < 1e-4, "{uv:?} -> {back:?}");
        }
        assert!((u.info[2] - 1.0).abs() > 1e-3 && (u.info[2] - 1.0).abs() < 0.1, "zoom {}", u.info[2]);
        // Vignetage : correction plus forte au coin (k négatifs).
        assert!(p.vig.unwrap()[0] < 0.0);
    }
}
