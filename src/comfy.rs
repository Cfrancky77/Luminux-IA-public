//! Génération dans une zone peinte par un serveur ComfyUI (API HTTP : /upload/image, /prompt,
//! /history, /view). La zone et son entourage sont envoyés avec un masque dans l'alpha ;
//! seul l'intérieur du masque est repris dans la photo.

use crate::adjust::BrushStroke;
use crate::retouch::{blur, dilate, erase_area, to_srgb, Canvas, Patch};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::time::{Duration, Instant};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8188";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Lit une réponse : JSON si le statut est bon, sinon une erreur avec le texte renvoyé par ComfyUI.
fn read_json(mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<Value> {
    let status = resp.status();
    let text = resp.body_mut().with_config().limit(64 << 20).read_to_string().unwrap_or_default();
    if !status.is_success() {
        // ComfyUI détaille les erreurs de graphe dans "error" et "node_errors".
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .map(|v| {
                let mut s = v["error"]["message"].as_str().unwrap_or("").to_string();
                if let Some(d) = v["error"]["details"].as_str().filter(|d| !d.is_empty()) {
                    s += &format!(" ({d})");
                }
                if let Some(nodes) = v["node_errors"].as_object() {
                    for n in nodes.values() {
                        for e in n["errors"].as_array().into_iter().flatten() {
                            s += &format!(" ; {} {}", e["message"].as_str().unwrap_or(""), e["details"].as_str().unwrap_or(""));
                        }
                    }
                }
                s
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| text.chars().take(300).collect());
        bail!("{what} : erreur {status} : {detail}");
    }
    serde_json::from_str(&text).with_context(|| format!("{what} : réponse illisible"))
}

/// Modèles (checkpoints) installés sur le serveur.
pub fn checkpoints(url: &str) -> Result<Vec<String>> {
    let u = format!("{}/object_info/CheckpointLoaderSimple", base(url));
    let resp = agent().get(&u).call().with_context(|| format!("serveur ComfyUI injoignable ({})", base(url)))?;
    let v = read_json(resp, "liste des modèles")?;
    combo(&v["CheckpointLoaderSimple"]["input"]["required"]["ckpt_name"]).ok_or_else(|| anyhow!("réponse inattendue : ce serveur est-il bien ComfyUI ?"))
}

/// Choix d'une entrée de nœud : ancien format [[a, b…], {…}] ou nouveau ["COMBO", {"options": [a, b…]}].
fn combo(input: &Value) -> Option<Vec<String>> {
    let list = match &input[0] {
        Value::Array(a) => a,
        Value::String(s) if s == "COMBO" => input[1]["options"].as_array()?,
        _ => return None,
    };
    Some(list.iter().filter_map(|x| x.as_str().map(String::from)).collect())
}

/// Modèles d'agrandissement installés (dossier models/upscale_models de ComfyUI).
pub fn upscalers(url: &str) -> Result<Vec<String>> {
    let u = format!("{}/object_info/UpscaleModelLoader", base(url));
    let resp = agent().get(&u).call().with_context(|| format!("serveur ComfyUI injoignable ({})", base(url)))?;
    let v = read_json(resp, "liste des modèles d'agrandissement")?;
    Ok(combo(&v["UpscaleModelLoader"]["input"]["required"]["model_name"]).unwrap_or_default())
}

/// Facteur natif d'un modèle d'agrandissement, lu dans son nom (RealESRGAN_x4plus, 2x-…) ; 4 par défaut.
pub fn model_scale(name: &str) -> u32 {
    let m = name.to_lowercase();
    if DENOISERS.iter().any(|k| m.contains(k)) {
        return 1;
    }
    [1u32, 2, 3, 4, 8].into_iter().rev().find(|k| m.contains(&format!("x{k}")) || m.contains(&format!("{k}x"))).unwrap_or(4)
}

/// Familles de modèles de restauration sans agrandissement (facteur 1) reconnues par ComfyUI (spandrel).
const DENOISERS: [&str; 6] = ["scunet", "nafnet", "dncnn", "drunet", "denois", "restormer"];

/// Modèle de débruitage (facteur 1) plutôt que d'agrandissement.
pub fn is_denoiser(name: &str) -> bool {
    model_scale(name) == 1
}

/// Modèle de vision (moondream, llava, qwen2.5-vl…) d'après sa famille ou son nom.
fn is_vision(m: &Value) -> bool {
    let name = m["name"].as_str().unwrap_or("").to_lowercase();
    let fam = m["details"]["families"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).any(|f| matches!(f, "clip" | "mllama"));
    fam || ["vl", "vision", "llava", "moondream", "minicpm-v", "bakllava"].iter().any(|k| name.contains(k))
}

/// Modèles installés sur Ollama (nom, taille) : les modèles de texte du plus petit au plus grand, puis ceux de
/// vision, sans les modèles d'embeddings.
pub fn ollama_models(url: &str) -> Result<Vec<(String, u64)>> {
    let resp = agent().get(&format!("{}/api/tags", base(url))).call().with_context(|| format!("serveur Ollama injoignable ({})", base(url)))?;
    let v = read_json(resp, "Ollama")?;
    let mut list: Vec<(String, u64, bool)> = v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| !m["name"].as_str().unwrap_or("").contains("embed") && m["details"]["family"].as_str().is_none_or(|f| !f.contains("bert")))
        .filter_map(|m| Some((m["name"].as_str()?.to_string(), m["size"].as_u64().unwrap_or(0), is_vision(m))))
        .collect();
    // Modèles de texte d'abord : « Auto » prend le premier, et un modèle de vision traduit mal.
    list.sort_by_key(|m| (m.2, m.1));
    Ok(list.into_iter().map(|m| (m.0, m.1)).collect())
}

/// Traduit une invite en anglais (texte court, sans commentaire). Renvoie (traduction, modèle utilisé).
pub fn translate(url: &str, model: &str, text: &str, unload_after: bool) -> Result<(String, String)> {
    let model = if model.is_empty() {
        ollama_models(url)?.into_iter().next().map(|m| m.0).ok_or_else(|| anyhow!("aucun modèle de texte sur Ollama"))?
    } else {
        model.to_string()
    };
    let instruction = format!(
        "Translate this image-generation prompt into English. Keep it short and descriptive. \
         If it is already English, return it unchanged. Answer with the translation only, no quotes, no explanation.\n\n{text}"
    );
    let mut body = json!({"model": model, "prompt": instruction, "stream": false, "think": false,
                          "options": {"temperature": 0, "seed": 1}});
    if unload_after {
        body["keep_alive"] = json!(0);
    }
    let resp = agent().post(&format!("{}/api/generate", base(url))).header("Content-Type", "application/json").send(&serde_json::to_vec(&body)?[..]).context("traduction par Ollama")?;
    let v = read_json(resp, "traduction")?;
    let raw = v["response"].as_str().unwrap_or("");
    // Les modèles « qui réfléchissent » peuvent renvoyer leur réflexion entre balises.
    let raw = raw.rsplit("</think>").next().unwrap_or(raw);
    let out = raw.trim().trim_matches(|c| c == '"' || c == '«' || c == '»').trim().to_string();
    if out.is_empty() {
        bail!("traduction vide (modèle {model})");
    }
    Ok((out, model))
}

/// Demande à Ollama une réponse JSON (traduction de l'interface). Modèle vide : le plus petit installé.
pub fn ollama_json(url: &str, model: &str, prompt: &str) -> Result<Value> {
    let model = if model.is_empty() {
        ollama_models(url)?.into_iter().next().map(|m| m.0).ok_or_else(|| anyhow!("aucun modèle de texte sur Ollama"))?
    } else {
        model.to_string()
    };
    let body = json!({"model": model, "prompt": prompt, "stream": false, "think": false, "format": "json",
                      "options": {"temperature": 0, "seed": 1, "num_ctx": 8192}});
    let resp = agent().post(&format!("{}/api/generate", base(url))).header("Content-Type", "application/json").send(&serde_json::to_vec(&body)?[..]).context("requête à Ollama")?;
    let v = read_json(resp, "Ollama")?;
    let raw = v["response"].as_str().unwrap_or("");
    let raw = raw.rsplit("</think>").next().unwrap_or(raw).trim();
    serde_json::from_str(raw).with_context(|| format!("réponse illisible du modèle {model}"))
}

/// Modèles chargés par Ollama : nom et mémoire vidéo occupée (octets).
pub fn ollama_loaded(url: &str) -> Result<Vec<(String, u64)>> {
    let resp = agent().get(&format!("{}/api/ps", base(url))).call().with_context(|| format!("serveur Ollama injoignable ({})", base(url)))?;
    let v = read_json(resp, "Ollama")?;
    Ok(v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["name"].as_str()?.to_string(), m["size_vram"].as_u64().unwrap_or(0))))
        .collect())
}

/// Décharge tous les modèles d'Ollama (keep_alive à 0) ; renvoie leurs noms.
pub fn ollama_unload(url: &str) -> Result<Vec<String>> {
    let loaded = ollama_loaded(url)?;
    let agent = agent();
    let mut failed = Vec::new();
    for (name, _) in &loaded {
        // /api/generate pour les modèles de texte, /api/embed pour ceux d'embeddings.
        let ok = ["generate", "embed"].iter().any(|ep| {
            let body = json!({"model": name, "keep_alive": 0});
            let body = serde_json::to_vec(&body).unwrap_or_default();
            agent
                .post(&format!("{}/api/{ep}", base(url)))
                .header("Content-Type", "application/json")
                .send(&body[..])
                .is_ok_and(|r| r.status().is_success())
        });
        if !ok {
            failed.push(name.clone());
        }
    }
    if !failed.is_empty() {
        bail!("modèle(s) non déchargé(s) : {}", failed.join(", "));
    }
    Ok(loaded.into_iter().map(|(n, _)| n).collect())
}

/// Modèle proposé par défaut : un modèle d'inpainting s'il y en a un.
pub fn preferred(models: &[String]) -> Option<String> {
    models.iter().find(|m| m.to_lowercase().contains("inpaint")).or_else(|| models.first()).cloned()
}

/// Réglages d'échantillonnage selon la famille du modèle (devinée d'après son nom).
struct Recipe {
    side: u32,
    steps: u32,
    cfg: f32,
    sampler: &'static str,
    scheduler: &'static str,
    inpaint_model: bool,
}

fn recipe(model: &str) -> Recipe {
    let m = model.to_lowercase();
    let xl = ["xl", "turbo", "lightning", "pony", "illustrious", "juggernaut"].iter().any(|k| m.contains(k));
    let fast = ["turbo", "lightning", "hyper", "lcm"].iter().any(|k| m.contains(k));
    Recipe {
        side: if xl { 1024 } else { 640 },
        steps: if fast { 8 } else { 28 },
        cfg: if fast { 1.5 } else { 6.5 },
        sampler: if m.contains("lcm") { "lcm" } else if fast { "euler_ancestral" } else { "dpmpp_2m" },
        scheduler: if fast { "sgm_uniform" } else { "karras" },
        inpaint_model: m.contains("inpaint"),
    }
}

#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub url: &'a str,
    pub model: &'a str,
    pub prompt: &'a str,
    pub negative: &'a str,
    pub seed: u64,
    pub strength: f32,
    /// Denoise imposé aux modèles d'inpainting (None : 1,0, la zone est entièrement recréée).
    pub inpaint_denoise: Option<f32>,
    /// Serveur Ollama à vider avant la génération (None : ne rien décharger).
    pub ollama: Option<&'a str>,
    /// Traduction de l'invite : serveur Ollama et modèle (vide : choix automatique).
    pub translate: Option<(&'a str, &'a str)>,
}

fn denoise(req: &Request, rc: &Recipe) -> f32 {
    if rc.inpaint_model { req.inpaint_denoise.unwrap_or(1.0).clamp(0.05, 1.0) } else { req.strength.clamp(0.05, 1.0) }
}

/// Graphe ComfyUI (format API). Nœud 2 : image envoyée, masque tiré de son alpha (ComfyUI : masque = 1 − alpha).
/// InpaintModelConditioning : nœud recommandé par ComfyUI pour les modèles d'inpainting (image masquée et
/// masque passés au modèle) ; il sert aussi aux modèles ordinaires (latent de l'image d'origine, bruit limité
/// au masque). Essai réel du 2026-10-20 avec VAEEncodeForInpaint : bords ajoutés remplis d'un aplat gris.
fn graph(req: &Request, image: &str, rc: &Recipe) -> Value {
    let denoise = denoise(req, rc);
    json!({
        "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": req.model}},
        "2": {"class_type": "LoadImage", "inputs": {"image": image}},
        "3": {"class_type": "CLIPTextEncode", "inputs": {"text": req.prompt, "clip": ["1", 1]}},
        "4": {"class_type": "CLIPTextEncode", "inputs": {"text": req.negative, "clip": ["1", 1]}},
        "6": {"class_type": "InpaintModelConditioning", "inputs": {
            "positive": ["3", 0], "negative": ["4", 0], "vae": ["1", 2], "pixels": ["2", 0], "mask": ["2", 1], "noise_mask": true}},
        "7": {"class_type": "KSampler", "inputs": {
            "model": ["1", 0], "positive": ["6", 0], "negative": ["6", 1], "latent_image": ["6", 2],
            "seed": req.seed, "steps": rc.steps, "cfg": rc.cfg, "sampler_name": rc.sampler,
            "scheduler": rc.scheduler, "denoise": denoise}},
        "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["1", 2]}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "luminux"}}
    })
}

fn upload(agent: &ureq::Agent, url: &str, name: &str, png: &[u8]) -> Result<String> {
    let boundary = format!("----luminux{:x}", std::process::id() as u64 ^ png.len() as u64);
    let mut body = Vec::with_capacity(png.len() + 512);
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\ntrue\r\n--{boundary}--\r\n").as_bytes());
    let resp = agent
        .post(&format!("{url}/upload/image"))
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .send(&body[..])
        .context("envoi de l'image à ComfyUI")?;
    let v = read_json(resp, "envoi de l'image")?;
    let name = v["name"].as_str().ok_or_else(|| anyhow!("envoi de l'image : réponse sans nom"))?;
    Ok(match v["subfolder"].as_str().filter(|s| !s.is_empty()) {
        Some(sub) => format!("{sub}/{name}"),
        None => name.to_string(),
    })
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Envoie la zone, attend le résultat ; renvoie l'image produite (taille d'envoi).
fn run(req: &Request, png: &[u8], rc: &Recipe) -> Result<image::RgbImage> {
    if crate::sdlocal::is_local(req.model) {
        let img = image::load_from_memory(png)?.to_rgba8();
        let s = crate::sdlocal::Sampling { prompt: req.prompt, negative: req.negative, seed: req.seed, denoise: denoise(req, rc), steps: rc.steps, cfg: rc.cfg, sampler: rc.sampler, scheduler: rc.scheduler };
        return crate::sdlocal::inpaint(req.model, &img, &s);
    }
    let name = format!("luminux_{:016x}.png", req.seed ^ png.len() as u64);
    run_graph(req.url, &name, png, |image| graph(req, image, rc))
}

/// Envoie l'image, exécute le graphe construit autour de son nom (sortie : nœud SaveImage « 9 ») et renvoie le résultat.
fn run_graph(url: &str, name: &str, png: &[u8], make: impl FnOnce(&str) -> Value) -> Result<image::RgbImage> {
    let url = base(url);
    let agent = agent();
    let image = upload(&agent, &url, name, png)?;
    let body = serde_json::to_vec(&json!({"prompt": make(&image), "client_id": "luminux"}))?;
    // Nouvelle connexion : celle de l'envoi a pu être fermée par le serveur après un gros fichier.
    let resp = self::agent().post(&format!("{url}/prompt")).header("Content-Type", "application/json").send(&body[..]).context("envoi du graphe")?;
    let id = read_json(resp, "graphe refusé")?["prompt_id"].as_str().ok_or_else(|| anyhow!("ComfyUI n'a pas renvoyé d'identifiant"))?.to_string();
    // Attente du résultat (le premier chargement d'un modèle peut être long).
    let start = Instant::now();
    let out = loop {
        if start.elapsed() > Duration::from_secs(900) {
            bail!("pas de résultat de ComfyUI après 15 minutes");
        }
        std::thread::sleep(Duration::from_millis(800));
        let resp = agent.get(&format!("{url}/history/{id}")).call().context("suivi de la génération")?;
        let h = read_json(resp, "suivi de la génération")?;
        let Some(entry) = h.get(&id) else { continue };
        if entry["status"]["status_str"].as_str() == Some("error") {
            let msg = entry["status"]["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m[0].as_str() == Some("execution_error"))
                .map(|m| m[1]["exception_message"].as_str().unwrap_or("").trim().to_string())
                .next()
                .unwrap_or_default();
            bail!("ComfyUI a échoué : {msg}");
        }
        if let Some(img) = entry["outputs"]["9"]["images"].get(0) {
            break img.clone();
        }
        if entry["status"]["completed"].as_bool() == Some(true) {
            bail!("ComfyUI n'a produit aucune image");
        }
    };
    let q = format!(
        "{url}/view?filename={}&subfolder={}&type={}",
        enc(out["filename"].as_str().unwrap_or("")),
        enc(out["subfolder"].as_str().unwrap_or("")),
        enc(out["type"].as_str().unwrap_or("output"))
    );
    let mut resp = agent.get(&q).call().context("récupération de l'image")?;
    if !resp.status().is_success() {
        bail!("récupération de l'image : erreur {}", resp.status());
    }
    let mut bytes = Vec::new();
    resp.body_mut().with_config().limit(1 << 30).reader().read_to_end(&mut bytes)?;
    Ok(image::load_from_memory(&bytes).context("image renvoyée illisible")?.to_rgb8())
}

/// Traduit l'invite et vide Ollama si demandé, exécute `f`, puis libère la mémoire de ComfyUI.
fn prepared<T>(req: &Request, f: impl FnOnce(&Request) -> Result<T>) -> Result<(T, Option<String>)> {
    // Traduction impossible (Ollama absent ou arrêté) : l'invite part telle quelle plutôt que d'échouer.
    let translated = match req.translate {
        Some((url, model)) if !req.prompt.trim().is_empty() => match translate(url, model, req.prompt, req.ollama.is_some()) {
            Ok((t, _)) => Some(t),
            Err(e) => {
                eprintln!("traduction de l'invite : {e:#}");
                None
            }
        },
        _ => None,
    };
    let req = &Request { prompt: translated.as_deref().unwrap_or(req.prompt), ..*req };
    if let Some(o) = req.ollama {
        // Échec non bloquant : ComfyUI dira lui-même s'il manque de mémoire.
        if let Err(e) = ollama_unload(o) {
            eprintln!("Ollama : {e:#}");
        }
    }
    let out = f(req);
    if req.ollama.is_some() && !crate::sdlocal::is_local(req.model) {
        let _ = agent().post(&format!("{}/free", base(req.url))).header("Content-Type", "application/json").send(&br#"{"unload_models":true,"free_memory":true}"#[..]);
    }
    Ok((out?, translated))
}

/// Taille maximale du résultat d'un agrandissement (pixels).
pub const UPSCALE_MAX_PX: u64 = 150_000_000;

/// Découpe d'une dimension en tuiles : (début, fin) sans marge, `tile` pixels au plus.
fn tile_spans(len: u32, tile: u32) -> Vec<(u32, u32)> {
    let n = len.div_ceil(tile).max(1);
    (0..n).map(|k| (k * len / n, (k + 1) * len / n)).collect()
}

/// Agrandit `img` d'un facteur `factor` avec un modèle d'agrandissement de ComfyUI (Real-ESRGAN…).
/// L'image part en tuiles de 1024 px (marge de 16 px recouverte) : essai réel du 2026-10-20 sur une
/// photo de 18 Mpx en un seul envoi, ComfyUI tournait encore au bout de plusieurs minutes (résultat
/// intermédiaire de 286 Mpx au facteur natif × 4). `progress(fait, total)` suit les tuiles.
pub fn upscale(url: &str, img: &image::RgbImage, model: &str, factor: f32, ollama: Option<&str>, progress: impl Fn(usize, usize)) -> Result<image::RgbImage> {
    if model.is_empty() {
        bail!("aucun modèle d'agrandissement dans ComfyUI (dossier models/upscale_models)");
    }
    let (w, h) = img.dimensions();
    let (tw, th) = ((w as f32 * factor).round() as u32, (h as f32 * factor).round() as u32);
    if tw as u64 * th as u64 > UPSCALE_MAX_PX {
        bail!("image finale trop grande ({tw} × {th}) : {} Mpx au plus", UPSCALE_MAX_PX / 1_000_000);
    }
    let ms = model_scale(model) as f32;
    let req = Request { url, model, prompt: "", negative: "", seed: 0, strength: 1.0, inpaint_denoise: None, ollama, translate: None };
    const TILE: u32 = 1024;
    const MARGIN: u32 = 16;
    let (xs, ys) = (tile_spans(w, TILE), tile_spans(h, TILE));
    let total = xs.len() * ys.len();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let (out, _) = prepared(&req, |_| {
        let mut out = image::RgbImage::new(tw, th);
        let mut done = 0;
        progress(0, total);
        for &(y0, y1) in &ys {
            for &(x0, x1) in &xs {
                // Tuile avec sa marge (bornée à l'image).
                let (mx0, my0) = (x0.saturating_sub(MARGIN), y0.saturating_sub(MARGIN));
                let (mx1, my1) = ((x1 + MARGIN).min(w), (y1 + MARGIN).min(h));
                let tile = image::imageops::crop_imm(img, mx0, my0, mx1 - mx0, my1 - my0).to_image();
                let mut png = Vec::new();
                tile.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
                let name = format!("luminux_up_{stamp:x}_{done}.png");
                let got = if crate::sdlocal::is_local(model) {
                    crate::sdlocal::upscale(model, &tile)
                } else {
                    run_graph(url, &name, &png, |image| {
                    // Le modèle agrandit de son facteur natif ; ImageScaleBy ramène au facteur voulu.
                    let last = if (factor - ms).abs() > 0.01 { "12" } else { "11" };
                    let mut g = json!({
                        "2": {"class_type": "LoadImage", "inputs": {"image": image}},
                        "10": {"class_type": "UpscaleModelLoader", "inputs": {"model_name": model}},
                        "11": {"class_type": "ImageUpscaleWithModel", "inputs": {"upscale_model": ["10", 0], "image": ["2", 0]}},
                        "9": {"class_type": "SaveImage", "inputs": {"images": [last, 0], "filename_prefix": "luminux_up"}}
                    });
                    if last == "12" {
                        g["12"] = json!({"class_type": "ImageScaleBy", "inputs": {"image": ["11", 0], "upscale_method": "lanczos", "scale_by": factor / ms}});
                    }
                    g
                    })
                }
                .with_context(|| format!("tuile {} sur {total}", done + 1))?;
                // Position des bords en sortie ; la marge est retirée.
                let sx = |x: u32| (x as f32 * factor).round() as u32;
                let (gw, gh) = (sx(mx1) - sx(mx0), sx(my1) - sx(my0));
                let got = if got.dimensions() == (gw, gh) { got } else { image::imageops::resize(&got, gw, gh, image::imageops::FilterType::Lanczos3) };
                let keep = image::imageops::crop_imm(&got, sx(x0) - sx(mx0), sx(y0) - sx(my0), sx(x1) - sx(x0), sx(y1) - sx(y0)).to_image();
                image::imageops::replace(&mut out, &keep, sx(x0) as i64, sx(y0) as i64);
                done += 1;
                progress(done, total);
            }
        }
        Ok(out)
    })?;
    Ok(out)
}

/// Agrandit le cadre (GenExpand) : marges gauche, haut, droite, bas en pixels, remplies par le modèle.
/// L'image d'origine est gardée telle quelle ; seul un liseré à son bord est fondu avec la génération.
pub fn expand(img: &image::RgbImage, margins: [u32; 4], req: &Request) -> Result<(image::RgbImage, Option<String>)> {
    if req.model.is_empty() {
        bail!("aucun modèle ComfyUI choisi");
    }
    let (w, h) = img.dimensions();
    let [l, t, r, b] = margins;
    let (cw, ch) = (w + l + r, h + t + b);
    if cw as u64 * ch as u64 > 150_000_000 {
        bail!("image finale trop grande ({cw} × {ch}) : 150 Mpx au plus");
    }
    // Toile envoyée, à la résolution native du modèle : les bandes ajoutées sont pré-remplies par la photo
    // étirée et floutée, la photo posée à sa place ; alpha 0 = à générer. Essai réel du 2026-10-20 sur
    // SDXL inpainting : avec des bandes grises ou des bords recopiés, le modèle dessinait une petite image
    // dans un cadre ; pré-remplies et avec un denoise de 0,85, la scène est prolongée.
    let rc = recipe(req.model);
    let s = rc.side as f32 / cw.max(ch) as f32;
    let (tw, th) = (((cw as f32 * s / 8.0).round() as u32).max(1) * 8, ((ch as f32 * s / 8.0).round() as u32).max(1) * 8);
    let (sx, sy) = (tw as f32 / cw as f32, th as f32 / ch as f32);
    let (pl, pt) = ((l as f32 * sx).round() as u32, (t as f32 * sy).round() as u32);
    let (pw, ph) = (((w as f32 * sx).round() as u32).clamp(1, tw - pl), ((h as f32 * sy).round() as u32).clamp(1, th - pt));
    let stretched = image::imageops::resize(img, tw, th, image::imageops::FilterType::Triangle);
    let mut fill = image::imageops::fast_blur(&stretched, tw.max(th) as f32 * 0.007);
    let photo = image::imageops::resize(img, pw, ph, image::imageops::FilterType::Triangle);
    image::imageops::replace(&mut fill, &photo, pl as i64, pt as i64);
    let small = image::RgbaImage::from_fn(tw, th, |x, y| {
        let p = fill.get_pixel(x, y);
        let inside = x >= pl && x < pl + pw && y >= pt && y < pt + ph;
        image::Rgba([p[0], p[1], p[2], if inside { 255 } else { 0 }])
    });
    let mut png = Vec::new();
    small.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    // Sans invite, une description neutre (déjà en anglais) ; le cadre est toujours exclu.
    let negative = [req.negative, "frame, border, picture frame, window, curtain, collage"].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join(", ");
    let (prompt, translate) = if req.prompt.trim().is_empty() { ("a natural continuation of the scene, photo", None) } else { (req.prompt, req.translate) };
    let req = Request { prompt, negative: &negative, translate, strength: 0.85, inpaint_denoise: Some(0.85), ..*req };
    let (made, translated) = prepared(&req, |req| run(req, &png, &rc))?;
    let made = image::imageops::resize(&made, cw, ch, image::imageops::FilterType::CatmullRom);
    // Fondu sur une bande à l'intérieur de la photo, seulement le long des côtés agrandis.
    let band = ((w.min(h) as f32 * 0.015) as u32).max(6) as f32;
    let out = image::RgbImage::from_fn(cw, ch, |x, y| {
        let g = *made.get_pixel(x, y);
        if x < l || x >= l + w || y < t || y >= t + h {
            return g;
        }
        let (ix, iy) = (x - l, y - t);
        let mut d = f32::INFINITY;
        if l > 0 { d = d.min(ix as f32); }
        if r > 0 { d = d.min((w - 1 - ix) as f32); }
        if t > 0 { d = d.min(iy as f32); }
        if b > 0 { d = d.min((h - 1 - iy) as f32); }
        let a = (d / band).clamp(0.0, 1.0);
        let a = a * a * (3.0 - 2.0 * a);
        let o = img.get_pixel(ix, iy);
        image::Rgb([0, 1, 2].map(|k| (o[k] as f32 * a + g[k] as f32 * (1.0 - a)).round() as u8))
    });
    Ok((out, translated))
}

/// Génère le contenu de la zone peinte et renvoie la pièce à poser sur la photo,
/// avec l'invite traduite si elle l'a été.
pub fn generate(img: &impl Canvas, strokes: &[BrushStroke], req: &Request) -> Result<(Patch, Option<String>)> {
    if req.model.is_empty() {
        bail!("aucun modèle ComfyUI choisi");
    }
    let (w, h) = img.size();
    let (r, mask) = erase_area(strokes, w, h).ok_or_else(|| anyhow!("aucune zone peinte"))?;
    let (rw, rh) = ((r[2] - r[0]) as u32, (r[3] - r[1]) as u32);
    let hole: Vec<f32> = dilate(&mask, rw, rh, ((rw.max(rh) as f32 / 200.0).ceil() as usize).max(2)).iter().map(|v| if *v > 0.05 { 1.0 } else { 0.0 }).collect();
    let rc = recipe(req.model);
    // Taille d'envoi : grand côté à la résolution native du modèle, multiple de 8.
    let s = rc.side as f32 / rw.max(rh) as f32;
    let (tw, th) = (((rw as f32 * s / 8.0).round() as u32).max(1) * 8, ((rh as f32 * s / 8.0).round() as u32).max(1) * 8);
    let crop = image::RgbaImage::from_fn(rw, rh, |x, y| {
        let v = img.get(r[0] + x as i32, r[1] + y as i32);
        let a = if hole[(y * rw + x) as usize] > 0.5 { 0 } else { 255 };
        let c = v.map(|c| (to_srgb(c) * 255.0 + 0.5) as u8);
        image::Rgba([c[0], c[1], c[2], a])
    });
    // L'alpha est réduit à part, au plus proche, pour garder un masque net.
    let mut small = image::imageops::resize(&crop, tw, th, image::imageops::FilterType::CatmullRom);
    let alpha = image::imageops::resize(&image::GrayImage::from_fn(rw, rh, |x, y| image::Luma([crop.get_pixel(x, y)[3]])), tw, th, image::imageops::FilterType::Nearest);
    for (p, a) in small.pixels_mut().zip(alpha.pixels()) {
        p[3] = a[0];
    }
    let mut png = Vec::new();
    small.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    let (out, translated) = prepared(req, |req| run(req, &png, &rc))?;
    let out = image::imageops::resize(&out, rw, rh, image::imageops::FilterType::CatmullRom);
    let soft = blur(&hole, rw, rh, ((rw.max(rh) / 300) as usize).max(2));
    let px = out
        .pixels()
        .zip(&soft)
        .map(|(p, a)| {
            let l = p.0.map(|c| crate::image_io::srgb_to_linear(c as f32 / 255.0));
            [l[0], l[1], l[2], a.clamp(0.0, 1.0)]
        })
        .collect();
    let rect = [r[0] as f32 / w as f32, r[1] as f32 / h as f32, rw as f32 / w as f32, rh as f32 / h as f32];
    Ok((Patch { rect, w: rw, h: rh, px }, translated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_models_are_recognised() {
        let m = |name: &str, fam: &[&str]| json!({"name": name, "details": {"families": fam}});
        assert!(is_vision(&m("moondream:latest", &["phi2", "clip"])));
        assert!(is_vision(&m("qwen25-vl:7b", &["qwen25vl"])));
        assert!(!is_vision(&m("qwen3-fast:4b", &["qwen3"])));
        assert!(!is_vision(&m("gpt-oss-work:latest", &["gptoss"])));
    }

    #[test]
    fn recipe_follows_model_family() {
        let t = recipe("sd_xl_turbo_1.0_fp16.safetensors");
        assert_eq!((t.side, t.steps, t.inpaint_model), (1024, 8, false));
        let i = recipe("512-inpainting-ema.safetensors");
        assert_eq!((i.side, i.inpaint_model), (640, true));
        assert_eq!(preferred(&["a.safetensors".into(), "sdxl-inpaint.safetensors".into()]).unwrap(), "sdxl-inpaint.safetensors");
    }

    #[test]
    fn graph_uses_mask_from_alpha() {
        let req = Request { url: "", model: "m.safetensors", prompt: "ciel", negative: "", seed: 7, strength: 0.9, inpaint_denoise: None, ollama: None, translate: None };
        let g = graph(&req, "luminux.png", &recipe(req.model));
        assert_eq!(g["6"]["inputs"]["mask"], json!(["2", 1]));
        assert_eq!(g["7"]["inputs"]["seed"], json!(7));
        assert!((g["7"]["inputs"]["denoise"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn combo_reads_both_formats() {
        assert_eq!(combo(&json!([["a.pth", "b.pth"], {}])).unwrap(), ["a.pth", "b.pth"]);
        assert_eq!(combo(&json!(["COMBO", {"multiselect": false, "options": ["c.safetensors"]}])).unwrap(), ["c.safetensors"]);
        assert!(combo(&json!(["COMBO", {"options": []}])).unwrap().is_empty());
    }

    #[test]
    fn tiles_cover_without_gaps() {
        for (len, tile) in [(5184, 1024), (1000, 1024), (2048, 1024), (1, 1024)] {
            let t = tile_spans(len, tile);
            assert_eq!(t.first().unwrap().0, 0);
            assert_eq!(t.last().unwrap().1, len);
            assert!(t.windows(2).all(|p| p[0].1 == p[1].0));
            assert!(t.iter().all(|&(a, b)| b > a && b - a <= tile));
        }
    }

    #[test]
    fn upscale_factor_from_name() {
        assert_eq!(model_scale("RealESRGAN_x4plus.pth"), 4);
        assert_eq!(model_scale("2x-AnimeSharpV2.pth"), 2);
        assert_eq!(model_scale("4x-UltraSharp.pth"), 4);
        assert_eq!(model_scale("ESRGAN.pth"), 4);
        assert_eq!(model_scale("scunet_color_real_psnr.pth"), 1);
        assert_eq!(model_scale("1x-NoiseToner-Poisson.pth"), 1);
        assert!(!is_denoiser("RealESRGAN_x4plus.safetensors"));
    }
}

/// Essai contre le faux serveur (`python3 mock.py dossier`) : `cargo test -- --ignored tiled_upscale`.
#[cfg(test)]
mod mock_tests {
    #[test]
    #[ignore]
    fn tiled_upscale_has_no_seams() {
        let img = image::RgbImage::from_fn(2500, 1300, |x, y| image::Rgb([(x * 255 / 2500) as u8, (y * 255 / 1300) as u8, ((x ^ y) & 255) as u8]));
        let calls = std::cell::Cell::new(0);
        let out = super::upscale("http://127.0.0.1:8188", &img, "RealESRGAN_x4plus.pth", 2.0, None, |_, t| { assert_eq!(t, 6); calls.set(calls.get() + 1) }).unwrap();
        assert_eq!(out.dimensions(), (5000, 2600));
        assert_eq!(calls.get(), 7);
        let refi = image::imageops::resize(&img, 5000, 2600, image::imageops::FilterType::Triangle);
        let mut worst = 0f64;
        for x in [1666u32, 1667, 3333, 3334, 2500] {
            let d: f64 = (0..2600).map(|y| (out.get_pixel(x, y)[0] as f64 - refi.get_pixel(x, y)[0] as f64).abs()).sum::<f64>() / 2600.0;
            worst = worst.max(d);
        }
        assert!(worst < 3.0, "écart aux jointures {worst}");
    }
}
