//! Calques : images posées sur la photo développée, chargées une fois et gardées en mémoire.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Côté maximal d'un calque dans la texture du GPU.
pub const LAYER_MAX: u32 = 2048;
pub const LAYER_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "tif", "tiff"];

/// Image d'un calque : RGBA 8 bits, couleurs encodées sRGB, alpha non prémultiplié.
pub struct LayerImage {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub fn load(path: &str) -> Option<Arc<LayerImage>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<LayerImage>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(l) = cache.lock().unwrap().get(path) {
        return l.clone();
    }
    let img = image::open(path).map_err(|e| eprintln!("calque {path} : {e:#}")).ok().map(|img| {
        let img = if img.width().max(img.height()) > LAYER_MAX { img.resize(LAYER_MAX, LAYER_MAX, image::imageops::FilterType::Lanczos3) } else { img };
        let rgba = img.to_rgba8();
        Arc::new(LayerImage { w: rgba.width(), h: rgba.height(), rgba: rgba.into_raw() })
    });
    cache.lock().unwrap().insert(path.to_string(), img.clone());
    img
}

/// Détourage IA de l'image d'un calque : prêt, en cours de calcul, ou en échec.
#[derive(Clone)]
pub enum Subject {
    Ready(Arc<crate::ai::AiMask>),
    Pending,
    Failed(String),
}

fn subjects() -> &'static Mutex<HashMap<String, Subject>> {
    static S: OnceLock<Mutex<HashMap<String, Subject>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Masque du sujet de l'image `path` : lu dans le cache des masques IA, sinon calculé en arrière-plan
/// (modèle téléchargé au besoin). `Pending` tant qu'il n'est pas prêt.
pub fn subject(path: &str) -> Subject {
    if let Some(s) = subjects().lock().unwrap().get(path) {
        return s.clone();
    }
    let p = std::path::PathBuf::from(path);
    if let Some(m) = crate::ai::cache_load(&p, crate::ai::Model::Subject) {
        let s = Subject::Ready(Arc::new(m));
        subjects().lock().unwrap().insert(path.to_string(), s.clone());
        return s;
    }
    subjects().lock().unwrap().insert(path.to_string(), Subject::Pending);
    let key = path.to_string();
    std::thread::spawn(move || {
        let r = (|| -> anyhow::Result<crate::ai::AiMask> {
            let img = load(&key).ok_or_else(|| anyhow::anyhow!("image illisible"))?;
            crate::ai::ensure_model(crate::ai::Model::Subject, &crate::ai::Progress::default())?;
            let guide: Vec<[f32; 4]> = img.rgba.chunks_exact(4).map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, 1.0]).collect();
            let m = crate::ai::compute(crate::ai::Model::Subject, &guide, img.w, img.h)?;
            let _ = crate::ai::cache_store(&std::path::PathBuf::from(&key), crate::ai::Model::Subject, &m);
            Ok(m)
        })();
        let s = match r {
            Ok(m) => Subject::Ready(Arc::new(m)),
            Err(e) => Subject::Failed(format!("{e:#}")),
        };
        subjects().lock().unwrap().insert(key, s);
    });
    Subject::Pending
}

/// Un calque attend-il son détourage ?
pub fn subject_pending(l: &crate::adjust::Layer) -> bool {
    l.subject && matches!(subjects().lock().unwrap().get(&l.image), Some(Subject::Pending))
}

/// Image d'un calque après découpe au lasso, détourage du sujet, gomme et harmonisation.
pub fn load_layer(l: &crate::adjust::Layer) -> Option<Arc<LayerImage>> {
    let img = load(&l.image)?;
    let subject = if l.subject { match subject(&l.image) { Subject::Ready(m) => Some(m), _ => None } } else { None };
    if l.clip.len() < 3 && subject.is_none() && l.strokes.is_empty() && l.harmony <= 0.0 {
        return Some(img);
    }
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<LayerImage>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let k = key(l);
    if let Some(c) = cache.lock().unwrap().get(&k) {
        return Some(c.clone());
    }
    let (w, h) = (img.w, img.h);
    let mut keep = vec![1f32; (w * h) as usize];
    if l.clip.len() >= 3 {
        let mut m = vec![0f32; (w * h) as usize];
        let poly: Vec<[f32; 2]> = l.clip.iter().map(|p| [p[0] * w as f32, p[1] * h as f32]).collect();
        crate::masks::fill_polygon(&mut m, w, h, &poly, 1.0, false);
        keep = crate::retouch::blur(&m, w, h, 1);
    }
    if let Some(m) = &subject {
        for (k, v) in keep.iter_mut().zip(m.at(w, h)) {
            *k *= v;
        }
    }
    if !l.strokes.is_empty() {
        // Traits de gomme : rayon en fraction de la largeur, comme paint_strokes l'attend.
        let mut g = vec![1f32; (w * h) as usize];
        crate::masks::paint_strokes(&mut g, w, h, &l.strokes);
        for (k, v) in keep.iter_mut().zip(&g) {
            *k *= v;
        }
    }
    let mut rgba = img.rgba.clone();
    for (px, a) in rgba.chunks_exact_mut(4).zip(&keep) {
        px[3] = (px[3] as f32 * a.clamp(0.0, 1.0)).round() as u8;
    }
    if l.harmony > 0.0 {
        harmonize(&mut rgba, l.harmony.min(1.0), &l.harmony_target);
    }
    let out = Arc::new(LayerImage { w, h, rgba });
    let mut c = cache.lock().unwrap();
    // Peu de variantes gardées : chaque tracé ou réglage en crée une nouvelle.
    if c.len() > 8 {
        c.clear();
    }
    c.insert(k, out.clone());
    Some(out)
}

/// Ambiance d'une image (valeurs affichées 0..1) : noir et blanc de la luminance (2 et 98 %), puis teinte
/// moyenne R, G, B rapportée à la luminance moyenne, et cette luminance. Pixels pondérés par `weight`.
pub fn stats(px: impl Iterator<Item = ([f32; 3], f32)>) -> [f32; 6] {
    let luma = |c: &[f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let mut hist = [0f64; 256];
    let (mut s, mut n) = ([0f64; 3], 0f64);
    for (c, a) in px {
        if a <= 0.0 {
            continue;
        }
        hist[(luma(&c).clamp(0.0, 1.0) * 255.0) as usize] += a as f64;
        for k in 0..3 {
            s[k] += (c[k] * a) as f64;
        }
        n += a as f64;
    }
    if n < 1.0 {
        return [0.0, 1.0, 1.0, 1.0, 1.0, 0.5];
    }
    let pct = |q: f64| {
        let mut acc = 0.0;
        for (i, h) in hist.iter().enumerate() {
            acc += h;
            if acc >= q * n {
                return i as f32 / 255.0;
            }
        }
        1.0
    };
    let m = s.map(|v| (v / n) as f32);
    let ml = luma(&m).max(1e-3);
    [pct(0.02), pct(0.98), m[0] / ml, m[1] / ml, m[2] / ml, ml]
}

/// Met le calque dans l'ambiance de la photo sans changer sa luminosité propre (un sujet sombre reste
/// sombre) : teinte d'ensemble reprise à moitié, noirs relevés comme le voile de la photo, blancs
/// plafonnés comme les siens.
fn harmonize(rgba: &mut [u8], amount: f32, target: &[f32; 6]) {
    let px = |p: &[u8]| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
    let src = stats(rgba.chunks_exact(4).map(|p| (px(p), p[3] as f32 / 255.0)));
    let cast = [2, 3, 4].map(|k| (target[k] / src[k].max(1e-3)).clamp(0.5, 2.0).sqrt());
    // Voile : les noirs du calque montent jusqu'à ceux de la photo, dans la teinte de la photo.
    let lift = (target[0] - src[0]).max(0.0);
    let haze = [2, 3, 4].map(|k| lift * target[k]);
    // Blancs : pas plus clairs que ceux de la photo.
    let top = 1.0 - (src[1] - target[1]).max(0.0);
    for p in rgba.chunks_exact_mut(4) {
        let c = px(p);
        for i in 0..3 {
            let v = haze[i] + c[i] * cast[i] * (top - lift).max(0.2);
            let v = c[i] + (v - c[i]) * amount;
            p[i] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

/// Identité de l'image d'un calque (fichier, découpe, détourage, gomme, harmonisation) : change quand il
/// faut la recharger.
pub fn key(l: &crate::adjust::Layer) -> String {
    use std::hash::{Hash, Hasher};
    let ready = l.subject && matches!(subjects().lock().unwrap().get(&l.image), Some(Subject::Ready(_)));
    if l.clip.len() < 3 && !ready && l.strokes.is_empty() && l.harmony <= 0.0 {
        return l.image.clone();
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in &l.clip {
        (p[0].to_bits(), p[1].to_bits()).hash(&mut h);
    }
    ready.hash(&mut h);
    for s in &l.strokes {
        (s.radius.to_bits(), s.hardness.to_bits(), s.erase, s.points.len()).hash(&mut h);
        for p in &s.points {
            (p[0].to_bits(), p[1].to_bits()).hash(&mut h);
        }
    }
    if l.harmony > 0.0 {
        l.harmony.to_bits().hash(&mut h);
        for v in l.harmony_target {
            v.to_bits().hash(&mut h);
        }
    }
    format!("{}|{:016x}", l.image, h.finish())
}

/// Nom affiché d'un calque : nom du fichier.
pub fn name(path: &str) -> &str {
    std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un sujet sombre posé sur une photo voilée et rosée : il prend le voile et la teinte, mais reste sombre.
    #[test]
    fn harmonize_keeps_a_dark_subject_dark() {
        let mut rgba: Vec<u8> = (0..400).flat_map(|i| { let v = 20 + (i % 60) as u8; [v, v, v, 255] }).collect();
        let photo: Vec<[f32; 3]> = (0..400).map(|i| { let v = 0.55 + (i % 40) as f32 / 200.0; [v + 0.06, v - 0.02, v + 0.03] }).collect();
        let target = stats(photo.iter().map(|c| (*c, 1.0)));
        harmonize(&mut rgba, 1.0, &target);
        let after = stats(rgba.chunks_exact(4).map(|p| ([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0], 1.0)));
        assert!(after[0] > 0.3, "noirs non relevés : {}", after[0]);
        assert!(after[5] < target[5] - 0.05, "sujet éclairci comme la photo : {} / {}", after[5], target[5]);
        assert!(after[2] > after[3], "teinte rosée non reprise");
    }

    #[test]
    fn key_changes_with_strokes() {
        let mut l = crate::adjust::Layer { image: "/x.png".into(), ..Default::default() };
        assert_eq!(key(&l), "/x.png");
        l.strokes.push(crate::adjust::BrushStroke { points: vec![[0.5, 0.5]], radius: 0.1, hardness: 0.4, flow: 1.0, erase: true, fill: false });
        let k1 = key(&l);
        l.strokes[0].points.push([0.6, 0.5]);
        assert_ne!(key(&l), k1);
    }
}
