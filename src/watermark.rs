//! Filigrane de l'export : un texte ou une image (logo PNG) posé dans un coin ou au centre.

use std::path::PathBuf;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
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
    TopRight,
    Center,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watermark {
    pub kind: Kind,
    pub text: String,
    pub image: Option<PathBuf>,
    pub place: Place,
    /// Largeur du filigrane, en part de la largeur de la photo.
    pub width: f32,
    pub opacity: f32,
    /// Texte blanc (sinon noir).
    pub white: bool,
}

impl Default for Watermark {
    fn default() -> Self {
        Self { kind: Kind::None, text: String::new(), image: None, place: Place::BottomRight, width: 0.2, opacity: 0.6, white: true }
    }
}

/// Couverture (0..1) et couleur d'un filigrane prêt à poser.
struct Mark {
    w: usize,
    h: usize,
    alpha: Vec<f32>,
    rgb: Vec<[f32; 3]>,
}

fn text_mark(text: &str, width: f32, white: bool) -> Option<Mark> {
    let font = FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).ok()?;
    let text = text.trim();
    if text.is_empty() || width < 4.0 {
        return None;
    }
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
    let c = if white { 1.0 } else { 0.0 };
    Some(Mark { w, h, alpha, rgb: vec![[c; 3]; w * h] })
}

fn image_mark(path: &std::path::Path, width: f32) -> Option<Mark> {
    let img = image::open(path).ok()?.to_rgba8();
    let w = (width.round() as u32).max(1);
    let h = ((img.height() as f32 * w as f32 / img.width().max(1) as f32).round() as u32).max(1);
    let img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3);
    let px = img.pixels();
    let (alpha, rgb) = px.map(|p| (p[3] as f32 / 255.0, [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0])).unzip();
    Some(Mark { w: w as usize, h: h as usize, alpha, rgb })
}

/// Pose le filigrane sur `rgb` (valeurs déjà encodées dans l'espace de sortie, 0..1).
pub fn apply(wm: &Watermark, w: u32, h: u32, rgb: &mut [f32]) {
    let target = wm.width.clamp(0.02, 0.9) * w as f32;
    let mark = match wm.kind {
        Kind::None => return,
        Kind::Text => text_mark(&wm.text, target, wm.white),
        Kind::Image => wm.image.as_deref().and_then(|p| image_mark(p, target)),
    };
    let Some(m) = mark else { return };
    let (w, h) = (w as usize, h as usize);
    let margin = (0.03 * w.min(h) as f32).round() as usize;
    let (x0, y0) = match wm.place {
        Place::TopLeft => (margin, margin),
        Place::TopRight => (w.saturating_sub(m.w + margin), margin),
        Place::Center => (w.saturating_sub(m.w) / 2, h.saturating_sub(m.h) / 2),
        Place::BottomLeft => (margin, h.saturating_sub(m.h + margin)),
        Place::BottomRight => (w.saturating_sub(m.w + margin), h.saturating_sub(m.h + margin)),
    };
    let op = wm.opacity.clamp(0.0, 1.0);
    if wm.kind == Kind::Text {
        // Ombre légère de la couleur opposée, pour rester lisible sur un fond clair ou foncé.
        let d = ((m.h as f32) * 0.05).round().max(1.0) as usize;
        let c = if wm.white { 0.0 } else { 1.0 };
        paint(&m, rgb, w, h, x0 + d, y0 + d, op * 0.35, Some([c; 3]));
    }
    paint(&m, rgb, w, h, x0, y0, op, None);
}

#[allow(clippy::too_many_arguments)]
fn paint(m: &Mark, rgb: &mut [f32], w: usize, h: usize, x0: usize, y0: usize, op: f32, color: Option<[f32; 3]>) {
    for y in 0..m.h.min(h.saturating_sub(y0)) {
        for x in 0..m.w.min(w.saturating_sub(x0)) {
            let a = m.alpha[y * m.w + x] * op;
            if a <= 0.0 {
                continue;
            }
            let c = color.unwrap_or(m.rgb[y * m.w + x]);
            let i = ((y0 + y) * w + x0 + x) * 3;
            for k in 0..3 {
                rgb[i + k] = rgb[i + k] * (1.0 - a) + c[k] * a;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_lands_in_the_chosen_corner_with_the_chosen_width() {
        let (w, h) = (400u32, 200u32);
        let mut rgb = vec![0f32; (w * h * 3) as usize];
        let wm = Watermark { kind: Kind::Text, text: "© Auteur".into(), width: 0.25, opacity: 1.0, ..Default::default() };
        apply(&wm, w, h, &mut rgb);
        let lit: Vec<(u32, u32)> = (0..w * h).filter(|i| rgb[*i as usize * 3] > 0.5).map(|i| (i % w, i / w)).collect();
        assert!(!lit.is_empty());
        let (minx, maxx) = (lit.iter().map(|p| p.0).min().unwrap(), lit.iter().map(|p| p.0).max().unwrap());
        let miny = lit.iter().map(|p| p.1).min().unwrap();
        // En bas à droite, environ 100 px de large.
        assert!(minx > 250 && maxx < 400 && miny > 120, "{minx} {maxx} {miny}");
        assert!((80..=105).contains(&(maxx - minx)), "{}", maxx - minx);
    }
}
