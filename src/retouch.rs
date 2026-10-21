//! Retouches de l'image source, appliquées dans l'ordre avant le développement :
//! correcteur de taches (texture prise à côté, raccord de luminosité par un plan ajusté sur le pourtour)
//! et gomme IA (LaMa affinée par des morceaux de la photo ; ces morceaux seuls si le modèle manque).
//! Les résultats de la gomme sont gardés en cache, car le calcul prend plusieurs secondes.

use crate::adjust::{BrushStroke, Retouch};
use anyhow::Result;
use half::f16;
use std::path::{Path, PathBuf};

/// Image RGBA modifiable, en f16 (éditeur) ou en f32 (export).
pub trait Canvas {
    fn size(&self) -> (u32, u32);
    fn get(&self, x: i32, y: i32) -> [f32; 3];
    fn set(&mut self, x: i32, y: i32, v: [f32; 3]);

    /// Lecture bilinéaire.
    fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let (x0, y0) = (x.floor(), y.floor());
        let (tx, ty) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let (a, b, c, d) = (self.get(x0, y0), self.get(x0 + 1, y0), self.get(x0, y0 + 1), self.get(x0 + 1, y0 + 1));
        std::array::from_fn(|i| {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bot = c[i] + (d[i] - c[i]) * tx;
            top + (bot - top) * ty
        })
    }
}

pub struct HalfCanvas<'a> {
    pub data: &'a mut [f16],
    pub w: u32,
    pub h: u32,
}

impl Canvas for HalfCanvas<'_> {
    fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        let i = (y.clamp(0, self.h as i32 - 1) as usize * self.w as usize + x.clamp(0, self.w as i32 - 1) as usize) * 4;
        [self.data[i].to_f32(), self.data[i + 1].to_f32(), self.data[i + 2].to_f32()]
    }
    fn set(&mut self, x: i32, y: i32, v: [f32; 3]) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        for c in 0..3 {
            self.data[i + c] = f16::from_f32(v[c].max(0.0));
        }
    }
}

pub struct F32Canvas<'a> {
    pub data: &'a mut [[f32; 4]],
    pub w: u32,
    pub h: u32,
}

impl Canvas for F32Canvas<'_> {
    fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        let p = self.data[y.clamp(0, self.h as i32 - 1) as usize * self.w as usize + x.clamp(0, self.w as i32 - 1) as usize];
        [p[0], p[1], p[2]]
    }
    fn set(&mut self, x: i32, y: i32, v: [f32; 3]) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let p = &mut self.data[y as usize * self.w as usize + x as usize];
        p[..3].copy_from_slice(&v.map(|c| c.max(0.0)));
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Points d'un anneau autour de `c` (rayons r0..r1), pour comparer des voisinages.
fn ring(c: [f32; 2], r0: f32, r1: f32) -> Vec<[f32; 2]> {
    let mut v = Vec::new();
    for k in 0..3 {
        let r = r0 + (r1 - r0) * (k as f32 + 0.5) / 3.0;
        let n = ((r * std::f32::consts::TAU / 2.0) as usize).clamp(12, 96);
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            v.push([c[0] + r * a.cos(), c[1] + r * a.sin()]);
        }
    }
    v
}

/// Source automatique d'une tache : la zone voisine dont le pourtour ressemble le plus à celui de la tache.
pub fn auto_offset(img: &impl Canvas, c: [f32; 2], r: f32) -> [f32; 2] {
    let (w, h) = img.size();
    let around = ring(c, r * 1.05, r * 1.6);
    let reference: Vec<[f32; 3]> = around.iter().map(|p| img.sample(p[0], p[1]).map(|v| v.max(0.0).sqrt())).collect();
    // Intérieur de la source : grille de points du disque, pour écarter une source qui contient
    // elle-même une tache (ou un détail) absent de son pourtour.
    let step = (r / 6.0).max(1.0);
    let n = (r / step).ceil() as i32;
    let disc: Vec<[f32; 2]> = (-n..=n)
        .flat_map(|j| (-n..=n).map(move |i| [i as f32 * step, j as f32 * step]))
        .filter(|d| d[0].hypot(d[1]) < r)
        .map(|d| [c[0] + d[0], c[1] + d[1]])
        .collect();
    // Sans source possible entièrement dans l'image : vers le centre de l'image, dans l'image.
    let fallback = {
        let (dx, dy) = (w as f32 * 0.5 - c[0], h as f32 * 0.5 - c[1]);
        let d = dx.hypot(dy).max(1e-3);
        let s = [(c[0] + dx / d * 2.2 * r).clamp(0.0, w as f32), (c[1] + dy / d * 2.2 * r).clamp(0.0, h as f32)];
        [s[0] - c[0], s[1] - c[1]]
    };
    let mut best = (fallback, f32::MAX);
    for dist in [2.2f32, 3.0, 4.0] {
        for i in 0..24 {
            let a = i as f32 / 24.0 * std::f32::consts::TAU;
            let o = [a.cos() * dist * r, a.sin() * dist * r];
            let s = [c[0] + o[0], c[1] + o[1]];
            // La source doit être entièrement dans l'image.
            if s[0] - 1.6 * r < 0.0 || s[1] - 1.6 * r < 0.0 || s[0] + 1.6 * r > w as f32 || s[1] + 1.6 * r > h as f32 {
                continue;
            }
            let mut err = 0.0;
            let (mut sum, mut sum2) = ([0f32; 3], 0f32);
            for (p, q) in around.iter().zip(&reference) {
                let v = img.sample(p[0] + o[0], p[1] + o[1]).map(|v| v.max(0.0).sqrt());
                err += (0..3).map(|k| (v[k] - q[k]).powi(2)).sum::<f32>();
                for k in 0..3 {
                    sum[k] += v[k];
                }
                sum2 += v.iter().map(|x| x * x).sum::<f32>();
            }
            let nr = around.len().max(1) as f32;
            let mean = sum.map(|x| x / nr);
            let ring_var = sum2 / nr - mean.iter().map(|x| x * x).sum::<f32>();
            let inner_var = disc
                .iter()
                .map(|p| {
                    let v = img.sample(p[0] + o[0], p[1] + o[1]).map(|v| v.max(0.0).sqrt());
                    (0..3).map(|k| (v[k] - mean[k]).powi(2)).sum::<f32>()
                })
                .sum::<f32>()
                / disc.len().max(1) as f32;
            let mut err = err / nr + 2.0 * (inner_var - ring_var).max(0.0);
            // Légère préférence pour les sources proches (éclairage semblable).
            err *= 1.0 + 0.05 * dist;
            if err < best.1 {
                best = (o, err);
            }
        }
    }
    best.0
}

/// Correcteur : texture prise en `c + o`, raccordée à l'entourage de la tache par un plan
/// (différence moyenne et gradient) ajusté sur l'anneau qui l'entoure.
pub fn heal(img: &mut impl Canvas, c: [f32; 2], r: f32, o: [f32; 2]) {
    // Plan d = a + b x + c y par canal, moindres carrés sur l'anneau (coordonnées relatives au centre).
    let pts = ring(c, r * 1.0, r * 1.4);
    let mut ata = [[0f64; 3]; 3];
    let mut atb = [[0f64; 3]; 3];
    for p in &pts {
        let d = img.sample(p[0], p[1]);
        let s = img.sample(p[0] + o[0], p[1] + o[1]);
        let row = [1.0, ((p[0] - c[0]) / r) as f64, ((p[1] - c[1]) / r) as f64];
        for i in 0..3 {
            for j in 0..3 {
                ata[i][j] += row[i] * row[j];
            }
            for ch in 0..3 {
                atb[ch][i] += row[i] * (d[ch] - s[ch]) as f64;
            }
        }
    }
    let plane: Vec<[f64; 3]> = (0..3).map(|ch| solve3(ata, atb[ch]).unwrap_or([0.0; 3])).collect();
    let (x0, x1) = ((c[0] - r).floor() as i32, (c[0] + r).ceil() as i32);
    let (y0, y1) = ((c[1] - r).floor() as i32, (c[1] + r).ceil() as i32);
    // On lit tout avant d'écrire (la source peut chevaucher la tache).
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((fx - c[0]).powi(2) + (fy - c[1]).powi(2)).sqrt() / r;
            if d >= 1.0 {
                continue;
            }
            let a = 1.0 - smoothstep(0.65, 1.0, d);
            let dst = img.get(x, y);
            let src = img.sample(fx - 0.5 + o[0], fy - 0.5 + o[1]);
            let (u, v) = (((fx - c[0]) / r) as f64, ((fy - c[1]) / r) as f64);
            let healed: [f32; 3] = std::array::from_fn(|ch| (src[ch] as f64 + plane[ch][0] + plane[ch][1] * u + plane[ch][2] * v).max(0.0) as f32);
            out.push((x, y, std::array::from_fn(|ch| dst[ch] + (healed[ch] - dst[ch]) * a)));
        }
    }
    for (x, y, v) in out {
        img.set(x, y, v);
    }
}

fn solve3(m: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-9 {
        return None;
    }
    Some(std::array::from_fn(|k| {
        let mut mk = m;
        for i in 0..3 {
            mk[i][k] = b[i];
        }
        det(mk) / d
    }))
}

// ---------- Gomme ----------

/// Résultat d'une gomme : rectangle (normalisé) et pixels RGBA linéaires (alpha = masque adouci).
pub struct Patch {
    pub rect: [f32; 4],
    pub w: u32,
    pub h: u32,
    pub px: Vec<[f32; 4]>,
}

/// Masque des traits dans un rectangle en pixels, et le rectangle (agrandi pour donner du contexte).
pub(crate) fn erase_area(strokes: &[BrushStroke], w: u32, h: u32) -> Option<([i32; 4], Vec<f32>)> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in strokes.iter().filter(|s| !s.erase) {
        let r = s.radius * w as f32;
        for p in &s.points {
            let (x, y) = (p[0] * w as f32, p[1] * h as f32);
            x0 = x0.min(x - r);
            y0 = y0.min(y - r);
            x1 = x1.max(x + r);
            y1 = y1.max(y + r);
        }
    }
    if x0 > x1 {
        return None;
    }
    // Contexte autour de l'objet : la moitié de sa taille, 48 px au moins, 8 % du côté de la photo au plus
    // (de longs traits fins, des branches, donneraient sinon une zone de plusieurs millions de pixels).
    let pad = ((x1 - x0).max(y1 - y0) * 0.5).clamp(48.0, (w.max(h) as f32 * 0.08).max(48.0));
    let r = [
        ((x0 - pad).floor() as i32).max(0),
        ((y0 - pad).floor() as i32).max(0),
        ((x1 + pad).ceil() as i32).min(w as i32),
        ((y1 + pad).ceil() as i32).min(h as i32),
    ];
    let (rw, rh) = ((r[2] - r[0]).max(1) as u32, (r[3] - r[1]).max(1) as u32);
    // Rasterisation des traits dans le repère du rectangle.
    let local: Vec<BrushStroke> = strokes
        .iter()
        .map(|s| BrushStroke {
            points: s.points.iter().map(|p| [(p[0] * w as f32 - r[0] as f32) / rw as f32, (p[1] * h as f32 - r[1] as f32) / rh as f32]).collect(),
            radius: s.radius * w as f32 / rw as f32,
            hardness: 0.9,
            flow: 1.0,
            erase: s.erase,
            fill: s.fill,
        })
        .collect();
    let mut m = vec![0f32; (rw * rh) as usize];
    crate::masks::paint_strokes(&mut m, rw, rh, &local);
    Some((r, m))
}

pub(crate) fn to_srgb(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// Calcule la gomme sur l'image (dans son état courant). `use_ai` : LaMa si possible.
pub fn compute_erase(img: &impl Canvas, strokes: &[BrushStroke], use_ai: bool) -> Result<(Patch, bool)> {
    let (w, h) = img.size();
    let (r, mask) = erase_area(strokes, w, h).ok_or_else(|| anyhow::anyhow!("aucun trait"))?;
    let (rw, rh) = ((r[2] - r[0]) as u32, (r[3] - r[1]) as u32);
    let crop: Vec<[f32; 4]> = (0..rh as i32)
        .flat_map(|y| (0..rw as i32).map(move |x| (x, y)))
        .map(|(x, y)| {
            let v = img.get(r[0] + x, r[1] + y);
            [v[0], v[1], v[2], 1.0]
        })
        .collect();
    // Masque élargi de quelques pixels, selon la taille du pinceau : les bords de l'objet disparaissent aussi.
    let brush = strokes.iter().filter(|s| !s.erase).map(|s| s.radius * w as f32).fold(0.0, f32::max);
    let hole: Vec<f32> = dilate(&mask, rw, rh, ((brush * 0.15).ceil() as usize).clamp(2, 12)).iter().map(|v| if *v > 0.05 { 1.0 } else { 0.0 }).collect();
    let mut filled = None;
    let mut used_ai = false;
    if use_ai {
        match lama(&crop, &hole, rw, rh) {
            Ok(f) => {
                filled = Some(f);
                used_ai = true;
            }
            Err(e) => eprintln!("gomme IA indisponible, texture voisine : {e:#}"),
        }
    }
    let filled = match (filled, use_ai) {
        // LaMa travaille en 512x512 : sa texture, trop lisse une fois agrandie, est remplacée
        // par des morceaux de la photo choisis d'après son résultat.
        (Some(f), _) => crate::inpaint::fill(&crop, &hole, rw, rh, Some(&f)),
        // LaMa indisponible : texture prise autour du trou.
        (None, true) => crate::inpaint::fill(&crop, &hole, rw, rh, None),
        // « Fond uni » : remplissage lisse et rapide.
        (None, false) => diffuse(&crop, &hole, rw, rh),
    };
    // Alpha : masque adouci pour fondre le raccord.
    let soft = blur(&hole, rw, rh, 2);
    let px = filled.iter().zip(&soft).map(|(c, a)| [c[0], c[1], c[2], a.max(0.0).min(1.0)]).collect();
    let rect = [r[0] as f32 / w as f32, r[1] as f32 / h as f32, rw as f32 / w as f32, rh as f32 / h as f32];
    Ok((Patch { rect, w: rw, h: rh, px }, used_ai))
}

pub(crate) fn dilate(m: &[f32], w: u32, h: u32, r: usize) -> Vec<f32> {
    let (w, h) = (w as usize, h as usize);
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            tmp[y * w + x] = (x.saturating_sub(r)..(x + r + 1).min(w)).map(|xx| m[y * w + xx]).fold(0.0, f32::max);
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = (y.saturating_sub(r)..(y + r + 1).min(h)).map(|yy| tmp[yy * w + x]).fold(0.0, f32::max);
        }
    }
    out
}

pub(crate) fn blur(m: &[f32], w: u32, h: u32, r: usize) -> Vec<f32> {
    let (w, h) = (w as usize, h as usize);
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r + 1).min(w));
            tmp[y * w + x] = (a..b).map(|xx| m[y * w + xx]).sum::<f32>() / (b - a) as f32;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (y.saturating_sub(r), (y + r + 1).min(h));
            out[y * w + x] = (a..b).map(|yy| tmp[yy * w + x]).sum::<f32>() / (b - a) as f32;
        }
    }
    out
}

/// Remplissage lisse du trou (diffusion multi-échelle) : solution de repli sans modèle.
fn diffuse(img: &[[f32; 4]], hole: &[f32], w: u32, h: u32) -> Vec<[f32; 4]> {
    if w <= 8 || h <= 8 {
        let n = hole.iter().filter(|v| **v < 0.5).count().max(1) as f32;
        let mut mean = [0f32; 4];
        for (p, m) in img.iter().zip(hole) {
            if *m < 0.5 {
                for c in 0..3 {
                    mean[c] += p[c] / n;
                }
            }
        }
        return img.iter().zip(hole).map(|(p, m)| if *m > 0.5 { [mean[0], mean[1], mean[2], 1.0] } else { *p }).collect();
    }
    // Niveau grossier : estimation de départ.
    let (sw, sh) = (w / 2, h / 2);
    let small: Vec<[f32; 4]> = (0..sh).flat_map(|y| (0..sw).map(move |x| (x, y))).map(|(x, y)| {
        let i = |xx: u32, yy: u32| ((yy.min(h - 1)) * w + xx.min(w - 1)) as usize;
        let ids = [i(2 * x, 2 * y), i(2 * x + 1, 2 * y), i(2 * x, 2 * y + 1), i(2 * x + 1, 2 * y + 1)];
        let known: Vec<usize> = ids.iter().copied().filter(|k| hole[*k] < 0.5).collect();
        if known.is_empty() { [0.0; 4] } else { std::array::from_fn(|c| known.iter().map(|k| img[*k][c]).sum::<f32>() / known.len() as f32) }
    }).collect();
    let small_hole: Vec<f32> = (0..sh).flat_map(|y| (0..sw).map(move |x| (x, y))).map(|(x, y)| {
        let i = |xx: u32, yy: u32| ((yy.min(h - 1)) * w + xx.min(w - 1)) as usize;
        if [i(2 * x, 2 * y), i(2 * x + 1, 2 * y), i(2 * x, 2 * y + 1), i(2 * x + 1, 2 * y + 1)].iter().all(|k| hole[*k] > 0.5) { 1.0 } else { 0.0 }
    }).collect();
    let coarse = diffuse(&small, &small_hole, sw, sh);
    let mut out: Vec<[f32; 4]> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| {
        let k = (y * w + x) as usize;
        if hole[k] > 0.5 { coarse[((y / 2).min(sh - 1) * sw + (x / 2).min(sw - 1)) as usize] } else { img[k] }
    }).collect();
    // Lissage (Gauss-Seidel) dans le trou seulement.
    for _ in 0..40 {
        for y in 0..h as usize {
            for x in 0..w as usize {
                let k = y * w as usize + x;
                if hole[k] < 0.5 {
                    continue;
                }
                let mut acc = [0f32; 3];
                let mut n = 0.0;
                for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                    let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                    if xx >= 0 && yy >= 0 && xx < w as i32 && yy < h as i32 {
                        let p = out[yy as usize * w as usize + xx as usize];
                        for c in 0..3 {
                            acc[c] += p[c];
                        }
                        n += 1.0;
                    }
                }
                out[k] = [acc[0] / n, acc[1] / n, acc[2] / n, 1.0];
            }
        }
    }
    out
}

/// LaMa sur le rectangle ramené à 512x512 ; la partie remplie est remise à l'échelle,
/// le reste de l'image n'est pas touché.
fn lama(crop: &[[f32; 4]], hole: &[f32], w: u32, h: u32) -> Result<Vec<[f32; 4]>> {
    crate::ai::ensure_model(crate::ai::Model::Inpaint, &Default::default())?;
    let s = 512u32;
    let to = |x: u32, y: u32, sw: u32, sh: u32| ((x as f32 + 0.5) * sw as f32 / s as f32 - 0.5, (y as f32 + 0.5) * sh as f32 / s as f32 - 0.5);
    let (mut rgb, mut msk) = (vec![0f32; 3 * 512 * 512], vec![0f32; 512 * 512]);
    let lin_canvas = F32Ref { data: crop, w, h };
    for y in 0..s {
        for x in 0..s {
            let (fx, fy) = to(x, y, w, h);
            let v = lin_canvas.sample(fx, fy);
            let k = (y * s + x) as usize;
            for c in 0..3 {
                rgb[c * 512 * 512 + k] = to_srgb(v[c]);
            }
            let (mx, my) = (fx.round().clamp(0.0, (w - 1) as f32) as u32, fy.round().clamp(0.0, (h - 1) as f32) as u32);
            // Un pixel réduit est un trou s'il recouvre une partie du trou d'origine.
            let r = ((w.max(h) as f32 / 512.0).ceil() as i32).max(1);
            let mut any = false;
            for dy in -r..=r {
                for dx in -r..=r {
                    let (xx, yy) = ((mx as i32 + dx).clamp(0, w as i32 - 1), (my as i32 + dy).clamp(0, h as i32 - 1));
                    any |= hole[(yy as u32 * w + xx as u32) as usize] > 0.5;
                }
            }
            msk[k] = if any { 1.0 } else { 0.0 };
        }
    }
    let out = crate::ai::inpaint_512(rgb, msk)?;
    let plane = (s * s) as usize;
    let out_canvas = PlanarRef { data: &out, s };
    Ok((0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| {
        let k = (y * w + x) as usize;
        if hole[k] < 0.5 {
            return crop[k];
        }
        let fx = (x as f32 + 0.5) * s as f32 / w as f32 - 0.5;
        let fy = (y as f32 + 0.5) * s as f32 / h as f32 - 0.5;
        let v = out_canvas.sample(fx, fy);
        let _ = plane;
        [crate::image_io::srgb_to_linear(v[0]), crate::image_io::srgb_to_linear(v[1]), crate::image_io::srgb_to_linear(v[2]), 1.0]
    }).collect())
}

pub(crate) struct F32Ref<'a> {
    data: &'a [[f32; 4]],
    w: u32,
    h: u32,
}

impl Canvas for F32Ref<'_> {
    fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        let p = self.data[y.clamp(0, self.h as i32 - 1) as usize * self.w as usize + x.clamp(0, self.w as i32 - 1) as usize];
        [p[0], p[1], p[2]]
    }
    fn set(&mut self, _: i32, _: i32, _: [f32; 3]) {}
}

/// Sortie du réseau : trois plans 512x512.
struct PlanarRef<'a> {
    data: &'a [f32],
    s: u32,
}

impl Canvas for PlanarRef<'_> {
    fn size(&self) -> (u32, u32) {
        (self.s, self.s)
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        let n = (self.s * self.s) as usize;
        let k = y.clamp(0, self.s as i32 - 1) as usize * self.s as usize + x.clamp(0, self.s as i32 - 1) as usize;
        [self.data[k], self.data[n + k], self.data[2 * n + k]]
    }
    fn set(&mut self, _: i32, _: i32, _: [f32; 3]) {}
}

/// Pose une gomme calculée (à n'importe quelle résolution de l'image).
pub fn apply_patch(img: &mut impl Canvas, p: &Patch) {
    let (w, h) = img.size();
    let (x0, y0) = ((p.rect[0] * w as f32).round() as i32, (p.rect[1] * h as f32).round() as i32);
    let (rw, rh) = (((p.rect[2] * w as f32).round() as i32).max(1), ((p.rect[3] * h as f32).round() as i32).max(1));
    let patch = F32Ref { data: &p.px, w: p.w, h: p.h };
    for y in 0..rh {
        for x in 0..rw {
            let fx = (x as f32 + 0.5) * p.w as f32 / rw as f32 - 0.5;
            let fy = (y as f32 + 0.5) * p.h as f32 / rh as f32 - 0.5;
            let v = patch.sample(fx, fy);
            // Alpha au plus proche (bilinéaire sur l'alpha via un échantillon voisin).
            let (ax, ay) = (fx.round().clamp(0.0, (p.w - 1) as f32) as usize, fy.round().clamp(0.0, (p.h - 1) as f32) as usize);
            let a = p.px[ay * p.w as usize + ax][3];
            if a <= 0.0 {
                continue;
            }
            let d = img.get(x0 + x, y0 + y);
            img.set(x0 + x, y0 + y, std::array::from_fn(|c| d[c] + (v[c] - d[c]) * a));
        }
    }
}

// ---------- Cache des gommes ----------

/// Fichier de cache d'une gomme : dépend de la photo et de toutes les retouches jusqu'à elle.
pub fn cache_file(photo: &Path, ops: &[Retouch]) -> Option<PathBuf> {
    let photo = &crate::library::source(photo);
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(photo).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    std::fs::canonicalize(photo).unwrap_or_else(|_| photo.to_path_buf()).hash(&mut hs);
    mtime.hash(&mut hs);
    serde_json::to_string(ops).ok()?.hash(&mut hs);
    Some(crate::library::data_dir().join("retouches").join(format!("{:016x}.patch", hs.finish())))
}

pub fn cache_load(f: &Path) -> Option<Patch> {
    let b = std::fs::read(f).ok()?;
    if b.len() < 28 || &b[..4] != b"LXP1" {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let rect = [f32::from_bits(u(4)), f32::from_bits(u(8)), f32::from_bits(u(12)), f32::from_bits(u(16))];
    let (w, h) = (u(20), u(24));
    let data = &b[28..];
    if data.len() != (w * h * 8) as usize {
        return None;
    }
    let halfs: &[u16] = bytemuck::cast_slice(data);
    let px = halfs.chunks(4).map(|c| std::array::from_fn(|i| f16::from_bits(c[i]).to_f32())).collect();
    Some(Patch { rect, w, h, px })
}

pub fn cache_store(f: &Path, p: &Patch) -> Result<()> {
    std::fs::create_dir_all(f.parent().unwrap())?;
    let mut b = Vec::with_capacity(28 + p.px.len() * 8);
    b.extend_from_slice(b"LXP1");
    for v in p.rect {
        b.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    b.extend_from_slice(&p.w.to_le_bytes());
    b.extend_from_slice(&p.h.to_le_bytes());
    for px in &p.px {
        for c in px {
            b.extend_from_slice(&f16::from_f32(*c).to_bits().to_le_bytes());
        }
    }
    std::fs::write(f, b)?;
    Ok(())
}

/// Calcule la pièce d'une gomme ou d'une génération ; renvoie aussi un message pour la barre d'état.
pub fn compute_patch(img: &impl Canvas, op: &Retouch) -> Result<(Patch, String)> {
    match op {
        Retouch::Erase { strokes, smooth } => {
            let (p, ai) = compute_erase(img, strokes, !smooth)?;
            Ok((p, if *smooth { t!("Gomme appliquée (fond uni).").into() } else if ai { t!("Gomme IA appliquée.").into() } else { t!("Gomme appliquée (texture voisine : modèle IA indisponible).").into() }))
        }
        Retouch::Generate { strokes, model, prompt, negative, seed, strength } => {
            let set = crate::settings::Settings::load();
            let ollama = set.free_vram.then_some(set.ollama_url.as_str());
            let translate = set.translate.then_some((set.ollama_url.as_str(), set.translate_model.as_str()));
            let req = crate::comfy::Request { url: &set.comfy_url, model, prompt, negative, seed: *seed, strength: *strength, inpaint_denoise: None, ollama, translate };
            let (patch, translated) = crate::comfy::generate(img, strokes, &req)?;
            Ok((patch, match (translated, crate::sdlocal::is_local(model)) {
                (Some(t), false) => tf!("Génération ComfyUI appliquée (invite traduite : « {t} »).", t),
                (None, false) => t!("Génération ComfyUI appliquée.").into(),
                (Some(t), true) => tf!("Génération locale appliquée (invite traduite : « {t} »).", t),
                (None, true) => t!("Génération locale appliquée.").into(),
            }))
        }
        Retouch::Heal { .. } => anyhow::bail!("le correcteur n'a pas de calcul différé"),
    }
}

/// Applique toutes les retouches. Une gomme sans résultat en cache est calculée si `compute`,
/// sinon ignorée ; renvoie les index des gommes ignorées.
pub fn apply_all(img: &mut impl Canvas, photo: Option<&Path>, ops: &[Retouch], compute: bool) -> Vec<usize> {
    let (w, h) = img.size();
    let mut missing = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        match op {
            Retouch::Heal { center, radius, offset } => {
                let c = [center[0] * w as f32, center[1] * h as f32];
                heal(img, c, (radius * w as f32).max(1.0), [offset[0] * w as f32, offset[1] * h as f32]);
            }
            Retouch::Erase { .. } | Retouch::Generate { .. } => {
                let file = photo.and_then(|p| cache_file(p, &ops[..=i]));
                let patch = file.as_deref().and_then(cache_load).or_else(|| {
                    if !compute {
                        return None;
                    }
                    let (p, _) = compute_patch(img, op).map_err(|e| eprintln!("retouche {} : {e:#}", i + 1)).ok()?;
                    if let Some(f) = &file {
                        let _ = cache_store(f, &p);
                    }
                    Some(p)
                });
                match patch {
                    Some(p) => apply_patch(img, &p),
                    None => missing.push(i),
                }
            }
        }
    }
    missing
}

/// Masque (un canal) vu comme une image grise.
struct MaskCanvas<'a> {
    data: &'a mut [f32],
    w: u32,
    h: u32,
}

impl Canvas for MaskCanvas<'_> {
    fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        let v = self.data[(y.clamp(0, self.h as i32 - 1) as u32 * self.w + x.clamp(0, self.w as i32 - 1) as u32) as usize];
        [v; 3]
    }
    fn set(&mut self, x: i32, y: i32, v: [f32; 3]) {
        if x >= 0 && y >= 0 && (x as u32) < self.w && (y as u32) < self.h {
            self.data[(y as u32 * self.w + x as u32) as usize] = v[0].clamp(0.0, 1.0);
        }
    }
}

/// Reporte les retouches sur un masque IA `w`x`h` calculé sur la photo d'origine : le correcteur y copie
/// le masque de sa source, la gomme remplit sa zone à partir du pourtour (un pylône gommé dans le ciel
/// devient du ciel). Les générations ComfyUI ne sont pas reportées : leur contenu est inconnu.
pub fn apply_to_mask(mask: &mut [f32], w: u32, h: u32, ops: &[Retouch]) {
    if mask.len() != (w * h) as usize {
        return;
    }
    for op in ops {
        match op {
            Retouch::Heal { center, radius, offset } => {
                let mut c = MaskCanvas { data: mask, w, h };
                heal(&mut c, [center[0] * w as f32, center[1] * h as f32], (radius * w as f32).max(1.0), [offset[0] * w as f32, offset[1] * h as f32]);
            }
            Retouch::Erase { strokes, .. } => {
                let Some((r, alpha)) = erase_area(strokes, w, h) else { continue };
                let (rw, rh) = ((r[2] - r[0]) as u32, (r[3] - r[1]) as u32);
                let at = |x: u32, y: u32| ((r[1] as u32 + y) * w + r[0] as u32 + x) as usize;
                let sub: Vec<[f32; 4]> = (0..rh).flat_map(|y| (0..rw).map(move |x| (x, y))).map(|(x, y)| [mask[at(x, y)]; 4]).collect();
                let hole: Vec<f32> = alpha.iter().map(|a| if *a > 0.02 { 1.0 } else { 0.0 }).collect();
                let filled = diffuse(&sub, &hole, rw, rh);
                for y in 0..rh {
                    for x in 0..rw {
                        let k = (y * rw + x) as usize;
                        let a = alpha[k].clamp(0.0, 1.0);
                        let i = at(x, y);
                        mask[i] = mask[i] * (1.0 - a) + filled[k][0].clamp(0.0, 1.0) * a;
                    }
                }
            }
            Retouch::Generate { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: u32, h: u32, f: impl Fn(u32, u32) -> f32) -> Vec<[f32; 4]> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| {
            let v = f(x, y);
            [v, v, v, 1.0]
        }).collect()
    }

    #[test]
    fn heal_removes_dark_spot_and_keeps_gradient() {
        let (w, h) = (200, 200);
        // Dégradé horizontal avec une tache sombre au centre.
        let mut px = canvas(w, h, |x, y| {
            let base = 0.2 + 0.3 * x as f32 / w as f32;
            if (x as f32 - 100.0).hypot(y as f32 - 100.0) < 8.0 { 0.01 } else { base }
        });
        let mut c = F32Canvas { data: &mut px, w, h };
        let o = auto_offset(&c, [100.0, 100.0], 12.0);
        heal(&mut c, [100.0, 100.0], 12.0, o);
        let v = c.get(100, 100)[0];
        let expect = 0.2 + 0.3 * 100.0 / 200.0;
        assert!((v - expect).abs() < 0.02, "{v} au lieu de {expect}");
    }

    #[test]
    fn diffusion_fills_hole_smoothly() {
        let (w, h) = (64u32, 64u32);
        let img = canvas(w, h, |_, _| 0.5);
        let hole: Vec<f32> = (0..w * h).map(|k| if (k % w).abs_diff(32) < 8 && (k / w).abs_diff(32) < 8 { 1.0 } else { 0.0 }).collect();
        let mut dirty = img.clone();
        for (p, m) in dirty.iter_mut().zip(&hole) {
            if *m > 0.5 {
                *p = [0.0, 0.0, 0.0, 1.0];
            }
        }
        let out = diffuse(&dirty, &hole, w, h);
        assert!((out[(32 * w + 32) as usize][0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn erased_pole_becomes_sky_in_mask() {
        let (w, h) = (100u32, 100u32);
        // Ciel (1) en haut, sol (0) en bas, pylône (0) au milieu du ciel.
        let mut m: Vec<f32> = (0..w * h).map(|k| { let (x, y) = (k % w, k / w); if y >= 70 || (48..52).contains(&x) { 0.0 } else { 1.0 } }).collect();
        let stroke = BrushStroke { points: vec![[0.5, 0.1], [0.5, 0.4]], radius: 0.04, hardness: 0.9, flow: 1.0, erase: false, fill: false };
        apply_to_mask(&mut m, w, h, &[Retouch::Erase { strokes: vec![stroke], smooth: false }]);
        assert!(m[(25 * w + 50) as usize] > 0.95, "{}", m[(25 * w + 50) as usize]);
        assert_eq!(m[(90 * w + 50) as usize], 0.0, "sol non touché");
    }

    #[test]
    fn patch_cache_round_trip() {
        let p = Patch { rect: [0.1, 0.2, 0.3, 0.4], w: 2, h: 1, px: vec![[0.5, 0.25, 1.0, 1.0], [0.0, 0.0, 0.0, 0.5]] };
        let f = std::env::temp_dir().join(format!("luminux_patch_{}", std::process::id()));
        cache_store(&f, &p).unwrap();
        let q = cache_load(&f).unwrap();
        assert_eq!(q.rect, p.rect);
        assert_eq!(q.px, p.px);
        std::fs::remove_file(f).unwrap();
    }
}

#[cfg(test)]
mod essai_branches {
    /// Essai manuel de durée : LUMINUX_BRANCHES=photo.jpg cargo test essai_branches -- --ignored --nocapture
    #[test]
    #[ignore]
    fn essai_branches() {
        let f = std::env::var("LUMINUX_BRANCHES").unwrap();
        let img = image::open(&f).unwrap();
        let img = img.to_rgb32f();
        let (w, h) = img.dimensions();
        let mut data: Vec<[f32; 4]> = img.pixels().map(|p| [p[0], p[1], p[2], 1.0]).collect();
        let lines: [&[[f32; 2]]; 6] = [
            &[[1090.0, 135.0], [1130.0, 10.0], [1165.0, 0.0]],
            &[[1245.0, 190.0], [1400.0, 150.0], [1600.0, 100.0], [1700.0, 50.0], [1830.0, 0.0]],
            &[[1380.0, 0.0], [1430.0, 70.0], [1490.0, 45.0]],
            &[[1960.0, 140.0], [2000.0, 180.0]],
            &[[1940.0, 410.0], [2000.0, 450.0]],
            &[[1930.0, 745.0], [2000.0, 735.0]],
        ];
        let strokes: Vec<crate::adjust::BrushStroke> = lines.iter().map(|l| crate::adjust::BrushStroke {
            points: l.iter().map(|p| [p[0] / 2000.0, p[1] / 1333.0]).collect(),
            radius: 0.006, hardness: 0.5, flow: 1.0, erase: false, fill: false,
        }).collect();
        let canvas = super::F32Canvas { data: &mut data, w, h };
        let t = std::time::Instant::now();
        let (p, _) = super::compute_erase(&canvas, &strokes, true).unwrap();
        eprintln!("{w}x{h}, zone {}x{} : {:?}", p.w, p.h, t.elapsed());
        let mut o = image::RgbImage::new(p.w, p.h);
        for (i, c) in p.px.iter().enumerate() {
            let v = |x: f32| (x.clamp(0.0, 1.0) * 255.0) as u8;
            o.put_pixel(i as u32 % p.w, i as u32 / p.w, image::Rgb([v(c[0]), v(c[1]), v(c[2])]));
        }
        o.save(format!("{f}.zone.png")).unwrap();
    }
}
