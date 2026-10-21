//! Export par lots : chaque photo est développée avec ses réglages (`.luminux`)
//! sur un Developer GPU dédié, dans un thread à part.

use crate::{app::load_sidecar, gpu::Developer, image_io};
use eframe::{egui, wgpu};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

pub enum BatchMsg {
    Progress { done: usize, total: usize },
    Done { ok: usize, errors: Vec<String>, dir: PathBuf },
}

/// Chemin de sortie libre : `nom.ext`, sinon `nom_1.ext`, `nom_2.ext`…
/// (jamais l'original, même si le dossier et le format sont les mêmes).
fn free_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut k = 1;
    while p.exists() {
        p = dir.join(format!("{stem}_{k}.{ext}"));
        k += 1;
    }
    p
}

fn export_one(photo: &Path, dir: &Path, ext: &str, opts: &crate::export::Options, device: &wgpu::Device, queue: &wgpu::Queue) -> anyhow::Result<()> {
    let mut img = image_io::load(photo)?;
    let adj = load_sidecar(photo).unwrap_or_default();
    let (w, h) = (img.width, img.height);
    let mut canvas = crate::retouch::F32Canvas { data: &mut img.pixels, w, h };
    crate::denoise::apply(&mut canvas, Some(photo), &adj.denoise, true);
    crate::retouch::apply_all(&mut canvas, Some(photo), &adj.retouch, true);
    let mut dev = Developer::new(device, queue, wgpu::TextureFormat::Bgra8Unorm, &img)?;
    drop(img);
    // Masques IA : cache, sinon calcul (modèle téléchargé si besoin).
    let masks = crate::ai::masks_for(photo, &adj, &dev, device, queue, true);
    dev.apply_assets_with(device, queue, &adj, &|m| masks.get(&m).cloned());
    crate::lens::setup(&mut dev, photo, &adj.lens);
    let (w, h, px) = dev.render_full(device, queue, &adj)?;
    let stem = crate::library::export_stem(photo);
    crate::export::write(&free_name(dir, &stem, ext), Some(photo), w, h, &px, opts)
}

pub fn run(photos: Vec<PathBuf>, dir: PathBuf, ext: String, opts: crate::export::Options, device: wgpu::Device, queue: wgpu::Queue, tx: Sender<BatchMsg>, ctx: egui::Context) {
    let total = photos.len();
    let mut errors = Vec::new();
    for (i, p) in photos.iter().enumerate() {
        if let Err(e) = export_one(p, &dir, &ext, &opts, &device, &queue) {
            errors.push(format!("{} : {e:#}", p.display()));
        }
        let _ = tx.send(BatchMsg::Progress { done: i + 1, total });
        ctx.request_repaint();
    }
    let _ = tx.send(BatchMsg::Done { ok: total - errors.len(), errors, dir });
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    #[test]
    fn free_name_never_overwrites() {
        let d = std::env::temp_dir().join(format!("luminux_batch_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.jpg"), b"x").unwrap();
        assert_eq!(super::free_name(&d, "a", "jpg"), d.join("a_1.jpg"));
        assert_eq!(super::free_name(&d, "b", "jpg"), d.join("b.jpg"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
