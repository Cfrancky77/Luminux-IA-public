//! Moteur de génération local, sans serveur : stable-diffusion.cpp (C++, Vulkan, sans Python),
//! lancé en ligne de commande pour chaque calcul. Le programme et ses modèles se téléchargent
//! depuis Luminux dans le dossier de données ; la carte graphique est prise par Vulkan (AMD, NVIDIA,
//! Intel), le processeur sert de secours.
//!
//! Un modèle local est désigné dans les réglages et les retouches par `local:` suivi du nom d'un
//! fichier du dossier des modèles, ou d'un chemin complet.

use crate::ai::Progress;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const PREFIX: &str = "local:";

/// Version de stable-diffusion.cpp utilisée (binaires officiels Linux Vulkan, Ubuntu 24.04).
const VERSION: &str = "master-987-a77d362";
const ENGINE_URL: &str = "https://github.com/leejet/stable-diffusion.cpp/releases/download/master-987-a77d362/sd-master-a77d362-bin-Linux-Ubuntu-24.04-x86_64-vulkan.zip";

/// Fichier à télécharger : nom, adresse, taille annoncée (Mo).
pub struct Asset {
    pub file: &'static str,
    pub url: &'static str,
    pub mb: u32,
}

/// Programme (archive zip, décompressée à l'installation).
pub const ENGINE: Asset = Asset { file: "stable-diffusion.cpp", url: ENGINE_URL, mb: 35 };
/// Modèle de génération conseillé : SD 1.5 inpainting (raccords propres, 4 Go de carte graphique suffisent).
pub const GEN_MODEL: Asset = Asset {
    file: "stable-diffusion-v1-5-inpainting-FP16.gguf",
    url: "https://huggingface.co/gpustack/stable-diffusion-v1-5-inpainting-GGUF/resolve/main/stable-diffusion-v1-5-inpainting-FP16.gguf",
    mb: 2130,
};
/// Modèle de génération fort : SDXL inpainting 0.1 (celui du serveur ComfyUI), lu tel quel par
/// stable-diffusion.cpp ; 8 Go de carte graphique avec le texte encodé par le processeur.
/// Adresse relevée par recherche web le 2026-10-21, téléchargement non essayé ici (Hugging Face bloqué).
pub const GEN_MODEL_XL: Asset = Asset {
    file: "sd_xl_base_1.0_inpainting_0.1.safetensors",
    url: "https://huggingface.co/wangqyqq/sd_xl_base_1.0_inpainting_0.1.safetensors/resolve/main/sd_xl_base_1.0_inpainting_0.1.safetensors",
    mb: 6940,
};
/// Modèle d'agrandissement conseillé : Real-ESRGAN x4 (photos).
pub const UPSCALE_MODEL: Asset = Asset { file: "RealESRGAN_x4plus.pth", url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.1.0/RealESRGAN_x4plus.pth", mb: 64 };

pub fn dir() -> PathBuf {
    crate::library::data_dir().join("sdcpp")
}

pub fn models_dir() -> PathBuf {
    dir().join("models")
}

fn engine_dir() -> PathBuf {
    dir().join(VERSION)
}

/// Programme `sd-cli` : `LUMINUX_SD` le remplace (essais, version compilée soi-même).
pub fn engine() -> PathBuf {
    std::env::var_os("LUMINUX_SD").map(PathBuf::from).unwrap_or_else(|| engine_dir().join("sd-cli"))
}

pub fn engine_ready() -> bool {
    engine().is_file()
}

pub fn is_local(model: &str) -> bool {
    model.starts_with(PREFIX)
}

/// Identifiant d'un modèle local : un fichier choisi (chemin complet) ou, vide, le modèle conseillé.
pub fn model_id(chosen: &str) -> String {
    format!("{PREFIX}{}", if chosen.is_empty() { GEN_MODEL.file } else { chosen })
}

/// Fichier d'un modèle local d'après son identifiant.
pub fn model_file(model: &str) -> PathBuf {
    let name = model.strip_prefix(PREFIX).unwrap_or(model);
    let p = Path::new(name);
    if p.is_absolute() { p.to_path_buf() } else { models_dir().join(name) }
}

/// Nom court d'un modèle local, pour l'affichage.
pub fn model_name(model: &str) -> String {
    let f = model_file(model);
    let n = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    n.trim_end_matches(".gguf").trim_end_matches(".safetensors").trim_end_matches(".ckpt").trim_end_matches(".pth").to_string()
}

pub fn asset_path(a: &Asset) -> PathBuf {
    if a.url == ENGINE_URL { engine() } else { models_dir().join(a.file) }
}

pub fn installed(a: &Asset) -> bool {
    asset_path(a).is_file()
}

/// Télécharge (et décompresse pour le programme) un élément du moteur local.
pub fn install(a: &Asset, progress: &Progress) -> Result<()> {
    if installed(a) {
        return Ok(());
    }
    std::fs::create_dir_all(models_dir())?;
    if a.url != ENGINE_URL {
        return crate::ai::download(a.url, &asset_path(a), progress);
    }
    let zip = dir().join("engine.zip");
    crate::ai::download(a.url, &zip, progress)?;
    let tmp = dir().join("engine.tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut z = zip::ZipArchive::new(std::fs::File::open(&zip)?).context("archive du moteur illisible")?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        // Fichiers à plat seulement (l'archive n'a pas de sous-dossier) ; un nom suspect est ignoré.
        let Some(name) = f.enclosed_name().and_then(|p| p.file_name().map(|n| n.to_owned())) else { continue };
        if f.is_dir() {
            continue;
        }
        let out = tmp.join(&name);
        std::io::copy(&mut f, &mut std::fs::File::create(&out)?)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))?;
    }
    if !tmp.join("sd-cli").is_file() {
        bail!("archive du moteur sans sd-cli");
    }
    let _ = std::fs::remove_dir_all(engine_dir());
    std::fs::rename(&tmp, engine_dir())?;
    let _ = std::fs::remove_file(&zip);
    // Versions précédentes du moteur : plus utilisées.
    for e in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("master-") && n != VERSION && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    Ok(())
}

/// Dossier de travail d'un calcul (supprimé à la fin).
struct Work(PathBuf);

impl Work {
    fn new(tag: &str) -> Result<Self> {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!("luminux-sd-{}-{tag}-{stamp:x}", std::process::id()));
        std::fs::create_dir_all(&p)?;
        Ok(Self(p))
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Lance `sd-cli` ; en cas d'échec, l'erreur reprend ses dernières lignes.
fn run(args: &[std::ffi::OsString]) -> Result<()> {
    let exe = engine();
    if !exe.is_file() {
        bail!("moteur local non installé (Préférences, IA)");
    }
    // L'AppImage règle LD_LIBRARY_PATH pour ses propres bibliothèques : le moteur a les siennes à côté de lui.
    let out = Command::new(&exe).args(args).env_remove("LD_LIBRARY_PATH").output().with_context(|| format!("lancement de {}", exe.display()))?;
    if out.status.success() {
        return Ok(());
    }
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let errors: Vec<&str> = text.lines().filter(|l| l.contains("ERROR")).collect();
    let tail: Vec<&str> = if errors.is_empty() { text.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect() } else { errors };
    bail!("moteur local : {}", tail.join(" ; ").trim())
}

/// Réglages d'échantillonnage passés au moteur.
pub struct Sampling<'a> {
    pub prompt: &'a str,
    pub negative: &'a str,
    pub seed: u64,
    pub denoise: f32,
    pub steps: u32,
    pub cfg: f32,
    /// Noms ComfyUI (dpmpp_2m, euler_ancestral, karras…), traduits pour stable-diffusion.cpp.
    pub sampler: &'a str,
    pub scheduler: &'a str,
}

fn sampler_name(comfy: &str) -> &str {
    match comfy {
        "dpmpp_2m" => "dpm++2m",
        "euler_ancestral" => "euler_a",
        "dpmpp_2m_sde" => "dpm++2m_sde",
        other => other,
    }
}

/// Arguments d'une génération dans la zone masquée (masque : blanc = à générer).
fn inpaint_args(model: &Path, init: &Path, mask: &Path, out: &Path, w: u32, h: u32, s: &Sampling) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = Vec::new();
    let mut push = |k: &str, v: std::ffi::OsString| {
        a.push(k.into());
        a.push(v);
    };
    push("-m", model.into());
    push("-p", s.prompt.into());
    if !s.negative.is_empty() {
        push("-n", s.negative.into());
    }
    push("-i", init.into());
    push("--mask", mask.into());
    push("-W", w.to_string().into());
    push("-H", h.to_string().into());
    push("--strength", format!("{:.3}", s.denoise.clamp(0.05, 1.0)).into());
    push("--steps", s.steps.to_string().into());
    push("--cfg-scale", format!("{:.2}", s.cfg).into());
    push("--sampling-method", sampler_name(s.sampler).into());
    push("--scheduler", s.scheduler.into());
    // Graine positive : une graine négative serait tirée au hasard.
    push("-s", (s.seed & 0x7fff_ffff_ffff).to_string().into());
    push("-o", out.into());
    if is_xl(model) {
        // SDXL : encodeurs de texte sur le processeur et décodage par tuiles pour tenir dans 8 Go.
        a.push("--clip-on-cpu".into());
        a.push("--vae-tiling".into());
    }
    a
}

/// Modèle de la famille SDXL (d'après son nom).
pub fn is_xl(model: &Path) -> bool {
    model.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("xl"))
}

/// Génère dans `img` (taille multiple de 8) là où l'alpha est nul ; renvoie l'image produite.
pub fn inpaint(model: &str, img: &image::RgbaImage, s: &Sampling) -> Result<image::RgbImage> {
    let file = model_file(model);
    if !file.is_file() {
        bail!("modèle local introuvable : {}", file.display());
    }
    let work = Work::new("gen")?;
    let (w, h) = img.dimensions();
    let init = work.0.join("init.png");
    let mask = work.0.join("mask.png");
    let out = work.0.join("out.png");
    image::DynamicImage::ImageRgba8(img.clone()).to_rgb8().save(&init)?;
    image::GrayImage::from_fn(w, h, |x, y| image::Luma([if img.get_pixel(x, y)[3] < 128 { 255 } else { 0 }])).save(&mask)?;
    run(&inpaint_args(&file, &init, &mask, &out, w, h, s))?;
    Ok(image::open(&out).map_err(|e| anyhow!("résultat du moteur local illisible : {e}"))?.to_rgb8())
}

/// Agrandit une image avec un modèle ESRGAN (facteur natif du modèle).
pub fn upscale(model: &str, img: &image::RgbImage) -> Result<image::RgbImage> {
    let file = model_file(model);
    if !file.is_file() {
        bail!("modèle d'agrandissement local introuvable : {}", file.display());
    }
    let work = Work::new("up")?;
    let (input, out) = (work.0.join("in.png"), work.0.join("out.png"));
    img.save(&input)?;
    let args: Vec<std::ffi::OsString> = vec!["-M".into(), "upscale".into(), "--upscale-model".into(), file.into(), "-i".into(), input.into(), "-o".into(), out.clone().into()];
    run(&args)?;
    Ok(image::open(&out).map_err(|e| anyhow!("résultat du moteur local illisible : {e}"))?.to_rgb8())
}

/// Modèles d'agrandissement présents dans le dossier des modèles (fichiers .pth), identifiants `local:…`.
pub fn upscalers() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(models_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".pth") || (n.ends_with(".safetensors") && n.to_lowercase().contains("esrgan")))
        .map(|n| format!("{PREFIX}{n}"))
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_files() {
        assert_eq!(model_id(""), format!("{PREFIX}{}", GEN_MODEL.file));
        assert!(model_file(&model_id("/opt/m/x.safetensors")).is_absolute());
        assert_eq!(model_file("local:a.gguf"), models_dir().join("a.gguf"));
        assert_eq!(model_name("local:/opt/m/dreamshaper-8-inpainting.safetensors"), "dreamshaper-8-inpainting");
        assert!(is_local("local:x") && !is_local("x.safetensors"));
    }

    #[test]
    fn inpaint_arguments() {
        let s = Sampling { prompt: "a boat", negative: "", seed: u64::MAX, denoise: 1.0, steps: 28, cfg: 6.5, sampler: "dpmpp_2m", scheduler: "karras" };
        let a: Vec<String> = inpaint_args(Path::new("m.gguf"), Path::new("i.png"), Path::new("k.png"), Path::new("o.png"), 640, 448, &s).iter().map(|x| x.to_string_lossy().into_owned()).collect();
        let val = |k: &str| a[a.iter().position(|x| x == k).unwrap() + 1].clone();
        assert_eq!(val("--sampling-method"), "dpm++2m");
        assert_eq!((val("-W"), val("-H")), ("640".into(), "448".into()));
        assert!(!val("-s").starts_with('-'));
        assert!(!a.contains(&"-n".to_string()));
        assert!(!a.contains(&"--clip-on-cpu".to_string()));
        assert!(is_xl(&model_file(&model_id(GEN_MODEL_XL.file))));
    }

    /// Essai avec le vrai moteur : LUMINUX_SD=…/sd-cli LUMINUX_ESSAI_UP=modele.pth cargo test essai_local -- --ignored
    #[test]
    #[ignore]
    fn essai_local_upscale() {
        let m = std::env::var("LUMINUX_ESSAI_UP").unwrap();
        let img = image::RgbImage::from_fn(48, 32, |x, y| image::Rgb([(x * 5) as u8, (y * 7) as u8, 90]));
        let out = upscale(&format!("{PREFIX}{m}"), &img).unwrap();
        assert_eq!(out.dimensions(), (192, 128));
    }
}
