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

/// Sélection de retouche tirée d'un masque IA (`w`x`h`, 0..1) : des rectangles pleins sur une grille
/// d'environ 192 cases en long, élargie d'une case pour couvrir le bord adouci du masque.
/// `erase` : la sélection est retirée au lieu d'être ajoutée. `down` : cases ajoutées sous l'objet
/// (ombre portée au sol d'une personne).
pub(crate) fn selection_from_mask(data: &[f32], w: u32, h: u32, erase: bool, down: usize) -> Vec<BrushStroke> {
    let cell = (w.max(h) as f32 / 192.0).max(1.0);
    let (gw, gh) = (((w as f32 / cell).ceil() as usize).max(1), ((h as f32 / cell).ceil() as usize).max(1));
    // Case retenue si une part notable de ses pixels est dans le masque.
    let mut sum = vec![(0f32, 0u32); gw * gh];
    for y in 0..h as usize {
        let gy = ((y as f32 / cell) as usize).min(gh - 1);
        for x in 0..w as usize {
            let gx = ((x as f32 / cell) as usize).min(gw - 1);
            let c = &mut sum[gy * gw + gx];
            c.0 += data[y * w as usize + x];
            c.1 += 1;
        }
    }
    let on: Vec<bool> = sum.iter().map(|(v, n)| *n > 0 && v / *n as f32 > 0.3).collect();
    let grown: Vec<bool> = (0..gw * gh)
        .map(|i| {
            let (x, y) = ((i % gw) as i64, (i / gw) as i64);
            (-1..=1 + down as i64).any(|dy| (-1..=1).any(|dx| {
                let (nx, ny) = (x + dx, y - dy);
                nx >= 0 && ny >= 0 && nx < gw as i64 && ny < gh as i64 && on[ny as usize * gw + nx as usize]
            }))
        })
        .collect();
    // Plages par ligne, fusionnées avec la ligne du dessous quand elles sont identiques.
    let mut open: Vec<(usize, usize, usize)> = Vec::new();
    let mut rects = Vec::new();
    for y in 0..=gh {
        let mut runs = Vec::new();
        if y < gh {
            let mut x = 0;
            while x < gw {
                if grown[y * gw + x] {
                    let x0 = x;
                    while x < gw && grown[y * gw + x] {
                        x += 1;
                    }
                    runs.push((x0, x));
                } else {
                    x += 1;
                }
            }
        }
        let mut next = Vec::new();
        for (x0, x1, y0) in open.drain(..) {
            if let Some(k) = runs.iter().position(|r| *r == (x0, x1)) {
                runs.remove(k);
                next.push((x0, x1, y0));
            } else {
                rects.push((x0, x1, y0, y));
            }
        }
        next.extend(runs.into_iter().map(|(x0, x1)| (x0, x1, y)));
        open = next;
    }
    let (fw, fh) = (gw as f32 * cell / w as f32, gh as f32 * cell / h as f32);
    rects
        .into_iter()
        .map(|(x0, x1, y0, y1)| {
            let (a, b) = (x0 as f32 / gw as f32 * fw, x1 as f32 / gw as f32 * fw);
            let (c, d) = (y0 as f32 / gh as f32 * fh, y1 as f32 / gh as f32 * fh);
            let (b, d) = (b.min(1.0), d.min(1.0));
            BrushStroke { points: vec![[a, c], [b, c], [b, d], [a, d]], radius: 0.0005, hardness: 0.9, flow: 1.0, erase, fill: true }
        })
        .collect()
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
    // Traits entièrement hors de la photo (pinceau passé dans la marge) : rien à effacer.
    if x0 > x1 || x1 < 0.0 || y1 < 0.0 || x0 > w as f32 || y0 > h as f32 {
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
    let groups = erase_groups(strokes, w, h);
    if groups.is_empty() {
        anyhow::bail!("aucune zone peinte");
    }
    if groups.len() <= 1 {
        return erase_one(img, strokes, use_ai, false);
    }
    // Objets éloignés (des passants dispersés) : chacun est effacé dans son propre cadre, à pleine
    // définition, au lieu d'un seul grand cadre que LaMa réduit à 512 px (traînées floues).
    let mut parts = Vec::new();
    let mut used_ai = false;
    for g in &groups {
        let (p, ai) = erase_one(img, g, use_ai, false)?;
        used_ai |= ai;
        parts.push(p);
    }
    Ok((merge_patches(&parts, w, h), used_ai))
}

/// Pièces de plusieurs cadres réunies en une seule, sur le cadre qui les contient toutes.
fn merge_patches(parts: &[Patch], w: u32, h: u32) -> Patch {
    let px_rect = |p: &Patch| [(p.rect[0] * w as f32).round() as i32, (p.rect[1] * h as f32).round() as i32];
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in parts {
        let [px, py] = px_rect(p);
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px + p.w as i32);
        y1 = y1.max(py + p.h as i32);
    }
    let (mw, mh) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut px = vec![[0f32; 4]; (mw * mh) as usize];
    for p in parts {
        let [ox, oy] = px_rect(p);
        for y in 0..p.h {
            for x in 0..p.w {
                let q = p.px[(y * p.w + x) as usize];
                let o = &mut px[((oy - y0) as u32 + y) as usize * mw as usize + ((ox - x0) as u32 + x) as usize];
                // Cadres voisins qui se touchent : on garde la pièce la plus opaque.
                if q[3] > o[3] {
                    *o = q;
                }
            }
        }
    }
    let rect = [x0 as f32 / w as f32, y0 as f32 / h as f32, mw as f32 / w as f32, mh as f32 / h as f32];
    Patch { rect, w: mw, h: mh, px }
}

/// Traits de gomme regroupés par objet : deux traits d'ajout sont du même groupe si leurs cadres,
/// élargis de 3 % de la photo, se touchent. Les traits de retrait vont dans chaque groupe.
fn erase_groups(strokes: &[BrushStroke], w: u32, h: u32) -> Vec<Vec<BrushStroke>> {
    erase_groups_by(strokes, w, h, 0.03)
}

/// `gap` : écart (en part du grand côté) en dessous duquel deux traits forment un même groupe.
fn erase_groups_by(strokes: &[BrushStroke], w: u32, h: u32, gap: f32) -> Vec<Vec<BrushStroke>> {
    let m = w.max(h) as f32 * gap;
    // Un trait hors de la photo formerait un groupe sans pixel (cadre inversé, allocation démesurée).
    let inside = |s: &BrushStroke| erase_area(std::slice::from_ref(s), w, h).is_some();
    let adds: Vec<usize> = (0..strokes.len()).filter(|&i| !strokes[i].erase && !strokes[i].points.is_empty() && inside(&strokes[i])).collect();
    let bbox: Vec<[f32; 4]> = adds
        .iter()
        .map(|&i| {
            let s = &strokes[i];
            let r = s.radius * w as f32 + m;
            s.points.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| {
                let (x, y) = (p[0] * w as f32, p[1] * h as f32);
                [b[0].min(x - r), b[1].min(y - r), b[2].max(x + r), b[3].max(y + r)]
            })
        })
        .collect();
    let mut parent: Vec<usize> = (0..adds.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for a in 0..adds.len() {
        for b in a + 1..adds.len() {
            let (p, q) = (bbox[a], bbox[b]);
            if p[0] <= q[2] && q[0] <= p[2] && p[1] <= q[3] && q[1] <= p[3] {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[ra] = rb;
            }
        }
    }
    let mut groups: Vec<(usize, Vec<BrushStroke>)> = Vec::new();
    for k in 0..adds.len() {
        let root = find(&mut parent, k);
        match groups.iter_mut().find(|g| g.0 == root) {
            Some(g) => g.1.push(strokes[adds[k]].clone()),
            None => groups.push((root, vec![strokes[adds[k]].clone()])),
        }
    }
    let removes: Vec<BrushStroke> = strokes.iter().filter(|s| s.erase).cloned().collect();
    groups.into_iter().map(|(_, mut g)| { g.extend(removes.iter().cloned()); g }).collect()
}

/// `raw` : sortie LaMa telle quelle, sans y recopier des morceaux de la photo (base de la gomme par
/// diffusion : les morceaux recopiés, un pan de kiosque par exemple, devenaient un mur une fois repassés).
fn erase_one(img: &impl Canvas, strokes: &[BrushStroke], use_ai: bool, raw: bool) -> Result<(Patch, bool)> {
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
        (Some(f), _) if raw => f,
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

/// Taille visée d'une tranche (en pixels de la photo) en passe précise.
const ZONE_SIDE: f32 = 768.0;

/// Gomme par diffusion d'un groupe d'objets : LaMa brute puis passe à mi-force.
/// `zones` : un groupe trop long (une foule au pied des façades) est découpé en 2 ou 3 tranches qui se
/// chevauchent, chacune traitée près de la résolution du modèle au lieu d'être réduite puis agrandie (flou).
/// Chaque tranche voit les précédentes déjà effacées. `saved(suffixe)` garde les fichiers de diagnostic.
fn erase_diffuse(img: &impl Canvas, g: &[BrushStroke], req: &crate::comfy::Request, zones: bool, saved: &dyn Fn(&str)) -> Result<Option<Patch>> {
    let (w, h) = img.size();
    fn one<C: Canvas>(cv: &C, st: &[BrushStroke], req: &crate::comfy::Request) -> Result<Option<Patch>> {
        let (pre, _) = erase_one(cv, st, true, true)?;
        // Objet entièrement retiré par des traits « Retirer » : rien à envoyer au moteur.
        if pre.px.iter().all(|p| p[3] < 0.01) {
            return Ok(None);
        }
        let (cw, ch) = cv.size();
        let over = Over { base: cv, patch: &pre, x0: (pre.rect[0] * cw as f32).round() as i32, y0: (pre.rect[1] * ch as f32).round() as i32 };
        crate::comfy::generate(&over, st, req).map(|r| Some(r.0))
    }
    let (r, _) = erase_area(g, w, h).ok_or_else(|| anyhow::anyhow!("aucune zone peinte"))?;
    let (rw, rh) = ((r[2] - r[0]) as f32, (r[3] - r[1]) as f32);
    let horiz = rw >= rh;
    let long = rw.max(rh);
    let n = if zones { ((long / ZONE_SIDE).ceil() as usize).clamp(1, 3) } else { 1 };
    if n == 1 {
        let p = one(img, g, req)?;
        saved("");
        return Ok(p);
    }
    // Recouvrement des trous de deux tranches voisines, et contexte visible de chaque côté.
    let (overlap, ctx) = (48.0, 160.0);
    let (start, size) = if horiz { (r[0] as f32, w as f32) } else { (r[1] as f32, h as f32) };
    let mut parts: Vec<Patch> = Vec::new();
    for k in 0..n {
        let a = start + long * k as f32 / n as f32;
        let b = start + long * (k + 1) as f32 / n as f32;
        let ha = if k == 0 { -1e6 } else { a - overlap };
        let (wa, wb) = ((a - ctx).floor().max(0.0), (b + ctx).ceil().min(size));
        let win_rect = if horiz { [wa as i32, 0, wb as i32, h as i32] } else { [0, wa as i32, w as i32, wb as i32] };
        let (ww, wh) = ((win_rect[2] - win_rect[0]) as u32, (win_rect[3] - win_rect[1]) as u32);
        let stack = Stack { base: img, parts: &parts };
        let win = Window { base: &stack, x0: win_rect[0], y0: win_rect[1], w: ww, h: wh };
        let to_win = |p: &[f32; 2]| [(p[0] * w as f32 - win_rect[0] as f32) / ww as f32, (p[1] * h as f32 - win_rect[1] as f32) / wh as f32];
        let mut st: Vec<BrushStroke> = g.iter().map(|s| BrushStroke { points: s.points.iter().map(to_win).collect(), radius: s.radius * w as f32 / ww as f32, ..s.clone() }).collect();
        // Avant la tranche : retiré (déjà effacé par la précédente, qui sert de contexte). Après : gardé dans
        // le trou jusqu'au bord du cadre, sinon les objets suivants serviraient de contexte et
        // déteindraient ; la tranche suivante repasse dessus.
        let lim = |v: f32| (v - wa) / (wb - wa);
        let clip = |u0: f32, u1: f32| {
            let pts = if horiz { vec![[u0, -1.0], [u1, -1.0], [u1, 2.0], [u0, 2.0]] } else { vec![[-1.0, u0], [2.0, u0], [2.0, u1], [-1.0, u1]] };
            BrushStroke { points: pts, radius: 0.0005, hardness: 1.0, flow: 1.0, erase: true, fill: true }
        };
        if k > 0 {
            st.push(clip(-1.0, lim(ha)));
        }
        if erase_area(&st, ww, wh).is_none() {
            continue;
        }
        let p = one(&win, &st, req)?;
        saved(&format!("-{}", k + 1));
        if let Some(p) = p {
            let rect = [(win_rect[0] as f32 + p.rect[0] * ww as f32) / w as f32, (win_rect[1] as f32 + p.rect[1] * wh as f32) / h as f32, p.rect[2] * ww as f32 / w as f32, p.rect[3] * wh as f32 / h as f32];
            parts.push(Patch { rect, ..p });
        }
    }
    Ok((!parts.is_empty()).then(|| stack_patches(&parts, w, h)))
}

/// Origine en pixels d'une pièce posée sur une image w x h.
fn patch_origin(p: &Patch, w: u32, h: u32) -> (i32, i32) {
    ((p.rect[0] * w as f32).round() as i32, (p.rect[1] * h as f32).round() as i32)
}

/// Pièces superposées dans l'ordre (la suivante par-dessus), réunies en une seule.
fn stack_patches(parts: &[Patch], w: u32, h: u32) -> Patch {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in parts {
        let (px, py) = patch_origin(p, w, h);
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px + p.w as i32);
        y1 = y1.max(py + p.h as i32);
    }
    let (mw, mh) = ((x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
    let mut px = vec![[0f32; 4]; (mw * mh) as usize];
    for p in parts {
        let (ox, oy) = patch_origin(p, w, h);
        for y in 0..p.h {
            for x in 0..p.w {
                let q = p.px[(y * p.w + x) as usize];
                let o = &mut px[((oy - y0) as u32 + y) as usize * mw as usize + ((ox - x0) as u32 + x) as usize];
                let a = q[3].clamp(0.0, 1.0);
                let out = a + o[3] * (1.0 - a);
                if out > 0.0 {
                    for c in 0..3 {
                        o[c] = (q[c] * a + o[c] * o[3] * (1.0 - a)) / out;
                    }
                }
                o[3] = out;
            }
        }
    }
    let rect = [x0 as f32 / w as f32, y0 as f32 / h as f32, mw as f32 / w as f32, mh as f32 / h as f32];
    Patch { rect, w: mw, h: mh, px }
}

/// Image avec des pièces déjà posées par-dessus, dans l'ordre (lecture seule).
struct Stack<'a, C: Canvas> {
    base: &'a C,
    parts: &'a [Patch],
}

impl<C: Canvas> Canvas for Stack<'_, C> {
    fn size(&self) -> (u32, u32) {
        self.base.size()
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        let (w, h) = self.base.size();
        let mut d = self.base.get(x, y);
        for p in self.parts {
            let (ox, oy) = patch_origin(p, w, h);
            let (px, py) = (x - ox, y - oy);
            if px < 0 || py < 0 || px >= p.w as i32 || py >= p.h as i32 {
                continue;
            }
            let q = p.px[(py as u32 * p.w + px as u32) as usize];
            let a = q[3].clamp(0.0, 1.0);
            d = std::array::from_fn(|c| d[c] + (q[c] - d[c]) * a);
        }
        d
    }
    fn set(&mut self, _: i32, _: i32, _: [f32; 3]) {}
}

/// Morceau d'une image vu comme une image entière (lecture seule).
struct Window<'a, C: Canvas> {
    base: &'a C,
    x0: i32,
    y0: i32,
    w: u32,
    h: u32,
}

impl<C: Canvas> Canvas for Window<'_, C> {
    fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        self.base.get(self.x0 + x.clamp(0, self.w as i32 - 1), self.y0 + y.clamp(0, self.h as i32 - 1))
    }
    fn set(&mut self, _: i32, _: i32, _: [f32; 3]) {}
}

/// Force de la diffusion qui repasse sur l'effacement LaMa : assez pour une texture crédible,
/// trop peu pour inventer un objet.
const ERASE_REFINE: f32 = 0.45;

/// Image vue avec une pièce posée dessus (lecture seule).
struct Over<'a, C: Canvas> {
    base: &'a C,
    patch: &'a Patch,
    x0: i32,
    y0: i32,
}

impl<C: Canvas> Canvas for Over<'_, C> {
    fn size(&self) -> (u32, u32) {
        self.base.size()
    }
    fn get(&self, x: i32, y: i32) -> [f32; 3] {
        let d = self.base.get(x, y);
        let (px, py) = (x - self.x0, y - self.y0);
        if px < 0 || py < 0 || px >= self.patch.w as i32 || py >= self.patch.h as i32 {
            return d;
        }
        let q = self.patch.px[(py as u32 * self.patch.w + px as u32) as usize];
        let a = q[3].clamp(0.0, 1.0);
        std::array::from_fn(|c| d[c] + (q[c] - d[c]) * a)
    }
    fn set(&mut self, _: i32, _: i32, _: [f32; 3]) {}
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
    if data.len() as u64 != w as u64 * h as u64 * 8 {
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
        Retouch::Generate { strokes, model, prompt, negative, seed, strength, zones } => {
            let set = crate::settings::Settings::load();
            let ollama = set.free_vram.then_some(set.ollama_url.as_str());
            // Gomme par diffusion : invite déjà en anglais, rien à traduire.
            let erase = prompt == crate::comfy::ERASE_PROMPT;
            let translate = (set.translate && !erase).then_some((set.ollama_url.as_str(), set.translate_model.as_str()));
            let req = crate::comfy::Request { url: &set.comfy_url, model, prompt, negative, seed: *seed, strength: *strength, inpaint_denoise: None, refine: false, ollama, translate };
            if erase {
                // Comme la gomme LaMa : un cadre par groupe d'objets, chacun à la résolution du modèle.
                let (w, h) = img.size();
                // LaMa efface d'abord (rien d'inventé), puis la diffusion à mi-force refait la texture
                // sans dessiner d'objets nouveaux (kiosques, rambardes à pleine force).
                let refine = crate::comfy::Request { strength: ERASE_REFINE, inpaint_denoise: Some(ERASE_REFINE), refine: true, ..req };
                // Fichiers de diagnostic gardés pour chaque groupe (groupe-1-envoi.png…), pas seulement le dernier.
                let debug = crate::library::data_dir().join("generation");
                if let Ok(rd) = std::fs::read_dir(&debug) {
                    rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("groupe-")).for_each(|e| {
                        let _ = std::fs::remove_file(e.path());
                    });
                }
                // Précise : groupes plus petits (les trous entre les blocs de personnes servent de coupures),
                // traités l'un après l'autre, chacun voyant les précédents déjà effacés.
                let groups = if *zones { erase_groups_by(strokes, w, h, 0.01) } else { erase_groups(strokes, w, h) };
                let mut parts: Vec<Patch> = Vec::new();
                for (i, g) in groups.iter().enumerate() {
                    let stack = Stack { base: img, parts: &parts };
                    let p = erase_diffuse(&stack, g, &refine, *zones, &|k| {
                        for n in ["envoi", "recu", "photo", "piece"] {
                            let _ = std::fs::copy(debug.join(format!("{n}.png")), debug.join(format!("groupe-{}{k}-{n}.png", i + 1)));
                        }
                    })?;
                    parts.extend(p);
                }
                if parts.is_empty() {
                    anyhow::bail!("aucune zone peinte");
                }
                let patch = stack_patches(&parts, w, h);
                return Ok((patch, if crate::sdlocal::is_local(model) { t!("Gomme (moteur local) appliquée.").into() } else { t!("Gomme ComfyUI appliquée.").into() }));
            }
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

    #[test]
    #[ignore]
    fn essai_gomme_diffusion() {
        // Faux moteur local (LUMINUX_SD, LUMINUX_FAKE_MODEL) : deux objets éloignés, deux cadres.
        let Ok(model) = std::env::var("LUMINUX_FAKE_MODEL") else { return };
        let (w, h) = (1600u32, 800u32);
        let mut data = canvas(w, h, |x, _| 0.2 + 0.6 * x as f32 / w as f32);
        let sq = |x0: f32, y0: f32| BrushStroke { points: vec![[x0, y0], [x0 + 0.04, y0], [x0 + 0.04, y0 + 0.1], [x0, y0 + 0.1]], radius: 0.001, hardness: 0.9, flow: 1.0, erase: false, fill: true };
        let op = Retouch::Generate { strokes: vec![sq(0.1, 0.5), sq(0.8, 0.6)], model: format!("local:{model}"), prompt: crate::comfy::ERASE_PROMPT.into(), negative: crate::comfy::ERASE_NEGATIVE.into(), seed: 1, strength: 1.0, zones: false };
        let cv = F32Canvas { data: &mut data, w, h };
        let (p, msg) = compute_patch(&cv, &op).unwrap();
        assert!(msg.contains("Gomme"), "{msg}");
        assert!(p.rect[0] < 0.1 && p.rect[0] + p.rect[2] > 0.84, "{:?}", p.rect);
        let at = |fx: f32, fy: f32| {
            let (x, y) = ((fx - p.rect[0]) / p.rect[2] * p.w as f32, (fy - p.rect[1]) / p.rect[3] * p.h as f32);
            p.px[(y as u32 * p.w + x as u32) as usize][3]
        };
        assert!(at(0.12, 0.55) > 0.9 && at(0.82, 0.65) > 0.9 && at(0.45, 0.55) < 0.01);
    }

    #[test]
    #[ignore]
    fn essai_gomme_diffusion_bords() {
        // Petites zones, collées aux bords et aux coins : aucun plantage.
        let Ok(model) = std::env::var("LUMINUX_FAKE_MODEL") else { return };
        let (w, h) = (1200u32, 800u32);
        let mut data = canvas(w, h, |x, y| ((x ^ y) % 7) as f32 / 7.0);
        let sq = |x0: f32, y0: f32, s: f32| BrushStroke { points: vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]], radius: 0.0005, hardness: 0.9, flow: 1.0, erase: false, fill: true };
        let cases = vec![
            vec![sq(0.0, 0.0, 0.01)],
            vec![sq(0.99, 0.99, 0.01)],
            vec![sq(0.0, 0.5, 0.002), sq(0.5, 0.0, 0.002), sq(0.998, 0.3, 0.002)],
            vec![sq(0.0, 0.0, 1.0)],
            vec![sq(0.3, 0.3, 0.0005)],
        ];
        let cv = F32Canvas { data: &mut data, w, h };
        for (k, st) in cases.into_iter().enumerate() {
            let op = Retouch::Generate { strokes: st, model: format!("local:{model}"), prompt: crate::comfy::ERASE_PROMPT.into(), negative: String::new(), seed: 1, strength: 1.0, zones: false };
            match compute_patch(&cv, &op) {
                Ok((p, _)) => assert_eq!(p.px.len(), (p.w * p.h) as usize, "cas {k}"),
                Err(e) => eprintln!("cas {k} : {e:#}"),
            }
        }
    }

    #[test]
    fn stroke_outside_the_photo_is_ignored() {
        // Trait passé dans la marge à gauche de la photo : il formait un groupe au cadre inversé
        // (largeur négative lue comme 4 milliards de pixels) et l'appli s'arrêtait faute de mémoire.
        let (w, h) = (600u32, 300u32);
        let out = BrushStroke { points: vec![[-0.5, 0.5], [-0.45, 0.6]], radius: 0.01, hardness: 0.9, flow: 1.0, erase: false, fill: false };
        let inside = BrushStroke { points: vec![[0.5, 0.5]], ..out.clone() };
        assert!(erase_area(std::slice::from_ref(&out), w, h).is_none());
        let g = erase_groups(&[out.clone(), inside], w, h);
        assert_eq!(g.len(), 1);
        let mut data = canvas(w, h, |_, _| 0.5);
        let cv = F32Canvas { data: &mut data, w, h };
        assert!(compute_erase(&cv, &[out], false).is_err());
    }

    #[test]
    #[ignore]
    fn essai_gomme_diffusion_zones() {
        // Faux moteur : une longue foule (bande de 2400 px), un piéton isolé à droite. Passe précise :
        // la bande est découpée en tranches, le piéton a son propre cadre, tout est effacé.
        let Ok(model) = std::env::var("LUMINUX_FAKE_MODEL") else { return };
        let (w, h) = (3200u32, 1200u32);
        let mut mask = vec![0f32; (w * h) as usize];
        for y in 500..800 {
            for x in 200..2600 {
                mask[(y * w + x) as usize] = 1.0;
            }
            for x in 2900..2980 {
                mask[(y * w + x) as usize] = 1.0;
            }
        }
        let strokes = selection_from_mask(&mask, w, h, false, 3);
        let mut data = canvas(w, h, |x, _| 0.2 + 0.6 * x as f32 / w as f32);
        for y in 500..800 {
            for x in (200..2600).chain(2900..2980) {
                data[(y * w + x) as usize] = [0.0, 0.0, 0.0, 1.0];
            }
        }
        for zones in [false, true] {
            let op = Retouch::Generate { strokes: strokes.clone(), model: format!("local:{model}"), prompt: crate::comfy::ERASE_PROMPT.into(), negative: String::new(), seed: 1, strength: 1.0, zones };
            let cv = F32Canvas { data: &mut data, w, h };
            let (p, _) = compute_patch(&cv, &op).unwrap();
            let at = |fx: f32, fy: f32| {
                let (x, y) = ((fx - p.rect[0]) / p.rect[2] * p.w as f32, (fy - p.rect[1]) / p.rect[3] * p.h as f32);
                p.px[(y as u32 * p.w + x as u32) as usize]
            };
            for fx in [0.1, 0.3, 0.5, 0.7, 0.92] {
                let q = at(fx, 0.54);
                let want = 0.2 + 0.6 * fx;
                // Le faux moteur pose un carré rouge (objet inventé) : seul le gris doit suivre le dégradé.
                if q[1] < 0.05 && q[0] > 0.2 {
                    continue;
                }
                assert!(q[3] > 0.95 && (q[0] - want).abs() < 0.15, "zones {zones} x {fx} : {q:?}");
            }
        }
    }

    #[test]
    fn distant_objects_are_erased_separately() {
        let (w, h) = (600u32, 300u32);
        let sq = |x0: f32, y0: f32| BrushStroke { points: vec![[x0, y0], [x0 + 0.05, y0], [x0 + 0.05, y0 + 0.1], [x0, y0 + 0.1]], radius: 0.001, hardness: 0.9, flow: 1.0, erase: false, fill: true };
        let strokes = vec![sq(0.1, 0.4), sq(0.12, 0.45), sq(0.8, 0.5)];
        let g = erase_groups(&strokes, w, h);
        assert_eq!(g.len(), 2);
        assert_eq!(g.iter().map(|g| g.len()).sum::<usize>(), 3);
        // Fond en dégradé, deux taches sombres : la pièce couvre les deux et les efface.
        let mut data = canvas(w, h, |x, _| 0.2 + 0.6 * x as f32 / w as f32);
        for (x, y) in [(70u32, 135u32), (500, 165)] {
            for dy in 0..10 {
                for dx in 0..10 {
                    data[((y + dy) * w + x + dx) as usize] = [0.0, 0.0, 0.0, 1.0];
                }
            }
        }
        let cv = F32Canvas { data: &mut data, w, h };
        let (p, _) = compute_erase(&cv, &strokes, false).unwrap();
        let at = |fx: u32, fy: u32| {
            let (ox, oy) = ((p.rect[0] * w as f32).round() as u32, (p.rect[1] * h as f32).round() as u32);
            p.px[((fy - oy) * p.w + fx - ox) as usize]
        };
        for (x, y) in [(75u32, 140u32), (505, 170)] {
            let q = at(x, y);
            let want = 0.2 + 0.6 * x as f32 / w as f32;
            assert!(q[3] > 0.9 && (q[0] - want).abs() < 0.08, "({x}, {y}) : {q:?}");
        }
        // Entre les deux objets, la pièce est transparente.
        assert!(at(300, 150)[3] < 0.01);
    }

    #[test]
    fn mask_becomes_selection() {
        // Deux silhouettes dans une photo en hauteur ; la sélection doit les couvrir et rester près d'elles.
        let (w, h) = (400u32, 900u32);
        let inside = |x: u32, y: u32| ((100..140).contains(&x) && (500..700).contains(&y)) || ((x as f32 - 300.0).powi(2) + (y as f32 - 300.0).powi(2) < 900.0);
        let data: Vec<f32> = (0..w * h).map(|i| if inside(i % w, i / w) { 1.0 } else { 0.0 }).collect();
        let strokes = selection_from_mask(&data, w, h, false, 0);
        assert!(!strokes.is_empty() && strokes.len() < 200, "{}", strokes.len());
        let mut sel = vec![0f32; (w * h) as usize];
        crate::masks::paint_strokes(&mut sel, w, h, &strokes);
        let missed = (0..w * h).filter(|&i| inside(i % w, i / w) && sel[i as usize] < 0.5).count();
        assert_eq!(missed, 0);
        // Rien de sélectionné loin des objets (au-delà de deux cases).
        assert!(sel[(100 * w + 50) as usize] < 0.5 && sel[(800 * w + 300) as usize] < 0.5);
        let extra = sel.iter().filter(|v| **v > 0.5).count() as f32 / data.iter().filter(|v| **v > 0.5).count() as f32;
        assert!(extra < 2.0, "{extra}");
        // En mode Retirer, les rectangles enlèvent.
        assert!(selection_from_mask(&data, w, h, true, 0).iter().all(|s| s.erase && s.fill));
        // Ombre : avec `down`, la sélection descend sous les pieds, pas au-dessus de la tête.
        let mut low = vec![0f32; (w * h) as usize];
        crate::masks::paint_strokes(&mut low, w, h, &selection_from_mask(&data, w, h, false, 3));
        assert!(low[(712 * w + 120) as usize] > 0.5 && sel[(712 * w + 120) as usize] < 0.5);
        assert!(low[(488 * w + 120) as usize] < 0.5);
    }

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
