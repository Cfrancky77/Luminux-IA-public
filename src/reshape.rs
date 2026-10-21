//! Remodelage du portrait (visage aminci, yeux agrandis, silhouette) : repères tirés des masques IA,
//! déformation faite par le shader (`warp` dans develop.wgsl).

pub const MAX_FACES: usize = 4;
pub const MAX_EYES: usize = 8;
/// Bandes horizontales des silhouettes : `MAX_PEOPLE` personnes de `PERSON_ROWS` bandes.
pub const BODY_ROWS: usize = 256;
pub const MAX_PEOPLE: usize = 4;
pub const PERSON_ROWS: usize = BODY_ROWS / MAX_PEOPLE;

/// Visage : milieu des yeux, axe des yeux (unitaire, de gauche à droite) et écart des yeux, en pixels source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceFrame {
    pub o: [f32; 2],
    pub u: [f32; 2],
    pub d: f32,
}

impl FaceFrame {
    /// Bas du menton (≈ 1,6 écart des yeux sous leur milieu).
    pub fn chin_y(&self) -> f32 {
        self.o[1] + self.u[0] * 1.6 * self.d
    }
}

/// Œil : centre et rayon d'influence, en pixels source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eye {
    pub c: [f32; 2],
    pub r: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Geometry {
    pub faces: Vec<FaceFrame>,
    pub eyes: Vec<Eye>,
    /// Silhouettes des personnes, de gauche à droite ; vide sans masque du sujet.
    pub body: Vec<Person>,
}

struct Blob {
    area: f32,
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
}

/// Taches du masque au-dessus de `thr` (4-connexité), en pixels du masque.
fn blobs(mask: &[f32], w: usize, h: usize, thr: f32) -> Vec<Blob> {
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if seen[start] || mask[start] < thr {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut n, mut sx, mut sy) = (0f32, 0f32, 0f32);
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            n += 1.0;
            sx += x as f32;
            sy += y as f32;
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            let mut push = |j: usize| {
                if !seen[j] && mask[j] >= thr {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        out.push(Blob { area: n, cx: sx / n + 0.5, cy: sy / n + 0.5, w: (x1 - x0 + 1) as f32, h: (y1 - y0 + 1) as f32 });
    }
    out
}

/// Yeux et visages d'après le masque des yeux (`w`x`h`) ; `scale` : pixels source par pixel du masque.
pub fn eyes_and_faces(eyes_mask: &[f32], w: usize, h: usize, scale: f32) -> (Vec<FaceFrame>, Vec<Eye>) {
    let min_area = ((w.max(h) as f32 / 400.0).powi(2)).max(4.0);
    let mut b: Vec<Blob> = blobs(eyes_mask, w, h, 0.4).into_iter().filter(|b| b.area >= min_area).collect();
    b.sort_by(|a, c| c.area.total_cmp(&a.area));
    b.truncate(MAX_EYES);
    // Paires d'yeux : même hauteur à peu près, écart de 1,5 à 5 largeurs d'œil, tailles voisines.
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for i in 0..b.len() {
        for j in i + 1..b.len() {
            let (dx, dy) = (b[j].cx - b[i].cx, b[j].cy - b[i].cy);
            let dist = (dx * dx + dy * dy).sqrt();
            let ew = b[i].w.max(b[j].w);
            let ratio = b[i].area.max(b[j].area) / b[i].area.min(b[j].area);
            if dist >= 1.5 * ew && dist <= 5.0 * ew && dy.abs() < 0.5 * dist && ratio < 4.0 {
                pairs.push((dist / ew - 2.2 + ratio.ln(), i, j));
            }
        }
    }
    pairs.sort_by(|a, c| a.0.abs().total_cmp(&c.0.abs()));
    let mut used = vec![false; b.len()];
    let mut faces = Vec::new();
    for (_, i, j) in pairs {
        if used[i] || used[j] || faces.len() >= MAX_FACES {
            continue;
        }
        used[i] = true;
        used[j] = true;
        let (l, r) = if b[i].cx <= b[j].cx { (&b[i], &b[j]) } else { (&b[j], &b[i]) };
        let (dx, dy) = ((r.cx - l.cx) * scale, (r.cy - l.cy) * scale);
        let d = (dx * dx + dy * dy).sqrt();
        faces.push(FaceFrame { o: [(l.cx + r.cx) * 0.5 * scale, (l.cy + r.cy) * 0.5 * scale], u: [dx / d, dy / d], d });
    }
    let eyes = b.iter().map(|e| Eye { c: [e.cx * scale, e.cy * scale], r: 1.1 * e.w.max(e.h) * scale }).collect();
    (faces, eyes)
}

/// Silhouette d'une personne : bandes horizontales entre `top` et `bottom` (pixels source).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Person {
    pub top: f32,
    pub bottom: f32,
    /// Par bande (`PERSON_ROWS`) : axe x, demi-largeur, poids 0..1 (nul sur la tête).
    pub rows: Vec<[f32; 3]>,
    /// Ventre : centre en y et étendue (pixels source) ; étendue nulle si le ventre est hors de l'image.
    pub belly: [f32; 2],
    /// Place libre de part et d'autre de l'axe (jusqu'à la personne voisine ou au bord de l'image).
    pub room: [f32; 2],
}

/// Étiquettes des taches du masque au-dessus de `thr` (4-connexité) : étiquette par pixel (0 = fond)
/// et surface de chaque tache (indice = étiquette - 1).
fn labels(mask: &[f32], w: usize, h: usize, thr: f32) -> (Vec<u32>, Vec<usize>) {
    let mut lab = vec![0u32; w * h];
    let mut areas = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if lab[start] != 0 || mask[start] < thr {
            continue;
        }
        let id = areas.len() as u32 + 1;
        lab[start] = id;
        stack.push(start);
        let mut n = 0;
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, y) = (i % w, i / w);
            let mut push = |j: usize| {
                if lab[j] == 0 && mask[j] >= thr {
                    lab[j] = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        areas.push(n);
    }
    (lab, areas)
}

/// Silhouettes des personnes d'après le masque du sujet : une par tache assez grande (les plus
/// grandes d'abord, `MAX_PEOPLE` au plus), triées de gauche à droite. Deux personnes qui se touchent
/// forment une seule silhouette.
pub fn body_profiles(subject: &[f32], w: usize, h: usize, scale: f32, faces: &[FaceFrame]) -> Vec<Person> {
    let (lab, areas) = labels(subject, w, h, 0.5);
    let largest = areas.iter().copied().max().unwrap_or(0);
    let mut ids: Vec<usize> = (0..areas.len()).filter(|&i| areas[i] >= (w * h / 500).max(largest / 12).max(16)).collect();
    ids.sort_by_key(|&i| std::cmp::Reverse(areas[i]));
    ids.truncate(MAX_PEOPLE);
    let mut people: Vec<(f32, Person, f32, f32)> = ids.iter().filter_map(|&i| person(&lab, i as u32 + 1, w, h, scale, faces)).collect();
    people.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Place libre : jusqu'au bord de la voisine, sinon de l'image.
    let n = people.len();
    for k in 0..n {
        let axis = people[k].0;
        let left = if k > 0 { people[k - 1].3 } else { 0.0 };
        let right = if k + 1 < n { people[k + 1].2 } else { w as f32 * scale };
        people[k].1.room = [(axis - left).max(0.0), (right - axis).max(0.0)];
    }
    people.into_iter().map(|p| p.1).collect()
}

/// Profil d'une personne (étiquette `id`) ; renvoie aussi son axe moyen et ses bords gauche et droit.
fn person(lab: &[u32], id: u32, w: usize, h: usize, scale: f32, faces: &[FaceFrame]) -> Option<(f32, Person, f32, f32)> {
    let rows: Vec<Option<(f32, f32)>> = (0..h)
        .map(|y| {
            let line = &lab[y * w..(y + 1) * w];
            let l = line.iter().position(|&v| v == id)?;
            let r = line.iter().rposition(|&v| v == id)?;
            Some((l as f32, r as f32 + 1.0))
        })
        .collect();
    let top = rows.iter().position(Option::is_some)?;
    let bottom = rows.iter().rposition(Option::is_some)?;
    let height = (bottom - top + 1) as f32;
    let (left, right) = rows.iter().flatten().fold((f32::MAX, 0f32), |(l, r), &(a, b)| (l.min(a), r.max(b)));
    // Tête : visage dont les yeux sont dans la silhouette, sinon le haut de la silhouette.
    let main = faces
        .iter()
        .filter(|f| {
            let (x, y) = (f.o[0] / scale, f.o[1] / scale);
            let yi = (y as usize).min(h - 1);
            y >= top as f32 && rows[yi].is_some_and(|(l, r)| x >= l - 2.0 && x <= r + 2.0)
        })
        .max_by(|a, b| a.d.total_cmp(&b.d));
    let (head0, head1) = match main {
        Some(f) => (f.chin_y() / scale, f.chin_y() / scale + 1.5 * f.d / scale),
        None => (top as f32 + 0.12 * height, top as f32 + 0.3 * height),
    };
    // Ventre : vers le nombril, ≈ 6 écarts des yeux sous le menton ; sinon au milieu de la silhouette.
    let (belly_c, belly_s) = match main {
        Some(f) => (f.chin_y() / scale + 6.0 * f.d / scale, 1.6 * f.d / scale),
        None => (top as f32 + 0.45 * height, 0.08 * height),
    };
    let belly = if belly_c + 0.5 * belly_s <= bottom as f32 { [belly_c * scale, belly_s * scale] } else { [0.0, 0.0] };
    // Bandes jusqu'un peu sous la silhouette : le poids s'y éteint (pas de cassure sous les pieds).
    let end = (bottom as f32 + 0.06 * h as f32).min(h as f32);
    let k = ((h as f32 * 0.05) as usize).max(1);
    let mut out = Vec::with_capacity(PERSON_ROWS);
    for band in 0..PERSON_ROWS {
        let yc = top as f32 + (band as f32 + 0.5) / PERSON_ROWS as f32 * (end - top as f32);
        let y = (yc as usize).min(h - 1);
        let (mut sl, mut sr, mut n, mut cov) = (0f32, 0f32, 0f32, 0f32);
        for yy in y.saturating_sub(k)..(y + k + 1).min(h) {
            cov += 1.0;
            if let Some((l, r)) = rows[yy] {
                sl += l;
                sr += r;
                n += 1.0;
            }
        }
        if n == 0.0 {
            out.push([0.0, 0.0, 0.0]);
            continue;
        }
        let (l, r) = (sl / n, sr / n);
        out.push([(l + r) * 0.5 * scale, (r - l) * 0.5 * scale, smoothstep(head0, head1, yc) * n / cov]);
    }
    // Une seule demi-largeur pour toutes les bandes : les lignes verticales du fond restent droites.
    let mut widths: Vec<f32> = out.iter().filter(|b| b[2] > 0.5).map(|b| b[1]).collect();
    if !widths.is_empty() {
        widths.sort_by(f32::total_cmp);
        let hw = widths[(widths.len() * 9 / 10).min(widths.len() - 1)];
        for b in &mut out {
            b[1] = hw;
        }
    }
    let weighted: Vec<&[f32; 3]> = out.iter().filter(|b| b[2] > 0.0).collect();
    let axis = if weighted.is_empty() { (left + right) * 0.5 * scale } else { weighted.iter().map(|b| b[0]).sum::<f32>() / weighted.len() as f32 };
    Some((axis, Person { top: top as f32 * scale, bottom: end * scale, rows: out, belly, room: [0.0; 2] }, left * scale, right * scale))
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(m: &mut [f32], w: usize, cx: f32, cy: f32, rx: f32, ry: f32) {
        let h = m.len() / w;
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
                if dx * dx + dy * dy <= 1.0 {
                    m[y * w + x] = 1.0;
                }
            }
        }
    }

    #[test]
    fn pairs_two_eyes_into_a_face() {
        let (w, h) = (400, 300);
        let mut m = vec![0f32; w * h];
        disc(&mut m, w, 150.0, 120.0, 14.0, 6.0);
        disc(&mut m, w, 210.0, 120.0, 14.0, 6.0);
        // Œil isolé d'un autre visage, de profil.
        disc(&mut m, w, 350.0, 60.0, 8.0, 4.0);
        let (faces, eyes) = eyes_and_faces(&m, w, h, 2.0);
        assert_eq!(eyes.len(), 3);
        assert_eq!(faces.len(), 1);
        let f = faces[0];
        assert!((f.o[0] - 360.0).abs() < 2.0 && (f.o[1] - 240.0).abs() < 2.0, "{f:?}");
        assert!((f.d - 120.0).abs() < 2.0 && (f.u[0] - 1.0).abs() < 1e-3, "{f:?}");
    }

    #[test]
    fn body_profile_skips_the_head() {
        let (w, h) = (200, 400);
        let mut m = vec![0f32; w * h];
        disc(&mut m, w, 100.0, 60.0, 30.0, 40.0); // tête
        for y in 100..400 {
            for x in 60..140 {
                m[y * w + x] = 1.0;
            }
        }
        let people = body_profiles(&m, w, h, 1.0, &[]);
        assert_eq!(people.len(), 1);
        let p = &people[0];
        assert_eq!(p.rows.len(), PERSON_ROWS);
        assert!(p.top < 21.0 && p.bottom >= 399.0, "{} {}", p.top, p.bottom);
        assert!(p.rows[3][2] < 0.05, "tête : {:?}", p.rows[3]);
        let torso = p.rows[40];
        assert!((torso[0] - 100.0).abs() < 1.0 && (torso[1] - 40.0).abs() < 1.0 && torso[2] > 0.99, "{torso:?}");
        assert!(p.rows.iter().all(|b| b[1] == torso[1]), "même demi-largeur partout");
        assert!(p.belly[1] > 0.0 && p.belly[0] > 150.0 && p.belly[0] < 250.0, "{:?}", p.belly);
        assert_eq!(p.room, [100.0, 100.0]);
    }

    #[test]
    fn two_people_are_separate() {
        let (w, h) = (300, 200);
        let mut m = vec![0f32; w * h];
        for y in 40..200 {
            for x in (40..90).chain(180..250) {
                m[y * w + x] = 1.0;
            }
        }
        let people = body_profiles(&m, w, h, 2.0, &[]);
        assert_eq!(people.len(), 2);
        let (a, b) = (&people[0], &people[1]);
        assert!(a.rows[40][0] < b.rows[40][0], "de gauche à droite");
        assert!((a.rows[40][0] - 130.0).abs() < 2.0 && (b.rows[40][0] - 430.0).abs() < 2.0);
        // La place à droite de la première s'arrête au bord gauche de la seconde (x = 360).
        assert!((a.room[1] - 230.0).abs() < 2.0, "{:?}", a.room);
        assert!((b.room[0] - (430.0 - 180.0)).abs() < 2.0, "{:?}", b.room);
    }

}
