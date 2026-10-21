//! Choix d'une image avec aperçus (photo à ouvrir, image d'un calque, logo du filigrane) : vignettes
//! des images d'un dossier, sous-dossiers, et le sélecteur de fichiers du système en secours.
//! Le sélecteur du système n'affiche souvent que des icônes génériques, d'où cette fenêtre.

use super::*;
use std::collections::HashSet;

/// Images montrées au plus dans un dossier.
const MAX_FILES: usize = 5000;
/// Côté des vignettes.
const THUMB: u32 = 160;

/// À quoi sert l'image choisie.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum PickFor {
    Open,
    Layer,
    Watermark,
    /// Dossier à ajouter à la bibliothèque : les vignettes aident à le reconnaître.
    Folder,
}

impl PickFor {
    fn exts(self) -> &'static [&'static str] {
        match self {
            PickFor::Open | PickFor::Folder => library::IMAGE_EXTS,
            PickFor::Layer => crate::layers::LAYER_EXTS,
            PickFor::Watermark => &["png", "webp", "jpg", "jpeg"],
        }
    }
    fn title(self) -> String {
        match self {
            PickFor::Open => t!("Ouvrir une photo").to_string(),
            PickFor::Layer => t!("Choisir l'image du calque").to_string(),
            PickFor::Watermark => t!("Choisir le logo").to_string(),
            PickFor::Folder => t!("Ajouter un dossier à la bibliothèque").to_string(),
        }
    }
}

/// Fils de décodage des vignettes.
const WORKERS: usize = 4;

pub(super) struct LayerPicker {
    pub(super) purpose: PickFor,
    dir: PathBuf,
    subdirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
    thumbs: HashMap<PathBuf, egui::TextureHandle>,
    /// Chemin tapé à la main (Entrée pour y aller).
    typed: String,
    /// Vignettes décodées en arrière-plan, à la demande : seules celles qui sont à l'écran sont
    /// demandées, les dernières d'abord (défilement rapide dans un gros dossier).
    rx: Receiver<(PathBuf, egui::ColorImage)>,
    wanted: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    asked: HashSet<PathBuf>,
    /// Faux quand la fenêtre change de dossier ou se ferme : les fils s'arrêtent.
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for LayerPicker {
    fn drop(&mut self) {
        self.alive.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl LayerPicker {
    pub(super) fn open(dir: PathBuf, purpose: PickFor, ctx: &egui::Context) -> Self {
        let mut subdirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
                if hidden {
                    continue;
                }
                if p.is_dir() {
                    subdirs.push(p);
                } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| purpose.exts().contains(&x.to_ascii_lowercase().as_str())) {
                    files.push(p);
                }
            }
        }
        let by_name = |a: &PathBuf, b: &PathBuf| a.file_name().map(|n| n.to_string_lossy().to_lowercase()).cmp(&b.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        subdirs.sort_by(by_name);
        files.sort_by(by_name);
        files.truncate(MAX_FILES);
        let (tx, rx) = channel();
        let wanted: Arc<std::sync::Mutex<Vec<PathBuf>>> = Arc::default();
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        for _ in 0..WORKERS {
            let (tx, wanted, alive, ctx) = (tx.clone(), wanted.clone(), alive.clone(), ctx.clone());
            std::thread::spawn(move || {
                while alive.load(std::sync::atomic::Ordering::Relaxed) {
                    let next = wanted.lock().ok().and_then(|mut w| w.pop());
                    let Some(p) = next else {
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        continue;
                    };
                    let ci = match crate::library::decode_quick(&p, THUMB) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], &rgba)
                        }
                        // Illisible : une case grise, pour ne pas la redemander.
                        Err(_) => egui::ColorImage::new([1, 1], vec![egui::Color32::from_gray(60)]),
                    };
                    if tx.send((p, ci)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        let typed = dir.display().to_string();
        Self { purpose, dir, subdirs, files, thumbs: HashMap::new(), typed, rx, wanted, asked: HashSet::new(), alive }
    }
}

impl App {
    /// Ouvre la fenêtre de choix dans le dernier dossier utilisé, sinon celui de la photo, sinon Images.
    pub(super) fn open_layer_picker(&mut self, ctx: &egui::Context) {
        let dir = self.settings.layer_dir.clone().filter(|d| d.is_dir())
            .or_else(|| self.path.as_ref().and_then(|p| library::source(p).parent().map(Path::to_path_buf)))
            .or_else(dirs::picture_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.layer_picker = Some(LayerPicker::open(dir, PickFor::Layer, ctx));
    }

    /// Ouverture d'une photo : dossier de la photo en cours, sinon le dernier dossier d'ouverture, sinon Images.
    pub(super) fn pick_open(&mut self, ctx: &egui::Context) {
        let dir = self.path.as_ref().and_then(|p| library::source(p).parent().map(Path::to_path_buf))
            .or_else(|| self.settings.open_dir.clone()).filter(|d| d.is_dir())
            .or_else(dirs::picture_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.layer_picker = Some(LayerPicker::open(dir, PickFor::Open, ctx));
    }

    /// Logo du filigrane.
    pub(super) fn pick_logo(&mut self, ctx: &egui::Context) {
        let dir = self.settings.layer_dir.clone().filter(|d| d.is_dir()).or_else(dirs::picture_dir).or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("/"));
        self.layer_picker = Some(LayerPicker::open(dir, PickFor::Watermark, ctx));
    }

    /// Raccourcis : dossiers personnels, dossiers de la bibliothèque, disques montés.
    fn picker_places(&self) -> Vec<(String, PathBuf)> {
        let mut v: Vec<(String, PathBuf)> = Vec::new();
        let mut add = |name: String, p: Option<PathBuf>| {
            if let Some(p) = p.filter(|p| p.is_dir()) {
                if !v.iter().any(|(_, q)| *q == p) {
                    v.push((name, p));
                }
            }
        };
        add(t!("Dossier personnel").to_string(), dirs::home_dir());
        add(t!("Images").to_string(), dirs::picture_dir());
        add(t!("Bureau").to_string(), dirs::desktop_dir());
        add(t!("Téléchargements").to_string(), dirs::download_dir());
        for f in &self.folders {
            let p = PathBuf::from(f);
            add(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.clone()), Some(p));
        }
        // Disques et clés USB montés (/media/utilisateur/…, /run/media/utilisateur/…), partages dans /mnt.
        let user = std::env::var("USER").unwrap_or_default();
        for base in [PathBuf::from("/media").join(&user), PathBuf::from("/run/media").join(&user), PathBuf::from("/mnt")] {
            if let Ok(rd) = std::fs::read_dir(&base) {
                for e in rd.flatten() {
                    add(e.file_name().to_string_lossy().into_owned(), Some(e.path()));
                }
            }
        }
        v
    }

    pub(super) fn layer_picker_window(&mut self, ctx: &egui::Context) {
        if self.layer_picker.is_none() {
            return;
        }
        let places = self.picker_places();
        let Some(pk) = &mut self.layer_picker else { return };
        while let Ok((p, img)) = pk.rx.try_recv() {
            let tex = ctx.load_texture(p.to_string_lossy(), img, egui::TextureOptions::LINEAR);
            pk.thumbs.insert(p, tex);
        }
        let mut open = true;
        let mut go: Option<PathBuf> = None;
        let mut chosen: Option<PathBuf> = None;
        let mut system = false;
        let mut take_dir = false;
        let mut need: Vec<PathBuf> = Vec::new();
        let purpose = pk.purpose;
        egui::Window::new(purpose.title()).id(egui::Id::unique("image_picker")).open(&mut open).collapsible(false).default_size(vec2(720.0, 520.0)).show(ctx, |ui| {
            if purpose == PickFor::Folder {
                let name = pk.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| pk.dir.display().to_string());
                if ui.add(egui::Button::new(egui::RichText::new(tf!("Ajouter « {0} »", name)).strong()).fill(palette::ACCENT)).clicked() {
                    take_dir = true;
                }
            }
            // Raccourcis, puis le chemin en boutons (chaque élément y ramène) et un champ pour le taper.
            ui.horizontal_wrapped(|ui| {
                for (name, p) in &places {
                    if ui.selectable_label(*p == pk.dir, name).on_hover_text(p.display().to_string()).clicked() {
                        go = Some(p.clone());
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(pk.dir.parent().is_some(), egui::Button::new(t!("Dossier parent"))).clicked() {
                    go = pk.dir.parent().map(Path::to_path_buf);
                }
                let parts: Vec<PathBuf> = pk.dir.ancestors().map(Path::to_path_buf).collect();
                for p in parts.iter().rev() {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
                    if ui.selectable_label(*p == pk.dir, name).clicked() {
                        go = Some(p.clone());
                    }
                }
            });
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut pk.typed).desired_width(ui.available_width() - 190.0).hint_text(t!("Chemin d'un dossier")));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let p = PathBuf::from(pk.typed.trim());
                    let p = if let Some(rest) = pk.typed.trim().strip_prefix("~/") { dirs::home_dir().map_or(p.clone(), |h| h.join(rest)) } else { p };
                    if p.is_dir() {
                        go = Some(p);
                    }
                }
                if ui.button(t!("Sélecteur du système…")).on_hover_text(t!("La fenêtre de fichiers habituelle, sans aperçu")).clicked() {
                    system = true;
                }
            });
            if !pk.subdirs.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    // Pas d'icône de dossier : la police de l'interface n'a pas ce symbole.
                    ui.label(t!("Dossiers :"));
                    for d in &pk.subdirs {
                        let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        if ui.button(format!("{name}/")).clicked() {
                            go = Some(d.clone());
                        }
                    }
                });
            }
            ui.separator();
            if pk.files.is_empty() {
                ui.label(t!("Aucune image dans ce dossier."));
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for f in &pk.files {
                        let (rect, resp) = ui.allocate_exact_size(vec2(132.0, 150.0), Sense::click());
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        if !pk.thumbs.contains_key(f) {
                            need.push(f.clone());
                        }
                        let p = ui.painter_at(rect);
                        p.rect_filled(rect, 6.0, if resp.hovered() { palette::ITEM_HOVER } else { palette::ITEM });
                        let area = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(120.0, 120.0));
                        match pk.thumbs.get(f) {
                            Some(t) => {
                                let s = t.size_vec2();
                                let k = (area.width() / s.x).min(area.height() / s.y);
                                let r = Rect::from_center_size(area.center(), s * k);
                                // Damier sous les images transparentes.
                                p.rect_filled(r, 0.0, Color32::from_gray(90));
                                p.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            None => {
                                p.text(area.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(18.0), Color32::GRAY);
                            }
                        }
                        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let galley = p.layout(name.clone(), egui::FontId::proportional(11.0), Color32::from_gray(210), 120.0);
                        let pos = pos2(rect.center().x - galley.size().x.min(120.0) / 2.0, rect.max.y - 20.0);
                        p.with_clip_rect(rect.shrink(4.0)).galley(pos, galley, Color32::WHITE);
                        if resp.on_hover_text(&name).clicked() && purpose != PickFor::Folder {
                            chosen = Some(f.clone());
                        }
                    }
                });
            });
        });
        // Vignettes à l'écran pas encore demandées : la première visible sera décodée en premier.
        if let Some(pk) = &mut self.layer_picker {
            let fresh: Vec<PathBuf> = need.into_iter().filter(|f| pk.asked.insert(f.clone())).collect();
            if !fresh.is_empty() {
                if let Ok(mut w) = pk.wanted.lock() {
                    w.extend(fresh.into_iter().rev());
                }
            }
        }
        if let Some(d) = go {
            self.layer_picker = Some(LayerPicker::open(d, purpose, ctx));
        }
        if system {
            self.layer_picker = None;
            let wrap = match purpose {
                PickFor::Open => Picked::Open,
                PickFor::Layer => Picked::LayerImage,
                PickFor::Watermark => Picked::WatermarkImage,
                PickFor::Folder => Picked::Folder,
            };
            if purpose == PickFor::Folder {
                self.dialog(ctx, move || rfd::FileDialog::new().pick_folder().map(wrap));
            } else {
                self.dialog(ctx, move || rfd::FileDialog::new().add_filter(t!("Images"), purpose.exts()).pick_file().map(wrap));
            }
        }
        if take_dir {
            if let Some(pk) = self.layer_picker.take() {
                self.folder_picked(pk.dir.clone(), ctx);
            }
        }
        if let Some(f) = chosen {
            self.layer_picker = None;
            match purpose {
                PickFor::Open => {
                    self.settings.open_dir = f.parent().map(Path::to_path_buf);
                    self.settings.save();
                    self.open(f, ctx);
                }
                PickFor::Layer => self.add_layer(f, ctx),
                PickFor::Watermark => self.picked_logo(f),
                PickFor::Folder => {}
            }
        }
        if !open {
            self.layer_picker = None;
        }
    }

    /// Nouveau calque sur l'image `f` ; son dossier devient le point de départ du prochain choix.
    pub(super) fn add_layer(&mut self, f: PathBuf, ctx: &egui::Context) {
        if self.adj.layers.len() >= crate::adjust::MAX_LAYERS {
            return;
        }
        self.settings.layer_dir = f.parent().map(Path::to_path_buf);
        self.settings.save();
        self.adj.layers.push(crate::adjust::Layer { image: f.display().to_string(), ..Default::default() });
        self.layer_sel = self.adj.layers.len() - 1;
        self.layer_tool = 0;
        history::mark(ctx, t!("Calque ajouté").to_string());
    }
}
