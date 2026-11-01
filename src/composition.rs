//! Aides au recadrage : redressement automatique, point d'intérêt et cadrages suggérés.
//! Heuristiques sur une petite vignette, sans modèle d'IA.

/// Angle (en degrés) de l'écart des lignes dominantes à l'horizontale ou à la verticale, dans
/// ±15°, mesuré dans le repère de l'image (y vers le bas). Le redressement à appliquer est son
/// opposé. `None` quand aucune orientation ne se dégage nettement.
pub fn detect_tilt(lum: &[f32], w: usize, h: usize) -> Option<f32> {
    if w < 16 || h < 16 || lum.len() < w * h {
        return None;
    }
    const RANGE: f32 = 15.0;
    const BIN: f32 = 0.1;
    let n = (2.0 * RANGE / BIN) as usize;
    // Flou léger : les blocs JPEG et les pixels agrandis, alignés sur la grille, fausseraient la mesure.
    let blurred = blur(&lum[..w * h], w, h, 1.0);
    let at = |x: usize, y: usize| blurred[y * w + x];
    // Gradients de Sobel, puis tenseur de structure lissé : une arête oblique en escalier
    // (marches de quelques pixels) garde ainsi sa vraie pente au lieu de se ranger sur l'axe.
    let (mut jxx, mut jyy, mut jxy) = (vec![0.0f32; w * h], vec![0.0f32; w * h], vec![0.0f32; w * h]);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1);
            let gy = at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1);
            let i = y * w + x;
            (jxx[i], jyy[i], jxy[i]) = (gx * gx, gy * gy, gx * gy);
        }
    }
    let sigma = (w.max(h) as f32 / 160.0).max(2.5);
    let (jxx, jyy, jxy) = (blur(&jxx, w, h, sigma), blur(&jyy, w, h, sigma), blur(&jxy, w, h, sigma));
    // Orientation du gradient dominant (degrés) et énergie cohérente de chaque pixel.
    let mut votes: Vec<(f32, f32)> = Vec::with_capacity(w * h);
    let mut max_mag = 0.0f32;
    for i in 0..w * h {
        let (a, b) = (jxx[i] - jyy[i], 2.0 * jxy[i]);
        let energy = (a * a + b * b).sqrt();
        max_mag = max_mag.max(energy);
        votes.push((energy, 0.5 * b.atan2(a).to_degrees()));
    }
    if max_mag < 0.01 {
        return None;
    }
    // Seules les arêtes les plus marquées (8 % des pixels) votent.
    let mut mags: Vec<f32> = votes.iter().map(|v| v.0).collect();
    let k = mags.len() * 92 / 100;
    let floor = (*mags.select_nth_unstable_by(k, f32::total_cmp).1).max(max_mag * 0.05);
    let mut hist = vec![0.0f32; n];
    let mut dev: Vec<(f32, f32)> = Vec::new();
    for &(m, a) in &votes {
        if m < floor {
            continue;
        }
        // Écart de la direction de l'arête (a + 90°) au multiple de 90° le plus proche.
        let d = (a + 45.0).rem_euclid(90.0) - 45.0;
        if d.abs() < RANGE {
            dev.push((m, d));
            let k = (((d + RANGE) / BIN) as usize).min(n - 1);
            hist[k] += m;
        }
    }
    let total: f32 = hist.iter().sum();
    if total <= 0.0 {
        return None;
    }
    // Lissage gaussien (σ = 3 cases) puis pic.
    let sigma = 3.0f32;
    let r = 9i32;
    let smooth: Vec<f32> = (0..n as i32)
        .map(|i| {
            (-r..=r)
                .filter(|j| (0..n as i32).contains(&(i + j)))
                .map(|j| hist[(i + j) as usize] * (-((j * j) as f32) / (2.0 * sigma * sigma)).exp())
                .sum()
        })
        .collect();
    let peak = smooth.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i)?;
    let centre = -RANGE + (peak as f32 + 0.5) * BIN;
    // Moyenne pondérée des votes à moins de 1° du pic, et part qu'ils représentent.
    let (mut sw, mut sd) = (0.0f32, 0.0f32);
    for &(m, d) in &dev {
        if (d - centre).abs() < 1.0 {
            sw += m;
            sd += m * d;
        }
    }
    // Uniforme, la fenêtre de 2° sur 30° recueillerait 6,7 % des votes.
    if sw / total < 0.2 {
        return None;
    }
    let d = sd / sw;
    Some(if d.abs() < 0.1 { 0.0 } else { d })
}

/// Flou gaussien séparable de `sigma` pixels.
fn blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (sigma * 2.5).ceil() as i32;
    let kernel: Vec<f32> = (-r..=r).map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp()).collect();
    let pass = |input: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (mut acc, mut wsum) = (0.0, 0.0);
                for (j, kv) in kernel.iter().enumerate() {
                    let o = j as i32 - r;
                    let (sx, sy) = if horizontal { (x as i32 + o, y as i32) } else { (x as i32, y as i32 + o) };
                    if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                        acc += kv * input[sy as usize * w + sx as usize];
                        wsum += kv;
                    }
                }
                out[y * w + x] = acc / wsum;
            }
        }
        out
    };
    pass(&pass(src, true), false)
}

/// Point d'intérêt (0..1, 0..1) : centre de gravité du contraste local et de l'écart de couleur,
/// légèrement tiré vers le centre. `rgb` est encodé sRGB, `w`x`h`.
pub fn interest_point(rgb: &[[f32; 4]], w: usize, h: usize) -> [f32; 2] {
    if w < 8 || h < 8 || rgb.len() < w * h {
        return [0.5, 0.5];
    }
    let lum: Vec<f32> = rgb.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    // Moyenne locale par image intégrale (fenêtre d'un sixième de la largeur).
    let mut integral = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0.0f64;
        for x in 0..w {
            row += lum[y * w + x] as f64;
            integral[(y + 1) * (w + 1) + x + 1] = integral[y * (w + 1) + x + 1] + row;
        }
    }
    let rad = (w / 12).max(2);
    let local = |x: usize, y: usize| {
        let (x0, x1, y0, y1) = (x.saturating_sub(rad), (x + rad + 1).min(w), y.saturating_sub(rad), (y + rad + 1).min(h));
        let s = integral[y1 * (w + 1) + x1] - integral[y0 * (w + 1) + x1] - integral[y1 * (w + 1) + x0] + integral[y0 * (w + 1) + x0];
        (s / ((x1 - x0) * (y1 - y0)) as f64) as f32
    };
    let n = (w * h) as f32;
    let mean: [f32; 3] = std::array::from_fn(|c| rgb.iter().map(|p| p[c]).sum::<f32>() / n);
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for y in 0..h {
        for x in 0..w {
            let p = rgb[y * w + x];
            let contrast = (lum[y * w + x] - local(x, y)).abs();
            let colour = ((p[0] - mean[0]).powi(2) + (p[1] - mean[1]).powi(2) + (p[2] - mean[2]).powi(2)).sqrt();
            let (nx, ny) = (x as f32 / w as f32 - 0.5, y as f32 / h as f32 - 0.5);
            let centre = 1.0 - 0.6 * (nx * nx + ny * ny).sqrt().min(0.7) / 0.7;
            let s = ((contrast * 2.0 + colour * 0.5) * centre).powi(2) as f64;
            sw += s;
            sx += s * (x as f64 + 0.5);
            sy += s * (y as f64 + 0.5);
        }
    }
    if sw <= 0.0 {
        return [0.5, 0.5];
    }
    [(sx / sw / w as f64) as f32, (sy / sw / h as f64) as f32]
}

/// Un cadrage suggéré : nom (clé de traduction) et rectangle normalisé x, y, largeur, hauteur.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub name: &'static str,
    pub rect: [f32; 4],
}

/// Cadrages de rapport `ratio` (largeur/hauteur en pixels) autour du point `subject`, pour une image
/// `w`x`h`. Les doublons (rectangles presque identiques) sont retirés.
pub fn candidates(subject: [f32; 2], ratio: f32, w: f32, h: f32) -> Vec<Candidate> {
    let ratio = if ratio > 0.0 { ratio } else { w / h };
    // Plus grand rectangle du rapport voulu dans l'image.
    let (mut fw, mut fh) = (1.0f32, w / (ratio * h));
    if fh > 1.0 {
        fh = 1.0;
        fw = h * ratio / w;
    }
    let place = |scale: f32, at: [f32; 2]| {
        let (cw, ch) = (fw * scale, fh * scale);
        [(subject[0] - at[0] * cw).clamp(0.0, 1.0 - cw), (subject[1] - at[1] * ch).clamp(0.0, 1.0 - ch), cw, ch]
    };
    let third = |v: f32| if v < 0.5 { 1.0 / 3.0 } else { 2.0 / 3.0 };
    let golden = |v: f32| if v < 0.5 { 0.382 } else { 0.618 };
    let thirds = [third(subject[0]), third(subject[1])];
    let phi = [golden(subject[0]), golden(subject[1])];
    let all = [
        (k!("Tiers"), place(0.85, thirds)),
        (k!("Centré"), place(0.85, [0.5, 0.5])),
        (k!("Triangle d'or"), place(0.85, phi)),
        (k!("Serré"), place(0.6, thirds)),
        (k!("Plein cadre"), place(1.0, [0.5, 0.5])),
    ];
    let mut out: Vec<Candidate> = Vec::new();
    for (name, rect) in all {
        let same = |o: &Candidate| o.rect.iter().zip(&rect).all(|(a, b)| (a - b).abs() < 0.02);
        if !out.iter().any(same) {
            out.push(Candidate { name, rect });
        }
    }
    out
}

/// Spirale d'or dans le carré unité : quarts de cercle dans des carrés successifs d'un rectangle
/// 1,618 x 1 (étiré ensuite sur le cadre). Points de (x, y), y vers le bas, parcourus dans le
/// sens horaire depuis l'extérieur vers le centre.
pub fn golden_spiral() -> Vec<[f32; 2]> {
    let phi = (1.0 + 5f32.sqrt()) / 2.0;
    let (mut x, mut y, mut w, mut h) = (0.0f32, 0.0f32, phi, 1.0f32);
    let mut pts = Vec::new();
    for k in 0..10 {
        let s = w.min(h);
        // (centre de l'arc, angle de départ) ; l'angle croît de 90° ; rectangle restant.
        let (c, a0) = match k % 4 {
            0 => ([x + s, y + s], 180.0f32),
            1 => ([x, y + s], 270.0),
            2 => ([x + w - s, y], 0.0),
            _ => ([x + s, y + h - s], 90.0),
        };
        let steps = 16;
        for i in 0..=steps {
            if k > 0 && i == 0 {
                continue;
            }
            let a = (a0 + 90.0 * i as f32 / steps as f32).to_radians();
            pts.push([(c[0] + s * a.cos()) / phi, c[1] + s * a.sin()]);
        }
        match k % 4 {
            0 => (x, w) = (x + s, w - s),
            1 => (y, h) = (y + s, h - s),
            2 => w -= s,
            _ => h -= s,
        }
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjust::Adjustments;

    /// Bord de pente `phi` degrés (y vers le bas) dans une image `n`x`n`.
    fn edge_image(n: usize, phi: f32) -> Vec<f32> {
        let t = phi.to_radians().tan();
        (0..n * n)
            .map(|i| {
                let (x, y) = ((i % n) as f32 - n as f32 / 2.0, (i / n) as f32 - n as f32 / 2.0);
                // Rampe large : un bord net crénelé biaiserait la mesure.
                (0.5 + (y - t * x) * 0.12).clamp(0.0, 1.0)
            })
            .collect()
    }

    #[test]
    fn tilt_is_found_and_straightens() {
        for phi in [-6.0f32, -2.0, 1.5, 4.0, 9.0] {
            let n = 256;
            let d = detect_tilt(&edge_image(n, phi), n, n).expect("inclinaison");
            assert!((d - phi).abs() < 0.5, "phi {phi}, trouvé {d}");
            // Redressement = -d : une ligne horizontale de sortie suit la ligne de la source.
            let a = Adjustments { angle: -d, ..Default::default() };
            let src = [1000.0, 1000.0];
            let p0 = a.out_to_src([0.0, 0.5], src, true);
            let p1 = a.out_to_src([1.0, 0.5], src, true);
            let slope = ((p1[1] - p0[1]) * src[1]).atan2((p1[0] - p0[0]) * src[0]).to_degrees();
            assert!((slope - phi).abs() < 0.5, "phi {phi}, pente source {slope}");
        }
    }

    #[test]
    fn level_image_gives_zero() {
        let n = 256;
        let d = detect_tilt(&edge_image(n, 0.0), n, n).expect("horizontale");
        assert_eq!(d, 0.0);
    }

    #[test]
    fn flat_image_has_no_tilt() {
        assert_eq!(detect_tilt(&vec![0.5; 64 * 64], 64, 64), None);
    }

    #[test]
    fn interest_follows_the_bright_blob() {
        let (w, h) = (96usize, 64usize);
        let rgb: Vec<[f32; 4]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let on = (x - 70.0).powi(2) + (y - 20.0).powi(2) < 100.0;
                if on { [0.9, 0.8, 0.2, 1.0] } else { [0.2, 0.2, 0.25, 1.0] }
            })
            .collect();
        let p = interest_point(&rgb, w, h);
        assert!(p[0] > 0.6 && p[1] < 0.4, "{p:?}");
    }

    #[test]
    fn candidates_keep_ratio_and_stay_inside() {
        let (w, h) = (6000.0, 4000.0);
        for ratio in [0.0, 1.0, 16.0 / 9.0, 0.8] {
            let want = if ratio > 0.0 { ratio } else { w / h };
            let list = candidates([0.7, 0.3], ratio, w, h);
            assert!(!list.is_empty());
            for c in list {
                let r = c.rect;
                assert!(r[0] >= -1e-4 && r[1] >= -1e-4 && r[0] + r[2] <= 1.0001 && r[1] + r[3] <= 1.0001, "{c:?}");
                assert!(((r[2] * w) / (r[3] * h) - want).abs() < 0.01, "{c:?}");
            }
        }
    }

    #[test]
    fn spiral_is_continuous_and_inside() {
        let pts = golden_spiral();
        for p in &pts {
            assert!((-1e-4..=1.0001).contains(&p[0]) && (-1e-4..=1.0001).contains(&p[1]), "{p:?}");
        }
        let longest = pts.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).fold(0.0f32, f32::max);
        assert!(longest < 0.12, "saut de {longest}");
    }

    /// Horizon tiltée de `phi` degrés avec nuages, relief et bruit pseudo-aléatoire.
    #[test]
    fn noisy_horizon_is_found() {
        let (w, h) = (640usize, 440usize);
        let noise = |x: usize, y: usize| {
            let mut v = (x as u32).wrapping_mul(374761393).wrapping_add((y as u32).wrapping_mul(668265263));
            v = (v ^ (v >> 13)).wrapping_mul(1274126177);
            ((v ^ (v >> 16)) & 0xffff) as f32 / 65535.0
        };
        for phi in [-4.0f32, 2.5, 5.0] {
            let t = phi.to_radians().tan();
            let lum: Vec<f32> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let above = (y as f32 - h as f32 / 2.0) < t * (x as f32 - w as f32 / 2.0);
                    let base = if above { 0.75 - 0.2 * (y as f32 / h as f32) } else { 0.25 };
                    // Bruit de valeur lissé (cellules de 6 px) plus un grain fin.
                    let (fx, fy) = (x as f32 / 6.0, y as f32 / 6.0);
                    let (ix, iy, ax, ay) = (fx as usize, fy as usize, fx.fract(), fy.fract());
                    let (sx, sy) = (ax * ax * (3.0 - 2.0 * ax), ay * ay * (3.0 - 2.0 * ay));
                    let top = noise(ix, iy) * (1.0 - sx) + noise(ix + 1, iy) * sx;
                    let bottom = noise(ix, iy + 1) * (1.0 - sx) + noise(ix + 1, iy + 1) * sx;
                    let smooth = top * (1.0 - sy) + bottom * sy;
                    (base + 0.12 * (smooth - 0.5) + 0.03 * (noise(x, y) - 0.5)).clamp(0.0, 1.0)
                })
                .collect();
            let d = detect_tilt(&lum, w, h).expect("horizon");
            assert!((d - phi).abs() < 0.8, "phi {phi}, trouvé {d}");
        }
    }
}
