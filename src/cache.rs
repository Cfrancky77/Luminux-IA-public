//! Purge du cache : masques IA (`masks/`), résultats des gommes et générations (`retouches/`),
//! débruitages (`denoise/`) et images de diagnostic de la gomme IA (`generation/`). Tout se recalcule ; la purge ne touche jamais aux photos, aux fichiers
//! `.luminux` ni au catalogue.
//!
//! Le nom d'un fichier du cache est un condensé (chemin, date, taille…) : on ne peut pas retrouver sa
//! photo. On calcule donc les noms attendus pour chaque photo connue (bibliothèque, photo ouverte,
//! dernière photo) ; tout fichier absent de cette liste est « orphelin » : photo déplacée, supprimée,
//! modifiée, ou retouche retirée. Les images de diagnostic ne servent à aucune photo : elles sont toutes
//! comptées comme orphelines.

use crate::ai::Model;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MODELS: [Model; 15] = [
    Model::Sky,
    Model::Subject,
    Model::Inpaint,
    Model::Depth,
    Model::People,
    Model::Buildings,
    Model::Vegetation,
    Model::Water,
    Model::Mountains,
    Model::Ground,
    Model::FaceSkin,
    Model::Eyes,
    Model::Lips,
    Model::Teeth,
    Model::FaceDetect,
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Masks,
    Patches,
    Denoise,
    Diagnostic,
}

/// Fichiers du cache, avec leur taille.
#[derive(Default)]
pub struct Plan {
    /// Tailles totales : masques, retouches, débruitages, images de diagnostic.
    pub total: [u64; 4],
    /// Fichiers à supprimer pour les orphelins seuls.
    pub orphans: Vec<(PathBuf, u64)>,
    /// Tous les fichiers du cache.
    pub all: Vec<(PathBuf, u64)>,
}

impl Plan {
    pub fn orphan_bytes(&self) -> u64 {
        self.orphans.iter().map(|f| f.1).sum()
    }
    pub fn all_bytes(&self) -> u64 {
        self.total.iter().sum()
    }
}

fn dir(kind: Kind) -> (PathBuf, &'static str) {
    let d = crate::library::data_dir();
    match kind {
        Kind::Masks => (d.join("masks"), "mask"),
        Kind::Patches => (d.join("retouches"), "patch"),
        Kind::Denoise => (d.join("denoise"), "png"),
        Kind::Diagnostic => (d.join("generation"), "png"),
    }
}

/// Noms de fichiers du cache encore utiles pour ces photos.
fn wanted(photos: &[PathBuf], denoise_model: &str) -> HashSet<PathBuf> {
    let mut keep = HashSet::new();
    for p in photos {
        for m in MODELS {
            keep.extend(crate::ai::cache_file(p, m));
        }
        let adj = crate::app::load_sidecar(p);
        let ops = adj.as_ref().map(|a| a.retouch.as_slice()).unwrap_or(&[]);
        for i in 0..ops.len() {
            keep.extend(crate::retouch::cache_file(p, &ops[..=i]));
        }
        keep.extend(crate::denoise::cache_file(p, denoise_model));
        if let Some(a) = &adj {
            if a.denoise.active() {
                keep.extend(crate::denoise::cache_file(p, &a.denoise.model));
            }
        }
    }
    keep
}

/// Analyse le cache sans rien supprimer.
pub fn plan(photos: &[PathBuf], denoise_model: &str) -> Plan {
    let keep = wanted(photos, denoise_model);
    let mut plan = Plan::default();
    for (i, kind) in [Kind::Masks, Kind::Patches, Kind::Denoise, Kind::Diagnostic].into_iter().enumerate() {
        let (d, ext) = dir(kind);
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some(ext) {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            plan.total[i] += meta.len();
            plan.all.push((path.clone(), meta.len()));
            if kind == Kind::Diagnostic || !keep.contains(&path) {
                plan.orphans.push((path, meta.len()));
            }
        }
    }
    plan
}

/// Supprime ces fichiers ; renvoie le nombre supprimé et les octets libérés.
pub fn delete(files: &[(PathBuf, u64)]) -> (usize, u64) {
    let mut n = 0;
    let mut bytes = 0;
    for (f, size) in files {
        if remove(f) {
            n += 1;
            bytes += size;
        }
    }
    (n, bytes)
}

fn remove(f: &Path) -> bool {
    std::fs::remove_file(f).is_ok()
}
