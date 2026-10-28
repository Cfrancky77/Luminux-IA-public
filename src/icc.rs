//! Profil ICC de l'écran : l'image (sRGB) est convertie vers l'espace de l'écran à l'affichage.
//! L'export reste en sRGB.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use moxcms::{ColorProfile, Layout, TransformOptions};

use crate::gpu::SCREEN_LUT;
use crate::looks::Lut3d;

/// LUT 3D sRGB -> profil `icc` (rouge le plus rapide).
pub fn lut_from_bytes(icc: &[u8]) -> Result<Lut3d> {
    let dst = ColorProfile::new_from_slice(icc).map_err(|e| anyhow::anyhow!("{e:?}")).context("profil ICC illisible")?;
    let src = ColorProfile::new_srgb();
    let t = src
        .create_transform_f32(Layout::Rgb, &dst, Layout::Rgb, TransformOptions::default())
        .map_err(|e| anyhow::anyhow!("{e:?}"))
        .context("conversion vers ce profil impossible")?;
    let n = SCREEN_LUT as usize;
    let step = 1.0 / (n - 1) as f32;
    let mut grid = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                grid.extend([r as f32 * step, g as f32 * step, b as f32 * step]);
            }
        }
    }
    let mut out = vec![0.0f32; grid.len()];
    t.transform(&grid, &mut out).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let data = out.chunks_exact(3).map(|c| [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0)]).collect();
    Ok(Lut3d { size: SCREEN_LUT, data })
}

pub fn lut_from_file(path: &Path) -> Result<Lut3d> {
    let bytes = std::fs::read(path).with_context(|| format!("lecture de {}", path.display()))?;
    lut_from_bytes(&bytes)
}

/// Profil de l'écran déclaré à colord (`colormgr`), s'il y en a un.
pub fn detect() -> Option<PathBuf> {
    let out = std::process::Command::new("colormgr").args(["get-devices-by-kind", "display"]).output().ok()?;
    profile_in(&String::from_utf8_lossy(&out.stdout))
}

/// Premier fichier .icc/.icm cité dans la sortie de `colormgr`.
fn profile_in(text: &str) -> Option<PathBuf> {
    text.split_whitespace()
        .filter(|w| w.starts_with('/'))
        .find(|w| {
            let l = w.to_ascii_lowercase();
            l.ends_with(".icc") || l.ends_with(".icm")
        })
        .map(PathBuf::from)
}

/// Réglage `screen_icc` -> LUT : "" aucun, "auto" colord, sinon chemin du fichier.
pub fn from_setting(setting: &str) -> Result<Option<Lut3d>> {
    let path = match setting {
        "" => return Ok(None),
        "auto" => match detect() {
            Some(p) => p,
            None => return Ok(None),
        },
        p => PathBuf::from(p),
    };
    lut_from_file(&path).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(l: &Lut3d, r: usize, g: usize, b: usize) -> [f32; 3] {
        let n = l.size as usize;
        l.data[(b * n + g) * n + r]
    }

    #[test]
    fn srgb_profile_is_identity() {
        let l = lut_from_bytes(&ColorProfile::new_srgb().encode().unwrap()).unwrap();
        let n = SCREEN_LUT as usize;
        for (r, g, b) in [(0, 0, 0), (n - 1, n - 1, n - 1), (n - 1, 0, 0), (5, 20, 31)] {
            let c = at(&l, r, g, b);
            let want = [r, g, b].map(|v| v as f32 / (n - 1) as f32);
            for k in 0..3 {
                assert!((c[k] - want[k]).abs() < 0.01, "{:?} -> {:?}", (r, g, b), c);
            }
        }
    }

    #[test]
    fn srgb_red_on_display_p3() {
        let l = lut_from_bytes(&ColorProfile::new_display_p3().encode().unwrap()).unwrap();
        let n = SCREEN_LUT as usize;
        let red = at(&l, n - 1, 0, 0);
        // Rouge sRGB dans Display P3 : environ (0.917, 0.200, 0.139).
        assert!((red[0] - 0.917).abs() < 0.02 && (red[1] - 0.200).abs() < 0.02 && (red[2] - 0.139).abs() < 0.02, "{red:?}");
        let white = at(&l, n - 1, n - 1, n - 1);
        assert!(white.iter().all(|v| (v - 1.0).abs() < 0.01), "{white:?}");
    }

    #[test]
    fn colormgr_output() {
        let text = "Object Path:   /org/freedesktop/ColorManager/devices/xrandr_DP_1\nProfile 1:     icc-abc\n               /home/user/.local/share/icc/edid-123.icc\n";
        assert_eq!(profile_in(text), Some(PathBuf::from("/home/user/.local/share/icc/edid-123.icc")));
        assert_eq!(profile_in("Object Path: /org/x\n"), None);
    }
}
