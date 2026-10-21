//! Langue de l'interface. Les textes du code sont en français et servent de clés : une langue est un fichier
//! JSON `{ "texte français": "traduction" }`, avec `"_name"` pour son nom. L'anglais est intégré ; les autres
//! langues vont dans `~/.local/share/luminux/lang/<code>.json`, importées ou traduites par Ollama.
//!
//! `t!("…")` traduit un texte, `tf!("… {n} …", n)` un texte à trous (valeurs dans l'ordre de leur première
//! apparition dans le texte français), `k!("…")` marque un texte traduit plus loin (constantes).
//! « texte|contexte » sépare deux sens d'un même mot (Annuler|défaire et Annuler).

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::RwLock;

pub const ENGLISH: &str = include_str!("../lang/en.json");

/// Traductions de la langue en cours (vide : français). Les valeurs sont gardées pour toute la session.
static CURRENT: RwLock<Option<HashMap<String, &'static str>>> = RwLock::new(None);

#[macro_export]
macro_rules! t {
    ($s:literal) => {
        $crate::i18n::tr($s)
    };
}

#[macro_export]
macro_rules! tf {
    ($s:literal $(, $a:expr)* $(,)?) => {
        $crate::i18n::fill($s, $crate::i18n::tr($s), &[$(&$a as &dyn std::fmt::Display),*])
    };
}

#[macro_export]
macro_rules! k {
    ($s:literal) => {
        $s
    };
}

/// Traduction de `s`. Une clé « texte|contexte » distingue deux sens d'un même mot ; sans traduction,
/// seul « texte » est affiché.
pub fn tr<'a>(s: &'a str) -> &'a str {
    match CURRENT.read().ok().as_deref().and_then(|m| m.as_ref()?.get(s).copied()) {
        Some(v) => v,
        None => s.split('|').next().unwrap_or(s),
    }
}

/// Trous `{nom}` ou `{nom:spec}` du texte, dans l'ordre de leur première apparition.
fn holes(s: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        let name = rest[i + 1..i + j].split(':').next().unwrap_or("");
        if !out.contains(&name) {
            out.push(name);
        }
        rest = &rest[i + j + 1..];
    }
    out
}

/// Remplit les trous de `text` (traduction de `key`) avec `args`, rangés selon l'ordre des trous de `key`.
pub fn fill(key: &str, text: &str, args: &[&dyn Display]) -> String {
    let names = holes(key);
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out += &rest[..i];
        let Some(j) = rest[i..].find('}') else {
            rest = &rest[i..];
            break;
        };
        let inner = &rest[i + 1..i + j];
        let (name, spec) = inner.split_once(':').unwrap_or((inner, ""));
        match names.iter().position(|n| *n == name).and_then(|k| args.get(k)) {
            Some(v) if spec == "#" => out += &format!("{v:#}"),
            Some(v) if spec.starts_with('.') => match spec[1..].parse::<usize>() {
                Ok(p) => out += &format!("{v:.p$}"),
                Err(_) => out += &v.to_string(),
            },
            Some(v) => out += &v.to_string(),
            None => out += &rest[i..i + j + 1],
        }
        rest = &rest[i + j + 1..];
    }
    out + rest
}

fn lang_dir() -> PathBuf {
    crate::library::data_dir().join("lang")
}

fn parse(json: &str) -> Result<HashMap<String, String>> {
    let v: serde_json::Value = serde_json::from_str(json).context("fichier de langue illisible (JSON attendu)")?;
    let obj = v.as_object().context("fichier de langue : un objet { \"texte\": \"traduction\" } est attendu")?;
    Ok(obj.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
}

/// Langues proposées : (code, nom). Le français et l'anglais d'abord, puis les fichiers installés.
pub fn languages() -> Vec<(String, String)> {
    let mut out = vec![("fr".to_string(), "Français".to_string()), ("en".to_string(), "English".to_string())];
    let mut extra: Vec<(String, String)> = std::fs::read_dir(lang_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()? != "json" {
                return None;
            }
            let code = p.file_stem()?.to_str()?.to_string();
            if code == "fr" || code == "en" {
                return None;
            }
            let name = std::fs::read_to_string(&p).ok().and_then(|s| parse(&s).ok()).and_then(|m| m.get("_name").cloned()).unwrap_or_else(|| code.clone());
            Some((code, name))
        })
        .collect();
    extra.sort_by(|a, b| a.1.cmp(&b.1));
    out.extend(extra);
    out
}

/// Langue par défaut : le français si le système est en français, l'anglais sinon.
pub fn system_language() -> String {
    let lang = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty())).unwrap_or_default();
    if lang.starts_with("fr") { "fr".into() } else { "en".into() }
}

/// Change la langue de l'interface (code vide : langue du système).
pub fn set_language(code: &str) -> Result<()> {
    let code = if code.is_empty() { system_language() } else { code.to_string() };
    let map = match code.as_str() {
        "fr" => None,
        "en" => Some(parse(ENGLISH)?),
        c => Some(parse(&std::fs::read_to_string(lang_dir().join(format!("{c}.json"))).with_context(|| format!("langue « {c} » introuvable"))?)?),
    };
    let map = map.map(|m| m.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k, &*Box::leak(v.into_boxed_str()))).collect());
    *CURRENT.write().unwrap() = map;
    Ok(())
}

/// Tous les textes de l'interface (clés du fichier anglais).
pub fn keys() -> Vec<String> {
    parse(ENGLISH).map(|m| m.into_keys().filter(|k| k != "_name").collect()).unwrap_or_default()
}

/// Code de fichier tiré d'un nom de langue (« Deutsch » → « deutsch »).
fn code_for(name: &str) -> String {
    let c: String = name.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let c = c.trim_matches('-').to_string();
    if c.is_empty() || c == "fr" || c == "en" { format!("{c}-perso") } else { c }
}

/// Installe un fichier de langue ; renvoie son code.
pub fn import(path: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    let map = parse(&text)?;
    if map.len() < 2 {
        bail!("fichier de langue vide");
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("langue");
    let code = code_for(map.get("_name").map_or(stem, |s| s.as_str()));
    std::fs::create_dir_all(lang_dir())?;
    std::fs::write(lang_dir().join(format!("{code}.json")), text)?;
    Ok(code)
}

/// Modèle à remplir : l'anglais, pour traduire à la main.
pub fn export_template(path: &std::path::Path) -> Result<()> {
    std::fs::write(path, ENGLISH).with_context(|| format!("écriture de {}", path.display()))
}

/// Traduit l'interface dans `name` (« Deutsch », « Español »…) avec Ollama, par lots ; renvoie le code.
/// Le français est la source et l'anglais sert de repère au modèle.
pub fn ai_translate(name: &str, url: &str, model: &str, progress: impl Fn(usize, usize)) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("indique la langue voulue");
    }
    let en = parse(ENGLISH)?;
    let mut keys = keys();
    keys.sort();
    let mut out: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    out.insert("_name".into(), name.into());
    let chunks: Vec<&[String]> = keys.chunks(25).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        progress(i, chunks.len());
        let src: serde_json::Map<String, serde_json::Value> = chunk.iter().map(|k| (k.clone(), en.get(k).cloned().unwrap_or_default().into())).collect();
        let prompt = format!(
            "You translate the user interface of a photo editor into {name}. Below is a JSON object: each key is the \
             original French text, each value its English translation. Answer with a JSON object with exactly the same \
             keys, whose values are the {name} translations. Keep placeholders such as {{n}} or {{e:#}} unchanged, keep \
             keyboard shortcuts (Ctrl+O, Alt…), file extensions and product names (ComfyUI, Ollama, Lensfun, LUT) as they \
             are, and keep the texts short.\n\n{}",
            serde_json::Value::Object(src)
        );
        let mut got = crate::comfy::ollama_json(url, model, &prompt);
        if got.is_err() {
            got = crate::comfy::ollama_json(url, model, &prompt);
        }
        let got = got.with_context(|| format!("lot {} sur {}", i + 1, chunks.len()))?;
        for k in chunk.iter() {
            // Texte manquant ou trous abîmés : on garde l'anglais.
            let v = got.get(k).and_then(|v| v.as_str()).filter(|v| holes(v).iter().all(|h| holes(k).contains(h)) && holes(k).iter().all(|h| v.contains(&format!("{{{h}")))).map(str::to_string);
            out.insert(k.clone(), v.unwrap_or_else(|| en.get(k).cloned().unwrap_or_default()).into());
        }
    }
    progress(chunks.len(), chunks.len());
    let code = code_for(name);
    std::fs::create_dir_all(lang_dir())?;
    std::fs::write(lang_dir().join(format!("{code}.json")), serde_json::to_string_pretty(&serde_json::Value::Object(out))?)?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_follows_names_of_the_french_text() {
        assert_eq!(fill("Tuile {k} sur {n}", "{n} tiles, now {k}", &[&2, &5]), "5 tiles, now 2");
        assert_eq!(fill("{x:.1} Go", "{x:.1} GB", &[&1.25f64]), "1.2 GB");
    }

    /// Traduction par un faux Ollama (tools/ : réponse « DE » + anglais) : cargo test -- --ignored.
    #[test]
    #[ignore]
    fn ai_translation_with_mock_ollama() {
        let code = ai_translate("Deutsch", "http://127.0.0.1:11435", "", |_, _| {}).unwrap();
        assert_eq!(code, "deutsch");
        set_language(&code).unwrap();
        assert_eq!(tr("Ouvrir"), "DE Open");
        assert_eq!(fill("Tuile {0} sur {total}", tr("Débruitage dans ComfyUI : tuile {0} sur {total}"), &[&1, &4]), "DE Denoising in ComfyUI: tile 1 of 4");
        set_language("fr").unwrap();
        assert_eq!(tr("Ouvrir"), "Ouvrir");
    }

    /// Chaque texte marqué dans le code a sa traduction anglaise, avec les mêmes trous.
    #[test]
    fn english_covers_every_text() {
        let en = parse(ENGLISH).unwrap();
        let re_lit = |s: &str, mac: &str| -> Vec<String> {
            let mut out = Vec::new();
            let pat = format!("{mac}!(\"");
            let mut rest = s;
            while let Some(i) = rest.find(&pat) {
                let body = &rest[i + pat.len()..];
                // « format!( » finit aussi par « t!( » : on veut le nom entier.
                if rest[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    rest = body;
                    continue;
                }
                let mut lit = String::new();
                let mut chars = body.chars();
                let len_before = chars.as_str().len();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some('n') => lit.push('\n'),
                            Some(o) => lit.push(o),
                            None => {}
                        },
                        '"' => break,
                        c => lit.push(c),
                    }
                }
                out.push(lit);
                rest = &body[len_before - chars.as_str().len()..];
            }
            out
        };
        let mut missing = Vec::new();
        let mut used = std::collections::HashSet::new();
        for f in walkdir::WalkDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).into_iter().flatten().filter(|f| f.file_type().is_file()) {
            let src = std::fs::read_to_string(f.path()).unwrap();
            if f.path().ends_with("i18n.rs") {
                continue;
            }
            for mac in ["t", "tf", "k"] {
                for key in re_lit(&src, mac) {
                    match en.get(&key) {
                        None => missing.push(key.clone()),
                        Some(v) => assert_eq!(holes(&key).len(), holes(v).len(), "trous différents : {key:?} → {v:?}"),
                    }
                    used.insert(key);
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "sans traduction anglaise : {missing:#?}");
        let unused: Vec<_> = en.keys().filter(|k| *k != "_name" && !used.contains(*k)).collect();
        assert!(unused.is_empty(), "traductions inutilisées : {unused:#?}");
    }
}
