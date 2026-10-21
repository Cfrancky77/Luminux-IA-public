//! Remplissage d'un trou par des morceaux de la photo elle-même, recopiés du bord vers l'intérieur
//! (« exemplar-based inpainting », Criminisi et al. 2004).
//! Le trou reçoit une vraie texture (vagues, herbe, grain) au lieu d'un dégradé lisse. Sert seul
//! quand LaMa manque, et après LaMa pour rendre la finesse que le passage en 512x512 lui fait perdre :
//! LaMa donne alors la forme d'ensemble, les morceaux recopiés donnent le grain.

use rayon::prelude::*;

/// Demi-côté d'un morceau : 9x9 px.
const R: i32 = 4;
/// Morceaux sources tirés au hasard à chaque pas, en plus des voisins déjà choisis.
const SAMPLES: usize = 1200;
/// Poids de l'estimation LaMa face aux pixels réellement connus.
const GUIDE: f32 = 0.35;
/// Décalage du morceau vers l'intérieur du trou, depuis le pixel du bord choisi.
const SHIFT: i32 = 3;

/// Remplit `hole` (> 0,5 = à remplacer). `guide` : estimation du trou par LaMa, qui oriente le choix
/// des morceaux. Les pixels hors trou sont rendus tels quels.
pub fn fill(img: &[[f32; 4]], hole: &[f32], w: u32, h: u32, guide: Option<&[[f32; 4]]>) -> Vec<[f32; 4]> {
    let (w, h) = (w as i32, h as i32);
    // Comparaison sur la racine de la valeur linéaire, proche de la perception.
    let to = |p: &[f32; 4]| [p[0].max(0.0).sqrt(), p[1].max(0.0).sqrt(), p[2].max(0.0).sqrt()];
    let mut px: Vec<[f32; 3]> = img.iter().map(to).collect();
    let guide: Option<Vec<[f32; 3]>> = guide.map(|g| g.iter().map(to).collect());
    let mut known: Vec<bool> = hole.iter().map(|v| *v <= 0.5).collect();
    let mut todo = known.iter().filter(|k| !**k).count();
    if todo == 0 || w <= 2 * R + 2 || h <= 2 * R + 2 {
        return img.to_vec();
    }
    let idx = |x: i32, y: i32| (y * w + x) as usize;
    // Sources : morceaux entièrement connus dès le départ (jamais un morceau déjà recopié).
    let orig = known.clone();
    let mut integral = vec![0u32; ((w + 1) * (h + 1)) as usize];
    for y in 0..h {
        for x in 0..w {
            let v = (!orig[idx(x, y)]) as u32;
            integral[((y + 1) * (w + 1) + x + 1) as usize] = v + integral[(y * (w + 1) + x + 1) as usize] + integral[((y + 1) * (w + 1) + x) as usize] - integral[(y * (w + 1) + x) as usize];
        }
    }
    let holes_in = |x0: i32, y0: i32, x1: i32, y1: i32| {
        let g = |x: i32, y: i32| integral[(y * (w + 1) + x) as usize];
        g(x1, y1) + g(x0, y0) - g(x0, y1) - g(x1, y0)
    };
    let valid = |x: i32, y: i32| x >= R && y >= R && x < w - R && y < h - R && holes_in(x - R, y - R, x + R + 1, y + R + 1) == 0;
    // Morceaux sources tirés au hasard parmi les positions valides (sans en dresser la liste : la zone
    // peut compter des millions de pixels).
    let Some(first) = (R..h - R).flat_map(|y| (R..w - R).map(move |x| (x, y))).find(|&(x, y)| valid(x, y)) else {
        return img.to_vec();
    };
    let draw = |rng: &mut Rng| {
        for _ in 0..64 {
            let c = (R + rng.below((w - 2 * R) as usize) as i32, R + rng.below((h - 2 * R) as usize) as i32);
            if valid(c.0, c.1) {
                return c;
            }
        }
        first
    };
    let mut conf: Vec<f32> = known.iter().map(|k| *k as u8 as f32).collect();
    // Pour chaque pixel recopié : pixel source d'où il vient (cohérence entre voisins).
    let mut origin: Vec<(i32, i32)> = vec![(-1, -1); known.len()];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ ((w as u64) << 32) ^ h as u64);
    let on_front = |known: &[bool], x: i32, y: i32| {
        !known[idx(x, y)] && [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|(dx, dy)| {
            let (xx, yy) = (x + dx, y + dy);
            xx >= 0 && yy >= 0 && xx < w && yy < h && known[idx(xx, yy)]
        })
    };
    let patch_conf = |conf: &[f32], x: i32, y: i32| {
        let mut s = 0.0;
        for yy in (y - R).max(0)..=(y + R).min(h - 1) {
            for xx in (x - R).max(0)..=(x + R).min(w - 1) {
                s += conf[idx(xx, yy)];
            }
        }
        s
    };
    // Bord du trou : file de priorité (pixel le plus « sûr » d'abord, le plus entouré de pixels connus).
    // Une priorité périmée reste dans la file ; elle est reconnue à sa version et sautée.
    let mut in_front = vec![false; known.len()];
    let mut version = vec![0u32; known.len()];
    let mut heap = std::collections::BinaryHeap::new();
    let key = |c: f32| (c * 4096.0) as u64;
    for y in 0..h {
        for x in 0..w {
            if on_front(&known, x, y) {
                in_front[idx(x, y)] = true;
                heap.push((key(patch_conf(&conf, x, y)), std::cmp::Reverse(idx(x, y)), 0u32));
            }
        }
    }
    while todo > 0 {
        let Some((_, std::cmp::Reverse(k), ver)) = heap.pop() else { break };
        if !in_front[k] || ver != version[k] {
            continue;
        }
        let (fx, fy) = ((k % w as usize) as i32, (k / w as usize) as i32);
        // Morceau décalé vers l'intérieur du trou : il remplit bien plus d'un pixel à chaque pas.
        let (mut mx, mut my, mut n) = (0f32, 0f32, 0f32);
        for dy in -R..=R {
            for dx in -R..=R {
                let (x, y) = (fx + dx, fy + dy);
                if x >= 0 && y >= 0 && x < w && y < h && !known[idx(x, y)] {
                    mx += dx as f32;
                    my += dy as f32;
                    n += 1.0;
                }
            }
        }
        let len = (mx * mx + my * my).sqrt();
        let (tx, ty) = if n > 0.0 && len > 1e-3 {
            let k = SHIFT as f32 / len;
            ((fx + (mx * k).round() as i32).clamp(0, w - 1), (fy + (my * k).round() as i32).clamp(0, h - 1))
        } else {
            (fx, fy)
        };
        let dist = |sx: i32, sy: i32, stop: f32| {
            let (mut sum, mut wsum) = (0f32, 0f32);
            for dy in -R..=R {
                let (y, yy) = (ty + dy, sy + dy);
                if y < 0 || y >= h {
                    continue;
                }
                for dx in -R..=R {
                    let x = tx + dx;
                    if x < 0 || x >= w {
                        continue;
                    }
                    let k = idx(x, y);
                    let (a, wt) = if known[k] {
                        (px[k], 1.0)
                    } else if let Some(g) = &guide {
                        (g[k], GUIDE)
                    } else {
                        continue;
                    };
                    let b = px[idx(sx + dx, yy)];
                    sum += wt * ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2));
                    wsum += wt;
                }
                if sum > stop * 81.0 {
                    return f32::MAX;
                }
            }
            sum / wsum.max(1e-6)
        };
        let mut best = (f32::MAX, first);
        let try_src = |c: (i32, i32), best: &mut (f32, (i32, i32))| {
            if valid(c.0, c.1) {
                let d = dist(c.0, c.1, best.0);
                if d < best.0 {
                    *best = (d, c);
                }
            }
        };
        // Cohérence : prolonger ce que les voisins recopiés ont pris.
        for dy in -R..=R {
            for dx in -R..=R {
                let (x, y) = (tx + dx, ty + dy);
                if x >= 0 && y >= 0 && x < w && y < h {
                    let o = origin[idx(x, y)];
                    if o.0 >= 0 {
                        try_src((o.0 - dx, o.1 - dy), &mut best);
                    }
                }
            }
        }
        // Tirage au hasard réparti sur les cœurs du processeur ; même résultat qu'en un seul fil.
        let cands: Vec<(i32, i32)> = (0..SAMPLES).map(|_| draw(&mut rng)).collect();
        let seed = best;
        best = cands
            .par_chunks(SAMPLES / 16)
            .map(|ch| {
                let mut b = seed;
                for &c in ch {
                    try_src(c, &mut b);
                }
                b
            })
            .reduce(|| seed, |a, b| if b.0 < a.0 { b } else { a });
        // Affinage autour du meilleur trouvé.
        let mut rad = 16;
        while rad >= 1 {
            for _ in 0..4 {
                let c = (best.1.0 + rng.range(rad), best.1.1 + rng.range(rad));
                try_src(c, &mut best);
            }
            rad /= 2;
        }
        let (sx, sy) = best.1;
        let c = patch_conf(&conf, tx, ty) / ((2 * R + 1) * (2 * R + 1)) as f32;
        for dy in -R..=R {
            for dx in -R..=R {
                let (x, y) = (tx + dx, ty + dy);
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let k = idx(x, y);
                if !known[k] {
                    px[k] = px[idx(sx + dx, sy + dy)];
                    known[k] = true;
                    conf[k] = c;
                    origin[k] = (sx + dx, sy + dy);
                    todo -= 1;
                }
            }
        }
        // Bord et priorités ne changent qu'autour du morceau recopié.
        for y in (ty - 2 * R - 1).max(0)..=(ty + 2 * R + 1).min(h - 1) {
            for x in (tx - 2 * R - 1).max(0)..=(tx + 2 * R + 1).min(w - 1) {
                let k = idx(x, y);
                in_front[k] = on_front(&known, x, y);
                if in_front[k] {
                    version[k] += 1;
                    heap.push((key(patch_conf(&conf, x, y)), std::cmp::Reverse(k), version[k]));
                }
            }
        }
    }
    px.iter().zip(img).zip(hole).map(|((v, o), m)| if *m > 0.5 { [v[0] * v[0], v[1] * v[1], v[2] * v[2], 1.0] } else { *o }).collect()
}

/// Petit générateur pseudo-aléatoire (xorshift), reproductible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// Entier dans [-r, r].
    fn range(&mut self, r: i32) -> i32 {
        (self.next() % (2 * r as u64 + 1)) as i32 - r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rayures : le trou doit être rempli de rayures (contraste conservé), pas d'un gris moyen.
    #[test]
    fn a_hole_in_stripes_gets_stripes_back() {
        let (w, h) = (96u32, 96u32);
        let img: Vec<[f32; 4]> = (0..h).flat_map(|_| (0..w).map(|x| if (x / 4) % 2 == 0 { [0.8, 0.8, 0.8, 1.0] } else { [0.05, 0.05, 0.05, 1.0] })).collect();
        let hole: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| if (32..64).contains(&x) && (32..64).contains(&y) { 1.0 } else { 0.0 })).collect();
        let out = fill(&img, &hole, w, h, None);
        let inside: Vec<f32> = (40..56).flat_map(|y| (36..60).map(move |x| (x, y))).map(|(x, y)| out[(y * w + x) as usize][0]).collect();
        let (lo, hi) = inside.iter().fold((1f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
        assert!(hi > 0.6 && lo < 0.15, "contraste perdu : {lo}..{hi}");
        // Hors du trou, rien ne change.
        assert_eq!(out[0], img[0]);
    }
}

#[cfg(test)]
mod essai {
    /// Essai manuel : LUMINUX_ESSAI=photo.png,x0,y0,x1,y1 cargo test essai_gomme -- --ignored
    #[test]
    #[ignore]
    fn essai_gomme() {
        let arg = std::env::var("LUMINUX_ESSAI").unwrap();
        let v: Vec<&str> = arg.split(',').collect();
        let img = image::open(v[0]).unwrap().to_rgba32f();
        let (w, h) = img.dimensions();
        let b: Vec<u32> = v[1..5].iter().map(|s| s.parse().unwrap()).collect();
        let px: Vec<[f32; 4]> = img.pixels().map(|p| [crate::image_io::srgb_to_linear(p[0]), crate::image_io::srgb_to_linear(p[1]), crate::image_io::srgb_to_linear(p[2]), 1.0]).collect();
        let hole: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| if x >= b[0] && x < b[2] && y >= b[1] && y < b[3] { 1.0 } else { 0.0 }).collect();
        let t = std::time::Instant::now();
        let out = super::fill(&px, &hole, w, h, None);
        eprintln!("{:?}", t.elapsed());
        let mut o = image::RgbImage::new(w, h);
        for (i, p) in out.iter().enumerate() {
            let c = |x: f32| (crate::retouch::to_srgb(x).clamp(0.0, 1.0) * 255.0).round() as u8;
            o.put_pixel(i as u32 % w, i as u32 / w, image::Rgb([c(p[0]), c(p[1]), c(p[2])]));
        }
        o.save(format!("{}.rempli.png", v[0])).unwrap();
    }
}
