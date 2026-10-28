//! Choix du moteur de génération (ComfyUI ou moteur local stable-diffusion.cpp) et installation du
//! moteur local depuis l'application.

use super::*;
use crate::sdlocal::{self, Asset};

/// Téléchargement en cours d'éléments du moteur local.
pub(super) struct LocalInstall {
    assets: Vec<&'static Asset>,
    /// Élément en cours (index dans `assets`).
    step: Arc<std::sync::atomic::AtomicUsize>,
    progress: Arc<crate::ai::Progress>,
    rx: Receiver<anyhow::Result<()>>,
}

impl App {
    /// Le modèle de génération du moteur choisi est-il utilisable ?
    pub(super) fn gen_ready(&self) -> bool {
        if self.settings.ai_local {
            sdlocal::engine_ready() && sdlocal::model_file(&self.settings.gen_model()).is_file()
        } else {
            !self.settings.comfy_model.is_empty()
        }
    }

    pub(super) fn upscale_ready(&self) -> bool {
        if self.settings.ai_local {
            sdlocal::engine_ready() && sdlocal::model_file(&self.settings.upscaler()).is_file()
        } else {
            self.comfy_upscalers.iter().any(|m| !crate::comfy::is_denoiser(m))
        }
    }

    /// Nom du moteur, pour les messages (« Calcul dans … »).
    pub(super) fn engine_name(&self) -> String {
        if self.settings.ai_local { t!("le moteur local").to_string() } else { "ComfyUI".to_string() }
    }

    /// En tête des outils de génération : connexion à ComfyUI ou état du moteur local.
    pub(super) fn engine_ui(&mut self, ui: &mut egui::Ui, upscale: bool) {
        if !self.settings.ai_local {
            self.comfy_connect_ui(ui);
            if !upscale {
                self.checkpoint_ui(ui);
            }
            return;
        }
        let model = if upscale { self.settings.upscaler() } else { self.settings.gen_model() };
        let missing = self.local_missing(upscale);
        if missing.is_empty() {
            ui.small(tf!("Moteur local, modèle : {0}", sdlocal::model_name(&model)));
        } else {
            self.local_install_ui(ui, missing);
        }
        ui.small(t!("Moteur choisi dans Préférences (Ctrl+,)."));
    }

    /// Éléments à télécharger pour la génération ou l'agrandissement locaux. Un modèle choisi à la main
    /// et absent n'est pas proposé au téléchargement.
    fn local_missing(&self, upscale: bool) -> Vec<&'static Asset> {
        let mut v = Vec::new();
        if !sdlocal::installed(&sdlocal::ENGINE) {
            v.push(&sdlocal::ENGINE);
        }
        let (asset, chosen) = if upscale { (&sdlocal::UPSCALE_MODEL, &self.settings.local_upscale) } else { (&sdlocal::GEN_MODEL, &self.settings.local_model) };
        if chosen.is_empty() && !sdlocal::installed(asset) {
            v.push(asset);
        }
        if !upscale && *chosen == sdlocal::GEN_MODEL_XL.file && !sdlocal::installed(&sdlocal::GEN_MODEL_XL) {
            v.push(&sdlocal::GEN_MODEL_XL);
        }
        v
    }

    /// Bouton de téléchargement, ou progression s'il est en cours.
    fn local_install_ui(&mut self, ui: &mut egui::Ui, missing: Vec<&'static Asset>) {
        if let Some(job) = &self.local_install {
            match job.rx.try_recv() {
                Ok(r) => {
                    self.local_msg = match r {
                        Ok(()) => t!("Moteur local prêt.").to_string(),
                        Err(e) => format!("{e:#}"),
                    };
                    self.local_install = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.local_install = None,
                Err(_) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(250)),
            }
        }
        if let Some(job) = &self.local_install {
            let i = job.step.load(std::sync::atomic::Ordering::Relaxed).min(job.assets.len() - 1);
            let (done, total) = (job.progress.done.load(std::sync::atomic::Ordering::Relaxed), job.progress.total.load(std::sync::atomic::Ordering::Relaxed));
            let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
            ui.label(tf!("Téléchargement : {0} ({1}/{2})", job.assets[i].file, i + 1, job.assets.len()));
            ui.add(egui::ProgressBar::new(frac).show_percentage());
            return;
        }
        let mb: u32 = missing.iter().map(|a| a.mb).sum();
        let size = if mb >= 1000 { format!("{:.1} Go", mb as f32 / 1000.0).replace('.', ",") } else { format!("{mb} Mo") };
        ui.label(missing.iter().map(|a| a.file).collect::<Vec<_>>().join(", "));
        if ui.button(tf!("Télécharger ({0})", size)).on_hover_text(sdlocal::dir().display().to_string()).clicked() {
            let (tx, rx) = channel();
            let step = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let progress = Arc::new(crate::ai::Progress::default());
            let (s, p, list, ctx) = (step.clone(), progress.clone(), missing.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                // Un échec n'arrête pas les éléments suivants ; les erreurs sont réunies à la fin.
                let mut errors = Vec::new();
                for (i, a) in list.iter().enumerate() {
                    s.store(i, std::sync::atomic::Ordering::Relaxed);
                    p.done.store(0, std::sync::atomic::Ordering::Relaxed);
                    p.total.store(0, std::sync::atomic::Ordering::Relaxed);
                    if let Err(e) = sdlocal::install(a, &p) {
                        errors.push(format!("{} : {e:#}", a.file));
                    }
                }
                let r = if errors.is_empty() { Ok(()) } else { Err(anyhow::anyhow!(errors.join("\n"))) };
                let _ = tx.send(r);
                ctx.request_repaint();
            });
            self.local_msg.clear();
            self.local_install = Some(LocalInstall { assets: missing, step, progress, rx });
        }
        if !self.local_msg.is_empty() {
            ui.small(&self.local_msg);
        }
    }

    /// Préférences : moteur de génération (outils Générer, Agrandir le cadre, Agrandir l'image).
    pub(super) fn engine_prefs_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(egui::RichText::new(t!("Moteur de génération IA")).strong());
        if segmented(ui, &mut self.settings.ai_local, &[(false, t!("ComfyUI (serveur)")), (true, t!("Local (ce PC)"))]) {
            self.settings.save();
        }
        if !self.settings.ai_local {
            ui.small(t!("Calcul sur un serveur ComfyUI, sur ce PC ou une autre machine du réseau : modèles au choix (SDXL…)."));
            self.comfy_connect_ui(ui);
            return;
        }
        ui.small(t!("stable-diffusion.cpp, téléchargé par Luminux : sans serveur ni Python, carte graphique par Vulkan (AMD, NVIDIA, Intel), processeur en secours. Le modèle conseillé (SD 1.5 inpainting) tourne avec 4 Go de mémoire vidéo."));
        let mut missing = self.local_missing(true);
        for a in self.local_missing(false) {
            if !missing.iter().any(|m| std::ptr::eq(*m, a)) {
                missing.push(a);
            }
        }
        if missing.is_empty() && self.local_install.is_none() {
            ui.small(t!("Moteur et modèles installés."));
        } else {
            self.local_install_ui(ui, missing);
        }
        // Choix rapide entre les deux modèles proposés au téléchargement.
        let mut pick = if self.settings.local_model.is_empty() { 0u8 } else if self.settings.local_model == sdlocal::GEN_MODEL_XL.file { 1 } else { 2 };
        let before = pick;
        ui.horizontal(|ui| {
            ui.label(t!("Modèle"));
            segmented(ui, &mut pick, &[(0, t!("SD 1.5 (léger, 2,1 Go)")), (1, t!("SDXL (meilleur, 6,9 Go)"))]);
        });
        if pick != before {
            self.settings.local_model = if pick == 1 { sdlocal::GEN_MODEL_XL.file.to_string() } else { String::new() };
            self.settings.save();
        }
        if pick == 1 {
            ui.small(t!("SDXL : 8 Go de mémoire vidéo conseillés, génération plus lente. Si le téléchargement échoue, « Autre modèle… » accepte le même fichier copié depuis un serveur ComfyUI (dossier models/checkpoints)."));
        }
        ui.label(tf!("Génération : {0}", sdlocal::model_name(&self.settings.gen_model())));
        ui.horizontal(|ui| {
            if ui.button(t!("Autre modèle…")).on_hover_text(t!("Fichier .safetensors, .ckpt ou .gguf (SD 1.5, SDXL, de préférence « inpainting »)")).clicked() {
                self.dialog(ctx, || rfd::FileDialog::new().add_filter(t!("Modèles"), &["safetensors", "ckpt", "gguf"]).pick_file().map(Picked::LocalModel));
            }
            if ui.add_enabled(!self.settings.local_model.is_empty(), egui::Button::new(t!("Modèle conseillé"))).clicked() {
                self.settings.local_model.clear();
                self.settings.save();
            }
        });
        let ups = sdlocal::upscalers();
        if ups.len() > 1 {
            ui.label(t!("Agrandissement"));
            let mut m = self.settings.upscaler();
            let items: Vec<(String, String)> = ups.iter().map(|u| (u.clone(), sdlocal::model_name(u))).collect();
            choice_list(ui, "local_upscale", &mut m, &items);
            if m != self.settings.upscaler() {
                self.settings.local_upscale = m;
                self.settings.save();
            }
        }
        ui.small(tf!("Autres modèles d'agrandissement (.pth, type ESRGAN) : à déposer dans {0}", sdlocal::models_dir().display()));
    }
}
