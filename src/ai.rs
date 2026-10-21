//! Masques IA (ciel, sujet) : modèles ONNX exécutés sur le CPU par ONNX Runtime,
//! chargé dynamiquement (libonnxruntime.so livrée dans l'AppImage).
//! Les modèles sont téléchargés au premier usage dans ~/.local/share/luminux/models,
//! les masques calculés sont gardés dans ~/.local/share/luminux/masks.

use crate::adjust::{Adjustments, AiKind, Shape};
use crate::gpu::Developer;
use anyhow::{Context, Result, anyhow, bail};
use eframe::wgpu;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Modèle de segmentation (l'arrière-plan est l'inverse du sujet).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Model {
    Sky,
    Subject,
    /// Remplissage (gomme IA) : LaMa.
    Inpaint,
    /// Carte de profondeur (proximité 0 = loin .. 1 = proche) : Depth Anything V2 Small.
    Depth,
    /// Masques sémantiques : un même réseau de segmentation de scène (SegFormer ADE20K),
    /// chaque masque regroupant plusieurs de ses 150 classes.
    People,
    Buildings,
    Vegetation,
    Water,
    Mountains,
    Ground,
    /// Masques du visage : détection (YuNet) puis découpage de chaque visage (BiSeNet, CelebAMask-HQ).
    /// Les quatre masques sont calculés ensemble.
    FaceSkin,
    Eyes,
    Lips,
    Teeth,
    /// Détecteur de visages, téléchargé avec le découpage (pas de masque à lui seul).
    FaceDetect,
}

/// Masques produits ensemble par le découpage du visage.
pub const FACE_MODELS: [Model; 4] = [Model::FaceSkin, Model::Eyes, Model::Lips, Model::Teeth];

impl Model {
    pub fn of(kind: AiKind) -> Model {
        match kind {
            AiKind::Sky => Model::Sky,
            AiKind::Subject | AiKind::Background => Model::Subject,
            AiKind::People => Model::People,
            AiKind::Buildings => Model::Buildings,
            AiKind::Vegetation => Model::Vegetation,
            AiKind::Water => Model::Water,
            AiKind::Mountains => Model::Mountains,
            AiKind::Ground => Model::Ground,
        }
    }

    /// Classes ADE20K (indices 0..149, ordre de objectInfo150) réunies dans un masque sémantique.
    pub fn scene_classes(self) -> Option<&'static [usize]> {
        Some(match self {
            // person
            Model::People => &[12],
            // building, house, skyscraper, hovel, tower
            Model::Buildings => &[1, 25, 48, 79, 84],
            // tree, grass, plant, field, flower, palm
            Model::Vegetation => &[4, 9, 17, 29, 66, 72],
            // water, sea, river, fountain, swimming pool, waterfall, lake
            Model::Water => &[21, 26, 60, 104, 109, 113, 128],
            // mountain, rock, hill
            Model::Mountains => &[16, 34, 68],
            // road, sidewalk, earth, sand, path, dirt track, land
            Model::Ground => &[6, 11, 13, 46, 52, 91, 94],
            _ => return None,
        })
    }

    /// Classes du découpage du visage (CelebAMask-HQ, 19 classes) réunies dans un masque du visage.
    pub fn face_classes(self) -> Option<&'static [usize]> {
        Some(match self {
            // skin, l_ear, r_ear, nose, neck
            Model::FaceSkin => &[1, 7, 8, 10, 14],
            // l_eye, r_eye
            Model::Eyes => &[4, 5],
            // u_lip, l_lip
            Model::Lips => &[12, 13],
            // mouth (intérieur de la bouche : dents)
            Model::Teeth => &[11],
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Model::Sky => k!("ciel"),
            Model::Subject => k!("sujet"),
            Model::Inpaint => k!("gomme"),
            Model::Depth => k!("profondeur"),
            Model::People => k!("personnes"),
            Model::Buildings => k!("bâtiments"),
            Model::Vegetation => k!("végétation"),
            Model::Water => k!("eau"),
            Model::Mountains => k!("montagnes"),
            Model::Ground => k!("sol"),
            Model::FaceSkin => k!("peau"),
            Model::Eyes => k!("yeux"),
            Model::Lips => k!("lèvres"),
            Model::Teeth => k!("dents"),
            Model::FaceDetect => k!("visages"),
        }
    }

    /// « du ciel », « de la profondeur »… pour les messages.
    pub fn of_what(self) -> &'static str {
        match self {
            Model::Sky => t!("du ciel"),
            Model::Subject => t!("du sujet"),
            Model::Inpaint => t!("de la gomme"),
            Model::Depth => t!("de la profondeur"),
            Model::People => t!("des personnes"),
            Model::Buildings => t!("des bâtiments"),
            Model::Vegetation => t!("de la végétation"),
            Model::Water => t!("de l'eau"),
            Model::Mountains => t!("des montagnes"),
            Model::Ground => t!("du sol"),
            Model::FaceSkin => t!("de la peau"),
            Model::Eyes => t!("des yeux"),
            Model::Lips => t!("des lèvres"),
            Model::Teeth => t!("des dents"),
            Model::FaceDetect => t!("des visages"),
        }
    }
}

struct Spec {
    file: &'static str,
    url: &'static str,
    /// Côté de l'entrée carrée du réseau.
    size: u32,
    mean: [f32; 3],
    std: [f32; 3],
    /// Normalisation façon rembg : division par le maximum de l'image (sinon par 1)
    /// et sortie étirée min-max (sinon simplement bornée à 0..1).
    rembg_norm: bool,
}

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

fn spec(m: Model) -> Spec {
    match m {
        // IS-Net (DIS), même prétraitement que rembg (dis_general_use.py).
        Model::Subject => Spec {
            file: "isnet-general-use.onnx",
            url: "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-general-use.onnx",
            size: 1024,
            mean: [0.5; 3],
            std: [1.0; 3],
            rembg_norm: true,
        },
        // U²-Net entraîné à la segmentation du ciel ; prétraitement repris de skyseg (Parskatt/skyseg,
        // _run_skyseg) : RGB / 255, normalisation ImageNet, sortie déjà dans 0..1.
        // LaMa exporté en ONNX (Carve/LaMa-ONNX, Apache-2.0) : entrée fixe 512x512,
        // image RGB / 255 et masque (1 = à effacer), sortie RGB 0..255.
        Model::Inpaint => Spec {
            file: "lama_fp32.onnx",
            url: "https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama_fp32.onnx",
            size: 512,
            mean: [0.0; 3],
            std: [1.0; 3],
            rembg_norm: false,
        },
        // Depth Anything V2 Small (Apache-2.0), export ONNX de fabio-sim/Depth-Anything-ONNX :
        // entrée 518x518 RGB / 255 normalisée ImageNet, sortie [1, 518, 518] de profondeur inverse relative.
        Model::Depth => Spec {
            file: "depth_anything_v2_vits.onnx",
            url: "https://github.com/fabio-sim/Depth-Anything-ONNX/releases/download/v2.0.0/depth_anything_v2_vits.onnx",
            size: 518,
            mean: IMAGENET_MEAN,
            std: IMAGENET_STD,
            rembg_norm: false,
        },
        // SegFormer-B0 entraîné sur ADE20K (150 classes), export ONNX de Xenova (transformers.js) :
        // entrée pixel_values [1, 3, 512, 512] RGB / 255 normalisée ImageNet, sortie logits [1, 150, 128, 128].
        Model::People | Model::Buildings | Model::Vegetation | Model::Water | Model::Mountains | Model::Ground => Spec {
            file: "segformer-b0-ade20k.onnx",
            url: "https://huggingface.co/Xenova/segformer-b0-finetuned-ade-512-512/resolve/main/onnx/model.onnx",
            size: 512,
            mean: IMAGENET_MEAN,
            std: IMAGENET_STD,
            rembg_norm: false,
        },
        // Découpage du visage BiSeNet / ResNet-18 entraîné sur CelebAMask-HQ (yakhyo/face-parsing, MIT) :
        // entrée [1, 3, 512, 512] RGB / 255 normalisée ImageNet, sortie [1, 19, 512, 512].
        Model::FaceSkin | Model::Eyes | Model::Lips | Model::Teeth => Spec {
            file: "face-parsing-resnet18.onnx",
            url: "https://github.com/yakhyo/face-parsing/releases/download/weights/resnet18.onnx",
            size: 512,
            mean: IMAGENET_MEAN,
            std: IMAGENET_STD,
            rembg_norm: false,
        },
        // YuNet (OpenCV Zoo, MIT) : entrée [1, 3, 640, 640] BGR 0..255, sorties cls/obj/bbox/kps par pas 8, 16, 32.
        Model::FaceDetect => Spec {
            file: "face_detection_yunet_2023mar.onnx",
            url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
            size: 640,
            mean: [0.0; 3],
            std: [1.0; 3],
            rembg_norm: false,
        },
        Model::Sky => Spec {
            file: "skyseg.onnx",
            url: "https://huggingface.co/JianyuanWang/skyseg/resolve/main/skyseg.onnx",
            size: 320,
            mean: IMAGENET_MEAN,
            std: IMAGENET_STD,
            rembg_norm: false,
        },
    }
}

pub fn models_dir() -> PathBuf {
    crate::library::data_dir().join("models")
}

pub fn model_path(m: Model) -> PathBuf {
    models_dir().join(spec(m).file)
}

/// Progression d'un téléchargement (octets reçus, taille totale si connue).
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

/// Télécharge le modèle s'il manque (fichier `.part` renommé à la fin).
pub fn ensure_model(m: Model, progress: &Progress) -> Result<PathBuf> {
    if m.face_classes().is_some() {
        ensure_model(Model::FaceDetect, progress)?;
    }
    let path = model_path(m);
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(models_dir())?;
    download(spec(m).url, &path, progress)?;
    Ok(path)
}

/// Télécharge `url` dans `path` (fichier `.part` renommé à la fin), en suivant `progress`.
pub fn download(url: &str, path: &Path, progress: &Progress) -> Result<()> {
    let mut resp = ureq::get(url).call().with_context(|| format!("téléchargement de {url}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    progress.total.store(total, Ordering::Relaxed);
    let part = path.with_extension("part");
    let mut out = std::fs::File::create(&part)?;
    let mut reader = resp.body_mut().with_config().limit(64 << 30).reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress.done.store(done, Ordering::Relaxed);
    }
    out.sync_all()?;
    if total > 0 && done != total {
        bail!("téléchargement incomplet de {url} ({done} / {total} octets)");
    }
    std::fs::rename(&part, path)?;
    Ok(())
}

// ---------- ONNX Runtime ----------

fn ort_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("LUMINUX_ORT") {
        v.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        v.push(dir.join("../lib/libonnxruntime.so.1"));
        v.push(dir.join("../lib/libonnxruntime.so"));
        v.push(dir.join("libonnxruntime.so"));
    }
    v.push(crate::library::data_dir().join("lib/libonnxruntime.so"));
    for d in ["/usr/lib", "/usr/local/lib", "/usr/lib/x86_64-linux-gnu"] {
        v.push(Path::new(d).join("libonnxruntime.so"));
    }
    v
}

static ORT: OnceLock<std::result::Result<ort::environment::Environment, String>> = OnceLock::new();

fn ensure_ort() -> Result<()> {
    let r = ORT.get_or_init(|| {
        let lib = ort_candidates().into_iter().find(|p| p.is_file()).ok_or_else(|| "libonnxruntime.so introuvable (fournie dans l'AppImage)".to_string())?;
        ort::init_from(&lib).map_err(|e| format!("{}: {e}", lib.display()))?.with_name("luminux").build().map_err(|e| e.to_string())
    });
    r.as_ref().map(|_| ()).map_err(|e| anyhow!("ONNX Runtime : {e}"))
}

/// Sessions par fichier de modèle (les masques sémantiques partagent le même réseau), et si elles
/// tournent sur la carte graphique.
static SESSIONS: OnceLock<Mutex<HashMap<&'static str, (ort::session::Session, bool)>>> = OnceLock::new();

/// Carte graphique refusée une fois (pilote, mémoire, opération non gérée) : processeur pour la session.
static GPU_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Où tournent les modèles : WebGPU passe par Vulkan, donc AMD, NVIDIA ou Intel ; le processeur sert de
/// secours. `LUMINUX_AI_CPU=1` ou la préférence force le processeur.
fn gpu_wanted() -> bool {
    !GPU_FAILED.load(std::sync::atomic::Ordering::Relaxed) && std::env::var_os("LUMINUX_AI_CPU").is_none() && crate::settings::Settings::load().ai_gpu
}

/// Le dernier modèle chargé tourne-t-il sur la carte graphique ? (None : aucun chargé)
pub fn on_gpu() -> Option<bool> {
    match LAST_DEVICE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// Où a tourné le dernier calcul (0 : aucun, 1 : processeur, 2 : carte graphique), lu par l'interface
/// sans attendre le verrou des sessions.
static LAST_DEVICE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Oublie les sessions (changement de préférence) : elles seront recréées au prochain calcul.
pub fn reset_sessions() {
    GPU_FAILED.store(false, std::sync::atomic::Ordering::Relaxed);
    LAST_DEVICE.store(0, std::sync::atomic::Ordering::Relaxed);
    // Sans attendre un calcul en cours : les sessions sont vidées au prochain calcul.
    RESET.store(true, std::sync::atomic::Ordering::Relaxed);
}

static RESET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Nœuds gardés sur le processeur avec WebGPU : l'AveragePool à grand noyau (moyenne globale) de BiSeNet
/// sort faux sur WebGPU (ONNX Runtime 1.27).
fn cpu_nodes(file: &str) -> &'static str {
    match file {
        "face-parsing-resnet18.onnx" => "/fpn/AveragePool\n/fpn/arm32/AveragePool\n/fpn/arm16/AveragePool\n/ffm/AveragePool",
        _ => "",
    }
}

/// Modèles déjà comparés carte graphique / processeur : fichier → résultat identique.
fn gpu_checks_file() -> PathBuf {
    crate::library::data_dir().join("gpu_check.json")
}

fn gpu_check(file: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(gpu_checks_file()).ok()?).ok()?;
    v.get(file)?.as_bool()
}

fn set_gpu_check(file: &str, ok: bool) {
    let mut v: serde_json::Value = std::fs::read(gpu_checks_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| serde_json::json!({}));
    v[file] = ok.into();
    let _ = std::fs::write(gpu_checks_file(), serde_json::to_vec_pretty(&v).unwrap_or_default());
}

/// Même résultat, à la précision près ? (écart rapporté à l'amplitude de chaque sortie)
fn outputs_match(a: &[(String, Vec<i64>, Vec<f32>)], b: &[(String, Vec<i64>, Vec<f32>)]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            let amp = x.2.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
            x.1 == y.1 && x.2.iter().zip(&y.2).all(|(p, q)| (p - q).abs() <= 2e-2 * amp)
        })
}

fn new_session(m: Model, gpu: bool) -> Result<ort::session::Session> {
    let sp = spec(m);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let env = ORT.get().and_then(|r| r.as_ref().ok()).context("ONNX Runtime absent")?;
    let mut b = ort::session::Session::builder(env).map_err(|e| anyhow!("{e}"))?.with_intra_threads(threads).map_err(|e| anyhow!("{e}"))?;
    if gpu {
        let ep = ort::ep::WebGPU::default().with_dawn_backend_type(ort::ep::webgpu::DawnBackendType::Vulkan).build().error_on_failure();
        let nodes = cpu_nodes(sp.file);
        if !nodes.is_empty() {
            b = b.with_config_entry("ep.webgpuexecutionprovider.forceCpuNodeNames", nodes).map_err(|e| anyhow!("{e}"))?;
        }
        b = b.with_execution_providers([ep]).map_err(|e| anyhow!("{e}"))?;
    }
    b.commit_from_file(model_path(m)).map_err(|e| anyhow!("chargement du modèle {} : {e}", sp.file))
}

/// Exécute le réseau sur une image sRGB `w`x`h` (0..1) ; renvoie la carte du réseau en 0..1.
fn infer(m: Model, rgb: &[[f32; 4]], w: u32, h: u32) -> Result<(u32, Vec<f32>)> {
    ensure_ort()?;
    let sp = spec(m);
    let s = sp.size;
    let small = resize(rgb, w, h, s, s);
    let mx = if sp.rembg_norm { small.iter().flat_map(|p| p[..3].iter().copied()).fold(1e-6f32, f32::max) } else { 1.0 };
    let plane = (s * s) as usize;
    let mut input = vec![0f32; 3 * plane];
    for (i, p) in small.iter().enumerate() {
        for c in 0..3 {
            input[c * plane + i] = (p[c] / mx - sp.mean[c]) / sp.std[c];
        }
    }
    let tensor = ort::value::Tensor::from_array(([1usize, 3, s as usize, s as usize], input)).map_err(|e| anyhow!("{e}"))?;
    let (shape, data) = run(m, vec![tensor.into()])?;
    if let Some(classes) = m.scene_classes() {
        return scene_mask(&shape, &data, classes);
    }
    // Sortie [1, 1, s, s] : premier canal.
    let n = shape.iter().rev().take(2).product::<i64>() as usize;
    let side = *shape.last().unwrap_or(&0) as u32;
    if n == 0 || data.len() < n || side * side != n as u32 {
        bail!("sortie inattendue du modèle : {shape:?}");
    }
    let map = &data[..n];
    if !sp.rembg_norm && m != Model::Depth {
        return Ok((side, map.iter().map(|v| v.clamp(0.0, 1.0)).collect()));
    }
    let (lo, hi) = map.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    Ok((side, map.iter().map(|v| (v - lo) / (hi - lo).max(1e-6)).collect()))
}

/// Probabilité (softmax sur les classes) que chaque pixel appartienne à l'une des `classes`.
fn scene_mask(shape: &[i64], logits: &[f32], classes: &[usize]) -> Result<(u32, Vec<f32>)> {
    let [_, k, h, w] = shape else { bail!("sortie inattendue du modèle : {shape:?}") };
    let (k, h, w) = (*k as usize, *h as usize, *w as usize);
    if k < 150 || h != w || logits.len() < k * h * w {
        bail!("sortie inattendue du modèle : {shape:?}");
    }
    let plane = h * w;
    let out = (0..plane)
        .map(|i| {
            let mx = (0..k).map(|c| logits[c * plane + i]).fold(f32::MIN, f32::max);
            let sum: f32 = (0..k).map(|c| (logits[c * plane + i] - mx).exp()).sum();
            classes.iter().map(|&c| (logits[c * plane + i] - mx).exp()).sum::<f32>() / sum
        })
        .collect();
    Ok((w as u32, out))
}

/// Exécute un modèle (session gardée en mémoire) ; renvoie la forme et les valeurs de la première sortie.
fn run(m: Model, inputs: Vec<ort::session::SessionInputValue<'static>>) -> Result<(Vec<i64>, Vec<f32>)> {
    let (_, shape, data) = run_outputs(m, inputs, true)?.into_iter().next().context("le modèle n'a pas de sortie")?;
    Ok((shape, data))
}

/// Sorties d'un modèle (nom, forme, valeurs) ; la première seulement si `first_only`.
fn run_outputs(m: Model, inputs: Vec<ort::session::SessionInputValue<'static>>, first_only: bool) -> Result<Vec<(String, Vec<i64>, Vec<f32>)>> {
    ensure_ort()?;
    let sp = spec(m);
    let mut sessions = SESSIONS.get_or_init(Default::default).lock().map_err(|_| anyhow!("verrou des modèles"))?;
    if RESET.swap(false, std::sync::atomic::Ordering::Relaxed) {
        sessions.clear();
    }
    if !sessions.contains_key(sp.file) {
        // LaMa (gomme) reste sur le processeur : ses convolutions de Fourier ne sont pas sûres en WebGPU,
        // et un calcul ponctuel de quelques secondes n'y gagne rien.
        let gpu = gpu_wanted() && gpu_check(sp.file) != Some(false) && m != Model::Inpaint;
        let session = match new_session(m, gpu) {
            Ok(s) => (s, gpu),
            Err(e) if gpu => {
                eprintln!("carte graphique indisponible pour {}, processeur : {e:#}", sp.file);
                GPU_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                (new_session(m, false)?, false)
            }
            Err(e) => return Err(e),
        };
        sessions.insert(sp.file, session);
    }
    let mut inputs = inputs;
    // Image puis masque : on suit l'ordre déclaré par le modèle s'il nomme le masque en premier.
    if inputs.len() == 2 && sessions[sp.file].0.inputs().first().is_some_and(|i| i.name().to_lowercase().contains("mask")) {
        inputs.swap(0, 1);
    }
    let on_gpu = sessions[sp.file].1;
    let first = sessions.get_mut(sp.file).unwrap().0.run(inputs.as_slice()).map(|o| collect_outputs(&o, first_only));
    let result = match first {
        // Premier passage d'un modèle sur la carte graphique : comparé une fois au processeur. S'ils
        // diffèrent (opération mal gérée par WebGPU), ce modèle reste sur le processeur.
        Ok(Ok(r)) if on_gpu && gpu_check(sp.file).is_none() => {
            let mut cpu_session = new_session(m, false)?;
            let cpu = collect_outputs(&cpu_session.run(inputs.as_slice()).map_err(|e| anyhow!("inférence : {e}"))?, first_only)?;
            let ok = outputs_match(&r, &cpu);
            set_gpu_check(sp.file, ok);
            if !ok {
                eprintln!("{} : résultat différent sur la carte graphique, processeur gardé", sp.file);
                sessions.insert(sp.file, (cpu_session, false));
            }
            Ok(cpu)
        }
        Ok(r) => r,
        Err(e) if on_gpu => {
            // Échec sur la carte graphique : même calcul sur le processeur, qui reste choisi ensuite.
            eprintln!("inférence sur la carte graphique impossible ({}), processeur : {e}", sp.file);
            GPU_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
            sessions.insert(sp.file, (new_session(m, false)?, false));
            let o = sessions.get_mut(sp.file).unwrap().0.run(inputs.as_slice()).map_err(|e| anyhow!("inférence : {e}"))?;
            collect_outputs(&o, first_only)
        }
        Err(e) => return Err(anyhow!("inférence : {e}")),
    };
    LAST_DEVICE.store(if sessions[sp.file].1 { 2 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
    result
}

fn collect_outputs(outputs: &ort::session::SessionOutputs, first_only: bool) -> Result<Vec<(String, Vec<i64>, Vec<f32>)>> {
    let mut all = Vec::new();
    for (name, v) in outputs.iter() {
        let (shape, data) = v.try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
        all.push((name.to_string(), shape.iter().copied().collect(), data.to_vec()));
        if first_only {
            break;
        }
    }
    Ok(all)
}

/// Gomme IA : `rgb` (sRGB 0..1, 512x512, plans R, G, B) et `mask` (512x512, 1 = à effacer).
/// Renvoie l'image remplie, sRGB 0..1, mêmes plans.
pub fn inpaint_512(rgb: Vec<f32>, mask: Vec<f32>) -> Result<Vec<f32>> {
    // Avant tout tenseur : sinon ort cherche sa propre bibliothèque et panique.
    ensure_ort()?;
    let img = ort::value::Tensor::from_array(([1usize, 3, 512, 512], rgb)).map_err(|e| anyhow!("{e}"))?;
    let msk = ort::value::Tensor::from_array(([1usize, 1, 512, 512], mask)).map_err(|e| anyhow!("{e}"))?;
    let (shape, data) = run(Model::Inpaint, vec![img.into(), msk.into()])?;
    if data.len() != 3 * 512 * 512 {
        bail!("sortie inattendue de LaMa : {shape:?}");
    }
    Ok(data.iter().map(|v| (v / 255.0).clamp(0.0, 1.0)).collect())
}

/// Réduction / agrandissement bilinéaire avec moyenne de zone quand on réduit.
fn resize(src: &[[f32; 4]], w: u32, h: u32, ow: u32, oh: u32) -> Vec<[f32; 4]> {
    let mut out = vec![[0f32; 4]; (ow * oh) as usize];
    let (sx, sy) = (w as f32 / ow as f32, h as f32 / oh as f32);
    for y in 0..oh {
        let (y0, y1) = ((y as f32 * sy) as u32, (((y + 1) as f32 * sy).ceil() as u32).clamp(1, h));
        for x in 0..ow {
            let (x0, x1) = ((x as f32 * sx) as u32, (((x + 1) as f32 * sx).ceil() as u32).clamp(1, w));
            let mut acc = [0f32; 4];
            let mut n = 0f32;
            for yy in y0.min(h - 1)..y1.max(y0 + 1).min(h) {
                for xx in x0.min(w - 1)..x1.max(x0 + 1).min(w) {
                    let p = src[(yy * w + xx) as usize];
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                    n += 1.0;
                }
            }
            out[(y * ow + x) as usize] = acc.map(|v| v / n.max(1.0));
        }
    }
    out
}

/// Échantillonnage bilinéaire d'une carte `w`x`h` vers `ow`x`oh`.
pub fn resample(map: &[f32], w: u32, h: u32, ow: u32, oh: u32) -> Vec<f32> {
    let mut out = vec![0f32; (ow * oh) as usize];
    for y in 0..oh {
        let fy = ((y as f32 + 0.5) * h as f32 / oh as f32 - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, ty) = (fy.floor() as u32, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..ow {
            let fx = ((x as f32 + 0.5) * w as f32 / ow as f32 - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, tx) = (fx.floor() as u32, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            let g = |xx: u32, yy: u32| map[(yy * w + xx) as usize];
            let a = g(x0, y0) + (g(x1, y0) - g(x0, y0)) * tx;
            let b = g(x0, y1) + (g(x1, y1) - g(x0, y1)) * tx;
            out[(y * ow + x) as usize] = a + (b - a) * ty;
        }
    }
    out
}

/// Moyenne sur une fenêtre (2r+1)² par image intégrale.
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut ii = vec![0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            row += src[y * w + x] as f64;
            ii[(y + 1) * (w + 1) + x + 1] = ii[y * (w + 1) + x + 1] + row;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
            let s = ii[y1 * (w + 1) + x1] - ii[y0 * (w + 1) + x1] - ii[y1 * (w + 1) + x0] + ii[y0 * (w + 1) + x0];
            out[y * w + x] = (s / ((y1 - y0) * (x1 - x0)) as f64) as f32;
        }
    }
    out
}

/// Filtre guidé (He et al.) : le masque épouse les contours de l'image guide.
pub fn guided_filter(guide: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let mean_i = box_mean(guide, w, h, r);
    let mean_p = box_mean(p, w, h, r);
    let ip: Vec<f32> = guide.iter().zip(p).map(|(a, b)| a * b).collect();
    let ii: Vec<f32> = guide.iter().map(|a| a * a).collect();
    let corr_ip = box_mean(&ip, w, h, r);
    let corr_ii = box_mean(&ii, w, h, r);
    let mut a = vec![0f32; w * h];
    let mut b = vec![0f32; w * h];
    for k in 0..w * h {
        let var = corr_ii[k] - mean_i[k] * mean_i[k];
        let cov = corr_ip[k] - mean_i[k] * mean_p[k];
        a[k] = cov / (var + eps);
        b[k] = mean_p[k] - a[k] * mean_i[k];
    }
    let ma = box_mean(&a, w, h, r);
    let mb = box_mean(&b, w, h, r);
    (0..w * h).map(|k| (ma[k] * guide[k] + mb[k]).clamp(0.0, 1.0)).collect()
}

/// Masque calculé, à la résolution des masques du Developer.
pub struct AiMask {
    pub w: u32,
    pub h: u32,
    pub data: Vec<f32>,
    /// Luminance linéaire moyenne de la photo sous le masque (ajustement du ciel de remplacement).
    pub mean_luma: f32,
}

impl AiMask {
    /// Données à la taille `mw`x`mh` (les vignettes ont une autre résolution).
    pub fn at(&self, mw: u32, mh: u32) -> Vec<f32> {
        if (mw, mh) == (self.w, self.h) { self.data.clone() } else { resample(&self.data, self.w, self.h, mw, mh) }
    }
}

/// Calcule le masque à partir de l'image guide (sRGB, `w`x`h`, sans réglages).
pub fn compute(m: Model, guide: &[[f32; 4]], w: u32, h: u32) -> Result<AiMask> {
    let (side, raw) = infer(m, guide, w, h)?;
    let coarse = resample(&raw, side, side, w, h);
    let luma: Vec<f32> = guide.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    // Rayon proportionnel à la taille (≈ 8 px pour 2048 px), le réseau ayant vu une image réduite.
    let r = ((w.max(h) as f32 / 256.0).round() as usize).max(2);
    let mut data = guided_filter(&luma, &coarse, w as usize, h as usize, r, 1e-3);
    // Contraste du masque : les zones franches restent pleines, la transition reste douce.
    // La profondeur garde sa gradation.
    for v in &mut data {
        *v = if m == Model::Depth { v.clamp(0.0, 1.0) } else { ((*v - 0.1) / 0.8).clamp(0.0, 1.0) };
    }
    let (mut num, mut den) = (0f64, 0f64);
    for (v, l) in data.iter().zip(&luma) {
        let lin = crate::image_io::srgb_to_linear(*l);
        num += (*v * lin) as f64;
        den += *v as f64;
    }
    let mean_luma = if den > 1.0 { (num / den) as f32 } else { 0.0 };
    Ok(AiMask { w, h, data, mean_luma })
}

/// Calcule le masque demandé et ceux qui viennent avec lui (les quatre masques du visage) ;
/// le masque demandé est le premier.
pub fn compute_set(m: Model, guide: &[[f32; 4]], w: u32, h: u32) -> Result<Vec<(Model, AiMask)>> {
    if m.face_classes().is_none() {
        return Ok(vec![(m, compute(m, guide, w, h)?)]);
    }
    let maps = face_masks(guide, w, h)?;
    let luma: Vec<f32> = guide.iter().map(|p| crate::image_io::srgb_to_linear(0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])).collect();
    let mut out: Vec<(Model, AiMask)> = FACE_MODELS
        .iter()
        .zip(maps)
        .map(|(&fm, data)| {
            let (num, den) = data.iter().zip(&luma).fold((0f64, 0f64), |(n, d), (v, l)| (n + (v * l) as f64, d + *v as f64));
            let mean_luma = if den > 1.0 { (num / den) as f32 } else { 0.0 };
            (fm, AiMask { w, h, data, mean_luma })
        })
        .collect();
    let i = out.iter().position(|(fm, _)| *fm == m).unwrap_or(0);
    out.swap(0, i);
    Ok(out)
}

/// Visage détecté, en pixels de l'image analysée : centre, taille, confiance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

fn iou(a: &Face, b: &Face) -> f32 {
    let iw = ((a.cx + a.w / 2.0).min(b.cx + b.w / 2.0) - (a.cx - a.w / 2.0).max(b.cx - b.w / 2.0)).max(0.0);
    let ih = ((a.cy + a.h / 2.0).min(b.cy + b.h / 2.0) - (a.cy - a.h / 2.0).max(b.cy - b.h / 2.0)).max(0.0);
    let i = iw * ih;
    i / (a.w * a.h + b.w * b.h - i).max(1e-6)
}

/// Suppression des détections qui se recouvrent (on garde la plus sûre).
fn nms(mut faces: Vec<Face>, thr: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Face> = Vec::new();
    for f in faces {
        if keep.iter().all(|k| iou(&f, k) < thr) {
            keep.push(f);
        }
    }
    keep.truncate(16);
    keep
}

/// Décode les sorties de YuNet (un pas de grille) : centre et taille relatifs à la cellule.
fn yunet_decode(cls: &[f32], obj: &[f32], bbox: &[f32], stride: u32, side: u32, scale: f32, out: &mut Vec<Face>) {
    let n = side / stride;
    let st = stride as f32;
    for i in 0..((n * n) as usize).min(cls.len()).min(obj.len()).min(bbox.len() / 4) {
        let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
        if score < 0.6 {
            continue;
        }
        let (r, c) = ((i as u32 / n) as f32, (i as u32 % n) as f32);
        let b = &bbox[4 * i..4 * i + 4];
        out.push(Face {
            cx: (c + b[0]) * st / scale,
            cy: (r + b[1]) * st / scale,
            w: b[2].exp() * st / scale,
            h: b[3].exp() * st / scale,
            score,
        });
    }
}

/// Détecte les visages de l'image sRGB `w`x`h` (0..1).
pub fn detect_faces(rgb: &[[f32; 4]], w: u32, h: u32) -> Result<Vec<Face>> {
    const SIDE: u32 = 640;
    // Avant tout tenseur : ONNX Runtime doit être chargé.
    ensure_ort()?;
    // Image réduite posée dans le coin haut gauche d'un carré noir, en BGR 0..255.
    let scale = SIDE as f32 / w.max(h) as f32;
    let (rw, rh) = (((w as f32 * scale).round() as u32).clamp(1, SIDE), ((h as f32 * scale).round() as u32).clamp(1, SIDE));
    let small = resize(rgb, w, h, rw, rh);
    let plane = (SIDE * SIDE) as usize;
    let mut input = vec![0f32; 3 * plane];
    for y in 0..rh {
        for x in 0..rw {
            let p = small[(y * rw + x) as usize];
            let i = (y * SIDE + x) as usize;
            for c in 0..3 {
                input[c * plane + i] = p[2 - c].clamp(0.0, 1.0) * 255.0;
            }
        }
    }
    let tensor = ort::value::Tensor::from_array(([1usize, 3, SIDE as usize, SIDE as usize], input)).map_err(|e| anyhow!("{e}"))?;
    let outs = run_outputs(Model::FaceDetect, vec![tensor.into()], false)?;
    let get = |name: String| outs.iter().find(|o| o.0 == name).map(|o| o.2.as_slice()).with_context(|| format!("sortie {name} absente du détecteur de visages"));
    let mut faces = Vec::new();
    for st in [8u32, 16, 32] {
        yunet_decode(get(format!("cls_{st}"))?, get(format!("obj_{st}"))?, get(format!("bbox_{st}"))?, st, SIDE, scale, &mut faces);
    }
    Ok(nms(faces, 0.3))
}

/// Échantillonnage bilinéaire (bords prolongés) d'une carte `w`x`h` au point (x, y) en pixels.
fn bilinear(map: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let a = map[y0 * w + x0] + (map[y0 * w + x1] - map[y0 * w + x0]) * tx;
    let b = map[y1 * w + x0] + (map[y1 * w + x1] - map[y1 * w + x0]) * tx;
    a + (b - a) * ty
}

/// Masques du visage (ordre de FACE_MODELS) à la taille `w`x`h` : chaque visage détecté est recadré
/// (carré de 1,7 fois sa taille), découpé par le réseau, et ses probabilités reportées sur l'image.
fn face_masks(rgb: &[[f32; 4]], w: u32, h: u32) -> Result<Vec<Vec<f32>>> {
    const S: usize = 512;
    let faces = detect_faces(rgb, w, h)?;
    if faces.is_empty() {
        bail!("aucun visage détecté");
    }
    let (wu, hu) = (w as usize, h as usize);
    let mut out = vec![vec![0f32; wu * hu]; FACE_MODELS.len()];
    let planes: [Vec<f32>; 3] = std::array::from_fn(|c| rgb.iter().map(|p| p[c]).collect());
    for f in &faces {
        let side = f.w.max(f.h) * 1.7;
        if side < 16.0 {
            continue;
        }
        let (x0, y0) = (f.cx - side / 2.0, f.cy - side / 2.0);
        let k = side / S as f32;
        let mut input = vec![0f32; 3 * S * S];
        for y in 0..S {
            for x in 0..S {
                let (sx, sy) = (x0 + (x as f32 + 0.5) * k - 0.5, y0 + (y as f32 + 0.5) * k - 0.5);
                for c in 0..3 {
                    let v = bilinear(&planes[c], wu, hu, sx, sy);
                    input[c * S * S + y * S + x] = (v - IMAGENET_MEAN[c]) / IMAGENET_STD[c];
                }
            }
        }
        let tensor = ort::value::Tensor::from_array(([1usize, 3, S, S], input)).map_err(|e| anyhow!("{e}"))?;
        let (shape, logits) = run(Model::FaceSkin, vec![tensor.into()])?;
        let classes = 19;
        if shape.len() != 4 || shape[1] as usize != classes || shape[2] as usize != S || shape[3] as usize != S || logits.len() < classes * S * S {
            bail!("sortie inattendue du découpage du visage : {shape:?}");
        }
        // Probabilité de chaque groupe de classes (softmax).
        let mut groups = vec![vec![0f32; S * S]; FACE_MODELS.len()];
        let mut e = [0f32; 19];
        for i in 0..S * S {
            let mx = (0..classes).map(|c| logits[c * S * S + i]).fold(f32::MIN, f32::max);
            for c in 0..classes {
                e[c] = (logits[c * S * S + i] - mx).exp();
            }
            let sum: f32 = e.iter().sum();
            for (g, m) in FACE_MODELS.iter().enumerate() {
                groups[g][i] = m.face_classes().unwrap().iter().map(|&c| e[c]).sum::<f32>() / sum;
            }
        }
        // Report dans l'image, avec un fondu près des bords du recadrage.
        let fade = S as f32 * 0.06;
        let (px0, py0) = (x0.floor().max(0.0) as usize, y0.floor().max(0.0) as usize);
        let (px1, py1) = (((x0 + side).ceil().max(0.0) as usize).min(wu), ((y0 + side).ceil().max(0.0) as usize).min(hu));
        for py in py0..py1 {
            for px in px0..px1 {
                let (u, v) = ((px as f32 + 0.5 - x0) / k - 0.5, (py as f32 + 0.5 - y0) / k - 0.5);
                let edge = u.min(v).min(S as f32 - 1.0 - u).min(S as f32 - 1.0 - v);
                if edge <= 0.0 {
                    continue;
                }
                let wgt = (edge / fade).min(1.0);
                for (g, map) in groups.iter().enumerate() {
                    let o = &mut out[g][py * wu + px];
                    *o = o.max(bilinear(map, S, S, u, v) * wgt);
                }
            }
        }
    }
    Ok(out)
}

/// Image guide : la source sans aucun réglage, à la taille des masques.
pub fn render_guide(dev: &Developer, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Vec<[f32; 4]>> {
    let u = Adjustments::default().uniforms([0.0, 0.0, 1.0, 1.0], true, false, [dev.width as f32, dev.height as f32], true);
    dev.render_to_cpu(device, queue, &u, dev.mask_w, dev.mask_h)
}

// ---------- Cache ----------

fn cache_file(photo: &Path, m: Model) -> Option<PathBuf> {
    // Les copies virtuelles partagent les masques de leur photo.
    let photo = &crate::library::source(photo);
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(photo).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::canonicalize(photo).unwrap_or_else(|_| photo.to_path_buf()).hash(&mut h);
    mtime.hash(&mut h);
    meta.len().hash(&mut h);
    Some(crate::library::data_dir().join("masks").join(format!("{:016x}_{}.mask", h.finish(), m.label())))
}

/// En-tête des masques en cache : LXM1 = octets bruts (anciennes versions), LXM2 = compressés (zlib).
const MAGIC_RAW: &[u8; 4] = b"LXM1";
const MAGIC: &[u8; 4] = b"LXM2";

pub fn cached(photo: &Path, m: Model) -> bool {
    cache_file(photo, m).is_some_and(|f| f.exists())
}

/// Masques préparés en arrière-plan pour les photos de la bibliothèque.
pub const BACKGROUND_MODELS: [Model; 2] = [Model::Subject, Model::Sky];

/// Calcule et met en cache les masques d'arrière-plan qui manquent, si leurs modèles sont déjà
/// téléchargés (rien n'est téléchargé ici). Renvoie le nombre de masques calculés.
pub fn precompute(photo: &Path, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<usize> {
    let todo: Vec<Model> = BACKGROUND_MODELS.into_iter().filter(|m| model_path(*m).exists() && !cached(photo, *m)).collect();
    if todo.is_empty() {
        return Ok(0);
    }
    let img = crate::image_io::load(photo)?;
    let dev = Developer::new(device, queue, wgpu::TextureFormat::Bgra8Unorm, &img)?;
    drop(img);
    let guide = render_guide(&dev, device, queue)?;
    let mut n = 0;
    for m in todo {
        for (sm, mask) in compute_set(m, &guide, dev.mask_w, dev.mask_h)? {
            cache_store(photo, sm, &mask)?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn cache_load(photo: &Path, m: Model) -> Option<AiMask> {
    let bytes = std::fs::read(cache_file(photo, m)?).ok()?;
    if bytes.len() < 16 {
        return None;
    }
    let rd = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let (w, h) = (rd(4), rd(8));
    let mean_luma = f32::from_bits(rd(12));
    let data = match &bytes[..4] {
        m if m == MAGIC => {
            let mut out = Vec::with_capacity((w * h) as usize);
            flate2::read::ZlibDecoder::new(&bytes[16..]).read_to_end(&mut out).ok()?;
            out
        }
        m if m == MAGIC_RAW => bytes[16..].to_vec(),
        _ => return None,
    };
    if data.len() != (w * h) as usize {
        return None;
    }
    Some(AiMask { w, h, data: data.iter().map(|v| *v as f32 / 255.0).collect(), mean_luma })
}

pub fn cache_store(photo: &Path, m: Model, mask: &AiMask) -> Result<()> {
    let f = cache_file(photo, m).context("photo illisible")?;
    std::fs::create_dir_all(f.parent().unwrap())?;
    let mut bytes = Vec::with_capacity(16 + mask.data.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&mask.w.to_le_bytes());
    bytes.extend_from_slice(&mask.h.to_le_bytes());
    bytes.extend_from_slice(&mask.mean_luma.to_bits().to_le_bytes());
    let raw: Vec<u8> = mask.data.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
    let mut z = flate2::write::ZlibEncoder::new(bytes, flate2::Compression::fast());
    z.write_all(&raw)?;
    std::fs::write(f, z.finish()?)?;
    Ok(())
}

/// Modèles dont les réglages ont besoin (masques locaux IA, remplacement du ciel).
pub fn needed(adj: &Adjustments) -> Vec<Model> {
    let mut v = Vec::new();
    for l in adj.locals.iter().take(crate::adjust::MAX_LOCALS) {
        if let Shape::Ai { kind, .. } = l.shape {
            v.push(Model::of(kind));
        }
    }
    if adj.sky.image.is_some() {
        v.push(Model::Sky);
    }
    if adj.depth.active() {
        v.push(Model::Depth);
    }
    let e = &adj.enhance;
    if (e.sky > 0.0 || e.structure != 0.0) && !v.contains(&Model::Sky) {
        v.push(Model::Sky);
    }
    if ((e.structure != 0.0 && e.protect_subject) || adj.portrait.body != 0.0 || adj.portrait.belly != 0.0) && !v.contains(&Model::Subject) {
        v.push(Model::Subject);
    }
    if adj.portrait.active() {
        v.extend(FACE_MODELS);
    }
    for l in adj.layers.iter().take(crate::adjust::MAX_LAYERS).filter(|l| l.visible) {
        match l.mask {
            crate::adjust::LayerMask::All => {}
            crate::adjust::LayerMask::Sky => v.push(Model::Sky),
            crate::adjust::LayerMask::Subject | crate::adjust::LayerMask::Background => v.push(Model::Subject),
        }
    }
    v.sort_by_key(|m| *m as u8);
    v.dedup();
    v
}

/// Masques pour un rendu hors interface (export par lots, vignettes) : cache,
/// sinon calcul si `compute` (le modèle doit être présent ou téléchargeable).
pub fn masks_for(
    photo: &Path,
    adj: &Adjustments,
    dev: &Developer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    compute_missing: bool,
) -> HashMap<Model, Arc<AiMask>> {
    masks_with(photo, adj, &[], dev, device, queue, compute_missing)
}

/// Comme `masks_for`, avec des masques en plus de ceux que demandent les réglages.
pub fn masks_with(
    photo: &Path,
    adj: &Adjustments,
    extra: &[Model],
    dev: &Developer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    compute_missing: bool,
) -> HashMap<Model, Arc<AiMask>> {
    let mut out = HashMap::new();
    let mut guide = None;
    let mut models = needed(adj);
    models.extend(extra.iter().filter(|m| !models.contains(m)).copied().collect::<Vec<_>>());
    for m in models {
        if let Some(mask) = cache_load(photo, m) {
            out.insert(m, Arc::new(mask));
            continue;
        }
        if !compute_missing {
            continue;
        }
        let r = (|| -> Result<AiMask> {
            ensure_model(m, &Progress::default())?;
            if guide.is_none() {
                guide = Some(render_guide(dev, device, queue)?);
            }
            let mut set = compute_set(m, guide.as_ref().unwrap(), dev.mask_w, dev.mask_h)?;
            for (sm, mask) in &set {
                let _ = cache_store(photo, *sm, mask);
            }
            Ok(set.swap_remove(0).1)
        })();
        match r {
            Ok(mask) => {
                out.insert(m, Arc::new(mask));
            }
            Err(e) => eprintln!("masque IA ({}) de {} : {e:#}", m.label(), photo.display()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Carte graphique contre processeur : LUMINUX_DUMP=fichier, photo LUMINUX_PHOTO ; à lancer deux fois
    /// (avec et sans LUMINUX_AI_CPU=1) puis comparer les fichiers.
    #[test]
    #[ignore]
    fn dump_masks_for_gpu_comparison() {
        let img = image::open(std::env::var("LUMINUX_PHOTO").unwrap()).unwrap().to_rgb32f();
        let (w, h) = img.dimensions();
        let guide: Vec<[f32; 4]> = img.pixels().map(|p| [p[0], p[1], p[2], 1.0]).collect();
        let mut out = Vec::new();
        for m in [Model::Subject, Model::Depth, Model::FaceSkin] {
            let t = std::time::Instant::now();
            for (mm, mask) in super::compute_set(m, &guide, w, h).unwrap() {
                eprintln!("{} : {:?}", mm.label(), t.elapsed());
                out.extend(mask.data.iter().flat_map(|v| v.to_le_bytes()));
            }
        }
        eprintln!("carte graphique : {:?}", super::on_gpu());
        std::fs::write(std::env::var("LUMINUX_DUMP").unwrap(), out).unwrap();
    }

    #[test]
    fn scene_mask_sums_class_probabilities() {
        // 150 classes, 2x2 : pixel 0 très « personne » (12), les autres neutres.
        let (k, plane) = (150usize, 4usize);
        let mut logits = vec![0f32; k * plane];
        logits[12 * plane] = 10.0;
        let (side, m) = scene_mask(&[1, 150, 2, 2], &logits, Model::People.scene_classes().unwrap()).unwrap();
        assert_eq!(side, 2);
        assert!(m[0] > 0.99 && (m[1] - 1.0 / 150.0).abs() < 1e-4);
        assert!(scene_mask(&[1, 1, 2, 2], &logits, &[0]).is_err());
    }

    #[test]
    fn guided_filter_keeps_flat_masks() {
        let g = vec![0.5f32; 100];
        let p = vec![1.0f32; 100];
        let o = guided_filter(&g, &p, 10, 10, 2, 1e-3);
        assert!(o.iter().all(|v| (v - 1.0).abs() < 1e-4));
    }

    #[test]
    fn guided_filter_snaps_to_edges() {
        // Guide : moitié gauche sombre, droite claire ; masque grossier décalé.
        let (w, h) = (40usize, 10usize);
        let g: Vec<f32> = (0..w * h).map(|k| if k % w < 20 { 0.1 } else { 0.9 }).collect();
        let p: Vec<f32> = (0..w * h).map(|k| if k % w < 23 { 0.0 } else { 1.0 }).collect();
        let o = guided_filter(&g, &p, w, h, 4, 1e-4);
        // À droite du vrai contour, le masque remonte malgré l'erreur du masque grossier.
        assert!(o[5 * w + 21] > 0.4, "{}", o[5 * w + 21]);
        assert!(o[5 * w + 17] < 0.2);
    }

    #[test]
    fn resample_round_trip() {
        let m: Vec<f32> = (0..16).map(|k| k as f32 / 15.0).collect();
        let r = resample(&m, 4, 4, 4, 4);
        assert_eq!(r, m);
    }
}
