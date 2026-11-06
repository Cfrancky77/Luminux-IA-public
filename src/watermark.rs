//! Signature de l'export et de l'impression : un texte ou une image (logo PNG) posé sur la photo,
//! à l'une des neuf places, avec retraits, ombre, mode de fusion, couleur et police.

use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef, FontVec, PxScale, ScaleFont, point};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    None,
    Text,
    Image,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    TopLeft,
    TopCenter,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Place {
    /// Les neuf places, ligne par ligne, de haut en bas.
    pub const GRID: [Place; 9] = [Place::TopLeft, Place::TopCenter, Place::TopRight, Place::Left, Place::Center, Place::Right, Place::BottomLeft, Place::BottomCenter, Place::BottomRight];

    /// Colonne et ligne (0, 1, 2).
    fn cell(self) -> (usize, usize) {
        let i = Self::GRID.iter().position(|p| *p == self).unwrap_or(8);
        (i % 3, i / 3)
    }
}

/// Façon dont la signature se mêle à la photo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blend {
    Normal,
    /// Assombrit : le blanc disparaît (logo sur fond blanc).
    Multiply,
    /// Éclaircit : le noir disparaît.
    Screen,
    /// Renforce le contraste du fond sous la signature.
    Overlay,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watermark {
    pub kind: Kind,
    pub text: String,
    pub image: Option<PathBuf>,
    pub place: Place,
    /// Largeur de la signature, en part de la largeur de la photo.
    pub width: f32,
    pub opacity: f32,
    /// Texte blanc (sinon noir) ; sert quand `color` est vide (réglages d'avant la couleur libre).
    pub white: bool,
    /// Couleur du texte (sRGB 0..1).
    pub color: Option<[f32; 3]>,
    /// Police du texte (fichier TTF/OTF) ; vide : police intégrée.
    pub font: Option<PathBuf>,
    /// Retraits depuis les bords, horizontal et vertical, en part du petit côté de la photo.
    pub inset_h: f32,
    pub inset_v: f32,
    /// Ombre portée, 0 : aucune.
    pub shadow: f32,
    pub blend: Blend,
}

impl Default for Watermark {
    fn default() -> Self {
        Self {
            kind: Kind::None,
            text: String::new(),
            image: None,
            place: Place::BottomRight,
            width: 0.2,
            opacity: 0.6,
            white: true,
            color: None,
            font: None,
            inset_h: 0.03,
            inset_v: 0.03,
            shadow: 0.35,
            blend: Blend::Normal,
        }
    }
}

impl Watermark {
    pub fn text_color(&self) -> [f32; 3] {
        self.color.unwrap_or(if self.white { [1.0; 3] } else { [0.0; 3] })
    }
}

/// Polices TTF/OTF du système et de l'utilisateur : (nom affiché, fichier), triées par nom.
pub fn system_fonts() -> Vec<(String, PathBuf)> {
    let mut dirs = vec![PathBuf::from("/usr/share/fonts"), PathBuf::from("/usr/local/share/fonts")];
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".fonts"));
        dirs.push(h.join(".local/share/fonts"));
    }
    if let Some(d) = dirs::font_dir() {
        dirs.push(d);
    }
    #[cfg(windows)]
    {
        let win = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        dirs.push(win.join("Fonts"));
        if let Some(l) = dirs::data_local_dir() {
            dirs.push(l.join(r"Microsoft\Windows\Fonts"));
        }
    }
    let mut out: Vec<(String, PathBuf)> = dirs
        .iter()
        .flat_map(|d| walkdir::WalkDir::new(d).follow_links(true).max_depth(6).into_iter().flatten())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "ttf" | "otf")))
        .map(|e| (e.path().file_stem().map(|s| s.to_string_lossy().replace(['_', '-'], " ")).unwrap_or_default(), e.into_path()))
        .collect();
    out.sort_by_key(|f| f.0.to_lowercase());
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Couverture (0..1) et couleur d'une signature prête à poser.
struct Mark {
    w: usize,
    h: usize,
    alpha: Vec<f32>,
    rgb: Vec<[f32; 3]>,
}

fn text_mark(wm: &Watermark, width: f32) -> Option<Mark> {
    let text = wm.text.trim();
    if text.is_empty() || width < 4.0 {
        return None;
    }
    let color = wm.text_color();
    if let Some(f) = wm.font.as_deref().and_then(|p| std::fs::read(p).ok()).and_then(|d| FontVec::try_from_vec(d).ok()) {
        return draw_text(&f, text, width, color);
    }
    draw_text(&FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).ok()?, text, width, color)
}

fn draw_text<F: Font>(font: &F, text: &str, width: f32, color: [f32; 3]) -> Option<Mark> {
    // Largeur du texte à l'échelle 100, puis échelle qui donne la largeur voulue.
    let advance = |scale: f32| {
        let f = font.as_scaled(PxScale::from(scale));
        let mut x = 0.0;
        let mut prev = None;
        for c in text.chars() {
            let id = f.glyph_id(c);
            if let Some(p) = prev {
                x += f.kern(p, id);
            }
            x += f.h_advance(id);
            prev = Some(id);
        }
        x
    };
    let scale = 100.0 * width / advance(100.0).max(1.0);
    let f = font.as_scaled(PxScale::from(scale));
    let (w, h) = (advance(scale).ceil() as usize + 2, (f.ascent() - f.descent()).ceil() as usize + 2);
    let mut alpha = vec![0f32; w * h];
    let mut x = 1.0;
    let mut prev = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            x += f.kern(p, id);
        }
        let glyph = id.with_scale_and_position(scale, point(x, 1.0 + f.ascent()));
        x += f.h_advance(id);
        prev = Some(id);
        if let Some(g) = font.outline_glyph(glyph) {
            let b = g.px_bounds();
            g.draw(|gx, gy, v| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let a = &mut alpha[py as usize * w + px as usize];
                    *a = (*a + v).min(1.0);
                }
            });
        }
    }
    Some(Mark { w, h, alpha, rgb: vec![color; w * h] })
}

fn image_mark(path: &Path, width: f32) -> Option<Mark> {
    let img = image::open(path).ok()?.to_rgba8();
    let w = (width.round() as u32).max(1);
    let h = ((img.height() as f32 * w as f32 / img.width().max(1) as f32).round() as u32).max(1);
    let img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3);
    let px = img.pixels();
    let (alpha, rgb) = px.map(|p| (p[3] as f32 / 255.0, [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0])).unzip();
    Some(Mark { w: w as usize, h: h as usize, alpha, rgb })
}

/// Coin haut gauche de la signature de taille `mw`×`mh` sur une image `w`×`h`.
fn origin(wm: &Watermark, w: usize, h: usize, mw: usize, mh: usize) -> (usize, usize) {
    let short = w.min(h) as f32;
    let ih = (wm.inset_h.clamp(0.0, 0.3) * short).round() as usize;
    let iv = (wm.inset_v.clamp(0.0, 0.3) * short).round() as usize;
    let (col, row) = wm.place.cell();
    let x = match col {
        0 => ih,
        1 => w.saturating_sub(mw) / 2,
        _ => w.saturating_sub(mw + ih),
    };
    let y = match row {
        0 => iv,
        1 => h.saturating_sub(mh) / 2,
        _ => h.saturating_sub(mh + iv),
    };
    (x.min(w.saturating_sub(mw)), y.min(h.saturating_sub(mh)))
}

/// Signature prête à poser sur une image de largeur `w`.
fn mark(wm: &Watermark, w: u32) -> Option<Mark> {
    let target = wm.width.clamp(0.02, 0.9) * w as f32;
    match wm.kind {
        Kind::None => None,
        Kind::Text => text_mark(wm, target),
        Kind::Image => wm.image.as_deref().and_then(|p| image_mark(p, target)),
    }
}

/// Ombre adoucie, décalée vers le bas à droite de `d` pixels, de la couleur opposée au texte (noire pour un
/// logo), avec son opacité.
fn shadow(wm: &Watermark, m: &Mark) -> Option<(usize, Mark, f32)> {
    let s = wm.shadow.clamp(0.0, 1.0);
    if s <= 0.0 {
        return None;
    }
    let d = ((m.h.min(m.w) as f32) * 0.05).round().max(1.0) as usize;
    let c = match wm.kind {
        Kind::Text if luma(wm.text_color()) < 0.4 => [1.0; 3],
        _ => [0.0; 3],
    };
    let soft = Mark { w: m.w, h: m.h, alpha: blur(&m.alpha, m.w, m.h, d / 2), rgb: vec![c; m.w * m.h] };
    Some((d, soft, wm.opacity.clamp(0.0, 1.0) * s))
}

/// Pose la signature sur `rgb` (valeurs déjà encodées dans l'espace de sortie, 0..1).
pub fn apply(wm: &Watermark, w: u32, h: u32, rgb: &mut [f32]) {
    let Some(m) = mark(wm, w) else { return };
    let (w, h) = (w as usize, h as usize);
    let (x0, y0) = origin(wm, w, h, m.w, m.h);
    if let Some((d, soft, op)) = shadow(wm, &m) {
        paint(&soft, rgb, w, h, x0 + d, y0 + d, op, Blend::Normal);
    }
    paint(&m, rgb, w, h, x0, y0, wm.opacity.clamp(0.0, 1.0), wm.blend);
}

/// Aperçu à l'écran : la signature et son ombre seules sur fond transparent, telles qu'elles se posent
/// sur une image `w`×`h`. Renvoie le coin haut gauche, la taille et les pixels RGBA (non prémultipliés).
/// Le mode de fusion n'est pas rendu : l'aperçu montre la place, la taille et l'aspect.
pub fn overlay(wm: &Watermark, w: u32, h: u32) -> Option<(usize, usize, usize, usize, Vec<u8>)> {
    let m = mark(wm, w)?;
    let (iw, ih) = (w as usize, h as usize);
    let (x0, y0) = origin(wm, iw, ih, m.w, m.h);
    let sh = shadow(wm, &m);
    let d = sh.as_ref().map_or(0, |s| s.0);
    let (rw, rh) = ((m.w + d).min(iw - x0), (m.h + d).min(ih - y0));
    // Couleur prémultipliée et couverture, composées « par-dessus ».
    let mut acc = vec![[0f32; 4]; rw * rh];
    let mut over = |mk: &Mark, dx: usize, op: f32| {
        for y in 0..mk.h.min(rh.saturating_sub(dx)) {
            for x in 0..mk.w.min(rw.saturating_sub(dx)) {
                let a = mk.alpha[y * mk.w + x] * op;
                let c = mk.rgb[y * mk.w + x];
                let p = &mut acc[(y + dx) * rw + x + dx];
                for k in 0..3 {
                    p[k] = c[k] * a + p[k] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    };
    if let Some((d, soft, op)) = &sh {
        over(soft, *d, *op);
    }
    over(&m, 0, wm.opacity.clamp(0.0, 1.0));
    let rgba = acc
        .iter()
        .flat_map(|p| {
            let a = p[3];
            let c = |v: f32| if a > 0.0 { (v / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
            [c(p[0]), c(p[1]), c(p[2]), (a * 255.0).round() as u8]
        })
        .collect();
    Some((x0, y0, rw, rh, rgba))
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Flou en boîte séparable de rayon `r`.
fn blur(a: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return a.to_vec();
    }
    let pass = |src: &[f32], horiz: bool| -> Vec<f32> {
        let mut out = vec![0f32; src.len()];
        let (n, m) = if horiz { (h, w) } else { (w, h) };
        for line in 0..n {
            let at = |i: usize| if horiz { line * w + i } else { i * w + line };
            for i in 0..m {
                let (lo, hi) = (i.saturating_sub(r), (i + r).min(m - 1));
                let s: f32 = (lo..=hi).map(|j| src[at(j)]).sum();
                out[at(i)] = s / (2 * r + 1) as f32;
            }
        }
        out
    };
    pass(&pass(a, true), false)
}

fn mix(b: f32, c: f32, mode: Blend) -> f32 {
    match mode {
        Blend::Normal => c,
        Blend::Multiply => b * c,
        Blend::Screen => 1.0 - (1.0 - b) * (1.0 - c),
        Blend::Overlay => {
            if b < 0.5 {
                2.0 * b * c
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - c)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(m: &Mark, rgb: &mut [f32], w: usize, h: usize, x0: usize, y0: usize, op: f32, mode: Blend) {
    for y in 0..m.h.min(h.saturating_sub(y0)) {
        for x in 0..m.w.min(w.saturating_sub(x0)) {
            let a = m.alpha[y * m.w + x] * op;
            if a <= 0.0 {
                continue;
            }
            let c = m.rgb[y * m.w + x];
            let i = ((y0 + y) * w + x0 + x) * 3;
            for k in 0..3 {
                let b = rgb[i + k];
                rgb[i + k] = b * (1.0 - a) + mix(b, c[k], mode) * a;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(rgb: &[f32], w: u32, h: u32) -> Vec<(u32, u32)> {
        (0..w * h).filter(|i| rgb[*i as usize * 3] > 0.5).map(|i| (i % w, i / w)).collect()
    }

    #[test]
    fn text_lands_in_the_chosen_corner_with_the_chosen_width() {
        let (w, h) = (400u32, 200u32);
        let mut rgb = vec![0f32; (w * h * 3) as usize];
        let wm = Watermark { kind: Kind::Text, text: "© Auteur".into(), width: 0.25, opacity: 1.0, ..Default::default() };
        apply(&wm, w, h, &mut rgb);
        let lit = lit(&rgb, w, h);
        assert!(!lit.is_empty());
        let (minx, maxx) = (lit.iter().map(|p| p.0).min().unwrap(), lit.iter().map(|p| p.0).max().unwrap());
        let miny = lit.iter().map(|p| p.1).min().unwrap();
        // En bas à droite, environ 100 px de large.
        assert!(minx > 250 && maxx < 400 && miny > 120, "{minx} {maxx} {miny}");
        assert!((80..=105).contains(&(maxx - minx)), "{}", maxx - minx);
    }

    #[test]
    fn nine_places_and_insets() {
        let (w, h) = (1000usize, 600usize);
        let wm = |place, ih, iv| Watermark { place, inset_h: ih, inset_v: iv, ..Default::default() };
        assert_eq!(origin(&wm(Place::TopLeft, 0.1, 0.05), w, h, 100, 50), (60, 30));
        assert_eq!(origin(&wm(Place::TopCenter, 0.1, 0.05), w, h, 100, 50), (450, 30));
        assert_eq!(origin(&wm(Place::Right, 0.1, 0.05), w, h, 100, 50), (840, 275));
        assert_eq!(origin(&wm(Place::BottomCenter, 0.0, 0.0), w, h, 100, 50), (450, 550));
        assert_eq!(origin(&wm(Place::BottomRight, 0.0, 0.0), w, h, 100, 50), (900, 550));
        // Signature plus grande que la photo : collée en haut à gauche, jamais hors cadre.
        assert_eq!(origin(&wm(Place::BottomRight, 0.1, 0.1), 50, 40, 100, 50), (0, 0));
    }

    #[test]
    fn color_shadow_and_blend() {
        let (w, h) = (300u32, 100u32);
        // Texte rouge sans ombre sur fond gris : seuls des pixels rouges apparaissent.
        let mut rgb = vec![0.5f32; (w * h * 3) as usize];
        let wm = Watermark { kind: Kind::Text, text: "Signature".into(), width: 0.5, opacity: 1.0, color: Some([1.0, 0.0, 0.0]), shadow: 0.0, place: Place::Center, ..Default::default() };
        apply(&wm, w, h, &mut rgb);
        let red = rgb.chunks(3).filter(|p| p[0] > 0.9 && p[1] < 0.1).count();
        let dark = rgb.chunks(3).filter(|p| p[0] < 0.4).count();
        assert!(red > 50 && dark == 0, "{red} {dark}");
        // Avec ombre : claire sous un texte foncé (le vert monte), foncée sous un texte clair.
        let mut rgb = vec![0.5f32; (w * h * 3) as usize];
        apply(&Watermark { shadow: 1.0, ..wm.clone() }, w, h, &mut rgb);
        assert!(rgb.chunks(3).any(|p| p[1] > 0.6));
        let mut rgb = vec![0.5f32; (w * h * 3) as usize];
        apply(&Watermark { shadow: 1.0, color: Some([1.0; 3]), ..wm.clone() }, w, h, &mut rgb);
        assert!(rgb.chunks(3).any(|p| p[0] < 0.4));
        // Produit : un texte blanc sur fond gris ne change rien.
        let mut rgb = vec![0.5f32; (w * h * 3) as usize];
        apply(&Watermark { color: Some([1.0; 3]), blend: Blend::Multiply, ..wm }, w, h, &mut rgb);
        assert!(rgb.iter().all(|v| (v - 0.5).abs() < 1e-5));
    }

    #[test]
    fn overlay_matches_the_export() {
        // Posé sur un fond noir, l'aperçu donne la même image que l'export.
        let (w, h) = (300u32, 120u32);
        let wm = Watermark { kind: Kind::Text, text: "Aperçu".into(), width: 0.4, opacity: 0.8, color: Some([1.0, 0.8, 0.2]), shadow: 0.6, place: Place::Right, ..Default::default() };
        let mut rgb = vec![0f32; (w * h * 3) as usize];
        apply(&wm, w, h, &mut rgb);
        let (x0, y0, rw, rh, px) = overlay(&wm, w, h).unwrap();
        assert!(x0 + rw <= w as usize && y0 + rh <= h as usize);
        let mut seen = 0;
        for y in 0..rh {
            for x in 0..rw {
                let p = &px[(y * rw + x) * 4..(y * rw + x) * 4 + 4];
                let a = p[3] as f32 / 255.0;
                let i = ((y0 + y) * w as usize + x0 + x) * 3;
                for k in 0..3 {
                    assert!((p[k] as f32 / 255.0 * a - rgb[i + k]).abs() < 0.02, "{x} {y}");
                }
                seen += usize::from(a > 0.0);
            }
        }
        assert!(seen > 100);
        // Rien ailleurs : tout l'export lumineux est dans le rectangle de l'aperçu.
        let outside = (0..(w * h) as usize).filter(|i| {
            let (x, y) = (i % w as usize, i / w as usize);
            !(x0..x0 + rw).contains(&x) || !(y0..y0 + rh).contains(&y)
        });
        assert!(outside.into_iter().all(|i| rgb[i * 3] == 0.0));
    }

    #[test]
    fn old_settings_still_load() {
        let old = r#"{"kind":"Text","text":"x","image":null,"place":"Center","width":0.3,"opacity":0.5,"white":false}"#;
        let wm: Watermark = serde_json::from_str(old).unwrap();
        assert_eq!(wm.place, Place::Center);
        assert_eq!(wm.text_color(), [0.0; 3]);
        assert_eq!(wm.blend, Blend::Normal);
    }
}
