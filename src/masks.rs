//! Rasterisation des masques locaux (pinceau, dégradés) sur le CPU, à résolution réduite.
//! Le GPU les lit dans une texture à couches et les interpole.

use crate::adjust::{BrushStroke, Local, Shape};

/// Côté maximal des masques (le GPU les interpole jusqu'à la pleine résolution).
pub const MASK_MAX: u32 = 2048;

/// Taille des masques pour une photo `w`x`h`.
pub fn mask_size(w: u32, h: u32) -> (u32, u32) {
    let s = (MASK_MAX as f32 / w.max(h) as f32).min(1.0);
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Applique des coups de pinceau sur `buf` (valeurs 0..1).
pub fn paint_strokes(buf: &mut [f32], mw: u32, mh: u32, strokes: &[BrushStroke]) {
    for s in strokes {
        let r = (s.radius * mw as f32).max(0.5);
        let inner = s.hardness.clamp(0.0, 0.99);
        // Tampons espacés d'un dixième de rayon le long de la polyligne.
        let step = (r * 0.1).max(0.5);
        let mut dabs: Vec<[f32; 2]> = Vec::new();
        let px = |p: &[f32; 2]| [p[0] * mw as f32, p[1] * mh as f32];
        if let Some(first) = s.points.first() {
            dabs.push(px(first));
        }
        let mut outline: Vec<[f32; 2]> = s.points.clone();
        if s.fill && s.points.len() >= 3 {
            fill_polygon(buf, mw, mh, &s.points.iter().map(px).collect::<Vec<_>>(), s.flow.clamp(0.0, 1.0), s.erase);
            // Contour fermé, adouci par le rayon.
            outline.push(s.points[0]);
        }
        for w in outline.windows(2) {
            let (a, b) = (px(&w[0]), px(&w[1]));
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let n = (len / step).ceil() as usize;
            for k in 1..=n {
                let t = k as f32 / n as f32;
                dabs.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
        // Chaque tampon ne dépose qu'une fraction de l'opacité, pour un trait régulier.
        let flow = 1.0 - (1.0 - s.flow.clamp(0.0, 1.0)).powf(step / r);
        for d in dabs {
            let (x0, x1) = (((d[0] - r).floor().max(0.0)) as u32, ((d[0] + r).ceil().min(mw as f32 - 1.0)) as i64);
            let (y0, y1) = (((d[1] - r).floor().max(0.0)) as u32, ((d[1] + r).ceil().min(mh as f32 - 1.0)) as i64);
            for y in y0 as i64..=y1 {
                for x in x0 as i64..=x1 {
                    let dist = (((x as f32 + 0.5 - d[0]).powi(2) + (y as f32 + 0.5 - d[1]).powi(2)).sqrt()) / r;
                    if dist >= 1.0 {
                        continue;
                    }
                    let a = flow * (1.0 - smoothstep(inner, 1.0, dist));
                    let v = &mut buf[(y as u32 * mw + x as u32) as usize];
                    *v = if s.erase { *v * (1.0 - a) } else { *v + (1.0 - *v) * a };
                }
            }
        }
    }
}

/// Remplit un polygone (en pixels) par la règle pair-impair, sur les centres des pixels.
pub(crate) fn fill_polygon(buf: &mut [f32], mw: u32, mh: u32, poly: &[[f32; 2]], a: f32, erase: bool) {
    let y0 = poly.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor().max(0.0) as u32;
    let y1 = (poly.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil() as i64).min(mh as i64 - 1);
    let mut xs = Vec::new();
    for y in y0 as i64..=y1 {
        let cy = y as f32 + 0.5;
        xs.clear();
        for i in 0..poly.len() {
            let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
            if (p[1] <= cy) != (q[1] <= cy) {
                xs.push(p[0] + (cy - p[1]) / (q[1] - p[1]) * (q[0] - p[0]));
            }
        }
        xs.sort_by(f32::total_cmp);
        for pair in xs.chunks(2) {
            let [l, r] = pair else { continue };
            let xa = (l - 0.5).ceil().max(0.0) as u32;
            let xb = ((r - 0.5).floor() as i64).min(mw as i64 - 1);
            for x in xa as i64..=xb {
                let v = &mut buf[(y as u32 * mw + x as u32) as usize];
                *v = if erase { *v * (1.0 - a) } else { *v + (1.0 - *v) * a };
            }
        }
    }
}

/// Masque d'un réglage local, `mw`x`mh` octets. `ai` fournit le masque IA déjà calculé
/// (à la même taille) pour les formes `Shape::Ai`.
pub fn rasterize(local: &Local, mw: u32, mh: u32, ai: Option<&[f32]>) -> Vec<u8> {
    let n = (mw * mh) as usize;
    let mut buf = vec![0.0f32; n];
    let (fw, fh) = (mw as f32, mh as f32);
    match &local.shape {
        Shape::Brush { strokes } => paint_strokes(&mut buf, mw, mh, strokes),
        Shape::Linear { a, b } => {
            let (ax, ay, bx, by) = (a[0] * fw, a[1] * fh, b[0] * fw, b[1] * fh);
            let (dx, dy) = (bx - ax, by - ay);
            let l2 = (dx * dx + dy * dy).max(1e-6);
            for y in 0..mh {
                for x in 0..mw {
                    let t = ((x as f32 + 0.5 - ax) * dx + (y as f32 + 0.5 - ay) * dy) / l2;
                    buf[(y * mw + x) as usize] = 1.0 - smoothstep(0.0, 1.0, t);
                }
            }
        }
        Shape::Radial { center, radius, angle, feather } => {
            let (cx, cy) = (center[0] * fw, center[1] * fh);
            let (rx, ry) = ((radius[0] * fw).max(1.0), (radius[1] * fw).max(1.0));
            let (s, c) = angle.to_radians().sin_cos();
            let f = feather.clamp(0.0, 1.0);
            for y in 0..mh {
                for x in 0..mw {
                    let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                    let (u, v) = (c * px + s * py, -s * px + c * py);
                    let d = ((u / rx).powi(2) + (v / ry).powi(2)).sqrt();
                    buf[(y * mw + x) as usize] = 1.0 - smoothstep(1.0 - f, 1.0 + 1e-3, d);
                }
            }
        }
        Shape::Ai { kind, strokes } => {
            if let Some(m) = ai.filter(|m| m.len() == n) {
                // L'arrière-plan est l'inverse du masque du sujet.
                let bg = *kind == crate::adjust::AiKind::Background;
                for (d, v) in buf.iter_mut().zip(m) {
                    *d = if bg { 1.0 - v } else { *v };
                }
            }
            // Retouches au pinceau du masque IA (ajout / gomme).
            paint_strokes(&mut buf, mw, mh, strokes);
        }
    }
    let amount = local.amount.clamp(0.0, 1.0);
    buf.iter()
        .map(|v| {
            let v = if local.invert { 1.0 - v } else { *v };
            (v * amount * 255.0 + 0.5) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_gradient_goes_from_full_to_none() {
        let l = Local { shape: Shape::Linear { a: [0.5, 0.0], b: [0.5, 1.0] }, ..Default::default() };
        let m = rasterize(&l, 4, 100, None);
        assert!(m[0] > 250 && m[99 * 4] < 5);
    }

    #[test]
    fn lasso_fills_its_inside_only() {
        // Triangle à gauche, rectangle à droite ; un clic (moins de 3 points) ne remplit rien.
        let tri = BrushStroke { points: vec![[0.1, 0.1], [0.4, 0.1], [0.1, 0.9]], radius: 0.01, hardness: 0.9, flow: 1.0, erase: false, fill: true };
        let rect = BrushStroke { points: vec![[0.6, 0.2], [0.9, 0.2], [0.9, 0.8], [0.6, 0.8]], ..tri.clone() };
        let mut m = vec![0f32; 100 * 100];
        paint_strokes(&mut m, 100, 100, &[tri, rect]);
        let at = |x: usize, y: usize| m[y * 100 + x];
        assert!(at(15, 20) > 0.99 && at(75, 50) > 0.99);
        assert!(at(35, 80) < 0.01 && at(50, 50) < 0.01 && at(75, 90) < 0.01);
    }

    #[test]
    fn brush_paints_and_erases() {
        let s = |erase| BrushStroke { points: vec![[0.5, 0.5]], radius: 0.2, hardness: 0.5, flow: 1.0, erase, fill: false };
        let l = Local { shape: Shape::Brush { strokes: vec![s(false)] }, ..Default::default() };
        let m = rasterize(&l, 50, 50, None);
        assert!(m[25 * 50 + 25] > 250 && m[0] == 0);
        let l = Local { shape: Shape::Brush { strokes: vec![s(false), s(true)] }, ..Default::default() };
        assert!(rasterize(&l, 50, 50, None)[25 * 50 + 25] < 5);
    }
}
