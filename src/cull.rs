//! Tri des photos : netteté (photos floues) et empreinte d'image (doublons, rafales).

use image::{DynamicImage, GrayImage};

/// En dessous : photo probablement floue (mesure sur 1024 px ; une photo nette donne 150 à 350,
/// la même floutée de 2 px environ 30 à 60).
pub const BLUR_LIMIT: f32 = 40.0;
/// Écart d'empreinte (bits sur 64) jusqu'auquel deux photos sont des doublons ou une rafale.
pub const SAME_LIMIT: u32 = 6;

/// Netteté et empreinte d'une photo, d'après un aperçu d'au plus 1024 px.
pub fn analyse(img: &DynamicImage) -> (f32, u64) {
    let g = img.to_luma8();
    let g = if g.width().max(g.height()) > 1024 {
        let s = 1024.0 / g.width().max(g.height()) as f32;
        image::imageops::resize(&g, ((g.width() as f32 * s).round() as u32).max(1), ((g.height() as f32 * s).round() as u32).max(1), image::imageops::FilterType::Triangle)
    } else {
        g
    };
    (sharpness(&g), dhash(&g))
}

/// Laplacien moyen au carré, par case d'une grille 8 × 8 ; moyenne des 10 % de cases les plus nettes, pour
/// qu'un fond flou volontaire (portrait) ne compte pas.
fn sharpness(g: &GrayImage) -> f32 {
    let (w, h) = (g.width() as usize, g.height() as usize);
    if w < 24 || h < 24 {
        return 0.0;
    }
    let px = |x: usize, y: usize| g.as_raw()[y * w + x] as f32 / 255.0;
    let n = 8;
    let (tw, th) = ((w - 2) / n, (h - 2) / n);
    let mut tiles = Vec::with_capacity(n * n);
    for ty in 0..n {
        for tx in 0..n {
            let mut sum = 0.0;
            for y in 1 + ty * th..1 + (ty + 1) * th {
                for x in 1 + tx * tw..1 + (tx + 1) * tw {
                    let l = 4.0 * px(x, y) - px(x - 1, y) - px(x + 1, y) - px(x, y - 1) - px(x, y + 1);
                    sum += l * l;
                }
            }
            tiles.push(sum / (tw * th).max(1) as f32);
        }
    }
    tiles.sort_by(|a, b| b.total_cmp(a));
    let k = (tiles.len() / 10).max(1);
    (tiles[..k].iter().sum::<f32>() / k as f32).sqrt() * 1000.0
}

/// Empreinte par différence (dHash) : 9 × 8 pixels, un bit par comparaison de voisins.
fn dhash(g: &GrayImage) -> u64 {
    let s = image::imageops::resize(g, 9, 8, image::imageops::FilterType::Triangle);
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h = (h << 1) | u64::from(s.get_pixel(x, y)[0] > s.get_pixel(x + 1, y)[0]);
        }
    }
    h
}

/// Groupes de photos semblables (indices dans `hashes`), chacun d'au moins deux photos.
pub fn groups(hashes: &[u64]) -> Vec<Vec<usize>> {
    let n = hashes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            if (hashes[i] ^ hashes[j]).count_ones() <= SAME_LIMIT {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut map: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..n {
        let r = root(&mut parent, i);
        map.entry(r).or_default().push(i);
    }
    map.into_values().filter(|g| g.len() > 1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> DynamicImage {
        // Damier et disque : des bords nets partout.
        let img = image::RgbImage::from_fn(800, 600, |x, y| {
            let d = ((x as f32 - 500.0).powi(2) + (y as f32 - 300.0).powi(2)).sqrt();
            let v = if ((x / 40) + (y / 40)) % 2 == 0 { 200 } else { 60 };
            let v = if d < 120.0 { 255 - v } else { v };
            image::Rgb([v, v / 2 + 40, 255 - v])
        });
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn blur_is_detected_and_resized_copies_match() {
        let sharp = scene();
        let (s, h) = analyse(&sharp);
        let blurred = sharp.blur(3.0);
        let (sb, hb) = analyse(&blurred);
        assert!(s > BLUR_LIMIT && sb < BLUR_LIMIT, "{s} {sb}");
        // Même photo, plus petite et un peu floue : même groupe.
        let small = sharp.resize(400, 300, image::imageops::FilterType::Triangle);
        let (_, hs) = analyse(&small);
        assert!((h ^ hs).count_ones() <= SAME_LIMIT && (h ^ hb).count_ones() <= SAME_LIMIT);
        // Autre photo : groupe à part.
        let other = DynamicImage::ImageRgb8(image::RgbImage::from_fn(800, 600, |x, y| image::Rgb([(x / 3) as u8, (y / 3) as u8, 128])));
        let (_, ho) = analyse(&other);
        assert_eq!(groups(&[h, hs, ho, hb]), vec![vec![0, 1, 3]]);
    }
}
