//! Préférences de l'application (`~/.local/share/luminux/settings.json`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Au démarrage sans photo en argument, rouvrir la dernière photo éditée.
    pub start_last: bool,
    pub last_photo: Option<PathBuf>,
    /// Format de l'export par lots : "jpg", "tif" ou "png".
    pub batch_format: String,
    /// Serveur ComfyUI (génération dans une zone peinte) et dernier modèle choisi.
    pub comfy_url: String,
    pub comfy_model: String,
    pub comfy_negative: String,
    /// Avant une génération, décharger les modèles d'Ollama (mémoire vidéo partagée avec ComfyUI),
    /// puis demander à ComfyUI de libérer la sienne une fois l'image reçue.
    pub free_vram: bool,
    pub ollama_url: String,
    /// Traduire l'invite en anglais par Ollama avant l'envoi (SDXL ne comprend bien que l'anglais).
    /// Modèle vide : le plus petit modèle de texte installé.
    pub translate: bool,
    pub translate_model: String,
    /// Modèle d'agrandissement ComfyUI (models/upscale_models).
    pub upscale_model: String,
    /// Modèle de débruitage ComfyUI (models/upscale_models, facteur 1).
    pub denoise_model: String,
    /// Langue de l'interface : "fr", "en" ou un fichier de lang/ ; vide : celle du système.
    pub language: String,
    /// Modèles IA locaux sur la carte graphique (WebGPU par Vulkan), le processeur en secours.
    pub ai_gpu: bool,
    /// Masques IA (sujet, ciel) préparés en arrière-plan pour les photos de la bibliothèque.
    pub ai_background: bool,
    /// Profil ICC de l'écran : "" aucun (sRGB), "auto" celui de colord, sinon le chemin du fichier.
    pub screen_icc: String,
    /// Panneau des réglages (et ses onglets) à gauche de la photo au lieu de la droite.
    pub panel_left: bool,
    /// Disposition « Réglages à part » : réglages (curseurs, courbes) d'un côté de la photo,
    /// barre d'outils et outil choisi de l'autre.
    pub split_layout: bool,
    /// Options d'export (espace couleur, taille, netteté, qualité, métadonnées).
    pub export: crate::export::Options,
    /// Accessibilité : taille de l'interface (1 = 100 %), contraste élevé, grandes zones de clic.
    pub ui_zoom: f32,
    pub high_contrast: bool,
    pub large_targets: bool,
    /// Accessibilité : pas d'animations (sections, panneau, fondu Avant/Après).
    pub reduce_motion: bool,
    /// Dernier dossier où une image de calque a été choisie.
    pub layer_dir: Option<PathBuf>,
    /// Dernier dossier où une photo a été ouverte.
    pub open_dir: Option<PathBuf>,
    /// Génération et agrandissement par le moteur local (stable-diffusion.cpp) au lieu de ComfyUI.
    pub ai_local: bool,
    /// Modèle de génération local : chemin d'un fichier choisi ; vide : le modèle conseillé.
    pub local_model: String,
    /// Modèle d'agrandissement local (`local:…`) ; vide : le modèle conseillé.
    pub local_upscale: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_last: false,
            last_photo: None,
            batch_format: "jpg".into(),
            comfy_url: crate::comfy::DEFAULT_URL.into(),
            comfy_model: String::new(),
            comfy_negative: "blurry, deformed, lowres, text, watermark, white background, halo, glow".into(),
            free_vram: false,
            translate: true,
            translate_model: String::new(),
            upscale_model: String::new(),
            denoise_model: String::new(),
            language: String::new(),
            ai_gpu: true,
            ai_background: true,
            screen_icc: String::new(),
            panel_left: false,
            split_layout: false,
            export: Default::default(),
            ui_zoom: 1.0,
            high_contrast: false,
            large_targets: false,
            reduce_motion: false,
            layer_dir: None,
            open_dir: None,
            ai_local: false,
            local_model: String::new(),
            local_upscale: String::new(),
            ollama_url: "http://127.0.0.1:11434".into(),
        }
    }
}

fn path() -> PathBuf {
    crate::library::data_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Modèle de génération du moteur choisi (vide : aucun choisi dans ComfyUI).
    pub fn gen_model(&self) -> String {
        if self.ai_local { crate::sdlocal::model_id(&self.local_model) } else { self.comfy_model.clone() }
    }

    /// Modèle d'agrandissement du moteur choisi.
    pub fn upscaler(&self) -> String {
        match (self.ai_local, self.local_upscale.is_empty()) {
            (true, true) => format!("{}{}", crate::sdlocal::PREFIX, crate::sdlocal::UPSCALE_MODEL.file),
            (true, false) => self.local_upscale.clone(),
            (false, _) => self.upscale_model.clone(),
        }
    }

    pub fn save(&self) {
        let p = path();
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap());
    }
}
