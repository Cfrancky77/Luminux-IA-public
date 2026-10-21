//! Looks : LUT 3D (.cube) et presets importés (Lightroom .xmp / .lrtemplate,
//! Luminar .lnpc / .lnp / .lmps), éventuellement dans des .zip imbriqués.
//!
//! Tout est converti une fois pour toutes à l'import dans
//! `~/.local/share/luminux/looks/` : `presets/<pack>/<look>.json` (réglages Luminux)
//! et `luts/<pack>/<look>.cube`. Les formules de Skylum et d'Adobe ne sont pas
//! publiques : la conversion des presets est une approximation, et ce qui n'a pas
//! d'équivalent est listé dans `ignored`.

use crate::adjust::{Adjustments, tint_offset};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub struct Lut3d {
    pub size: u32,
    /// N³ couleurs, rouge le plus rapide, puis vert, puis bleu (ordre .cube).
    pub data: Vec<[f32; 3]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Look {
    pub name: String,
    pub category: String,
    /// « LUT », « Lightroom » ou « Luminar ».
    pub source: String,
    pub adjustments: Adjustments,
    /// Réglages du preset d'origine sans équivalent dans Luminux.
    #[serde(default)]
    pub ignored: Vec<String>,
    /// Chemin du fichier .json (rempli à la lecture).
    #[serde(skip)]
    pub file: PathBuf,
}

pub fn looks_dir() -> PathBuf {
    crate::library::data_dir().join("looks")
}

fn slug(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let t = t.trim_matches('_').to_string();
    if t.is_empty() { "sans_nom".into() } else { t }
}

// ---------------------------------------------------------------- LUT .cube

pub fn parse_cube(text: &str) -> Result<Lut3d> {
    let mut size = 0u32;
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let first = it.next().unwrap_or("");
        match first {
            "LUT_3D_SIZE" => size = it.next().context("LUT_3D_SIZE sans valeur")?.parse()?,
            "LUT_1D_SIZE" => bail!("LUT 1D non prise en charge"),
            "TITLE" | "DOMAIN_MIN" | "DOMAIN_MAX" | "LUT_3D_INPUT_RANGE" => {}
            _ if first.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') => {
                let r: f32 = first.parse()?;
                let g: f32 = it.next().context("ligne incomplète")?.parse()?;
                let b: f32 = it.next().context("ligne incomplète")?.parse()?;
                data.push([r, g, b]);
            }
            _ => {}
        }
    }
    if size < 2 || size > crate::gpu::LUT3D_MAX {
        bail!("taille de LUT {size} non prise en charge (2 à {})", crate::gpu::LUT3D_MAX);
    }
    if data.len() != (size * size * size) as usize {
        bail!("LUT incomplète : {} valeurs pour {size}³", data.len());
    }
    Ok(Lut3d { size, data })
}

/// LUT d'un look, chemin relatif au dossier `luts/`. Gardée en cache.
pub fn load_lut(rel: &str) -> Option<Arc<Lut3d>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Lut3d>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(l) = cache.lock().unwrap().get(rel) {
        return l.clone();
    }
    let path = looks_dir().join("luts").join(rel);
    let lut = std::fs::read_to_string(&path)
        .map_err(anyhow::Error::from)
        .and_then(|t| parse_cube(&t))
        .map_err(|e| eprintln!("LUT {} : {e}", path.display()))
        .ok()
        .map(Arc::new);
    cache.lock().unwrap().insert(rel.to_string(), lut.clone());
    lut
}

// ---------------------------------------------------------------- Lightroom

/// Écart de température (K) converti en réglage Luminux. L'échelle en mireds
/// (1e6 / K) suit la perception : 36 mireds ≈ rapport R/B de 1,34.
fn kelvin_shift(kelvin: f32, reference: f32) -> f32 {
    if kelvin <= 0.0 || reference <= 0.0 {
        return 0.0;
    }
    ((1e6 / reference - 1e6 / kelvin) * 0.008).clamp(-1.0, 1.0)
}

/// Réglages Lightroom (clé crs sans préfixe -> valeur texte) et courbe des tons.
struct LrPreset {
    name: Option<String>,
    values: HashMap<String, String>,
    curve: Vec<[f32; 2]>,
}

fn parse_xmp(text: &str) -> LrPreset {
    let mut values = HashMap::new();
    // Forme attribut : crs:Cle="valeur"
    let mut rest = text;
    while let Some(i) = rest.find("crs:") {
        rest = &rest[i + 4..];
        let key_end = rest.find(|c: char| !c.is_alphanumeric()).unwrap_or(rest.len());
        let key = &rest[..key_end];
        let after = rest[key_end..].trim_start();
        if let Some(v) = after.strip_prefix("=\"") {
            if let Some(end) = v.find('"') {
                values.insert(key.to_string(), v[..end].to_string());
            }
        } else if let Some(v) = after.strip_prefix('>') {
            // Forme élément : <crs:Cle>valeur</crs:Cle>
            if let Some(end) = v.find('<') {
                let val = v[..end].trim();
                if !val.is_empty() {
                    values.entry(key.to_string()).or_insert_with(|| val.to_string());
                }
            }
        }
    }
    let mut curve = Vec::new();
    if let Some(i) = text.find("<crs:ToneCurvePV2012>") {
        let block = &text[i..text[i..].find("</crs:ToneCurvePV2012>").map_or(text.len(), |e| i + e)];
        for li in block.split("<rdf:li>").skip(1) {
            let v = li.split('<').next().unwrap_or("");
            let mut p = v.split(',').map(|s| s.trim().parse::<f32>());
            if let (Some(Ok(x)), Some(Ok(y))) = (p.next(), p.next()) {
                curve.push([x / 255.0, y / 255.0]);
            }
        }
    }
    let name = text.find("<crs:Name>").and_then(|i| {
        let b = &text[i..];
        let li = b.find("<rdf:li")?;
        let start = li + b[li..].find('>')? + 1;
        let end = start + b[start..].find('<')?;
        Some(b[start..end].trim().to_string()).filter(|s| !s.is_empty())
    });
    LrPreset { name, values, curve }
}

/// Presets Lightroom Classic historiques (.lrtemplate, syntaxe Lua).
fn parse_lrtemplate(text: &str) -> LrPreset {
    let mut values = HashMap::new();
    let mut curve = Vec::new();
    let settings = text.find("settings").map_or(text, |i| &text[i..]);
    for line in settings.lines() {
        let line = line.trim().trim_end_matches(',');
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k == "ToneCurvePV2012" {
            let nums: Vec<f32> = v
                .trim_matches(|c| c == '{' || c == '}')
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            curve = nums.chunks_exact(2).map(|p| [p[0] / 255.0, p[1] / 255.0]).collect();
        } else if k.chars().all(|c| c.is_alphanumeric()) {
            values.insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    let name = text
        .find("title = \"")
        .map(|i| &text[i + 9..])
        .and_then(|t| t.find('"').map(|e| t[..e].to_string()));
    LrPreset { name, values, curve }
}

/// Réglages Lightroom qui n'ont pas d'équivalent (signalés s'ils sont non nuls).
const LR_UNSUPPORTED: &[(&str, f32)] = &[
    ("Dehaze", 0.0), ("ParametricShadows", 0.0), ("ParametricDarks", 0.0), ("ParametricLights", 0.0),
    ("ParametricHighlights", 0.0), ("ShadowTint", 0.0), ("RedHue", 0.0), ("RedSaturation", 0.0), ("GreenHue", 0.0),
    ("GreenSaturation", 0.0), ("BlueHue", 0.0), ("BlueSaturation", 0.0), ("ColorGradeGlobalHue", 0.0),
    ("ColorGradeGlobalSat", 0.0), ("ColorGradeGlobalLum", 0.0), ("ColorGradeShadowLum", 0.0),
    ("ColorGradeMidtoneLum", 0.0), ("ColorGradeHighlightLum", 0.0), ("GrainFrequency", 50.0),
    ("PostCropVignetteRoundness", 0.0), ("PostCropVignetteFeather", 50.0),
];

fn lr_to_adjustments(p: &LrPreset) -> (Adjustments, Vec<String>) {
    let num = |k: &str| p.values.get(k).and_then(|v| v.trim_start_matches('+').parse::<f32>().ok());
    let pct = |k: &str| num(k).map(|v| (v / 100.0).clamp(-1.0, 1.0));
    let mut a = Adjustments::default();
    let mut ignored = Vec::new();

    if let Some(v) = num("Exposure2012").or(num("Exposure")) {
        a.exposure = v.clamp(-5.0, 5.0);
    }
    if let Some(v) = pct("Contrast2012") { a.contrast = v; }
    if let Some(v) = pct("Highlights2012") { a.highlights = v; }
    if let Some(v) = pct("Shadows2012") { a.shadows = v; }
    if let Some(v) = pct("Whites2012") { a.whites = v; }
    if let Some(v) = pct("Blacks2012") { a.blacks = v; }
    if let Some(v) = pct("Vibrance") { a.vibrance = v; }
    if let Some(v) = pct("Saturation") { a.saturation = v; }
    if let Some(v) = pct("Clarity2012") { a.clarity = 0.7 * v; }
    if let Some(v) = pct("Texture") { a.texture = v; }

    // Balance des blancs : relative à celle de prise de vue du preset.
    let wb = p.values.get("WhiteBalance").map(String::as_str).unwrap_or("");
    if let Some(v) = pct("IncrementalTemperature") {
        a.temperature = v;
    } else if let (Some(t), false) = (num("Temperature"), wb == "As Shot") {
        let reference = num("AsShotTemperature").unwrap_or(5500.0);
        a.temperature = kelvin_shift(t, reference);
    }
    if let Some(v) = pct("IncrementalTint") {
        a.tint = v;
    } else if let (Some(t), false) = (num("Tint"), wb == "As Shot") {
        a.tint = ((t - num("AsShotTint").unwrap_or(0.0)) / 150.0).clamp(-1.0, 1.0);
    }

    for (i, band) in ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"].iter().enumerate() {
        if let Some(v) = pct(&format!("HueAdjustment{band}")) { a.hsl[i].hue = v; }
        if let Some(v) = pct(&format!("SaturationAdjustment{band}")) { a.hsl[i].sat = v; }
        if let Some(v) = pct(&format!("LuminanceAdjustment{band}")) { a.hsl[i].lum = v; }
    }

    let sh_hue = num("SplitToningShadowHue").or(num("ColorGradeShadowHue"));
    let sh_sat = num("SplitToningShadowSaturation").or(num("ColorGradeShadowSat"));
    let hl_hue = num("SplitToningHighlightHue").or(num("ColorGradeHighlightHue"));
    let hl_sat = num("SplitToningHighlightSaturation").or(num("ColorGradeHighlightSat"));
    a.split_shadow_hue = sh_hue.unwrap_or(0.0);
    a.split_shadow_sat = sh_sat.unwrap_or(0.0) / 100.0;
    a.split_highlight_hue = hl_hue.unwrap_or(0.0);
    a.split_highlight_sat = hl_sat.unwrap_or(0.0) / 100.0;
    if let Some(v) = pct("SplitToningBalance") { a.split_balance = v; }
    if let (Some(h), Some(s)) = (num("ColorGradeMidtoneHue"), num("ColorGradeMidtoneSat")) {
        // La balance des tons moyens est 2,4 fois moins forte que le virage partiel dans le shader.
        let o = tint_offset(h, (s / 100.0 * 2.4).min(1.0));
        a.balance_midtones = [o[0], o[1], o[2]];
    }

    if let Some(v) = pct("GrainAmount") { a.grain = v.max(0.0); }
    if let Some(v) = pct("GrainSize") { a.grain_size = v.max(0.0); }
    if let Some(v) = num("Sharpness") { a.sharpen = (v / 150.0 * 0.5).clamp(0.0, 1.0); }
    if let Some(v) = pct("PostCropVignetteAmount") { a.vignette = v; }
    if let Some(v) = pct("PostCropVignetteMidpoint") { a.vignette_mid = v.max(0.0); }
    if p.values.get("ConvertToGrayscale").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        a.saturation = -1.0;
    }
    if p.curve.len() >= 2 && p.curve.iter().any(|q| (q[0] - q[1]).abs() > 1e-3) {
        a.curve = p.curve.clone();
    }

    for (k, default) in LR_UNSUPPORTED {
        if num(k).is_some_and(|v| (v - default).abs() > 1e-3) {
            ignored.push(k.to_string());
        }
    }
    (a, ignored)
}

// ---------------------------------------------------------------- Luminar

fn luminar_to_adjustments(json: &serde_json::Value) -> (Adjustments, Vec<String>, bool) {
    let mut a = Adjustments::default();
    let mut ignored = Vec::new();
    let mut uses_lut = false;
    let f = |p: &serde_json::Value, k: &str| p.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse::<f32>().ok());
    let bands = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

    for layer in json["layers_array"].as_array().into_iter().flatten() {
        if layer["enabled"].as_str() == Some("false") {
            continue;
        }
        for edit in layer["edits_array"].as_array().into_iter().flatten() {
            if edit["enabled"].as_str() == Some("false") {
                continue;
            }
            let tool = edit["tool_name"].as_str().unwrap_or("?");
            let masked = edit["mask"].as_str().is_some_and(|m| !m.is_empty());
            let mut tool_used = false;
            for effect in edit["effects_array"].as_array().into_iter().flatten() {
                let id = effect["id"].as_str().unwrap_or("");
                for p in effect["params_array"].as_array().into_iter().flatten() {
                    let name = p["name"].as_str().unwrap_or("");
                    let (Some(v), Some(d)) = (f(p, "value"), f(p, "default")) else { continue };
                    if (v - d).abs() < 1e-4 {
                        continue;
                    }
                    let pc = (v / 100.0).clamp(-1.0, 1.0);
                    tool_used = true;
                    let handled = match (id, name) {
                        // Échelle supposée : ±100 = ±4 IL (curseur affiché de -4 à +4 dans Luminar Neo).
                        ("MIPLRawDevelopEffect" | "MIPLDevelopCommonEffectID", "Exposure") => { a.exposure += v / 25.0; true }
                        ("MIPLRawDevelopEffect" | "MIPLDevelopCommonEffectID", "Contrast") => { a.contrast = pc; true }
                        ("MIPLRawDevelopEffect" | "MIPLDevelopCommonEffectID", "Highlights") => { a.highlights = pc; true }
                        ("MIPLRawDevelopEffect" | "MIPLDevelopCommonEffectID", "Shadows") => { a.shadows = pc; true }
                        ("MIPLRawDevelopEffect" | "MIPLDevelopCommonEffectID", "Temperature") => {
                            a.temperature = if d > 1000.0 { kelvin_shift(v, d) } else { pc };
                            true
                        }
                        ("MIPLRawDevelopEffect" | "MIPLDevelopCommonEffectID", "Tint") => { a.tint = ((v - d) / 150.0).clamp(-1.0, 1.0); true }
                        ("MIPLBlackWhiteEffect", "Whites") => { a.whites = pc; true }
                        ("MIPLBlackWhiteEffect", "Blacks") => { a.blacks = pc; true }
                        ("MIPLSaturationEffect", "Saturation") => { a.saturation = (a.saturation + pc).clamp(-1.0, 1.0); true }
                        ("MIPLVibranceEffect", "Vibrance") => { a.vibrance = (a.vibrance + pc).clamp(-1.0, 1.0); true }
                        ("MIPLChannelsEffect", n) if n.len() > 1 && bands.contains(&&n[1..]) => {
                            let i = bands.iter().position(|b| *b == &n[1..]).unwrap();
                            match &n[..1] {
                                "h" => a.hsl[i].hue = pc,
                                "s" => a.hsl[i].sat = pc,
                                _ => a.hsl[i].lum = pc,
                            }
                            true
                        }
                        ("MIPLVignetteEffect", "Amount") => { a.vignette = pc; true }
                        ("MIPLSplitToningEffect", "HighlightsHue") => { a.split_highlight_hue = v; true }
                        ("MIPLSplitToningEffect", "HighlightsSaturation") => { a.split_highlight_sat = pc.max(0.0); true }
                        ("MIPLSplitToningEffect", "ShadowsHue") => { a.split_shadow_hue = v; true }
                        ("MIPLSplitToningEffect", "ShadowsSaturation") => { a.split_shadow_sat = pc.max(0.0); true }
                        ("MIPLSplitToningEffect", "Balance") => { a.split_balance = pc; true }
                        ("MIPLGrainNewEffect" | "MIPLGrainEffect", "Amount") => { a.grain = pc.max(0.0); true }
                        ("MIPLGrainNewEffect" | "MIPLGrainEffect", "Size") => { a.grain_size = pc.max(0.0); true }
                        ("MIPLColorBalanceEffect", n) if n.len() > 1 => {
                            let target = match &n[..1] {
                                "S" => &mut a.balance_shadows,
                                "M" => &mut a.balance_midtones,
                                _ => &mut a.balance_highlights,
                            };
                            match &n[1..] {
                                "Red" => target[0] = pc,
                                "Green" => target[1] = pc,
                                "Blue" => target[2] = pc,
                                _ => {}
                            }
                            true
                        }
                        // Couleurs chaudes / froides : saturation des bandes correspondantes.
                        ("MIPLSplitWarmthEffect", "Warm Colors") => {
                            for i in [0, 1, 2] { a.hsl[i].sat = (a.hsl[i].sat + 0.5 * pc).clamp(-1.0, 1.0); }
                            true
                        }
                        ("MIPLSplitWarmthEffect", "Cool Colors") => {
                            for i in [4, 5, 6] { a.hsl[i].sat = (a.hsl[i].sat + 0.5 * pc).clamp(-1.0, 1.0); }
                            true
                        }
                        ("MIPLBrillianceWarmthEffect", "Warmth") => { a.temperature = (a.temperature + 0.3 * pc).clamp(-1.0, 1.0); true }
                        (_, "Structure" | "Clarity") => { a.clarity = 0.7 * pc; true }
                        (_, n) if n.contains("Lut") || n.contains("LUT") => { uses_lut = true; true }
                        _ => false,
                    };
                    if !handled {
                        let label = format!("{tool} / {name}");
                        if !ignored.contains(&label) {
                            ignored.push(label);
                        }
                    }
                }
            }
            if masked && tool_used {
                ignored.push(format!("{tool} : masque local appliqué à toute l'image"));
            }
        }
    }
    (a, ignored, uses_lut)
}

/// Entrées `clé = valeur` d'un fichier .strings (Apple).
fn parse_strings(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let unq = |s: &str| s.trim().trim_end_matches(';').trim().trim_matches('"').replace("\\\"", "\"");
            Some((unq(k), unq(v)))
        })
        .collect()
}

// ---------------------------------------------------------------- Import

/// Fichiers utiles d'une sélection, zip compris, sous forme chemin virtuel -> octets.
type Files = BTreeMap<String, Vec<u8>>;

const USEFUL: &[&str] = &["cube", "xmp", "lrtemplate", "lmps", "lmp", "strings", "zip", "lnpc"];

fn ext_of(p: &str) -> String {
    Path::new(p).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn add_zip(prefix: &str, bytes: Vec<u8>, out: &mut Files, depth: u32) -> Result<()> {
    if depth > 4 {
        return Ok(());
    }
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let entry = f.name()?.into_owned();
        if f.is_dir() || entry.contains("__MACOSX") || f.size() > 256 << 20 {
            continue;
        }
        let name = format!("{prefix}/{}", entry.trim_end_matches('/'));
        let ext = ext_of(&name);
        if !USEFUL.contains(&ext.as_str()) {
            continue;
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        if ext == "zip" || ext == "lnpc" {
            add_zip(&name, buf, out, depth + 1)?;
        } else {
            out.insert(name, buf);
        }
    }
    Ok(())
}

fn collect(path: &Path, out: &mut Files) -> Result<()> {
    let base = path.parent().unwrap_or(Path::new("/"));
    let walk: Vec<PathBuf> = if path.is_dir() {
        walkdir::WalkDir::new(path).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).map(|e| e.into_path()).collect()
    } else {
        vec![path.to_path_buf()]
    };
    for f in walk {
        let rel = f.strip_prefix(base).unwrap_or(&f).to_string_lossy().into_owned();
        let ext = ext_of(&rel);
        if !USEFUL.contains(&ext.as_str()) {
            continue;
        }
        let bytes = std::fs::read(&f).with_context(|| f.display().to_string())?;
        if ext == "zip" || ext == "lnpc" {
            add_zip(&rel, bytes, out, 0).with_context(|| f.display().to_string())?;
        } else {
            out.insert(rel, bytes);
        }
    }
    Ok(())
}

/// Nom de pack : premier dossier sous l'archive la plus extérieure, sinon le dossier parent.
fn pack_name(vpath: &str) -> String {
    let parts: Vec<&str> = vpath.split('/').collect();
    if let Some(i) = parts.iter().position(|p| p.to_ascii_lowercase().ends_with(".zip")) {
        if parts.len() > i + 2 {
            return parts[i + 1].to_string();
        }
        return stem(parts[i]);
    }
    if parts.len() >= 2 { parts[parts.len() - 2].to_string() } else { k!("Importés").into() }
}

fn stem(p: &str) -> String {
    Path::new(p).file_stem().and_then(|s| s.to_str()).unwrap_or(p).to_string()
}

fn parent(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}

#[derive(Default)]
pub struct ImportReport {
    pub added: Vec<String>,
    pub errors: Vec<String>,
    /// Réglages non convertis -> nombre de presets concernés.
    pub ignored: BTreeMap<String, usize>,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        let mut s = tf!("{0} look(s) importé(s)", self.added.len());
        if !self.errors.is_empty() {
            s += &tf!(", {0} erreur(s)", self.errors.len());
        }
        if !self.ignored.is_empty() {
            s += &tf!(", {0} réglage(s) sans équivalent", self.ignored.len());
        }
        s
    }
}

struct Store {
    root: PathBuf,
}

impl Store {
    fn save(&self, look: &mut Look, lut: Option<&[u8]>, report: &mut ImportReport) -> Result<()> {
        let cat = slug(&look.category);
        let file_stem = slug(&format!("{}_{}", look.name, look.source));
        if let Some(bytes) = lut {
            let rel = format!("{cat}/{}.cube", slug(&look.name));
            let path = self.root.join("luts").join(&rel);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, bytes)?;
            look.adjustments.lut = Some(rel);
        }
        let path = self.root.join("presets").join(&cat).join(format!("{file_stem}.json"));
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_string_pretty(look)?)?;
        for i in &look.ignored {
            *report.ignored.entry(format!("{} : {i}", look.source)).or_default() += 1;
        }
        report.added.push(format!("{} / {}", look.category, look.name));
        Ok(())
    }
}

/// Importe des fichiers ou dossiers (.cube, .xmp, .lrtemplate, .lnpc, .lnp, .lmps, .zip).
pub fn import(paths: &[PathBuf]) -> ImportReport {
    import_into(paths, &looks_dir())
}

fn import_into(paths: &[PathBuf], root: &Path) -> ImportReport {
    let mut report = ImportReport::default();
    let mut files = Files::new();
    for p in paths {
        if let Err(e) = collect(p, &mut files) {
            report.errors.push(format!("{} : {e:#}", p.display()));
        }
    }
    let store = Store { root: root.to_path_buf() };
    for (vpath, bytes) in &files {
        let text = || String::from_utf8_lossy(bytes).into_owned();
        let result = match ext_of(vpath).as_str() {
            "cube" if !vpath.contains(".lnp/") => parse_cube(&text()).and_then(|_| {
                let mut look = Look {
                    name: stem(vpath),
                    category: pack_name(vpath),
                    source: "LUT".into(),
                    adjustments: Adjustments::default(),
                    ignored: vec![],
                    file: PathBuf::new(),
                };
                store.save(&mut look, Some(bytes), &mut report)
            }),
            "xmp" | "lrtemplate" => {
                let p = if vpath.ends_with("xmp") { parse_xmp(&text()) } else { parse_lrtemplate(&text()) };
                if p.values.is_empty() {
                    Ok(()) // pas un preset (XMP de métadonnées)
                } else {
                    let (adjustments, ignored) = lr_to_adjustments(&p);
                    let mut look = Look {
                        name: p.name.clone().unwrap_or_else(|| stem(vpath)),
                        category: format!("{} · Lightroom", pack_name(vpath)),
                        source: "Lightroom".into(),
                        adjustments,
                        ignored,
                        file: PathBuf::new(),
                    };
                    store.save(&mut look, None, &mut report)
                }
            }
            "lmps" | "lmp" => import_luminar(vpath, bytes, &files, &store, &mut report),
            _ => Ok(()),
        };
        if let Err(e) = result {
            report.errors.push(format!("{vpath} : {e:#}"));
        }
    }
    report
}

fn import_luminar(vpath: &str, bytes: &[u8], files: &Files, store: &Store, report: &mut ImportReport) -> Result<()> {
    let json: serde_json::Value = serde_json::from_slice(bytes).context("JSON Luminar illisible")?;
    if json.get("layers_array").is_none() {
        bail!("pas un preset Luminar");
    }
    let dir = parent(vpath);
    let strings = |d: &str| {
        ["fr", "en"]
            .iter()
            .find_map(|l| files.get(&format!("{d}/localizations/{l}.strings")))
            .map(|b| parse_strings(&String::from_utf8_lossy(b)))
            .unwrap_or_default()
    };
    let raw_name = json["name"].as_str().map(str::to_string).unwrap_or_else(|| stem(dir));
    let name = strings(dir).get(&raw_name).cloned().unwrap_or(raw_name);
    // Collection .lnpc : dossier parent avec « Pack Name » localisé.
    let pack_dir = parent(dir);
    let category = strings(pack_dir)
        .get("Pack Name")
        .cloned()
        .unwrap_or_else(|| {
            let lnpc = vpath.split('/').find(|p| p.to_ascii_lowercase().ends_with(".lnpc"));
            lnpc.map(stem).unwrap_or_else(|| pack_name(vpath))
        });
    let (adjustments, mut ignored, uses_lut) = luminar_to_adjustments(&json);
    // LUT fournie dans les ressources du preset.
    let lut = files
        .iter()
        .find(|(k, _)| k.starts_with(&format!("{dir}/")) && ext_of(k) == "cube")
        .map(|(_, v)| v.as_slice());
    if uses_lut && lut.is_none() {
        ignored.push("LUT référencée mais absente du preset".into());
    }
    let mut look = Look {
        name,
        category: format!("{category} · Luminar"),
        source: "Luminar".into(),
        adjustments,
        ignored,
        file: PathBuf::new(),
    };
    store.save(&mut look, lut, report)
}

/// Pack des looks créés dans Luminux.
pub const USER_CATEGORY: &str = k!("Mes looks");

/// Enregistre les réglages comme look personnel (sans la géométrie ni le débruitage,
/// propres à chaque photo). Un look du même nom est remplacé.
pub fn save_user_look(name: &str, adj: &Adjustments) -> Result<()> {
    let d = Adjustments::default();
    let adjustments = Adjustments { crop: d.crop, angle: d.angle, nr_luma: d.nr_luma, nr_color: d.nr_color, locals: vec![], sky: d.sky.clone(), lens: d.lens.clone(), retouch: vec![], depth: d.depth.clone(), ..adj.clone() };
    let mut look = Look {
        name: name.trim().to_string(),
        category: USER_CATEGORY.into(),
        source: "Perso".into(),
        adjustments,
        ignored: vec![],
        file: PathBuf::new(),
    };
    if look.name.is_empty() {
        bail!("nom vide");
    }
    Store { root: looks_dir() }.save(&mut look, None, &mut ImportReport::default())
}

/// Supprime un look (et sa LUT si aucun autre look ne l'utilise).
pub fn remove_look(look: &Look) -> Result<()> {
    std::fs::remove_file(&look.file)?;
    if let Some(rel) = &look.adjustments.lut {
        if !list().iter().any(|l| l.adjustments.lut.as_ref() == Some(rel)) {
            let _ = std::fs::remove_file(looks_dir().join("luts").join(rel));
        }
    }
    Ok(())
}

/// Tous les looks installés, triés par pack puis par nom.
pub fn list() -> Vec<Look> {
    let mut out: Vec<Look> = walkdir::WalkDir::new(looks_dir().join("presets"))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let mut l: Look = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            l.file = e.into_path();
            Some(l)
        })
        .collect();
    out.sort_by(|a, b| (a.category != USER_CATEGORY, &a.category, &a.name).cmp(&(b.category != USER_CATEGORY, &b.category, &b.name)));
    out
}

/// Supprime un pack entier (presets et LUT).
pub fn remove_category(category: &str) -> Result<()> {
    for look in list().into_iter().filter(|l| l.category == category) {
        if let Some(rel) = &look.adjustments.lut {
            let _ = std::fs::remove_file(looks_dir().join("luts").join(rel));
        }
        std::fs::remove_file(&look.file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_identity() {
        let mut t = String::from("TITLE \"x\"\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    t += &format!("{r} {g} {b}\n");
                }
            }
        }
        let l = parse_cube(&t).unwrap();
        assert_eq!(l.size, 2);
        assert_eq!(l.data[1], [1.0, 0.0, 0.0]);
        assert_eq!(l.data[2], [0.0, 1.0, 0.0]);
    }

    #[test]
    fn xmp_values() {
        let x = r#"<rdf:Description crs:Exposure2012="+0.15" crs:Contrast2012="+25" crs:WhiteBalance="Custom"
            crs:Temperature="6768" crs:AsShotTemperature="7650" crs:SaturationAdjustmentOrange="+16">
            <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">Autumn Gold</rdf:li></rdf:Alt></crs:Name>
            <crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 20</rdf:li><rdf:li>255, 240</rdf:li></rdf:Seq></crs:ToneCurvePV2012>"#;
        let p = parse_xmp(x);
        assert_eq!(p.name.as_deref(), Some("Autumn Gold"));
        let (a, _) = lr_to_adjustments(&p);
        assert!((a.exposure - 0.15).abs() < 1e-6);
        assert!((a.contrast - 0.25).abs() < 1e-6);
        assert!(a.temperature < 0.0);
        assert!((a.hsl[1].sat - 0.16).abs() < 1e-6);
        assert_eq!(a.curve.len(), 2);
        assert!((a.curve[0][1] - 20.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn lrtemplate_values() {
        let t = "s = {\n title = \"Vieux\",\n settings = {\n Exposure2012 = 0.5,\n Vibrance = 20,\n ToneCurvePV2012 = { 0, 10, 255, 255, },\n },\n}";
        let p = parse_lrtemplate(t);
        assert_eq!(p.name.as_deref(), Some("Vieux"));
        let (a, _) = lr_to_adjustments(&p);
        assert!((a.exposure - 0.5).abs() < 1e-6);
        assert!((a.vibrance - 0.2).abs() < 1e-6);
        assert_eq!(a.curve.len(), 2);
    }
}
