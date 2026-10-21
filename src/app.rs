//! Interface : bibliothèque (grille de vignettes), et éditeur avec vue de l'image
//! (zoom, déplacement), panneau de réglages, historique, fichier `.luminux` et export.

use crate::adjust::{Adjustments, AiKind, Retouch, BrushStroke, HSL_BANDS, Local, LocalAdj, MAX_LOCALS, Shape, crop_inside, eval_curve, fit_crop};
use crate::gpu::{Developer, ImageCallback};
use crate::image_io;
use crate::library::{self, Catalogue, Filter, Photo, ScanMsg, Sort, Thumbnailer};
use crate::batch::BatchMsg;
use crate::looks::{self, ImportReport, Look};
use crate::settings::Settings;
use crate::ai::{self, AiMask, Model};
use crate::sky;
use crate::export;
use crate::history::{self, Snapshot};
use std::sync::Arc;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use eframe::egui_wgpu;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

mod menus;
mod engine;
mod picker;

/// Couleurs du thème anthracite.
mod palette {
    use eframe::egui::Color32;
    pub const VIEW: Color32 = Color32::from_rgb(20, 21, 24);
    pub const PANEL: Color32 = Color32::from_rgb(32, 33, 37);
    pub const WINDOW: Color32 = Color32::from_rgb(38, 39, 44);
    pub const FIELD: Color32 = Color32::from_rgb(24, 25, 28);
    pub const ITEM: Color32 = Color32::from_rgb(46, 48, 54);
    pub const ITEM_HOVER: Color32 = Color32::from_rgb(56, 58, 66);
    pub const ITEM_ON: Color32 = Color32::from_rgb(66, 69, 78);
    pub const LINE: Color32 = Color32::from_rgb(48, 50, 56);
    pub const LINE_STRONG: Color32 = Color32::from_rgb(90, 94, 104);
    pub const ACCENT: Color32 = Color32::from_rgb(58, 130, 246);
    /// Repère d'un réglage modifié.
    pub const CHANGED: Color32 = Color32::from_rgb(240, 175, 50);
}
use menus::apply_access;

/// Réglages du pinceau des masques.
struct Brush {
    /// Rayon en fraction de la largeur de l'image.
    radius: f32,
    hardness: f32,
    flow: f32,
    erase: bool,
}

/// Poignée d'un dégradé saisie à la souris.
#[derive(Clone, Copy, PartialEq)]
enum Handle {
    LinA,
    LinB,
    Center,
    RadX,
    RadY,
}

/// Clé de rasterisation d'un masque : sa forme, l'inversion et l'opacité.
type MaskKey = (Shape, bool, f32, bool);

/// Ce que l'utilisateur fait glisser en mode recadrage.
#[derive(Clone, Copy)]
enum CropDrag {
    Move,
    /// Coin : 0 haut-gauche, 1 haut-droit, 2 bas-gauche, 3 bas-droit.
    Corner(usize),
}

const ASPECTS: &[(&str, f32)] = &[(k!("Libre"), 0.0), (k!("Original"), -1.0), ("1:1", 1.0), ("4:3", 4.0 / 3.0), ("3:2", 1.5), ("16:9", 16.0 / 9.0), ("5:4", 1.25)];

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Library,
    Editor,
}

enum Tex {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

#[derive(Serialize, Deserialize)]
struct Sidecar {
    version: u32,
    adjustments: Adjustments,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    snapshots: Vec<Snapshot>,
}

pub fn sidecar_path(photo: &Path) -> PathBuf {
    let mut s = photo.as_os_str().to_owned();
    s.push(".luminux");
    PathBuf::from(s)
}

pub fn load_sidecar(photo: &Path) -> Option<Adjustments> {
    let txt = std::fs::read_to_string(sidecar_path(photo)).ok()?;
    serde_json::from_str::<Sidecar>(&txt).ok().map(|s| s.adjustments)
}

/// Écrit les réglages en gardant les versions nommées déjà enregistrées.
pub fn save_sidecar(photo: &Path, adj: &Adjustments) -> std::io::Result<()> {
    save_snapshots(photo, adj, &load_snapshots(photo))
}

pub fn load_snapshots(photo: &Path) -> Vec<Snapshot> {
    let Ok(txt) = std::fs::read_to_string(sidecar_path(photo)) else { return Vec::new() };
    serde_json::from_str::<Sidecar>(&txt).map(|s| s.snapshots).unwrap_or_default()
}

pub fn save_snapshots(photo: &Path, adj: &Adjustments, snapshots: &[Snapshot]) -> std::io::Result<()> {
    let txt = serde_json::to_string_pretty(&Sidecar { version: 1, adjustments: adj.clone(), snapshots: snapshots.to_vec() }).unwrap();
    std::fs::write(sidecar_path(photo), txt)
}

/// Photo prête à envoyer au GPU : chemin, taille, pixels f16, durée du chargement.
type Loaded = anyhow::Result<(PathBuf, u32, u32, Vec<half::f16>, std::time::Duration)>;

/// Photo suivante déjà décodée (préchargement), avec la date du fichier pour la valider.
struct Preloaded {
    path: PathBuf,
    mtime: Option<std::time::SystemTime>,
    w: u32,
    h: u32,
    half: Vec<half::f16>,
}

fn file_mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(library::source(p)).and_then(|m| m.modified()).ok()
}

/// Nombre maximal de photos préparées en arrière-plan pour un dossier.
const BG_LIMIT: usize = 500;

/// Masques IA calculés en arrière-plan : progression, pause pendant le travail interactif, arrêt.
struct BgMasks {
    done: Arc<std::sync::atomic::AtomicUsize>,
    total: usize,
    pause: Arc<std::sync::atomic::AtomicBool>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// Nombre avec séparateur des milliers (35 201).
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// Un dossier de l'arborescence (`dirs` : chemins et nombres de photos, triés) et ses sous-dossiers,
/// repliables. Renvoie la réponse du libellé (menu contextuel, infobulle).
fn dir_node(ui: &mut egui::Ui, dirs: &[(String, usize)], dir: &str, label: &str, current: Option<&str>, pick: &mut Option<Option<String>>) -> egui::Response {
    let n = dirs.iter().find(|d| d.0 == dir.trim_end_matches('/')).map_or(0, |d| d.1);
    let kids: Vec<&(String, usize)> = dirs.iter().filter(|d| Path::new(&d.0).parent().is_some_and(|p| p.as_os_str() == dir.trim_end_matches('/'))).collect();
    let text = format!("{label} ({})", thousands(n));
    let mut resp = None;
    if kids.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            resp = Some(ui.selectable_label(current == Some(dir), text));
        });
    } else {
        let id = ui.make_persistent_id(("dossier", dir));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| resp = Some(ui.selectable_label(current == Some(dir), text)))
            .body(|ui| {
                for (k, _) in kids {
                    let name = Path::new(k).file_name().map_or(k.clone(), |s| s.to_string_lossy().to_string());
                    dir_node(ui, dirs, k, &name, current, pick);
                }
            });
    }
    let r = resp.unwrap();
    if r.clicked() {
        *pick = Some(Some(dir.to_string()));
    }
    r
}

/// Rayon minimal du pinceau (fraction de la largeur) : 0,1 % de diamètre, 6 px sur une photo de 6000 px.
const BRUSH_MIN: f32 = 0.0005;

/// Action du menu contextuel d'une vignette.
#[derive(Clone, Copy)]
enum LibAction {
    Copy,
    DeleteCopy,
    Trash,
    ShowFile,
}

/// Résultat d'une boîte de dialogue, ouverte dans son propre thread pour ne pas figer la fenêtre.
enum Picked {
    Open(PathBuf),
    Folder(PathBuf),
    Export(PathBuf),
    /// PNG détouré (fond transparent).
    ExportCutout(PathBuf),
    Import(Vec<PathBuf>),
    BatchExport(PathBuf),
    Skies(Vec<PathBuf>),
    /// Fichier de langue à installer, ou modèle à enregistrer pour traduire à la main.
    LangImport(PathBuf),
    LangTemplate(PathBuf),
    /// Profil ICC de l'écran.
    ScreenIcc(PathBuf),
    /// Logo du filigrane.
    WatermarkImage(PathBuf),
    /// Image d'un nouveau calque.
    LayerImage(PathBuf),
    /// Dossier à montrer dans le choix d'image de calque.
    /// Modèle de génération du moteur local.
    LocalModel(PathBuf),
}

/// Fenêtre « Nouveau document ».
struct NewDoc {
    w: u32,
    h: u32,
    bg: [u8; 3],
    generate: bool,
    prompt: String,
}

const DOC_SIZES: &[(&str, u32, u32)] = &[
    (k!("Carré 1024"), 1024, 1024),
    (k!("Paysage 3:2"), 1536, 1024),
    (k!("Portrait 2:3"), 1024, 1536),
    (k!("Full HD 16:9"), 1920, 1080),
    (k!("4K 16:9"), 3840, 2160),
    (k!("A4 300 dpi"), 2480, 3508),
];

/// Outil de retouche actif.
#[derive(Clone, Copy, PartialEq)]
enum RetouchTool {
    Heal,
    Erase,
    Generate,
}

/// Gomme en cours de calcul (index de la retouche).
/// Débruitage IA en cours dans ComfyUI.
struct DenoiseJob {
    photo: PathBuf,
    model: String,
    rx: Receiver<anyhow::Result<()>>,
    /// Avancement par tuiles (fait << 16 | total).
    progress: Arc<std::sync::atomic::AtomicU32>,
}

struct EraseJob {
    index: usize,
    photo: PathBuf,
    start: std::time::Instant,
    rx: Receiver<anyhow::Result<String>>,
}

/// Durée lisible d'un calcul : « 8 s », « 1 min 30 s ».
fn duration_text(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s} s") } else { format!("{} min {:02} s", s / 60, s % 60) }
}

/// Outil IA qui produit un nouveau fichier (cadre agrandi, image agrandie).
#[derive(Clone, Copy, PartialEq)]
enum FileTool {
    Expand,
    Upscale,
    /// Photo développée et ses calques aplatis (sans calcul IA).
    Flatten,
}

/// Calcul ComfyUI d'un nouveau fichier en cours.
struct FileJob {
    tool: FileTool,
    start: std::time::Instant,
    rx: Receiver<anyhow::Result<(PathBuf, String)>>,
    /// Avancement par tuiles (fait << 16 | total), 0 tant qu'inconnu.
    progress: Arc<std::sync::atomic::AtomicU32>,
}

/// Calcul d'un masque IA en cours (téléchargement du modèle compris).
struct AiJob {
    model: Model,
    photo: PathBuf,
    progress: Arc<ai::Progress>,
    /// Masque demandé suivi de ceux calculés avec lui (masques du visage).
    rx: Receiver<anyhow::Result<Vec<(Model, AiMask)>>>,
}

/// Extensions reconnues comme looks (glisser-déposer et boîte d'import).
const LOOK_EXTS: &[&str] = &["cube", "xmp", "lrtemplate", "lnpc", "lmps", "lmp", "zip"];

fn is_look_file(p: &Path) -> bool {
    p.is_dir() && p.extension().is_some_and(|e| e == "lnp")
        || p.extension().and_then(|e| e.to_str()).is_some_and(|e| LOOK_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

pub struct App {
    path: Option<PathBuf>,
    size: Option<(u32, u32)>,
    adj: Adjustments,
    committed: Adjustments,
    history: Vec<Adjustments>,
    hist_pos: usize,
    /// Libellé de chaque entrée de `history`.
    history_labels: Vec<String>,
    /// Libellé de la prochaine entrée, posé par une action (Auto, Coller, version…).
    edit_label: Option<String>,
    /// Photo en cours de décodage : sa vignette, agrandie et floue, tient lieu d'aperçu.
    opening: Option<PathBuf>,
    /// Versions nommées de la photo ouverte, et nom de la prochaine.
    snapshots: Vec<Snapshot>,
    snap_name: String,
    before: bool,
    /// Avant/après côte à côte : rideau à la position `split_pos` (fraction de la largeur), déplacé à la souris.
    split_view: bool,
    split_pos: f32,
    split_drag: bool,
    fit: bool,
    /// Pixels physiques par pixel d'image.
    zoom: f32,
    /// Point de l'image (en pixels image) au centre de la vue.
    center: Vec2,
    loading: Option<Receiver<Loaded>>,
    exporting: Option<Receiver<anyhow::Result<PathBuf>>>,
    status: String,
    gpu_name: String,
    curve_uploaded: Vec<[f32; 2]>,
    histogram: Option<Box<[[u32; 256]; 4]>>,
    hist_for: Option<Adjustments>,
    hsl_tab: usize,

    rs: Option<egui_wgpu::RenderState>,
    mode: Mode,
    catalogue: Option<Catalogue>,
    folders: Vec<String>,
    /// Par dossier de la bibliothèque : ses sous-dossiers avec photos (et leur nombre), présence du dossier.
    dir_tree: HashMap<String, (Vec<(String, usize)>, bool)>,
    /// Dernier scan lancé (relecture automatique au retour dans l'application).
    last_scan: Option<std::time::Instant>,
    was_focused: bool,
    about_copied: bool,
    about_open: bool,
    keys_open: bool,
    /// Panneau Informations de la bibliothèque, et détails lus pour la photo choisie.
    info_open: bool,
    info: Option<(PathBuf, library::Details)>,
    folder: Option<String>,
    photos: Vec<Photo>,
    selected: Option<usize>,
    sort: Sort,
    filter: Filter,
    /// Filtre par mot-clé, et saisie du prochain mot-clé.
    keyword: Option<String>,
    /// Recherche de la bibliothèque (appareil, date, mot-clé, nom, dossier).
    search: String,
    focus_search: bool,
    /// Analyse de netteté et d'empreinte en cours : résultats, total.
    cull: Option<(Receiver<(PathBuf, f32, u64)>, usize, usize)>,
    kw_input: String,
    cell: f32,
    grid_cols: usize,
    scroll_to_sel: bool,
    scanning: Option<Receiver<ScanMsg>>,
    scan_count: usize,
    scan_listed: usize,
    thumbs: Option<Thumbnailer>,
    textures: HashMap<PathBuf, Tex>,
    /// Position de la photo ouverte dans la liste de la bibliothèque.
    lib_index: Option<usize>,
    edits_since_open: bool,
    picking: Option<Receiver<Picked>>,
    last_hist: std::time::Instant,
    /// Photo (sans calques) dont les couleurs ont servi à harmoniser les calques.
    harmony_for: Option<Adjustments>,
    harmony_t: [f32; 6],
    last_harmony: std::time::Instant,
    crop_mode: bool,
    /// Index dans ASPECTS, et orientation verticale du ratio.
    crop_aspect: usize,
    crop_portrait: bool,
    crop_drag: Option<CropDrag>,
    looks: Vec<Look>,
    look_category: Option<String>,
    /// Aperçus des looks sur la photo ouverte, par fichier de look.
    look_thumbs: HashMap<PathBuf, egui::TextureHandle>,
    looks_visible: bool,
    /// Onglet du panneau de droite, section ouverte et sections épinglées de « Réglages ».
    tab: Tab,
    open_section: Option<&'static str>,
    pinned: HashSet<&'static str>,
    lut_uploaded: Option<Option<String>>,
    /// Réglage du profil de l'écran envoyé au GPU, et le profil trouvé (ou l'erreur).
    screen_icc_uploaded: Option<String>,
    screen_icc_msg: String,
    importing: Option<Receiver<ImportReport>>,
    import_report: Option<String>,
    /// Photos à mettre à la corbeille, en attente de confirmation.
    trash_ask: Vec<PathBuf>,
    /// Panneau des réglages et outils masqué (touche P) ; la barre d'icônes reste.
    panel_hidden: bool,
    /// Calques : 0 = déplacer à la souris, 1 = découper au lasso.
    layer_tool: u8,
    /// Gomme du calque : rayon en fraction de la largeur du calque.
    layer_brush: f32,
    /// Lasso de découpe en cours (coordonnées de l'image du calque).
    layer_lasso: Vec<[f32; 2]>,
    /// Fenêtre de choix d'image de calque, avec aperçus.
    layer_picker: Option<picker::LayerPicker>,
    /// Glissement du calque : par la poignée de rotation, et point saisi par rapport à son centre.
    layer_rotating: bool,
    layer_grab: Option<egui::Vec2>,
    /// Téléchargement du moteur local et de ses modèles, dernier message.
    local_install: Option<engine::LocalInstall>,
    local_msg: String,
    confirm_remove: bool,
    balance_tab: usize,
    settings: Settings,
    /// Sélection multiple de la bibliothèque (Ctrl+clic, Maj+clic, Ctrl+A).
    picked: HashSet<PathBuf>,
    clipboard: Option<Adjustments>,
    batch: Option<Receiver<BatchMsg>>,
    batch_progress: (usize, usize),
    new_look_name: Option<String>,
    local_sel: Option<usize>,
    show_mask: bool,
    brush: Brush,
    /// Le glissement en cours appartient à un outil local (pas de déplacement de la vue).
    tool_drag: Option<Handle>,
    painting: bool,
    masks_uploaded: Vec<Option<MaskKey>>,
    /// Masques IA de la photo ouverte.
    ai_masks: HashMap<Model, Arc<AiMask>>,
    ai_job: Option<AiJob>,
    /// Modèles en échec pour cette photo (pas de nouvel essai automatique).
    ai_failed: HashSet<Model>,
    sky_uploaded: Option<(Option<String>, bool)>,
    skies: Vec<String>,
    sky_thumbs: HashMap<String, Option<egui::TextureHandle>>,
    sky_show_mask: bool,
    /// Calque dont les réglages sont affichés.
    layer_sel: usize,
    /// Carte de profondeur affichée en niveaux de gris.
    depth_show_mask: bool,
    depth_uploaded: Option<bool>,
    subject_uploaded: Option<bool>,
    face_uploaded: Option<bool>,
    /// Clic sur l'image pour choisir la mise au point du flou.
    focus_pick: bool,
    /// Section « Détourer » affichée : le masque du sujet est calculé ; aperçu sur damier.
    cutout_active: bool,
    cutout_show: bool,
    /// Boîtier, objectif, focale de la photo ouverte (EXIF).
    shot: Option<crate::lens::Shot>,
    lens_uploaded: Option<crate::adjust::LensFix>,
    lens_search: String,
    /// Pixels d'origine de la photo ouverte et version retouchée envoyée au GPU.
    orig_half: Option<Arc<Vec<half::f16>>>,
    work_half: Vec<half::f16>,
    retouch_uploaded: Option<Vec<Retouch>>,
    retouch_tool: Option<RetouchTool>,
    erase_strokes: Vec<BrushStroke>,
    /// Forme de sélection de la gomme et de la génération : 0 pinceau, 1 lasso, 2 rectangle.
    select_shape: u8,
    /// Gomme par remplissage simple (rapide), sans LaMa.
    erase_smooth: bool,
    erase_job: Option<EraseJob>,
    /// Débruitage IA : résultat chargé (fichier de cache, image), calcul en cours, échec (fichier),
    /// et état envoyé au GPU (modèle, intensité, résultat chargé).
    denoise_img: Option<(PathBuf, Arc<image::RgbImage>)>,
    denoise_job: Option<DenoiseJob>,
    /// Traduction de l'interface par Ollama : avancement (fait << 16 | total) et résultat (code de la langue).
    /// Correction d'objectif affichée : les tracés et clics passent de l'image corrigée à la source.
    lens_geo: Option<crate::lens::LensUniforms>,
    lang_job: Option<(Arc<std::sync::atomic::AtomicU32>, Receiver<anyhow::Result<String>>)>,
    lang_ai_name: String,
    lang_msg: String,
    denoise_failed: Option<PathBuf>,
    denoise_uploaded: Option<(String, f32, bool)>,
    orig_dims: (u32, u32),
    /// Zone peinte affichée (clé des traits à l'écran, texture).
    stroke_tex: Option<(u64, egui::TextureHandle)>,
    /// Gommes en échec (pas de nouvel essai automatique).
    erase_failed: HashSet<usize>,
    /// ComfyUI : modèles du serveur, test de connexion en cours, invite et réglages de la prochaine génération.
    comfy_models: Vec<String>,
    new_doc: Option<NewDoc>,
    /// Fenêtre des préférences ouverte.
    prefs_open: bool,
    preloaded: Arc<std::sync::Mutex<Option<Preloaded>>>,
    preloading: Arc<std::sync::atomic::AtomicBool>,
    /// Sens de la navigation (+1 photo suivante, -1 précédente) : ce qu'on précharge.
    nav_dir: isize,
    bg_masks: Option<BgMasks>,
    /// Dossier de la bibliothèque pour lequel les masques en arrière-plan ont été lancés.
    bg_scope: Option<Option<String>>,
    export_open: bool,
    /// Invite à générer sur toute l'image dès que le document créé est ouvert.
    pending_gen: Option<(PathBuf, String)>,
    comfy_probe: Option<Receiver<anyhow::Result<(Vec<String>, Vec<String>)>>>,
    comfy_upscalers: Vec<String>,
    /// Agrandissement du cadre : marges gauche, haut, droite, bas (fraction de la largeur / hauteur).
    expand: [f32; 4],
    expand_prompt: String,
    upscale_factor: u32,
    /// Lancement demandé par le panneau (le rendu complet se fait là où le GPU est accessible).
    file_request: Option<FileTool>,
    file_job: Option<FileJob>,
    /// Message à afficher une fois le fichier produit ouvert.
    pending_status: Option<String>,
    comfy_msg: String,
    ollama_job: Option<Receiver<String>>,
    ollama_msg: String,
    /// Modèles de texte d'Ollama (pour la traduction des invites), chargés à la demande.
    ollama_models: Vec<(String, u64)>,
    ollama_models_job: Option<Receiver<anyhow::Result<Vec<(String, u64)>>>>,
    gen_prompt: String,
    gen_strength: f32,
    /// Retouche dont on déplace la source.
    heal_drag: Option<usize>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        let gpu_name = cc
            .wgpu_render_state
            .as_ref()
            .map(|rs| {
                let i = rs.adapter.get_info();
                format!("{} ({:?})", i.name, i.backend)
            })
            .unwrap_or_else(|| t!("pas de GPU").into());
        let mut app = Self {
            path: None,
            size: None,
            opening: None,
            adj: Adjustments::default(),
            committed: Adjustments::default(),
            history: vec![Adjustments::default()],
            hist_pos: 0,
            history_labels: vec![String::new()],
            edit_label: None,
            snapshots: Vec::new(),
            snap_name: String::new(),
            before: false,
            split_view: false,
            split_pos: 0.5,
            split_drag: false,
            fit: true,
            zoom: 1.0,
            center: Vec2::ZERO,
            loading: None,
            exporting: None,
            status: t!("Ouvre une photo (Ctrl+O) ou glisse-la dans la fenêtre.").into(),
            gpu_name,
            curve_uploaded: Vec::new(),
            histogram: None,
            hist_for: None,
            hsl_tab: 0,
            rs: cc.wgpu_render_state.clone(),
            mode: Mode::Library,
            catalogue: None,
            folders: Vec::new(),
            dir_tree: HashMap::new(),
            last_scan: None,
            was_focused: true,
            about_copied: false,
            about_open: false,
            keys_open: false,
            info_open: true,
            info: None,
            folder: None,
            photos: Vec::new(),
            selected: None,
            sort: Sort::DateDesc,
            filter: Filter::All,
            keyword: None,
            search: String::new(),
            focus_search: false,
            cull: None,
            kw_input: String::new(),
            cell: 200.0,
            grid_cols: 1,
            scroll_to_sel: false,
            scanning: None,
            scan_count: 0,
            scan_listed: 0,
            thumbs: None,
            textures: HashMap::new(),
            lib_index: None,
            edits_since_open: false,
            picking: None,
            last_hist: std::time::Instant::now(),
            harmony_for: None,
            harmony_t: [0.0, 1.0, 1.0, 1.0, 1.0, 0.5],
            last_harmony: std::time::Instant::now(),
            crop_mode: false,
            crop_aspect: 0,
            crop_portrait: false,
            crop_drag: None,
            looks: looks::list(),
            look_category: None,
            look_thumbs: HashMap::new(),
            looks_visible: false,
            tab: Tab::Develop,
            open_section: Some("Lumière"),
            pinned: HashSet::new(),
            lut_uploaded: None,
            screen_icc_uploaded: None,
            screen_icc_msg: String::new(),
            importing: None,
            import_report: None,
            trash_ask: Vec::new(),
            panel_hidden: false,
            layer_tool: 0,
            layer_brush: 0.05,
            layer_lasso: Vec::new(),
            layer_picker: None,
            layer_rotating: false,
            layer_grab: None,
            local_install: None,
            local_msg: String::new(),
            confirm_remove: false,
            balance_tab: 1,
            settings: Settings::load(),
            picked: HashSet::new(),
            clipboard: None,
            batch: None,
            batch_progress: (0, 0),
            new_look_name: None,
            local_sel: None,
            show_mask: false,
            brush: Brush { radius: 0.04, hardness: 0.5, flow: 0.7, erase: false },
            tool_drag: None,
            painting: false,
            masks_uploaded: Vec::new(),
            ai_masks: HashMap::new(),
            ai_job: None,
            ai_failed: HashSet::new(),
            sky_uploaded: None,
            skies: Vec::new(),
            sky_thumbs: HashMap::new(),
            sky_show_mask: false,
            depth_show_mask: false,
            depth_uploaded: None,
            subject_uploaded: None,
            face_uploaded: None,
            focus_pick: false,
            cutout_active: false,
            cutout_show: false,
            shot: None,
            lens_uploaded: None,
            lens_search: String::new(),
            orig_half: None,
            work_half: Vec::new(),
            retouch_uploaded: None,
            retouch_tool: None,
            erase_strokes: Vec::new(),
            select_shape: 0,
            erase_smooth: false,
            layer_sel: 0,
            erase_job: None,
            denoise_img: None,
            denoise_job: None,
            lang_job: None,
            lens_geo: None,
            lang_ai_name: String::new(),
            lang_msg: String::new(),
            denoise_failed: None,
            denoise_uploaded: None,
            orig_dims: (0, 0),
            stroke_tex: None,
            erase_failed: HashSet::new(),
            comfy_models: Vec::new(),
            new_doc: None,
            prefs_open: false,
            preloaded: Default::default(),
            preloading: Default::default(),
            nav_dir: 1,
            bg_masks: None,
            bg_scope: None,
            export_open: false,
            pending_gen: None,
            comfy_probe: None,
            comfy_upscalers: Vec::new(),
            expand: [0.0; 4],
            expand_prompt: String::new(),
            upscale_factor: 2,
            file_request: None,
            file_job: None,
            pending_status: None,
            comfy_msg: String::new(),
            ollama_job: None,
            ollama_msg: String::new(),
            ollama_models: Vec::new(),
            ollama_models_job: None,
            gen_prompt: String::new(),
            gen_strength: 1.0,
            heal_drag: None,
        };
        apply_access(&cc.egui_ctx, &app.settings);
        cc.egui_ctx.set_zoom_factor(app.settings.ui_zoom);
        match Catalogue::open() {
            Ok(c) => {
                app.folders = c.folders();
                app.catalogue = Some(c);
                app.refresh_dirs();
            }
            Err(e) => app.status = tf!("Catalogue indisponible : {e:#}", e),
        }
        let gpu = app.rs.as_ref().map(|rs| (rs.device.clone(), rs.queue.clone()));
        app.thumbs = Some(Thumbnailer::new(cc.egui_ctx.clone(), gpu));
        app.refresh_list();
        app.start_scan(&cc.egui_ctx);
        let last = app.settings.last_photo.clone().filter(|p| app.settings.start_last && p.exists());
        if let Some(p) = initial.or(last) {
            app.open(p, &cc.egui_ctx);
        }
        app
    }

    // ---------- Bibliothèque ----------

    fn refresh_list(&mut self) {
        let Some(cat) = &self.catalogue else { return };
        let sel = self.selected.and_then(|i| self.photos.get(i)).map(|p| p.path.clone());
        let open = self.lib_index.and_then(|i| self.photos.get(i)).map(|p| p.path.clone());
        self.photos = cat.list(self.folder.as_deref(), self.filter, self.sort, self.keyword.as_deref());
        if !self.search.trim().is_empty() {
            let kw = cat.keyword_map();
            self.photos.retain(|p| library::matches(&self.search, p, kw.get(&p.path).map(Vec::as_slice).unwrap_or_default()));
        }
        let find = |p: Option<PathBuf>| p.and_then(|p| self.photos.iter().position(|x| x.path == p));
        self.selected = find(sel).or(if self.photos.is_empty() { None } else { Some(0) });
        self.lib_index = find(open.or_else(|| self.path.clone()));
    }

    /// Arborescence des dossiers de la bibliothèque (d'après le catalogue).
    fn refresh_dirs(&mut self) {
        let Some(cat) = &self.catalogue else { return };
        self.dir_tree = self.folders.iter().map(|f| (f.clone(), (cat.subfolders(f), Path::new(f).is_dir()))).collect();
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        if self.scanning.is_some() || self.folders.is_empty() {
            return;
        }
        self.last_scan = Some(std::time::Instant::now());
        let (tx, rx) = channel();
        let (folders, ctx) = (self.folders.clone(), ctx.clone());
        std::thread::spawn(move || library::scan(folders, tx, ctx));
        self.scanning = Some(rx);
        self.scan_count = 0;
        self.scan_listed = 0;
    }

    fn add_folder(&mut self, ctx: &egui::Context) {
        // Départ : à côté du dernier dossier de la bibliothèque, sinon Images.
        let dir = self.folders.last().and_then(|f| Path::new(f).parent().map(Path::to_path_buf)).filter(|d| d.is_dir())
            .or_else(dirs::picture_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.layer_picker = Some(picker::LayerPicker::open(dir, picker::PickFor::Folder, ctx));
    }

    fn folder_picked(&mut self, dir: PathBuf, ctx: &egui::Context) {
        let Some(cat) = &self.catalogue else { return };
        let d = dir.to_string_lossy().to_string();
        if let Err(e) = cat.add_folder(&d) {
            self.status = tf!("Impossible d'ajouter le dossier : {e:#}", e);
            return;
        }
        self.folders = cat.folders();
        self.folder = Some(d);
        self.refresh_dirs();
        // Un scan déjà en cours ne connaît pas ce dossier : on relancera à sa fin.
        if self.scanning.is_some() {
            self.status = t!("Le nouveau dossier sera parcouru après le scan en cours.").into();
        }
        self.start_scan(ctx);
        self.refresh_list();
    }

    fn set_rating(&mut self, r: u8) {
        let paths = match self.mode {
            Mode::Library => self.targets(),
            Mode::Editor => self.path.clone().into_iter().collect(),
        };
        let Some(cat) = &self.catalogue else { return };
        for p in paths {
            cat.set_rating(&p, r);
            if let Some(ph) = self.photos.iter_mut().find(|x| x.path == p) {
                ph.rating = r;
            }
        }
    }

    /// Photos visées par une action de la bibliothèque : la sélection multiple, sinon la photo courante.
    fn targets(&self) -> Vec<PathBuf> {
        let multi: Vec<PathBuf> = self.photos.iter().filter(|p| self.picked.contains(&p.path)).map(|p| p.path.clone()).collect();
        if multi.is_empty() {
            self.selected.and_then(|i| self.photos.get(i)).map(|p| p.path.clone()).into_iter().collect()
        } else {
            multi
        }
    }

    fn copy_settings(&mut self) {
        let adj = match self.mode {
            Mode::Editor => Some(self.adj.clone()),
            Mode::Library => self.selected.and_then(|i| self.photos.get(i)).map(|p| load_sidecar(&p.path).unwrap_or_default()),
        };
        if let Some(a) = adj {
            self.clipboard = Some(a);
            self.status = t!("Réglages copiés (sans recadrage ni masques dessinés).").into();
        }
    }

    /// Colle les réglages copiés : tout sauf le recadrage et le redressement de chaque photo.
    fn paste_settings(&mut self) {
        let Some(clip) = self.clipboard.clone() else { return };
        // Les masques dessinés dépendent du cadrage de chaque photo : ils ne sont pas recopiés.
        // Les masques IA (sans retouches au pinceau) le sont : ils sont recalculés sur chaque photo.
        let keep_geometry = |target: &Adjustments| {
            let mut locals = target.locals.clone();
            for l in &clip.locals {
                if let Shape::Ai { kind, .. } = l.shape {
                    let present = locals.iter().any(|t| t.name == l.name && matches!(t.shape, Shape::Ai { kind: k, .. } if k == kind));
                    if !present && locals.len() < MAX_LOCALS {
                        locals.push(Local { shape: Shape::Ai { kind, strokes: vec![] }, ..l.clone() });
                    }
                }
            }
            Adjustments { crop: target.crop, angle: target.angle, locals, retouch: target.retouch.clone(), ..clip.clone() }
        };
        if self.mode == Mode::Editor {
            self.adj = keep_geometry(&self.adj);
            self.edit_label = Some(t!("Coller les réglages").into());
            return;
        }
        let targets = self.targets();
        let mut n = 0;
        for p in &targets {
            let cur = load_sidecar(p).unwrap_or_default();
            if save_sidecar(p, &keep_geometry(&cur)).is_ok() {
                n += 1;
                if let Some(cat) = &self.catalogue {
                    cat.set_edited(p, library::is_edited(p));
                }
                self.textures.remove(p);
            }
        }
        self.status = tf!("Réglages collés sur {n} photo(s).", n);
        self.refresh_list();
    }

    fn start_batch(&mut self, dir: PathBuf, ctx: &egui::Context) {
        let Some(rs) = &self.rs else { return };
        let photos = self.targets();
        if photos.is_empty() || self.batch.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let (device, queue, ctx, ext) = (rs.device.clone(), rs.queue.clone(), ctx.clone(), self.settings.batch_format.clone());
        let opts = self.settings.export.clone();
        self.batch_progress = (0, photos.len());
        std::thread::spawn(move || crate::batch::run(photos, dir, ext, opts, device, queue, tx, ctx));
        self.batch = Some(rx);
    }

    fn current_rating(&self) -> u8 {
        match (&self.path, &self.catalogue) {
            (Some(p), Some(c)) => c.rating(p),
            _ => 0,
        }
    }

    /// Met à jour la vignette de la photo qu'on quitte si elle a été retouchée.
    fn leave_photo(&mut self) {
        if !self.edits_since_open {
            return;
        }
        self.edits_since_open = false;
        let (Some(path), Some(rs)) = (self.path.clone(), self.rs.clone()) else { return };
        let renderer = rs.renderer.read();
        if let Some(dev) = renderer.callback_resources.get::<Developer>() {
            let (w, h) = small_size(&self.adj, dev.width, dev.height, library::THUMB_SIZE);
            let u = self.adj.uniforms([0.0, 0.0, 1.0, 1.0], false, false, [dev.width as f32, dev.height as f32], false);
            if let Ok(px) = dev.render_to_cpu(&rs.device, &rs.queue, &u, w, h) {
                let _ = library::save_thumb(&path, w, h, &px);
            }
        }
        drop(renderer);
        self.textures.remove(&path);
        if let Some(cat) = &self.catalogue {
            cat.set_edited(&path, library::is_edited(&path));
        }
        self.refresh_list();
    }

    fn open_index(&mut self, i: usize, ctx: &egui::Context) {
        if let Some(p) = self.photos.get(i).map(|p| p.path.clone()) {
            if let Some(old) = self.lib_index {
                self.nav_dir = if i < old { -1 } else { 1 };
            }
            self.selected = Some(i);
            self.lib_index = Some(i);
            self.open(p, ctx);
        }
    }

    /// Tri, filtre et taille des vignettes : l'état de la vue. Les commandes sont dans les menus.
    fn library_toolbar(&mut self, ui: &mut egui::Ui) {
        let r = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .id_salt("recherche")
                .hint_text(t!("Rechercher : appareil, 2024, juillet, mot-clé…"))
                .desired_width(240.0),
        );
        if self.focus_search {
            r.request_focus();
            self.focus_search = false;
        }
        let mut changed = r.changed();
        if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) && !self.search.is_empty() {
            self.search.clear();
            changed = true;
        }
        r.on_hover_text(t!("Tous les mots doivent correspondre. Année, mois, date (14/07/2024 ou 07/2024), appareil, mot-clé, nom ou dossier. Ctrl+F."));
        if !self.search.is_empty() && ui.small_button("×").on_hover_text(t!("Effacer la recherche")).clicked() {
            self.search.clear();
            changed = true;
        }
        if changed {
            self.refresh_list();
        }
        ui.separator();
        let (old_sort, old_filter) = (self.sort, self.filter);
        for (v, label) in [(Sort::DateDesc, t!("Récentes")), (Sort::DateAsc, t!("Anciennes")), (Sort::Name, t!("Nom"))] {
            ui.selectable_value(&mut self.sort, v, label).on_hover_text(t!("Tri"));
        }
        ui.separator();
        for (v, label) in [(Filter::All, t!("Toutes")), (Filter::Edited, t!("Modifiées"))] {
            ui.selectable_value(&mut self.filter, v, label);
        }
        ui.selectable_value(&mut self.filter, Filter::Blurry, t!("Floues"))
            .on_hover_text(t!("Photos probablement floues, les moins nettes d'abord. Une photo au fond flou volontaire reste nette si son sujet l'est."));
        ui.selectable_value(&mut self.filter, Filter::Similar, t!("Semblables"))
            .on_hover_text(t!("Doublons et rafales, groupe par groupe ; la plus nette de chaque groupe en premier."));
        // Filtre par note : clic sur la k-ième étoile = « k étoiles et plus » ; re-clic pour l'enlever.
        let min = if let Filter::MinRating(r) = self.filter { r } else { 0 };
        let (rect, resp) = ui.allocate_exact_size(vec2(5.0 * 16.0, 18.0), Sense::click());
        paint_stars(ui.painter(), rect.left_top() + vec2(0.0, 1.0), min, 16.0);
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let k = (((pos.x - rect.left()) / 16.0).floor() as u8 + 1).clamp(1, 5);
                self.filter = if min == k { Filter::All } else { Filter::MinRating(k) };
            }
        }
        resp.on_hover_text(t!("Note minimale (re-clic : toutes)"));
        if self.sort != old_sort || self.filter != old_filter {
            if matches!(self.filter, Filter::Blurry | Filter::Similar) {
                self.start_cull(ui.ctx());
            }
            self.refresh_list();
        }
        ui.separator();
        ui.label(t!("Taille"));
        ui.add(egui::Slider::new(&mut self.cell, 120.0..=360.0).show_value(false));
        if self.batch.is_some() {
            ui.separator();
            ui.spinner();
            ui.label(tf!("Export {0}/{1}", self.batch_progress.0, self.batch_progress.1));
        }
    }

    fn library_side(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            self.folders_ui(ui);
            ui.add_space(12.0);
            ui.separator();
            self.keywords_ui(ui);
            if self.info_open {
                ui.add_space(12.0);
                ui.separator();
                self.info_ui(ui);
            }
        });
    }

    /// Informations de la photo choisie : emplacement, taille, dimensions, prise de vue.
    fn info_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading(t!("Informations"));
        ui.add_space(4.0);
        let Some(photo) = self.selected.and_then(|i| self.photos.get(i)).cloned() else {
            ui.small(t!("Choisis une photo."));
            return;
        };
        if self.info.as_ref().is_none_or(|(p, _)| *p != photo.path) {
            self.info = Some((photo.path.clone(), library::details(&photo.path)));
        }
        let Some((_, d)) = &self.info else { return };
        let dir = photo.path.parent().map(|d| d.display().to_string()).unwrap_or_default();
        let size = d.size.or(if self.path.as_ref() == Some(&photo.path) { self.size } else { None });
        let mut rows: Vec<(&str, String)> = vec![(t!("Nom"), photo.name.clone()), (t!("Dossier"), dir), (t!("Fichier"), library::format_bytes(d.bytes))];
        if let Some((w, h)) = size {
            let mpx = format!("{:.1}", w as f64 * h as f64 / 1e6).replace('.', t!(",|séparateur décimal"));
            rows.push((t!("Dimensions"), format!("{w} × {h} px ({mpx} Mpx)")));
        }
        rows.push((t!("Date"), library::format_date(photo.taken)));
        if !photo.camera.is_empty() {
            rows.push((t!("Appareil"), photo.camera.clone()));
        }
        if let Some(l) = &d.lens {
            rows.push((t!("Objectif"), l.clone()));
        }
        if !d.exposure.is_empty() {
            rows.push((t!("Prise de vue"), d.exposure.clone()));
        }
        if photo.sharp >= 0.0 {
            let v = if photo.sharp < crate::cull::BLUR_LIMIT { tf!("{0} (floue probable)", photo.sharp.round()) } else { format!("{}", photo.sharp.round()) };
            rows.push((t!("Netteté"), v));
        }
        if let Some(n) = library::copy_number(&photo.path) {
            rows.push((t!("Copie virtuelle"), format!("n° {n}")));
        }
        egui::Grid::new("infos_photo").num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
            for (k, v) in rows {
                ui.label(egui::RichText::new(k).weak());
                ui.add(egui::Label::new(v).wrap());
                ui.end_row();
            }
        });
        if ui.button(t!("Afficher dans le dossier")).clicked() {
            library::show_in_folder(&photo.path);
        }
    }

    /// Mots-clés : filtre de la grille, et ceux des photos visées (ajout, retrait).
    fn keywords_ui(&mut self, ui: &mut egui::Ui) {
        let Some(cat) = &self.catalogue else { return };
        ui.heading(t!("Mots-clés"));
        ui.add_space(4.0);
        let all = cat.all_keywords();
        let mut filter = None;
        if all.is_empty() {
            ui.small(t!("Aucun mot-clé pour l'instant."));
        } else {
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(self.keyword.is_none(), t!("Tous")).clicked() {
                    filter = Some(None);
                }
                for (w, n) in &all {
                    let on = self.keyword.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(w));
                    if ui.selectable_label(on, format!("{w} ({n})")).on_hover_text(t!("Afficher seulement ces photos (re-clic : toutes)")).clicked() {
                        filter = Some(if on { None } else { Some(w.clone()) });
                    }
                }
            });
        }
        let targets = self.targets();
        ui.add_space(8.0);
        if targets.is_empty() {
            ui.small(t!("Sélectionne une photo pour lui donner des mots-clés."));
        } else {
            ui.label(if targets.len() == 1 { t!("Photo sélectionnée :").to_string() } else { tf!("{0} photos sélectionnées :", targets.len()) });
            // Mots-clés de la photo courante (ou communs à la sélection).
            let mut common = cat.keywords(&targets[0]);
            for p in &targets[1..] {
                let k = cat.keywords(p);
                common.retain(|w| k.iter().any(|x| x.eq_ignore_ascii_case(w)));
            }
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                for w in &common {
                    if ui.small_button(format!("{w} ×")).on_hover_text(t!("Retirer ce mot-clé")).clicked() {
                        remove = Some(w.clone());
                    }
                }
            });
            let mut add = None;
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.kw_input).hint_text(t!("ex. vacances, famille")).desired_width(ui.available_width() - 70.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button(t!("Ajouter")).clicked() || enter) && !self.kw_input.trim().is_empty() {
                    add = Some(std::mem::take(&mut self.kw_input));
                    if enter {
                        r.request_focus();
                    }
                }
            });
            if let Some(text) = add {
                // Plusieurs mots-clés séparés par des virgules.
                for w in text.split(',').map(str::trim).filter(|w| !w.is_empty()) {
                    for p in &targets {
                        cat.add_keyword(p, w);
                    }
                }
            }
            if let Some(w) = remove {
                for p in &targets {
                    cat.remove_keyword(p, &w);
                }
                if self.keyword.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(&w)) {
                    self.refresh_list();
                }
            }
        }
        if let Some(f) = filter {
            self.keyword = f;
            self.refresh_list();
        }
    }

    fn folders_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading(t!("Dossiers"));
        ui.add_space(4.0);
        let mut pick: Option<Option<String>> = None;
        let total: usize = self.dir_tree.values().filter_map(|(d, _)| d.first().map(|x| x.1)).sum();
        if ui.selectable_label(self.folder.is_none(), format!("{} ({})", t!("Toutes les photos"), thousands(total))).clicked() {
            pick = Some(None);
        }
        let mut remove = None;
        for f in &self.folders {
            let short: Vec<&str> = f.rsplit('/').filter(|s| !s.is_empty()).take(2).collect();
            let label = short.into_iter().rev().collect::<Vec<_>>().join("/");
            let (dirs, present) = self.dir_tree.get(f).map_or((&[][..], true), |(d, ok)| (&d[..], *ok));
            let r = dir_node(ui, dirs, f, &label, self.folder.as_deref(), &mut pick);
            let r = if present {
                r.on_hover_text(f.as_str())
            } else {
                r.on_hover_text(tf!("{0}\nDossier absent (disque ou partage réseau non monté) : ses photos restent dans la bibliothèque.", f))
            };
            r.context_menu(|ui| {
                if ui.button(t!("Retirer de la bibliothèque")).on_hover_text(t!("Les fichiers restent sur le disque.")).clicked() {
                    remove = Some(f.clone());
                    ui.close();
                }
            });
        }
        let mut changed = false;
        if let Some(p) = pick {
            self.folder = p;
            changed = true;
        }
        if let Some(f) = remove {
            if let Some(cat) = &self.catalogue {
                let _ = cat.remove_folder(&f);
                self.folders = cat.folders();
            }
            self.refresh_dirs();
            if self.folder.as_ref().is_some_and(|d| d == &f || d.starts_with(&format!("{f}/"))) {
                self.folder = None;
            }
            changed = true;
        }
        if changed {
            self.refresh_list();
        }
        if self.folders.is_empty() {
            ui.add_space(8.0);
            ui.label(t!("Aucun dossier. Clique sur « Ajouter un dossier… » pour commencer."));
        }
        ui.add_space(12.0);
        ui.small(t!("Flèche : sous-dossiers. Clic droit sur un dossier pour le retirer."));
        ui.add_space(8.0);
        ui.small(t!("Sélection : Ctrl+clic, Maj+clic, Ctrl+A. Échap pour désélectionner."));
        ui.add_space(8.0);
        ui.small(t!("Langue, écran et IA : menu Édition, Préférences (Ctrl+,)."));
    }

    /// Préférences : langue, côté du panneau, écran, IA, démarrage.
    fn prefs_window(&mut self, ctx: &egui::Context) {
        if !self.prefs_open {
            return;
        }
        let mut open = true;
        let mut start_bg = false;
        egui::Window::new(t!("Préférences")).open(&mut open).collapsible(false).resizable(false).vscroll(true).default_height(ctx.content_rect().height() - 120.0).default_width(340.0).show(ctx, |ui| {
            ui.set_width(340.0);
            self.language_ui(ui);
            ui.separator();
            ui.label(egui::RichText::new(t!("Côté de la barre d'outils")).strong());
            if segmented(ui, &mut self.settings.panel_left, &[(true, t!("À gauche")), (false, t!("À droite"))]) {
                self.settings.save();
            }
            ui.label(egui::RichText::new(t!("Disposition")).strong());
            if segmented(ui, &mut self.settings.split_layout, &[(false, t!("Tout ensemble")), (true, t!("Réglages à part"))]) {
                self.settings.save();
            }
            ui.small(if self.settings.split_layout {
                t!("Réglages (curseurs, courbes) de l'autre côté de la photo ; re-clic sur un outil pour replier son panneau.")
            } else {
                t!("Barre d'outils et panneau de l'outil choisi du même côté.")
            });
            ui.separator();
            self.screen_icc_ui(ui);
            ui.separator();
            if ui.checkbox(&mut self.settings.start_last, t!("Rouvrir la dernière photo au démarrage")).changed() {
                self.settings.save();
            }
            self.ai_gpu_ui(ui);
            ui.separator();
            self.engine_prefs_ui(ui, ctx);
            ui.separator();
            if ui
                .checkbox(&mut self.settings.ai_background, t!("Préparer les masques IA en arrière-plan"))
                .on_hover_text(t!("Sujet et ciel calculés à l'avance pour les photos du dossier affiché dans la bibliothèque (500 au plus), quand l'application ne travaille pas ; seulement avec des modèles déjà téléchargés."))
                .changed()
            {
                self.settings.save();
                start_bg = true;
            }
        });
        self.prefs_open = open;
        if start_bg {
            self.start_bg_masks(ctx);
        }
    }

    /// À propos : version, carte graphique, emplacement des données, licence, informations à copier.
    fn about_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(t!("À propos")).strong());
        let version = env!("CARGO_PKG_VERSION");
        let ai = match crate::ai::on_gpu() {
            Some(true) => t!("carte graphique"),
            Some(false) => t!("processeur"),
            None => t!("pas encore utilisée"),
        };
        let data = library::data_dir();
        ui.label(egui::RichText::new(format!("Luminux IA {version}")).heading());
        ui.small(t!("Éditeur photo non destructif pour Linux, rendu par la carte graphique (wgpu, Vulkan)."));
        egui::Grid::new("a_propos").num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
            ui.label(t!("Carte graphique"));
            ui.label(&self.gpu_name);
            ui.end_row();
            ui.label(t!("Calcul IA"));
            ui.label(ai);
            ui.end_row();
            ui.label(t!("Données"));
            ui.add(egui::Label::new(data.display().to_string()).truncate())
                .on_hover_text(format!("{}\n{}", data.display(), t!("Catalogue, masques IA en cache, réglages, looks")));
            ui.end_row();
            ui.label(t!("Licence"));
            ui.label("MIT");
            ui.end_row();
        });
        ui.horizontal(|ui| {
            ui.hyperlink_to(t!("Code source"), "https://github.com/Cfrancky77/Luminux-IA-public");
            if ui.button(t!("Copier ces informations")).on_hover_text(t!("Pour un signalement de bug")).clicked() {
                let text = format!(
                    "Luminux IA {version}\n{} : {}\n{} : {ai}\n{} : {}\n{} : {} {}",
                    t!("Carte graphique"),
                    self.gpu_name,
                    t!("Calcul IA"),
                    t!("Données"),
                    data.display(),
                    t!("Système"),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                );
                ui.ctx().copy_text(text);
                self.about_copied = true;
            }
            if self.about_copied {
                ui.small(t!("Copié."));
            }
        });
    }

    fn ai_gpu_ui(&mut self, ui: &mut egui::Ui) {
        let tip = t!("Masques, profondeur et visages calculés par la carte graphique (Vulkan : AMD, NVIDIA ou Intel). Chaque modèle est comparé une fois au processeur ; s'il diffère, il reste sur le processeur.");
        if ui.checkbox(&mut self.settings.ai_gpu, t!("IA sur la carte graphique")).on_hover_text(tip).changed() {
            self.settings.save();
            crate::ai::reset_sessions();
        }
        match crate::ai::on_gpu() {
            Some(true) => ui.small(t!("Dernier calcul IA : carte graphique.")),
            Some(false) => ui.small(t!("Dernier calcul IA : processeur.")),
            None => ui.small(""),
        };
    }

    /// Profil ICC de l'écran : aucun (sRGB), celui de colord, ou un fichier.
    fn screen_icc_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("Profil de l'écran"))
            .on_hover_text(t!("Convertit l'affichage vers le profil ICC de l'écran (écran calibré ou large gamut). L'espace couleur de l'export se choisit dans la fenêtre Exporter."));
        let before = match self.settings.screen_icc.as_str() {
            "" => 0,
            "auto" => 1,
            _ => 2,
        };
        let mut mode = before;
        segmented(ui, &mut mode, &[(0, t!("Aucun")), (1, t!("Système")), (2, t!("Fichier…"))]);
        if mode != before {
            match mode {
                0 => self.settings.screen_icc.clear(),
                1 => self.settings.screen_icc = "auto".into(),
                _ => self.dialog(ui.ctx(), || rfd::FileDialog::new().add_filter("ICC", &["icc", "icm", "ICC", "ICM"]).pick_file().map(Picked::ScreenIcc)),
            }
            self.settings.save();
        }
        if !self.screen_icc_msg.is_empty() {
            ui.small(&self.screen_icc_msg);
        }
    }

    /// Langue de l'interface : choix, import d'un fichier, modèle à traduire, traduction par Ollama.
    fn language_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(t!("Langue")).strong());
        let langs = crate::i18n::languages();
        let current = if self.settings.language.is_empty() { crate::i18n::system_language() } else { self.settings.language.clone() };
        let shown = langs.iter().find(|l| l.0 == current).map_or(current.clone(), |l| l.1.clone());
        let mut chosen = current.clone();
        egui::ComboBox::from_id_salt("langue").width(ui.available_width() - 8.0).truncate().selected_text(shown).show_ui(ui, |ui| {
            for (code, name) in &langs {
                ui.selectable_value(&mut chosen, code.clone(), name);
            }
        });
        if chosen != current {
            self.set_language(chosen);
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button(t!("Importer…")).on_hover_text(t!("Fichier de langue .json (texte français → traduction)")).clicked() {
                self.dialog(ui.ctx(), || rfd::FileDialog::new().add_filter(t!("Langue"), &["json"]).pick_file().map(Picked::LangImport));
            }
            if ui.button(t!("Modèle à traduire…")).on_hover_text(t!("Enregistre le fichier anglais, à traduire puis importer")).clicked() {
                self.dialog(ui.ctx(), || rfd::FileDialog::new().set_file_name("luminux_lang.json").add_filter(t!("Langue"), &["json"]).save_file().map(Picked::LangTemplate));
            }
        });
        ui.horizontal(|ui| {
            let busy = self.lang_job.is_some();
            ui.add(egui::TextEdit::singleline(&mut self.lang_ai_name).hint_text(t!("Langue, ex. Deutsch")).desired_width(ui.available_width() - 90.0));
            if ui.add_enabled(!busy && !self.lang_ai_name.trim().is_empty(), egui::Button::new(t!("Traduire"))).on_hover_text(t!("Traduction de l'interface par Ollama (réglé dans Retouche → Générer)")).clicked() {
                self.start_lang_ai(ui.ctx());
            }
        });
        if let Some((prog, _)) = &self.lang_job {
            let v = prog.load(std::sync::atomic::Ordering::Relaxed);
            let (done, total) = ((v >> 16) as usize, (v & 0xffff) as usize);
            ui.small(tf!("Traduction par Ollama : lot {0} sur {total}", (done + 1).min(total.max(1)), total));
        } else if !self.lang_msg.is_empty() {
            ui.small(&self.lang_msg);
        }
    }

    fn set_language(&mut self, code: String) {
        match crate::i18n::set_language(&code) {
            Ok(()) => {
                self.settings.language = code;
                self.settings.save();
                self.lang_msg.clear();
            }
            Err(e) => self.lang_msg = tf!("Langue indisponible : {e:#}", e),
        }
    }

    fn start_lang_ai(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        let progress = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let prog = progress.clone();
        let (name, url, model) = (self.lang_ai_name.trim().to_string(), self.settings.ollama_url.clone(), self.settings.translate_model.clone());
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = crate::i18n::ai_translate(&name, &url, &model, |done, total| {
                prog.store(((done as u32) << 16) | total as u32, std::sync::atomic::Ordering::Relaxed);
                ctx2.request_repaint();
            });
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.lang_job = Some((progress, rx));
    }

    fn library_keys(&mut self, ctx: &egui::Context) {
        use egui::Key;
        if ctx.egui_wants_keyboard_input() || self.photos.is_empty() || !self.trash_ask.is_empty() {
            return;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, Key::A))) {
            self.picked = self.photos.iter().map(|p| p.path.clone()).collect();
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.picked.clear();
        }
        let n = self.photos.len() as i64;
        let cols = self.grid_cols.max(1) as i64;
        let cur = self.selected.unwrap_or(0) as i64;
        let mut next = cur;
        ctx.input(|i| {
            if i.key_pressed(Key::ArrowRight) { next += 1; }
            if i.key_pressed(Key::ArrowLeft) { next -= 1; }
            if i.key_pressed(Key::ArrowDown) { next += cols; }
            if i.key_pressed(Key::ArrowUp) { next -= cols; }
        });
        let next = next.clamp(0, n - 1) as usize;
        if next as i64 != cur || self.selected.is_none() {
            self.selected = Some(next);
            self.scroll_to_sel = true;
        }
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            self.open_index(next, ctx);
        }
        if ctx.input(|i| i.key_pressed(Key::Delete)) {
            self.ask_trash(self.photos.get(next).map(|p| p.path.clone()));
        }
    }

    /// Demande confirmation avant de mettre à la corbeille la sélection multiple si elle contient
    /// `p`, sinon `p` seule. Une copie virtuelle n'a pas de fichier : elle est seulement retirée.
    fn ask_trash(&mut self, p: Option<PathBuf>) {
        let Some(p) = p else { return };
        self.trash_ask = if self.picked.contains(&p) {
            self.photos.iter().map(|x| x.path.clone()).filter(|x| self.picked.contains(x)).collect()
        } else {
            vec![p]
        };
    }

    /// Boîte « Mettre à la corbeille ? ».
    fn trash_window(&mut self, ctx: &egui::Context) {
        if self.trash_ask.is_empty() {
            return;
        }
        let n = self.trash_ask.len();
        let mut go = None;
        egui::Modal::new(egui::Id::unique("corbeille")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(if n == 1 {
                tf!("Mettre « {0} » à la corbeille ?", self.trash_ask[0].file_name().unwrap_or_default().to_string_lossy())
            } else {
                tf!("Mettre {0} photos à la corbeille ?", n)
            });
            ui.add_space(6.0);
            ui.label(t!("Les fichiers et leurs réglages vont dans la corbeille du système : le gestionnaire de fichiers peut les restaurer. Les copies virtuelles sont supprimées."));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let b = ui.add(egui::Button::new(egui::RichText::new(t!("Mettre à la corbeille")).color(Color32::WHITE)).fill(Color32::from_rgb(190, 60, 55)));
                if b.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = Some(true);
                }
                if ui.button(t!("Annuler")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    go = Some(false);
                }
            });
        });
        match go {
            Some(true) => self.trash_now(),
            Some(false) => self.trash_ask.clear(),
            None => {}
        }
    }

    fn trash_now(&mut self) {
        let Some(cat) = &self.catalogue else { return };
        let (mut done, mut errors) = (0, Vec::new());
        for p in std::mem::take(&mut self.trash_ask) {
            match cat.trash_photo(&p) {
                Ok(()) => {
                    done += 1;
                    // Une vraie photo emporte ses copies virtuelles.
                    let gone = |o: &Path| o == p || (library::copy_number(&p).is_none() && library::source(o) == p);
                    self.picked.retain(|x| !gone(x));
                    self.textures.retain(|t, _| !gone(t));
                    if self.path.as_deref().is_some_and(gone) {
                        // Photo ouverte : plus rien à enregistrer pour elle.
                        self.edits_since_open = false;
                        self.path = None;
                        self.size = None;
                    }
                }
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        self.status = if errors.is_empty() {
            tf!("{0} photo(s) mise(s) à la corbeille", done)
        } else {
            tf!("{0} photo(s) mise(s) à la corbeille, échec : {1}", done, errors.join(" ; "))
        };
        self.refresh_list();
        self.refresh_dirs();
    }

    fn library_grid(&mut self, ui: &mut egui::Ui) {
        if self.photos.is_empty() {
            let msg = if self.scanning.is_some() { t!("Recherche des photos…") } else { t!("Aucune photo ici.") };
            ui.centered_and_justified(|ui| ui.label(msg));
            return;
        }
        let gap = 8.0;
        let cell = self.cell;
        let cell_h = cell + 38.0;
        let cols = (((ui.available_width() - gap) / (cell + gap)).floor() as usize).max(1);
        self.grid_cols = cols;
        let rows = self.photos.len().div_ceil(cols);
        let mut visible = HashSet::new();
        let mut clicked = None;
        let mut opened = None;
        let mut menu = None;
        let sel_row = self.selected.map(|s| s / cols);
        let scroll_to = std::mem::take(&mut self.scroll_to_sel);
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, cell_h + gap, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    ui.add_space(gap);
                    for col in 0..cols {
                        let i = row * cols + col;
                        let Some(photo) = self.photos.get(i) else { break };
                        let (rect, resp) = ui.allocate_exact_size(vec2(cell, cell_h), Sense::click());
                        visible.insert(photo.path.clone());
                        let selected = self.selected == Some(i) || self.picked.contains(&photo.path);
                        let p = ui.painter();
                        p.rect_filled(rect, 6.0, if selected { palette::ITEM_ON } else { palette::WINDOW });
                        if selected {
                            p.rect_stroke(rect, 6.0, Stroke::new(2.0, Color32::from_rgb(90, 160, 255)), egui::StrokeKind::Inside);
                        }
                        let img_area = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(cell - 12.0, cell - 12.0));
                        match self.textures.get(&photo.path) {
                            Some(Tex::Ready(t)) => {
                                let s = t.size_vec2();
                                let k = (img_area.width() / s.x).min(img_area.height() / s.y);
                                let r = Rect::from_center_size(img_area.center(), s * k);
                                p.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            Some(Tex::Failed) => {
                                p.text(img_area.center(), egui::Align2::CENTER_CENTER, "illisible", egui::FontId::proportional(13.0), Color32::GRAY);
                            }
                            other => {
                                if other.is_none() {
                                    if let Some(t) = &self.thumbs {
                                        t.request(&photo.path);
                                    }
                                    self.textures.insert(photo.path.clone(), Tex::Pending);
                                }
                                p.text(img_area.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(18.0), Color32::GRAY);
                            }
                        }
                        let text_y = rect.top() + cell - 2.0;
                        let name = egui::text::LayoutJob::simple_singleline(photo.name.clone(), egui::FontId::proportional(12.0), Color32::LIGHT_GRAY);
                        let mut galley_job = name;
                        galley_job.wrap = egui::text::TextWrapping::truncate_at_width(cell - 12.0);
                        let g = ui.fonts_mut(|f| f.layout_job(galley_job));
                        p.galley(pos2(rect.left() + 6.0, text_y), g, Color32::LIGHT_GRAY);
                        paint_stars(p, pos2(rect.left() + 6.0, text_y + 17.0), photo.rating, 12.0);
                        let date = p.text(pos2(rect.right() - 6.0, text_y + 17.0), egui::Align2::RIGHT_TOP, library::format_date(photo.taken), egui::FontId::proportional(11.0), Color32::GRAY);
                        if photo.edited {
                            // Pastille bleue : la photo a des réglages.
                            p.circle_filled(pos2(date.left() - 7.0, date.center().y), 3.5, Color32::from_rgb(90, 160, 255));
                        }
                        if library::copy_number(&photo.path).is_some() {
                            // Badge : copie virtuelle.
                            let b = Rect::from_min_size(img_area.min + vec2(4.0, 4.0), vec2(46.0, 16.0));
                            p.rect_filled(b, 4.0, Color32::from_black_alpha(170));
                            p.text(b.center(), egui::Align2::CENTER_CENTER, t!("copie"), egui::FontId::proportional(11.0), Color32::WHITE);
                        }
                        // Nom lu par le lecteur d'écran ; infobulle : date, appareil, taille, emplacement.
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &photo.name));
                        let resp = resp.on_hover_ui(|ui| {
                            ui.label(egui::RichText::new(&photo.name).strong());
                            let mut line = library::format_date(photo.taken);
                            if !photo.camera.is_empty() {
                                line = format!("{line} · {}", photo.camera);
                            }
                            if let Ok(m) = std::fs::metadata(library::source(&photo.path)) {
                                line = format!("{line} · {}", library::format_bytes(m.len()));
                            }
                            ui.label(line);
                            ui.label(egui::RichText::new(photo.path.parent().map(|d| d.display().to_string()).unwrap_or_default()).weak());
                        });
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                        resp.context_menu(|ui| {
                            if ui.button(t!("Créer une copie virtuelle")).on_hover_text(t!("Même photo, réglages indépendants (pas de fichier image en plus)")).clicked() {
                                menu = Some((i, LibAction::Copy));
                                ui.close();
                            }
                            if library::copy_number(&photo.path).is_some() && ui.button(t!("Supprimer la copie virtuelle")).clicked() {
                                menu = Some((i, LibAction::DeleteCopy));
                                ui.close();
                            }
                            if ui.button(t!("Ouvrir l'emplacement du fichier")).clicked() {
                                menu = Some((i, LibAction::ShowFile));
                                ui.close();
                            }
                            if library::copy_number(&photo.path).is_none() && ui.button(t!("Mettre à la corbeille…")).on_hover_text(t!("Touche Suppr")).clicked() {
                                menu = Some((i, LibAction::Trash));
                                ui.close();
                            }
                        });
                        if resp.double_clicked() {
                            opened = Some(i);
                        }
                        if scroll_to && sel_row == Some(row) {
                            ui.scroll_to_rect(rect, None);
                        }
                        ui.add_space(gap);
                    }
                });
                ui.add_space(gap);
            }
        });
        if let Some(i) = clicked {
            let m = ui.input(|inp| inp.modifiers);
            if m.shift {
                let a = self.selected.unwrap_or(i);
                for k in a.min(i)..=a.max(i) {
                    if let Some(p) = self.photos.get(k) {
                        self.picked.insert(p.path.clone());
                    }
                }
            } else if m.command {
                if self.picked.is_empty() {
                    if let Some(p) = self.selected.and_then(|s| self.photos.get(s)) {
                        self.picked.insert(p.path.clone());
                    }
                }
                let p = self.photos[i].path.clone();
                if !self.picked.remove(&p) {
                    self.picked.insert(p);
                }
            } else {
                self.picked.clear();
            }
            self.selected = Some(i);
        }
        if let Some(i) = opened {
            self.open_index(i, ui.ctx());
        }
        if let Some((i, action)) = menu {
            if let Some(p) = self.photos.get(i).map(|p| p.path.clone()) {
                self.library_action(&p, action);
                self.refresh_dirs();
            }
        }
        // Les vignettes hors écran ne sont plus prioritaires.
        if let Some(t) = &self.thumbs {
            t.retain(&visible);
        }
        self.textures.retain(|p, t| visible.contains(p) || !matches!(t, Tex::Pending));
        if self.textures.len() > 1500 {
            self.textures.retain(|p, _| visible.contains(p));
        }
    }

    fn library_action(&mut self, p: &Path, action: LibAction) {
        let Some(cat) = &self.catalogue else { return };
        match action {
            LibAction::Copy => match cat.create_copy(p) {
                Ok(copy) => {
                    self.picked.clear();
                    self.refresh_list();
                    self.selected = self.photos.iter().position(|x| x.path == copy).or(self.selected);
                    self.scroll_to_sel = true;
                    self.status = t!("Copie virtuelle créée.").into();
                }
                Err(e) => self.status = tf!("Copie impossible : {e:#}", e),
            },
            LibAction::DeleteCopy => {
                if let Err(e) = cat.delete_copy(p) {
                    self.status = tf!("Suppression impossible : {e:#}", e);
                }
                self.picked.remove(p);
                self.textures.remove(p);
                if self.path.as_deref() == Some(p) {
                    // Copie ouverte : plus rien à enregistrer pour elle.
                    self.edits_since_open = false;
                    self.path = None;
                    self.size = None;
                }
                self.refresh_list();
            }
            LibAction::Trash => self.ask_trash(Some(p.to_path_buf())),
            LibAction::ShowFile => library::show_in_folder(p),
        }
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.leave_photo();
        self.mode = Mode::Editor;
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.status = tf!("Chargement de {0}…", path.display());
        let ready = {
            let mut slot = self.preloaded.lock().unwrap();
            match slot.take() {
                Some(p) if p.path == path && p.mtime.is_some() && p.mtime == file_mtime(&path) => Some(p),
                other => {
                    *slot = other;
                    None
                }
            }
        };
        if let Some(p) = ready {
            debug(format_args!("{} : préchargée", path.display()));
            let _ = tx.send(Ok((p.path, p.w, p.h, p.half, std::time::Duration::ZERO)));
            ctx.request_repaint();
            self.loading = Some(rx);
            return;
        }
        self.opening = Some(path.clone());
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let r = image_io::load(&path).map(|img| {
                let t_dec = t.elapsed();
                let half = img.to_half();
                debug(format_args!("décodage {:?}, conversion f16 {:?}", t_dec, t.elapsed() - t_dec));
                (path, img.width, img.height, half, t.elapsed())
            });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.loading = Some(rx);
    }

    /// Décode en tâche de fond la photo voisine dans le sens de navigation.
    fn preload_next(&mut self) {
        let Some(i) = self.lib_index else { return };
        let Some(next) = i.checked_add_signed(self.nav_dir).and_then(|j| self.photos.get(j)).map(|p| p.path.clone()) else { return };
        if self.preloaded.lock().unwrap().as_ref().is_some_and(|p| p.path == next) {
            return;
        }
        if self.preloading.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let (slot, busy) = (self.preloaded.clone(), self.preloading.clone());
        std::thread::spawn(move || {
            let mtime = file_mtime(&next);
            if let Ok(img) = image_io::load(&next) {
                let half = img.to_half();
                *slot.lock().unwrap() = Some(Preloaded { path: next, mtime, w: img.width, h: img.height, half });
            }
            busy.store(false, std::sync::atomic::Ordering::Release);
        });
    }

    /// Lance (ou relance) le calcul des masques IA en arrière-plan sur les photos du catalogue.
    fn start_bg_masks(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        if let Some(bg) = self.bg_masks.take() {
            bg.stop.store(true, Ordering::Release);
        }
        self.bg_scope = Some(self.folder.clone());
        let (Some(cat), Some(rs)) = (&self.catalogue, &self.rs) else { return };
        if !self.settings.ai_background || !ai::BACKGROUND_MODELS.iter().any(|m| ai::model_path(*m).exists()) {
            return;
        }
        // Photos du dossier affiché (les plus récentes d'abord, BG_LIMIT au plus), une fois par fichier :
        // les copies virtuelles partagent les masques de leur photo.
        let mut seen = HashSet::new();
        let photos: Vec<PathBuf> = cat
            .list(self.folder.as_deref(), Filter::All, Sort::DateDesc, None)
            .into_iter()
            .map(|p| p.path)
            .filter(|p| library::copy_number(p).is_none() && seen.insert(p.clone()))
            .take(BG_LIMIT)
            .filter(|p| ai::BACKGROUND_MODELS.iter().any(|m| ai::model_path(*m).exists() && !ai::cached(p, *m)))
            .collect();
        if photos.is_empty() {
            return;
        }
        let bg = BgMasks { done: Arc::new(AtomicUsize::new(0)), total: photos.len(), pause: Arc::new(AtomicBool::new(false)), stop: Arc::new(AtomicBool::new(false)) };
        let (done, pause, stop) = (bg.done.clone(), bg.pause.clone(), bg.stop.clone());
        let (device, queue, ctx) = (rs.device.clone(), rs.queue.clone(), ctx.clone());
        std::thread::spawn(move || {
            for p in photos {
                while pause.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                if stop.load(Ordering::Acquire) {
                    return;
                }
                if let Err(e) = ai::precompute(&p, &device, &queue) {
                    debug(format_args!("masques en arrière-plan, {} : {e:#}", p.display()));
                }
                done.fetch_add(1, Ordering::AcqRel);
                ctx.request_repaint();
            }
        });
        self.bg_masks = Some(bg);
    }

    fn dialog(&mut self, ctx: &egui::Context, f: impl FnOnce() -> Option<Picked> + Send + 'static) {
        if self.picking.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if let Some(p) = f() {
                let _ = tx.send(p);
            }
            ctx.request_repaint();
        });
        self.picking = Some(rx);
    }

    fn picked_logo(&mut self, f: PathBuf) {
        self.settings.export.watermark.image = Some(f);
        self.settings.export.watermark.kind = crate::watermark::Kind::Image;
        self.settings.save();
    }

    fn finish_load(&mut self, r: Loaded, rs: &egui_wgpu::RenderState) {
        self.opening = None;
        let (path, width, height, half, took) = match r {
            Ok(v) => v,
            Err(e) => {
                self.status = tf!("Échec du chargement : {e:#}", e);
                return;
            }
        };
        let t = std::time::Instant::now();
        match Developer::new_half(&rs.device, &rs.queue, rs.target_format, width, height, &half) {
            Ok(dev) => {
                rs.renderer.write().callback_resources.insert(dev);
                self.orig_half = Some(Arc::new(half));
                self.orig_dims = (width, height);
                self.work_half.clear();
                self.retouch_uploaded = None;
                self.denoise_img = None;
                self.denoise_uploaded = None;
                self.denoise_failed = None;
                self.erase_failed.clear();
                self.erase_strokes.clear();
            }
            Err(e) => {
                self.status = tf!("Échec GPU : {e:#}", e);
                return;
            }
        }
        self.adj = load_sidecar(&path).unwrap_or_default();
        self.committed = self.adj.clone();
        self.history = vec![self.adj.clone()];
        self.history_labels = vec![t!("Ouverture").into()];
        self.hist_pos = 0;
        self.snapshots = load_snapshots(&path);
        self.snap_name.clear();
        self.curve_uploaded.clear();
        self.lut_uploaded = None;
        self.screen_icc_uploaded = None;
        self.look_thumbs.clear();
        self.masks_uploaded.clear();
        self.sky_uploaded = None;
        self.depth_uploaded = None;
        self.subject_uploaded = None;
        self.face_uploaded = None;
        self.lens_uploaded = None;
        self.shot = Some(crate::lens::shot_cached(&path));
        self.ai_failed.clear();
        self.ai_masks = [Model::Sky, Model::Subject, Model::Depth].into_iter().chain(ai::FACE_MODELS).filter_map(|m| ai::cache_load(&path, m).map(|k| (m, Arc::new(k)))).collect();
        self.local_sel = None;
        self.hist_for = None;
        self.harmony_for = None;
        debug(format_args!("envoi GPU + mips {:?}", t.elapsed()));
        self.size = Some((width, height));
        self.fit = true;
        self.status = tf!("{0} · ouverte en {1:.1} s", path.display(), (took + t.elapsed()).as_secs_f32());
        self.lib_index = self.photos.iter().position(|p| p.path == path);
        self.edits_since_open = false;
        self.settings.last_photo = Some(path.clone());
        self.settings.save();
        if let Some(msg) = self.pending_status.take() {
            self.status = msg;
        }
        if let Some((p, prompt)) = self.pending_gen.take() {
            if p == path {
                // Toute l'image : un seul trait centré, assez large pour couvrir les coins.
                let strokes = vec![BrushStroke { points: vec![[0.5, 0.5]], radius: 0.5 * (1.0 + (height as f32 / width as f32).powi(2)).sqrt() + 0.05, hardness: 1.0, flow: 1.0, erase: false, fill: false }];
                self.adj.retouch.push(Retouch::Generate {
                    strokes,
                    model: self.settings.gen_model(),
                    prompt,
                    negative: self.settings.comfy_negative.clone(),
                    seed: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64 & 0xffff_ffff_ffff),
                    strength: 1.0,
                });
                self.status = t!("Génération de l'image en cours…").into();
            }
        }
        self.path = Some(path);
    }

    fn open_new_doc(&mut self) {
        if self.new_doc.is_none() {
            self.new_doc = Some(NewDoc { w: 1536, h: 1024, bg: [255, 255, 255], generate: false, prompt: String::new() });
        }
    }

    /// Dossier des nouveaux documents : Images/Luminux.
    fn docs_dir() -> PathBuf {
        dirs::picture_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir).join("Luminux")
    }

    fn new_doc_window(&mut self, ctx: &egui::Context) {
        let model = if self.gen_ready() { self.settings.gen_model() } else { String::new() };
        let Some(doc) = &mut self.new_doc else { return };
        let mut open = true;
        let mut create = false;
        let model_label = if crate::sdlocal::is_local(&model) { crate::sdlocal::model_name(&model) } else { model.clone() };
        egui::Window::new(t!("Nouveau document")).open(&mut open).collapsible(false).resizable(false).vscroll(true).default_height(ctx.content_rect().height() - 120.0).default_width(360.0).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, w, h) in DOC_SIZES {
                    if ui.selectable_label(doc.w == *w && doc.h == *h, crate::i18n::tr(name)).on_hover_text(format!("{w} × {h}")).clicked() {
                        (doc.w, doc.h) = (*w, *h);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(t!("Taille"));
                ui.add(egui::DragValue::new(&mut doc.w).range(64..=12000).suffix(" px"));
                ui.label("×");
                ui.add(egui::DragValue::new(&mut doc.h).range(64..=12000).suffix(" px"));
                if ui.small_button(t!("Inverser|échanger")).clicked() {
                    std::mem::swap(&mut doc.w, &mut doc.h);
                }
            });
            ui.horizontal(|ui| {
                ui.label(t!("Fond"));
                ui.color_edit_button_srgb(&mut doc.bg);
                for (name, c) in [(t!("Blanc"), [255, 255, 255]), (t!("Gris"), [128, 128, 128]), (t!("Noir"), [0, 0, 0])] {
                    if ui.small_button(name).clicked() {
                        doc.bg = c;
                    }
                }
            });
            ui.separator();
            ui.add_enabled_ui(!model.is_empty(), |ui| {
                ui.checkbox(&mut doc.generate, t!("Générer l'image par IA"));
            });
            if model.is_empty() {
                ui.small(t!("Pour générer : choisis le moteur IA dans Préférences (ComfyUI ou local)."));
                doc.generate = false;
            }
            if doc.generate {
                ui.add(egui::TextEdit::multiline(&mut doc.prompt).hint_text(t!("Décris l'image…")).desired_rows(3).desired_width(f32::INFINITY));
                ui.small(tf!("Modèle : {model}. L'image est calculée en petit puis agrandie à la taille du document.", model_label));
            }
            ui.add_space(4.0);
            let ok = !doc.generate || !doc.prompt.trim().is_empty();
            if ui.add_enabled(ok, egui::Button::new(t!("Créer"))).clicked() {
                create = true;
            }
        });
        if !open {
            self.new_doc = None;
        }
        if create {
            let doc = self.new_doc.take().unwrap();
            match Self::write_blank(&doc) {
                Ok(path) => {
                    self.add_doc_folder(&path, ctx);
                    if doc.generate {
                        self.pending_gen = Some((path.clone(), doc.prompt.trim().to_string()));
                        self.set_tab(Tab::Retouch);
                        self.retouch_tool = Some(RetouchTool::Generate);
                    }
                    self.open(path, ctx);
                }
                Err(e) => self.status = tf!("Document non créé : {e:#}", e),
            }
        }
    }

    /// Écrit l'image de fond dans Images/Luminux sous un nom libre (Document_1.png, Document_2.png…).
    fn write_blank(doc: &NewDoc) -> anyhow::Result<PathBuf> {
        let dir = Self::docs_dir();
        std::fs::create_dir_all(&dir)?;
        let path = (1..).map(|i| dir.join(format!("Document_{i}.png"))).find(|p| !p.exists()).unwrap();
        image::RgbImage::from_pixel(doc.w, doc.h, image::Rgb(doc.bg)).save(&path)?;
        Ok(path)
    }

    /// Historique cliquable et versions nommées de la photo ouverte.
    /// Onglet Historique : versions nommées et liste des modifications.
    fn history_ui(&mut self, ui: &mut egui::Ui) {
        let mut goto = None;
        let mut apply = None;
        let mut remove = None;
        let mut save_new = false;
        {
            ui.label(egui::RichText::new(t!("Versions")).strong());
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.snap_name).hint_text(t!("Nom, ex. noir et blanc")).desired_width(170.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button(t!("Enregistrer")).on_hover_text(t!("Garde l'état actuel sous ce nom")).clicked() || enter {
                    save_new = true;
                }
            });
            if self.snapshots.is_empty() {
                ui.small(t!("Aucune version : donne un nom à l'état actuel pour y revenir plus tard."));
            }
            for (i, snap) in self.snapshots.iter().enumerate() {
                ui.horizontal(|ui| {
                    let current = snap.adjustments == self.adj;
                    if ui.selectable_label(current, &snap.name).on_hover_text(t!("Appliquer cette version (annulable)")).clicked() {
                        apply = Some(i);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(t!("Supprimer")).on_hover_text(t!("Supprimer la version")).clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
            ui.separator();
            ui.label(egui::RichText::new(t!("Modifications")).strong());
            {
                for (i, label) in self.history_labels.iter().enumerate() {
                    let r = ui.selectable_label(i == self.hist_pos, egui::RichText::new(format!("{}. {label}", i + 1)).color(if i > self.hist_pos { ui.visuals().weak_text_color() } else { ui.visuals().text_color() }));
                    if r.clicked() {
                        goto = Some(i);
                    }
                }
            }
            ui.small(t!("Clic sur une ligne pour revenir à cet état ; une nouvelle modification efface les lignes grisées."));
        }
        if let Some(i) = goto {
            self.hist_pos = i;
            self.restore();
        }
        if let Some(i) = apply {
            self.adj = self.snapshots[i].adjustments.clone();
            self.edit_label = Some(tf!("Version « {0} »", self.snapshots[i].name));
        }
        let mut changed = false;
        if let Some(i) = remove {
            self.snapshots.remove(i);
            changed = true;
        }
        if save_new {
            let name = self.snap_name.trim();
            let name = if name.is_empty() { tf!("Version {0}", self.snapshots.len() + 1) } else { name.to_string() };
            match self.snapshots.iter_mut().find(|s| s.name == name) {
                Some(s) => s.adjustments = self.adj.clone(),
                None => self.snapshots.push(Snapshot { name, adjustments: self.adj.clone() }),
            }
            self.snap_name.clear();
            changed = true;
        }
        if changed {
            if let Some(p) = &self.path {
                if let Err(e) = save_snapshots(p, &self.adj, &self.snapshots) {
                    self.status = tf!("Impossible d'écrire {0} : {e}", sidecar_path(p).display(), e);
                }
            }
        }
    }

    fn undo(&mut self) {
        if self.hist_pos > 0 {
            self.hist_pos -= 1;
            self.restore();
        }
    }

    fn redo(&mut self) {
        if self.hist_pos + 1 < self.history.len() {
            self.hist_pos += 1;
            self.restore();
        }
    }

    fn restore(&mut self) {
        self.adj = self.history[self.hist_pos].clone();
        self.committed = self.adj.clone();
        self.save();
    }

    fn save(&mut self) {
        if let Some(p) = &self.path {
            if let Err(e) = save_sidecar(p, &self.adj) {
                self.status = tf!("Impossible d'écrire {0} : {e}", sidecar_path(p).display(), e);
            }
        }
    }

    /// Fenêtre d'export : options communes à l'export d'une photo et à l'export par lots.
    fn export_window(&mut self, ctx: &egui::Context) {
        if !self.export_open {
            return;
        }
        let batch = self.mode == Mode::Library;
        let n = if batch { self.targets().len() } else { usize::from(self.size.is_some()) };
        let mut open = true;
        let mut go = false;
        let mut pick_logo = false;
        let before = self.settings.export.clone();
        let fmt_before = self.settings.batch_format.clone();
        let screen = ctx.content_rect();
        let x = if self.settings.panel_left || batch { screen.right() - 380.0 } else { screen.left() + 12.0 };
        egui::Window::new(t!("Exporter"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .vscroll(true).default_height(ctx.content_rect().height() - 120.0)
            .default_pos(pos2(x, screen.top() + 60.0))
            .default_width(340.0)
            .show(ctx, |ui| {
            ui.set_width(340.0);
            let o = &mut self.settings.export;
            ui.label(egui::RichText::new(t!("Format")).strong());
            let mut f = self.settings.batch_format.clone();
            segmented(ui, &mut f, &[("jpg".to_string(), "JPEG"), ("webp".to_string(), "WebP"), ("avif".to_string(), "AVIF"), ("png".to_string(), "PNG"), ("tif".to_string(), "TIFF")]);
            ui.small(match f.as_str() {
                "jpg" => t!("8 bits, lu partout."),
                "webp" => t!("8 bits, environ un tiers plus léger que le JPEG à qualité égale. Navigateurs et logiciels récents."),
                "avif" => t!("8 bits, le plus léger, encodage plus lent. Toujours en sRGB."),
                "png" => t!("16 bits, sans perte, fichiers lourds."),
                _ => t!("16 bits, sans perte, pour l'impression ou un autre logiciel."),
            });
            self.settings.batch_format = f;
            let avif = self.settings.batch_format == "avif";
            ui.add_space(4.0);
            ui.label(egui::RichText::new(t!("Espace couleur")).strong());
            ui.add_enabled_ui(!avif, |ui| {
                segmented(ui, &mut o.space, &[(export::Space::Srgb, "sRGB"), (export::Space::DisplayP3, "Display P3"), (export::Space::AdobeRgb, "Adobe RGB")]);
            });
            ui.small(if avif { t!("AVIF : sRGB seulement (pas de profil ICC dans le fichier).") } else { match o.space {
                export::Space::Srgb => t!("Web, écrans courants, réseaux sociaux."),
                export::Space::DisplayP3 => t!("Écrans larges (Mac, téléphones récents)."),
                export::Space::AdobeRgb => t!("Impression et retouche dans un autre logiciel."),
            } });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(t!("Taille")).strong());
            let mut sized = o.long_edge > 0;
            segmented(ui, &mut sized, &[(false, t!("Originale")), (true, t!("Bord long"))]);
            if sized && o.long_edge == 0 {
                o.long_edge = 2048;
            } else if !sized {
                o.long_edge = 0;
            }
            if sized {
                ui.horizontal_wrapped(|ui| {
                    for px in [1080, 2048, 3840] {
                        if ui.selectable_label(o.long_edge == px, format!("{px}")).clicked() {
                            o.long_edge = px;
                        }
                    }
                    ui.add(egui::DragValue::new(&mut o.long_edge).range(64..=20000).suffix(" px"));
                });
                ui.small(t!("Une photo plus petite n'est jamais agrandie."));
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new(t!("Netteté de sortie")).strong());
            segmented(ui, &mut o.sharpen, &[(export::Sharpen::None, t!("Aucune")), (export::Sharpen::Screen, t!("Écran")), (export::Sharpen::Print, t!("Impression"))]);
            ui.add_space(4.0);
            if matches!(self.settings.batch_format.as_str(), "jpg" | "webp" | "avif") {
                ui.horizontal(|ui| {
                    ui.label(t!("Qualité"));
                    ui.add(egui::Slider::new(&mut o.quality, 50..=100));
                });
            }
            ui.checkbox(&mut o.metadata, t!("Garder les métadonnées (EXIF)"));
            ui.add_enabled_ui(o.metadata, |ui| ui.checkbox(&mut o.gps, t!("Avec la position GPS")));
            ui.small(t!("Le profil ICC de l'espace choisi est intégré au fichier (sauf AVIF). Le TIFF ne reçoit pas les EXIF."));
            ui.add_space(4.0);
            pick_logo = owner_ui(ui, o);
            ui.separator();
            let label = if batch { tf!("Choisir le dossier et exporter {0} photo(s)…", n) } else { t!("Choisir le fichier et exporter…").to_string() };
            if ui.add_enabled(n > 0 && self.batch.is_none(), egui::Button::new(label)).clicked() {
                go = true;
            }
        });
        if self.settings.export != before || self.settings.batch_format != fmt_before {
            self.settings.save();
        }
        self.export_open = open && !go;
        if pick_logo {
            self.pick_logo(ctx);
        }
        if go {
            if batch {
                self.dialog(ctx, || rfd::FileDialog::new().set_title(t!("Dossier d'export")).pick_folder().map(Picked::BatchExport));
            } else {
                self.pick_export_file(ctx);
            }
        }
    }

    fn pick_export_file(&mut self, ctx: &egui::Context) {
        let Some(path) = &self.path else { return };
        let stem = library::export_stem(path);
        let dir = path.parent().map(Path::to_path_buf);
        // Le format vient de la fenêtre Exporter : le dialogue ne propose que celui-là.
        let ext = self.settings.batch_format.clone();
        let name = match ext.as_str() {
            "webp" => "WebP",
            "avif" => "AVIF",
            "png" => "PNG",
            "tif" => "TIFF",
            _ => "JPEG",
        };
        self.dialog(ctx, move || {
            let mut d = rfd::FileDialog::new().set_file_name(format!("{stem}.{ext}")).add_filter(name, &[ext.as_str()]);
            if let Some(dir) = dir {
                d = d.set_directory(dir);
            }
            d.save_file().map(Picked::Export)
        });
    }

    fn export_to(&mut self, out: PathBuf, rs: &egui_wgpu::RenderState, ctx: &egui::Context) {
        let out = if out.extension().is_none() { out.with_extension(&self.settings.batch_format) } else { out };
        let renderer = rs.renderer.read();
        let Some(dev) = renderer.callback_resources.get::<Developer>() else { return };
        let (w, h, pixels) = match dev.render_full(&rs.device, &rs.queue, &self.adj) {
            Ok(v) => v,
            Err(e) => {
                self.status = tf!("Échec de l'export : {e:#}", e);
                return;
            }
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let (src, opts) = (self.path.clone(), self.settings.export.clone());
        self.status = tf!("Export vers {0}…", out.display());
        std::thread::spawn(move || {
            let _ = tx.send(export::write(&out, src.as_deref(), w, h, &pixels, &opts).map(|_| out));
            ctx.request_repaint();
        });
        self.exporting = Some(rx);
    }

    fn export_cutout(&mut self, out: PathBuf, rs: &egui_wgpu::RenderState, ctx: &egui::Context) {
        let renderer = rs.renderer.read();
        let Some(dev) = renderer.callback_resources.get::<Developer>() else { return };
        let (w, h, pixels) = match dev.render_full_with(&rs.device, &rs.queue, &self.adj, true) {
            Ok(v) => v,
            Err(e) => {
                self.status = tf!("Échec de l'export : {e:#}", e);
                return;
            }
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.status = tf!("Export détouré vers {0}…", out.display());
        std::thread::spawn(move || {
            let _ = tx.send(image_io::export_rgba(&out, w, h, &pixels).map(|_| out));
            ctx.request_repaint();
        });
        self.exporting = Some(rx);
    }

    /// Envoie la courbe au GPU si elle a changé et recalcule l'histogramme.
    fn sync_gpu(&mut self, rs: &egui_wgpu::RenderState, ctx: &egui::Context) {
        let mut renderer = rs.renderer.write();
        let Some(dev) = renderer.callback_resources.get_mut::<Developer>() else { return };
        if self.looks_visible && self.render_look_thumbs(dev, rs, ctx) {
            // Les aperçus ont changé la courbe et la LUT du GPU : on remet celles de la photo.
            self.curve_uploaded.clear();
            self.lut_uploaded = None;
        }
        // Débruitage IA : résultat en cache chargé, puis image source refaite quand il change (pas pendant
        // qu'un curseur est tiré : le mélange sur toute la photo prend un moment).
        let den_file = if self.adj.denoise.active() { self.path.as_deref().and_then(|p| crate::denoise::cache_file(p, &self.adj.denoise.model)) } else { None };
        if let Some(f) = &den_file {
            if self.denoise_img.as_ref().map(|d| &d.0) != Some(f) && f.exists() {
                if let Some(img) = crate::denoise::load(f) {
                    self.denoise_img = Some((f.clone(), Arc::new(img)));
                }
            }
        }
        let den = self.denoise_img.as_ref().filter(|d| Some(&d.0) == den_file.as_ref()).map(|d| d.1.clone());
        let den_key = (self.adj.denoise.model.clone(), self.adj.denoise.amount, den.is_some());
        if self.denoise_uploaded.as_ref() != Some(&den_key) && !ctx.input(|i| i.pointer.any_down()) {
            self.denoise_uploaded = Some(den_key);
            self.retouch_uploaded = None;
        }
        if self.retouch_uploaded.as_ref() != Some(&self.adj.retouch) {
            if let Some(mut orig) = self.orig_half.clone() {
                let changed = !self.work_half.is_empty() || !self.adj.retouch.is_empty() || den.is_some();
                if let Some(d) = &den {
                    let mut base = (*orig).clone();
                    crate::denoise::blend(&mut crate::retouch::HalfCanvas { data: &mut base, w: dev.width, h: dev.height }, d, self.adj.denoise.amount);
                    orig = Arc::new(base);
                }
                let mut work = (*orig).clone();
                let missing = {
                    let mut canvas = crate::retouch::HalfCanvas { data: &mut work, w: dev.width, h: dev.height };
                    crate::retouch::apply_all(&mut canvas, self.path.as_deref(), &self.adj.retouch, false)
                };
                if changed {
                    dev.upload_image(&rs.device, &rs.queue, &work);
                }
                // Les masques IA restent calculés sur la photo d'origine ; les retouches y sont reportées.
                dev.set_retouch(&self.adj.retouch);
                self.masks_uploaded.clear();
                self.sky_uploaded = None;
                self.subject_uploaded = None;
                self.face_uploaded = None;
                self.depth_uploaded = None;
                self.work_half = work;
                if self.erase_job.is_none() {
                    if let (Some(&index), Some(photo)) = (missing.iter().find(|i| !self.erase_failed.contains(i)), self.path.clone()) {
                        let ops = self.adj.retouch[..=index].to_vec();
                        let (w, h) = (dev.width, dev.height);
                        let (tx, rx) = channel();
                        let (ctx2, photo2) = (ctx.clone(), photo.clone());
                        std::thread::spawn(move || {
                            let r = (|| -> anyhow::Result<String> {
                                let mut work = (*orig).clone();
                                let mut canvas = crate::retouch::HalfCanvas { data: &mut work, w, h };
                                crate::retouch::apply_all(&mut canvas, Some(&photo2), &ops[..index], false);
                                let (patch, msg) = crate::retouch::compute_patch(&canvas, &ops[index])?;
                                let f = crate::retouch::cache_file(&photo2, &ops).ok_or_else(|| anyhow::anyhow!(t!("photo illisible")))?;
                                crate::retouch::cache_store(&f, &patch)?;
                                Ok(msg)
                            })();
                            let _ = tx.send(r);
                            ctx2.request_repaint();
                        });
                        self.erase_job = Some(EraseJob { index, photo, start: std::time::Instant::now(), rx });
                    }
                }
            }
            self.retouch_uploaded = Some(self.adj.retouch.clone());
        }
        if self.curve_uploaded != self.adj.curve {
            dev.set_curve(&rs.queue, &self.adj.curve_lut());
            self.curve_uploaded = self.adj.curve.clone();
        }
        for (i, l) in self.adj.locals.iter().take(MAX_LOCALS).enumerate() {
            let m = match l.shape {
                Shape::Ai { kind, .. } => self.ai_masks.get(&Model::of(kind)),
                _ => None,
            };
            let key: MaskKey = (l.shape.clone(), l.invert, l.amount, m.is_some());
            if self.masks_uploaded.get(i).and_then(|k| k.as_ref()) != Some(&key) {
                dev.upload_local(&rs.queue, i, l, m.map(|m| m.as_ref()));
                if self.masks_uploaded.len() <= i {
                    self.masks_uploaded.resize(i + 1, None);
                }
                self.masks_uploaded[i] = Some(key);
            }
        }
        if self.lut_uploaded.as_ref() != Some(&self.adj.lut) {
            let lut = self.adj.lut.as_deref().and_then(looks::load_lut);
            dev.set_lut(&rs.queue, lut.as_deref());
            self.lut_uploaded = Some(self.adj.lut.clone());
        }
        if self.screen_icc_uploaded.as_ref() != Some(&self.settings.screen_icc) {
            let lut = match crate::icc::from_setting(&self.settings.screen_icc) {
                Ok(l) => {
                    self.screen_icc_msg = match (&l, self.settings.screen_icc.as_str()) {
                        (_, "") => String::new(),
                        (None, _) => t!("Aucun profil déclaré pour l'écran (colord) : affichage sRGB.").to_string(),
                        (Some(_), "auto") => crate::icc::detect().map(|p| p.display().to_string()).unwrap_or_default(),
                        (Some(_), p) => p.to_string(),
                    };
                    l
                }
                Err(e) => {
                    self.screen_icc_msg = tf!("Profil de l'écran inutilisable : {e:#}", e);
                    None
                }
            };
            dev.set_screen_lut(&rs.queue, lut.as_ref());
            self.screen_icc_uploaded = Some(self.settings.screen_icc.clone());
        }
        if self.lens_uploaded.as_ref() != Some(&self.adj.lens) {
            if let Some(p) = &self.path {
                crate::lens::setup(dev, p, &self.adj.lens);
            }
            self.lens_geo = dev.lens();
            self.lens_uploaded = Some(self.adj.lens.clone());
        }
        // Harmoniser : couleurs de la photo développée sans ses calques, relevées quand elle change.
        if self.adj.layers.iter().any(|l| l.harmony > 0.0) {
            let mut base = self.adj.clone();
            base.layers.clear();
            if self.harmony_for.as_ref() != Some(&base) && self.last_harmony.elapsed().as_millis() >= 250 {
                self.last_harmony = std::time::Instant::now();
                let (w, h) = small_size(&base, dev.width, dev.height, 200);
                let u = base.uniforms([0.0, 0.0, 1.0, 1.0], false, false, [dev.width as f32, dev.height as f32], false);
                if let Ok(px) = dev.render_to_cpu(&rs.device, &rs.queue, &u, w, h) {
                    let t = crate::layers::stats(px.iter().map(|p| ([p[0], p[1], p[2]], 1.0))).map(|v| (v * 512.0).round() / 512.0);
                    self.harmony_t = t;
                }
                self.harmony_for = Some(base);
            } else if self.harmony_for.as_ref() != Some(&base) {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
            let t = self.harmony_t;
            for l in self.adj.layers.iter_mut().filter(|l| l.harmony > 0.0 && l.harmony_target != t) {
                l.harmony_target = t;
            }
        }
        if self.adj.layers.iter().any(crate::layers::subject_pending) {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        dev.set_layers(&rs.device, &rs.queue, &self.adj.layers);
        let sky_key = (self.adj.sky.image.clone(), self.ai_masks.contains_key(&Model::Sky));
        if self.sky_uploaded.as_ref() != Some(&sky_key) {
            let img = self.adj.sky.image.as_deref().and_then(sky::load);
            dev.set_sky(&rs.queue, img.as_deref());
            dev.set_sky_mask(&rs.queue, self.ai_masks.get(&Model::Sky).map(|m| m.as_ref()));
            self.sky_uploaded = Some(sky_key);
        }
        let subject_key = self.ai_masks.contains_key(&Model::Subject);
        if self.subject_uploaded != Some(subject_key) {
            dev.set_subject(&rs.queue, self.ai_masks.get(&Model::Subject).map(|m| m.as_ref()));
            self.subject_uploaded = Some(subject_key);
        }
        let face_key = ai::FACE_MODELS.iter().all(|m| self.ai_masks.contains_key(m));
        if self.face_uploaded != Some(face_key) {
            let masks = ai::FACE_MODELS.map(|m| self.ai_masks.get(&m).cloned());
            let all: Option<Vec<Arc<AiMask>>> = masks.into_iter().collect();
            dev.set_face(&rs.queue, all.as_ref().map(|v| [v[0].as_ref(), v[1].as_ref(), v[2].as_ref(), v[3].as_ref()]));
            self.face_uploaded = Some(face_key);
        }
        let depth_key = self.ai_masks.contains_key(&Model::Depth);
        if self.depth_uploaded != Some(depth_key) {
            dev.set_depth(&rs.queue, self.ai_masks.get(&Model::Depth).map(|m| m.as_ref()));
            self.depth_uploaded = Some(depth_key);
        }
        // Masque IA manquant : on lance son calcul (un à la fois), à partir de la source sans réglages.
        if self.ai_job.is_none() {
            let extra = [self.depth_show_mask.then_some(Model::Depth), self.cutout_active.then_some(Model::Subject)];
            let want = ai::needed(&self.adj).into_iter().chain(extra.into_iter().flatten()).find(|m| !self.ai_masks.contains_key(m) && !self.ai_failed.contains(m));
            if let (Some(model), Some(photo)) = (want, self.path.clone()) {
                match ai::render_guide(dev, &rs.device, &rs.queue) {
                    Ok(guide) => {
                        let (w, h) = (dev.mask_w, dev.mask_h);
                        let progress = Arc::new(ai::Progress::default());
                        let (tx, rx) = channel();
                        let (p2, photo2, ctx2) = (progress.clone(), photo.clone(), ctx.clone());
                        std::thread::spawn(move || {
                            let r = ai::ensure_model(model, &p2).and_then(|_| ai::compute_set(model, &guide, w, h));
                            for (m, mask) in r.iter().flatten() {
                                if let Err(e) = ai::cache_store(&photo2, *m, mask) {
                                    eprintln!("cache du masque : {e:#}");
                                }
                            }
                            let _ = tx.send(r);
                            ctx2.request_repaint();
                        });
                        self.ai_job = Some(AiJob { model, photo, progress, rx });
                    }
                    Err(e) => {
                        self.status = tf!("Masque IA impossible : {e:#}", e);
                        self.ai_failed.insert(model);
                    }
                }
            }
        }
        let key = if self.before { None } else { Some(self.adj.clone()) };
        let stale = self.hist_for.as_ref() != Some(&key.clone().unwrap_or_default()) || self.histogram.is_none();
        // Pendant un glissement, l'histogramme suit au plus 10 fois par seconde.
        if stale && self.last_hist.elapsed().as_millis() < 100 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Pendant un tracé ou un déplacement sur la photo, pas d'histogramme : sa relecture du GPU
        // ralentirait le geste. Il est refait au lâcher.
        if stale && !self.painting && self.last_hist.elapsed().as_millis() >= 100 {
            self.last_hist = std::time::Instant::now();
            let (w, h) = small_size(&self.adj, dev.width, dev.height, 400);
            let u = self.adj.uniforms([0.0, 0.0, 1.0, 1.0], self.before, false, [dev.width as f32, dev.height as f32], false);
            if let Ok(px) = dev.render_to_cpu(&rs.device, &rs.queue, &u, w, h) {
                let mut hist = Box::new([[0u32; 256]; 4]);
                for p in &px {
                    for k in 0..3 {
                        hist[k][(p[k].clamp(0.0, 1.0) * 255.0) as usize] += 1;
                    }
                    let l = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                    hist[3][(l.clamp(0.0, 1.0) * 255.0) as usize] += 1;
                }
                self.histogram = Some(hist);
            }
            self.hist_for = Some(key.unwrap_or_default());
        }
    }

    fn categories(&self) -> Vec<String> {
        let mut c: Vec<String> = self.looks.iter().map(|l| l.category.clone()).collect();
        c.dedup();
        c
    }

    /// Calcule quelques aperçus manquants du pack affiché (deux par image pour rester fluide).
    /// Renvoie vrai si la courbe ou la LUT du GPU ont été modifiées.
    fn render_look_thumbs(&mut self, dev: &mut Developer, rs: &egui_wgpu::RenderState, ctx: &egui::Context) -> bool {
        let Some(cat) = self.look_category.clone() else { return false };
        let todo: Vec<Look> = self
            .looks
            .iter()
            .filter(|l| l.category == cat && !self.look_thumbs.contains_key(&l.file))
            .take(2)
            .cloned()
            .collect();
        if todo.is_empty() {
            return false;
        }
        let src = [dev.width as f32, dev.height as f32];
        for look in todo {
            let adj = self.adj.with_look(&look.adjustments);
            // Les masques locaux sont ceux de la photo : déjà sur le GPU.
            dev.set_curve(&rs.queue, &adj.curve_lut());
            let lut = adj.lut.as_deref().and_then(looks::load_lut);
            dev.set_lut(&rs.queue, lut.as_deref());
            let (w, h) = small_size(&adj, dev.width, dev.height, 200);
            let u = adj.uniforms([0.0, 0.0, 1.0, 1.0], false, false, src, false);
            if let Ok(px) = dev.render_to_cpu(&rs.device, &rs.queue, &u, w, h) {
                let rgba: Vec<u8> = px.iter().flat_map(|p| [p[0], p[1], p[2], 1.0].map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)).collect();
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                let tex = ctx.load_texture(format!("look {}", look.file.display()), img, egui::TextureOptions::LINEAR);
                self.look_thumbs.insert(look.file.clone(), tex);
            }
        }
        ctx.request_repaint();
        true
    }

    fn start_import(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.status = tf!("Import de {0} élément(s)…", paths.len());
        std::thread::spawn(move || {
            let _ = tx.send(looks::import(&paths));
            ctx.request_repaint();
        });
        self.importing = Some(rx);
    }

    fn finish_import(&mut self, r: ImportReport) {
        let before: HashSet<String> = self.categories().into_iter().collect();
        self.looks = looks::list();
        self.look_thumbs.clear();
        // On affiche le premier pack nouvellement ajouté.
        if let Some(c) = self.categories().into_iter().find(|c| !before.contains(c)) {
            self.look_category = Some(c);
        }
        self.status = r.summary();
        let mut text = r.summary() + ".";
        if !r.ignored.is_empty() {
            text += t!("\n\nRéglages d'origine sans équivalent dans Luminux (rendu approché) :");
            for (k, n) in &r.ignored {
                text += &tf!("\n• {k} ({n} preset{0})", k, n, if *n > 1 { "s" } else { "" });
            }
        }
        if !r.errors.is_empty() {
            text += t!("\n\nErreurs :");
            for e in r.errors.iter().take(20) {
                text += &format!("\n• {e}");
            }
        }
        self.import_report = Some(text);
    }

    fn looks_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.looks_visible = true;
        let cats = self.categories();
        if self.look_category.as_ref().is_none_or(|c| !cats.contains(c)) {
            self.look_category = cats.first().cloned();
        }
        ui.horizontal(|ui| {
            if ui.button(t!("Importer…")).on_hover_text(t!("LUT .cube, presets Lightroom (.xmp, .lrtemplate), Luminar (.lnpc, .lnp), ou un .zip de pack. Tu peux aussi glisser les fichiers sur la fenêtre.")).clicked() {
                self.dialog(ctx, || {
                    rfd::FileDialog::new()
                        .add_filter(t!("Looks et presets"), LOOK_EXTS)
                        .pick_files()
                        .map(Picked::Import)
                });
            }
            if ui.button(t!("Dossier…")).on_hover_text(t!("Importer tout un dossier de packs")).clicked() {
                self.dialog(ctx, || rfd::FileDialog::new().pick_folder().map(|d| Picked::Import(vec![d])));
            }
            if ui.add_enabled(self.size.is_some(), egui::Button::new(t!("Enregistrer…"))).on_hover_text(t!("Enregistrer les réglages actuels dans « Mes looks »")).clicked() {
                self.new_look_name = Some(String::new());
            }
            if self.importing.is_some() {
                ui.spinner();
            }
        });
        if let Some(mut name) = self.new_look_name.take() {
            let mut keep = true;
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(t!("Nom du look")).desired_width(200.0));
                if name.is_empty() && !r.has_focus() {
                    r.request_focus();
                }
                let enter = (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button(t!("OK")).clicked() || enter {
                    keep = false;
                    match looks::save_user_look(&name, &self.adj) {
                        Ok(()) => {
                            self.looks = looks::list();
                            self.look_category = Some(looks::USER_CATEGORY.into());
                            self.look_thumbs.clear();
                            self.status = tf!("Look « {0} » enregistré.", name.trim());
                        }
                        Err(e) => self.status = tf!("Look non enregistré : {e:#}", e),
                    }
                }
                if ui.button(t!("Annuler")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
            if keep {
                self.new_look_name = Some(name);
            }
        }
        if cats.is_empty() {
            ui.small(t!("Aucun look. Importe tes packs Skylum, Lightroom ou des LUT .cube."));
            return;
        }
        ui.horizontal(|ui| {
            let current = self.look_category.clone().unwrap_or_default();
            egui::ComboBox::from_id_salt("pack").width(250.0).selected_text(crate::i18n::tr(&current)).show_ui(ui, |ui| {
                for c in &cats {
                    let n = self.looks.iter().filter(|l| &l.category == c).count();
                    ui.selectable_value(&mut self.look_category, Some(c.clone()), format!("{} ({n})", crate::i18n::tr(c)));
                }
            });
            if self.confirm_remove {
                if ui.button(t!("Confirmer")).on_hover_text(t!("Supprime ce pack de Luminux (tes fichiers d'origine ne sont pas touchés)")).clicked() {
                    if let Err(e) = looks::remove_category(&current) {
                        self.status = tf!("Suppression impossible : {e:#}", e);
                    }
                    self.looks = looks::list();
                    self.confirm_remove = false;
                }
            } else if ui.button(t!("Retirer")).clicked() {
                self.confirm_remove = true;
            }
        });
        let cat = self.look_category.clone();
        let items: Vec<Look> = self.looks.iter().filter(|l| Some(&l.category) == cat.as_ref()).cloned().collect();
        let mut chosen = None;
        let mut removed: Option<Look> = None;
        let w = (ui.available_width() - 24.0) / 3.0;
        egui::Grid::new("looks").spacing(vec2(4.0, 4.0)).show(ui, |ui| {
            for (i, look) in items.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(w);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, w * 0.7), Sense::click());
                    let p = ui.painter_at(rect);
                    p.rect_filled(rect, 3.0, Color32::from_gray(35));
                    if let Some(t) = self.look_thumbs.get(&look.file) {
                        let s = t.size_vec2();
                        let k = (rect.width() / s.x).min(rect.height() / s.y);
                        p.image(t.id(), Rect::from_center_size(rect.center(), s * k), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    } else {
                        p.text(rect.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(16.0), Color32::GRAY);
                    }
                    if resp.hovered() {
                        p.rect_stroke(rect, 3.0, Stroke::new(2.0, Color32::from_rgb(90, 160, 255)), egui::StrokeKind::Inside);
                    }
                    let mut tip = format!("{} ({})", look.name, look.source);
                    if !look.ignored.is_empty() {
                        tip += t!("\nSans équivalent : ");
                        tip += &look.ignored.join(", ");
                    }
                    let resp = resp.on_hover_text(tip);
                    if resp.clicked() {
                        chosen = Some(look.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button(t!("Supprimer ce look")).clicked() {
                            removed = Some(look.clone());
                            ui.close();
                        }
                    });
                    ui.add(egui::Label::new(egui::RichText::new(&look.name).small()).truncate());
                });
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
        if let Some(look) = removed {
            if let Err(e) = looks::remove_look(&look) {
                self.status = tf!("Suppression impossible : {e:#}", e);
            }
            self.looks = looks::list();
        }
        if let Some(look) = chosen {
            self.adj = self.adj.with_look(&look.adjustments);
            self.status = tf!("Look appliqué : {0} ({1})", look.name, look.source);
        }
        if let Some(lut) = self.adj.lut.clone() {
            ui.horizontal(|ui| {
                ui.label(tf!("LUT : {0}", lut.rsplit('/').next().unwrap_or(&lut)));
                if ui.small_button(t!("Retirer la LUT")).clicked() {
                    self.adj.lut = None;
                }
            });
            slider(ui, t!("Intensité LUT"), &mut self.adj.lut_amount, 0.0..=1.0, 1.0);
        }
    }

    /// Réglage automatique : exposition sur la médiane, blancs/noirs sur les extrêmes,
    /// un peu de contraste et de vibrance. Heuristique, pas un modèle d'IA.
    fn auto_enhance(&mut self) {
        let Some(rs) = self.rs.clone() else { return };
        let renderer = rs.renderer.read();
        let Some(dev) = renderer.callback_resources.get::<Developer>() else { return };
        let base = Adjustments { crop: self.adj.crop, angle: self.adj.angle, ..Default::default() };
        let (w, h) = small_size(&base, dev.width, dev.height, 400);
        let u = base.uniforms([0.0, 0.0, 1.0, 1.0], false, false, [dev.width as f32, dev.height as f32], false);
        let Ok(px) = dev.render_to_cpu(&rs.device, &rs.queue, &u, w, h) else { return };
        drop(renderer);
        let mut l: Vec<f32> = px.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
        l.sort_by(f32::total_cmp);
        let pct = |q: f32| l[((l.len() - 1) as f32 * q) as usize];
        let (p01, p05, p50, p99) = (pct(0.01), pct(0.05), pct(0.5), pct(0.99));
        let median_lin = image_io::srgb_to_linear(p50).max(1e-4);
        let a = &mut self.adj;
        a.exposure = (0.18 / median_lin).log2().clamp(-2.0, 2.0);
        // Valeurs après exposition, approximées en encodé.
        let k = 2f32.powf(a.exposure / 2.4);
        let (hi, lo, sh) = ((p99 * k).min(1.2), p01 * k, p05 * k);
        a.highlights = if hi > 0.97 { -0.4 } else { 0.0 };
        a.whites = if hi < 0.9 { ((0.97 - hi) * 2.0).min(0.6) } else { 0.0 };
        a.shadows = if sh < 0.08 { 0.3 } else { 0.0 };
        a.blacks = if lo > 0.05 { -(lo * 3.0).min(0.5) } else { 0.0 };
        a.contrast = 0.1;
        a.vibrance = 0.2;
    }

    /// Applique le ratio choisi au recadrage actuel, centré, puis supprime les coins vides.
    fn apply_aspect(&mut self) {
        let Some((w, h)) = self.size else { return };
        let (w, h) = (w as f32, h as f32);
        let mut ratio = ASPECTS[self.crop_aspect].1;
        if ratio == 0.0 {
            return;
        }
        if ratio < 0.0 {
            ratio = w / h;
            if self.crop_portrait != (h > w) {
                ratio = 1.0 / ratio;
            }
        } else if self.crop_portrait {
            ratio = 1.0 / ratio;
        }
        let c = self.adj.crop;
        let center = [c[0] + c[2] / 2.0, c[1] + c[3] / 2.0];
        // Largeur et hauteur normalisées telles que (cw*w)/(ch*h) = ratio, au plus grand dans l'actuel.
        let (mut cw, mut ch) = (c[2], c[2] * w / (ratio * h));
        if ch > c[3] {
            ch = c[3];
            cw = ch * h * ratio / w;
        }
        let nc = [center[0] - cw / 2.0, center[1] - ch / 2.0, cw, ch];
        self.adj.crop = fit_crop(nc, self.adj.angle, w, h);
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let sc = |m, k| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        if sc(Modifiers::COMMAND, Key::O) {
            self.pick_open(ctx);
        }
        if sc(Modifiers::COMMAND, Key::N) {
            self.open_new_doc();
        }
        if sc(Modifiers::COMMAND, Key::F) {
            self.open_search();
        }
        if sc(Modifiers::COMMAND, Key::Comma) {
            self.prefs_open = !self.prefs_open;
        }
        if sc(Modifiers::COMMAND, Key::Q) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if sc(Modifiers::NONE, Key::F1) {
            self.keys_open = !self.keys_open;
        }
        if sc(Modifiers::NONE, Key::F11) {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }
        if sc(Modifiers::COMMAND, Key::E) && (self.mode == Mode::Library || self.size.is_some()) {
            self.export_open = true;
        }
        if sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::C) {
            self.copy_settings();
        }
        if sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::V) {
            self.paste_settings();
        }
        if sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || sc(Modifiers::COMMAND, Key::Y) {
            self.redo();
        }
        if sc(Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
        if sc(Modifiers::COMMAND, Key::Num0) {
            self.fit = true;
        }
        if sc(Modifiers::COMMAND, Key::Num1) {
            self.fit = false;
            self.zoom = 1.0;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let digits = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5];
        for (r, k) in digits.iter().enumerate() {
            if ctx.input(|i| i.key_pressed(*k) && i.modifiers.is_none()) {
                self.set_rating(r as u8);
            }
        }
        if self.mode == Mode::Editor {
            if ctx.input(|i| i.key_pressed(Key::P) && i.modifiers.is_none()) {
                self.toggle_panel();
            }
            if ctx.input(|i| i.key_pressed(Key::B)) {
                self.before = !self.before;
            }
            if ctx.input(|i| i.key_pressed(Key::Y)) {
                self.split_view = !self.split_view;
            }
            if ctx.input(|i| i.key_pressed(Key::M)) {
                self.show_mask = !self.show_mask;
            }
            if ctx.input(|i| i.key_pressed(Key::H) && i.modifiers.is_none()) {
                self.toggle_history();
            }
            if ctx.input(|i| i.key_pressed(Key::OpenBracket)) {
                self.brush.radius = (self.brush.radius / 1.2).max(BRUSH_MIN);
            }
            if ctx.input(|i| i.key_pressed(Key::CloseBracket)) {
                self.brush.radius = (self.brush.radius * 1.2).min(0.3);
            }
            if ctx.input(|i| i.key_pressed(Key::C)) && self.size.is_some() {
                if self.tab == Tab::Crop {
                    self.crop_mode = !self.crop_mode;
                    self.fit = true;
                } else {
                    self.set_tab(Tab::Crop);
                }
            }
            if self.crop_mode && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.crop_mode = false;
                self.fit = true;
                return;
            }
            if ctx.memory(|m| m.focused().is_none()) && self.loading.is_none() {
                if let Some(i) = self.lib_index {
                    if ctx.input(|k| k.key_pressed(Key::ArrowRight)) && i + 1 < self.photos.len() {
                        self.open_index(i + 1, ctx);
                    } else if ctx.input(|k| k.key_pressed(Key::ArrowLeft)) && i > 0 {
                        self.open_index(i - 1, ctx);
                    }
                }
            }
            if ctx.input(|i| i.key_pressed(Key::G) || i.key_pressed(Key::Escape)) {
                self.leave_photo();
                self.mode = Mode::Library;
                self.scroll_to_sel = true;
            }
        } else {
            if ctx.input(|i| i.key_pressed(Key::I) && i.modifiers.is_none()) {
                self.info_open = !self.info_open;
            }
            self.library_keys(ctx);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        // Passe à la ligne quand la fenêtre est étroite (la barre de la bibliothèque débordait à 1400 px).
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(self.mode == Mode::Library, egui::RichText::new(t!("Bibliothèque")).strong()).on_hover_text(t!("G ou Échap")).clicked() {
                self.leave_photo();
                self.mode = Mode::Library;
                self.scroll_to_sel = true;
            }
            if ui
                .add_enabled(self.size.is_some(), egui::Button::selectable(self.mode == Mode::Editor, egui::RichText::new(t!("Édition")).strong()))
                .clicked()
            {
                self.mode = Mode::Editor;
            }
            ui.separator();
            if self.mode == Mode::Library {
                self.library_toolbar(ui);
                return;
            }
            // Seulement l'état de la vue et la note : les commandes sont dans les menus.
            ui.toggle_value(&mut self.before, t!("Avant")).on_hover_text(t!("Afficher l'original (B)"));
            ui.toggle_value(&mut self.split_view, t!("Avant/Après")).on_hover_text(t!("Rideau glissant entre l'original et le résultat (Y)"));
            ui.separator();
            if ui.selectable_label(self.fit, t!("Ajuster")).on_hover_text(t!("Ctrl+0")).clicked() {
                self.fit = true;
            }
            if ui.selectable_label(!self.fit && (self.zoom - 1.0).abs() < 1e-3, "100 %").on_hover_text(t!("Ctrl+1")).clicked() {
                self.fit = false;
                self.zoom = 1.0;
            }
            if self.lib_index.is_some() {
                ui.separator();
                let r = self.current_rating();
                let (rect, resp) = ui.allocate_exact_size(vec2(5.0 * 16.0, 16.0), Sense::click());
                paint_stars(ui.painter(), rect.min, r, 16.0);
                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let k = (((pos.x - rect.left()) / 16.0).floor() as u8 + 1).min(5);
                        self.set_rating(if r == k { 0 } else { k });
                    }
                }
                resp.on_hover_text(t!("Note : clic ou touches 1 à 5, 0 pour effacer"));
                if let Some(i) = self.lib_index {
                    ui.label(format!("{}/{}", i + 1, self.photos.len())).on_hover_text(t!("Flèches gauche/droite : photo précédente/suivante"));
                }
            }
        });
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, palette::VIEW);
        // Pendant le décodage : la vignette agrandie (douce) au lieu d'un écran vide ou de la photo précédente.
        if let Some(Tex::Ready(t)) = self.opening.as_ref().and_then(|p| self.textures.get(p)) {
            let size = t.size_vec2();
            let s = (rect.width() / size.x).min(rect.height() / size.y);
            let r = Rect::from_center_size(rect.center(), size * s);
            ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_gray(150));
            let c = r.center();
            ui.painter().rect_filled(Rect::from_center_size(c, vec2(150.0, 34.0)), 17.0, Color32::from_black_alpha(160));
            ui.painter().text(c, egui::Align2::CENTER_CENTER, t!("Ouverture…"), egui::FontId::proportional(14.0), Color32::from_gray(230));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        let Some((w, h)) = self.size else {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, &self.status, egui::FontId::proportional(16.0), Color32::GRAY);
            return;
        };
        let ppp = ui.ctx().pixels_per_point();
        let src = [w as f32, h as f32];
        let (w, h) = if self.crop_mode { (w, h) } else { self.adj.output_size(w, h) };
        let img = vec2(w as f32, h as f32);
        // Aperçu de l'agrandissement du cadre : la vue ajustée montre aussi les marges.
        let ex = self.expand_preview();
        let [el, et, er, eb] = ex.unwrap_or([0.0; 4]);
        let fit_zoom = (rect.width() * ppp / (img.x * (1.0 + el + er))).min(rect.height() * ppp / (img.y * (1.0 + et + eb)));
        if self.fit {
            self.zoom = fit_zoom;
            self.center = img / 2.0 + vec2(img.x * (er - el), img.y * (eb - et)) / 2.0;
        }

        // Molette : zoom autour du curseur.
        if let Some(hover) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            let factor = ui.input(|i| i.zoom_delta()) * (scroll / 200.0).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let new_zoom = (self.zoom * factor).clamp(fit_zoom.min(1.0) * 0.5, 16.0);
                let off = (hover - rect.center()) * ppp;
                let p = self.center + off / self.zoom;
                self.center = p - off / new_zoom;
                self.zoom = new_zoom;
                self.fit = false;
            }
        }
        if resp.double_clicked() && self.tool_drag.is_none() && !self.local_tool_active() && self.retouch_tool.is_none() {
            if self.fit {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let off = (pos - rect.center()) * ppp;
                    self.center = self.center + off / self.zoom - off;
                }
                self.zoom = 1.0;
                self.fit = false;
            } else {
                self.fit = true;
            }
        }

        let scale = self.zoom / ppp;
        let img_rect = Rect::from_min_max(rect.center() - self.center * scale, rect.center() + (img - self.center) * scale);
        let vis = img_rect.intersect(rect);
        if vis.width() <= 0.5 || vis.height() <= 0.5 {
            return;
        }
        let uv = [
            (vis.min.x - img_rect.min.x) / img_rect.width(),
            (vis.min.y - img_rect.min.y) / img_rect.height(),
            vis.width() / img_rect.width(),
            vis.height() / img_rect.height(),
        ];
        let mut uniforms = self.adj.uniforms(uv, self.before, false, src, self.crop_mode);
        // Fondu entre le résultat et l'original (misc.x : 0 après, 1 avant).
        uniforms.misc[0] = ui.ctx().animate_bool_with_time(egui::Id::unique("fondu_avant"), self.before, ui.style().animation_time * 1.5);
        if self.show_mask && !self.crop_mode {
            uniforms.mask_info[1] = self.local_sel.map_or(-1.0, |i| i as f32);
        }
        if self.sky_show_mask && !self.crop_mode {
            uniforms.mask_info[1] = crate::gpu::SKY_LAYER as f32;
        }
        if self.depth_show_mask && !self.crop_mode {
            uniforms.mask_info[1] = crate::gpu::DEPTH_LAYER as f32;
        }
        if self.cutout_show && self.cutout_active && !self.crop_mode {
            uniforms.mask_info[1] = crate::gpu::SUBJECT_LAYER as f32;
        }
        let split = self.split_view && !self.before && !self.crop_mode;
        if split {
            uniforms.geo[3] = self.split_pos.clamp(0.001, 0.999);
        }
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(vis, ImageCallback { uniforms }));
        if split {
            // Rideau : trait vertical et poignée ; on le tire n'importe où sur sa hauteur.
            let sx = img_rect.min.x + self.split_pos * img_rect.width();
            let p = ui.painter_at(vis);
            p.line_segment([pos2(sx, vis.min.y), pos2(sx, vis.max.y)], Stroke::new(3.0, Color32::from_black_alpha(120)));
            p.line_segment([pos2(sx, vis.min.y), pos2(sx, vis.max.y)], Stroke::new(1.5, Color32::WHITE));
            let hc = pos2(sx, vis.center().y);
            p.circle_filled(hc, 13.0, Color32::from_black_alpha(170));
            p.circle_stroke(hc, 13.0, Stroke::new(1.5, Color32::WHITE));
            for d in [-1.0f32, 1.0] {
                let tip = hc + vec2(d * 8.0, 0.0);
                p.add(egui::Shape::convex_polygon(vec![tip, tip - vec2(d * 4.0, 4.0), tip - vec2(d * 4.0, -4.0)], Color32::WHITE, Stroke::NONE));
            }
            let font = egui::FontId::proportional(13.0);
            let tag = |pos: Pos2, align: egui::Align2, text: &str| {
                let r = p.text(pos, align, text, font.clone(), Color32::WHITE);
                p.rect_filled(r.expand(4.0), 4.0, Color32::from_black_alpha(140));
                p.text(pos, align, text, font.clone(), Color32::WHITE);
            };
            tag(pos2(vis.min.x + 10.0, vis.min.y + 10.0), egui::Align2::LEFT_TOP, t!("AVANT"));
            tag(pos2(vis.max.x - 10.0, vis.min.y + 10.0), egui::Align2::RIGHT_TOP, t!("APRÈS"));
            let near = |x: f32| (x - sx).abs() < 12.0;
            if resp.hover_pos().is_some_and(|h| near(h.x)) || self.split_drag {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.drag_started_by(egui::PointerButton::Primary) && ui.input(|i| i.pointer.press_origin()).is_some_and(|o| near(o.x)) {
                self.split_drag = true;
            }
            if self.split_drag {
                if let Some(pos) = resp.interact_pointer_pos() {
                    self.split_pos = ((pos.x - img_rect.min.x) / img_rect.width()).clamp(0.0, 1.0);
                }
            }
        }
        if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            self.split_drag = false;
        }
        if ex.is_some() {
            let (iw, ih) = (img_rect.width(), img_rect.height());
            let outer = Rect::from_min_max(img_rect.min - vec2(el * iw, et * ih), img_rect.max + vec2(er * iw, eb * ih));
            let p = ui.painter_at(rect);
            let fill = Color32::from_rgba_unmultiplied(120, 120, 120, 70);
            p.rect_filled(Rect::from_min_max(outer.min, pos2(outer.max.x, img_rect.min.y)), 0.0, fill);
            p.rect_filled(Rect::from_min_max(pos2(outer.min.x, img_rect.max.y), outer.max), 0.0, fill);
            p.rect_filled(Rect::from_min_max(pos2(outer.min.x, img_rect.min.y), pos2(img_rect.min.x, img_rect.max.y)), 0.0, fill);
            p.rect_filled(Rect::from_min_max(pos2(img_rect.max.x, img_rect.min.y), pos2(outer.max.x, img_rect.max.y)), 0.0, fill);
            p.rect_stroke(outer, 0.0, Stroke::new(1.5, Color32::from_rgb(240, 190, 60)), egui::StrokeKind::Outside);
        }
        if self.split_drag {
            // Le rideau prend la souris.
        } else if self.crop_mode {
            self.crop_overlay(ui, &resp, img_rect, rect, src);
        } else if self.focus_pick && !self.before {
            // Mise au point du flou : proximité lue dans la carte de profondeur au point cliqué.
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if let (true, Some(pos), Some(d)) = (resp.clicked(), resp.interact_pointer_pos(), self.ai_masks.get(&Model::Depth)) {
                let uv = self.adj.out_to_src([(pos.x - img_rect.min.x) / img_rect.width(), (pos.y - img_rect.min.y) / img_rect.height()], src, false);
                let uv = self.lens_geo.map_or(uv, |l| l.to_source(uv, src));
                let x = ((uv[0].clamp(0.0, 1.0) * d.w as f32) as u32).min(d.w - 1);
                let y = ((uv[1].clamp(0.0, 1.0) * d.h as f32) as u32).min(d.h - 1);
                self.adj.depth.focus = d.data[(y * d.w + x) as usize].clamp(0.0, 1.0);
                self.focus_pick = false;
            }
        } else if !self.before && self.retouch_tool.is_some() {
            self.retouch_overlay(ui, &resp, img_rect, rect, src);
        } else if !self.before && self.tab == Tab::Layers && !self.adj.layers.is_empty() {
            self.layer_overlay(ui, &resp, img_rect, rect);
        } else if !self.before {
            self.local_overlay(ui, &resp, img_rect, rect, src);
        }
        // Déplacement de la vue : clic gauche (sauf outil local), ou clic droit / molette enfoncée.
        let pan = (resp.dragged_by(egui::PointerButton::Primary) && self.tool_drag.is_none() && !self.painting && self.heal_drag.is_none() && !self.split_drag)
            || resp.dragged_by(egui::PointerButton::Secondary)
            || resp.dragged_by(egui::PointerButton::Middle);
        if pan && !self.crop_mode {
            self.center -= resp.drag_delta() * ppp / self.zoom;
            self.fit = false;
        }
        if self.before {
            ui.painter().text(rect.left_top() + vec2(12.0, 12.0), egui::Align2::LEFT_TOP, "AVANT", egui::FontId::proportional(14.0), Color32::YELLOW);
        }
    }

    /// Un réglage local est sélectionné et son outil répond à la souris.
    fn local_tool_active(&self) -> bool {
        !self.crop_mode && self.local_sel.is_some_and(|i| i < self.adj.locals.len())
    }

    fn add_local(&mut self, shape: Shape, name: &str) {
        if self.adj.locals.len() >= MAX_LOCALS {
            return;
        }
        let n = self.adj.locals.iter().filter(|l| l.name.starts_with(name)).count();
        let name = if n == 0 { name.to_string() } else { format!("{name} {}", n + 1) };
        self.adj.locals.push(Local { name, shape, ..Default::default() });
        self.local_sel = Some(self.adj.locals.len() - 1);
    }

    fn local_overlay(&mut self, ui: &egui::Ui, resp: &egui::Response, img_rect: Rect, view: Rect, src: [f32; 2]) {
        let Some(i) = self.local_sel.filter(|i| *i < self.adj.locals.len()) else { return };
        let adj = self.adj.clone();
        let lens = self.lens_geo;
        let to_screen = |uv: [f32; 2]| {
            let uv = lens.map_or(uv, |l| l.to_corrected(uv, src));
            let o = adj.src_to_out(uv, src, false);
            pos2(img_rect.min.x + o[0] * img_rect.width(), img_rect.min.y + o[1] * img_rect.height())
        };
        let to_src = |p: Pos2| {
            let s = adj.out_to_src([(p.x - img_rect.min.x) / img_rect.width(), (p.y - img_rect.min.y) / img_rect.height()], src, false);
            lens.map_or(s, |l| l.to_source(s, src))
        };
        // Pixels écran par pixel source (le recadrage ne change pas l'échelle).
        let (ow, _) = adj.output_size(src[0] as u32, src[1] as u32);
        let px_scale = img_rect.width() / ow as f32;
        let p = ui.painter_at(view);
        let white = Stroke::new(1.5, Color32::WHITE);
        let dark = Stroke::new(3.5, Color32::from_black_alpha(140));
        let primary = resp.dragged_by(egui::PointerButton::Primary);
        let started = resp.drag_started_by(egui::PointerButton::Primary);
        let origin = ui.input(|inp| inp.pointer.press_origin());
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        if resp.drag_stopped() {
            self.tool_drag = None;
            self.painting = false;
        }
        let handle_at = |handles: &[(Handle, Pos2)], pos: Option<Pos2>| {
            pos.and_then(|pos| handles.iter().find(|(_, h)| h.distance(pos) < 14.0).map(|(k, _)| *k))
        };
        let dot = |pos: Pos2, filled: bool| {
            p.circle_stroke(pos, 6.0, dark);
            if filled { p.circle_filled(pos, 5.0, Color32::WHITE) } else { p.circle_stroke(pos, 5.0, white) }
        };

        match self.adj.locals[i].shape.clone() {
            Shape::Brush { .. } | Shape::Ai { .. } => {
                let erase = self.brush.erase || ui.input(|inp| inp.modifiers.alt);
                if let Some(pos) = resp.hover_pos() {
                    let r = self.brush.radius * src[0] * px_scale;
                    p.circle_stroke(pos, r, dark);
                    p.circle_stroke(pos, r, Stroke::new(1.0, if erase { Color32::from_rgb(255, 120, 120) } else { Color32::WHITE }));
                    p.circle_stroke(pos, r * self.brush.hardness.max(0.05), Stroke::new(1.0, Color32::from_white_alpha(90)));
                }
                if started {
                    if let Some(pos) = origin.or(pointer) {
                        self.painting = true;
                        let stroke = BrushStroke { points: vec![to_src(pos)], radius: self.brush.radius, hardness: self.brush.hardness, flow: self.brush.flow, erase, fill: false };
                        match &mut self.adj.locals[i].shape {
                            Shape::Brush { strokes } | Shape::Ai { strokes, .. } => strokes.push(stroke),
                            _ => {}
                        }
                    }
                }
                if self.painting && primary {
                    if let Some(pos) = pointer {
                        let q = to_src(pos);
                        if let Shape::Brush { strokes } | Shape::Ai { strokes, .. } = &mut self.adj.locals[i].shape {
                            if let Some(s) = strokes.last_mut() {
                                let last = *s.points.last().unwrap();
                                // Un point tous les ~quart de rayon suffit (le tracé est interpolé).
                                if ((q[0] - last[0]) * src[0]).hypot((q[1] - last[1]) * src[1]) > s.radius * src[0] * 0.25 {
                                    s.points.push(q);
                                }
                            }
                        }
                    }
                }
            }
            Shape::Linear { a, b } => {
                let (pa, pb) = (to_screen(a), to_screen(b));
                let dir = (pb - pa).normalized();
                let perp = vec2(-dir.y, dir.x) * 4000.0;
                for (pt, stroke) in [(pa, white), (pb, Stroke::new(1.0, Color32::from_white_alpha(150)))] {
                    p.line_segment([pt - perp, pt + perp], dark);
                    p.line_segment([pt - perp, pt + perp], stroke);
                }
                p.line_segment([pa, pb], Stroke::new(1.0, Color32::from_white_alpha(160)));
                dot(pa, true);
                dot(pb, false);
                let handles = [(Handle::LinA, pa), (Handle::LinB, pb)];
                if started {
                    self.tool_drag = handle_at(&handles, origin);
                }
                if let (Some(h), true, Some(pos)) = (self.tool_drag, primary, pointer) {
                    let q = to_src(pos);
                    if let Shape::Linear { a, b } = &mut self.adj.locals[i].shape {
                        if h == Handle::LinA { *a = q } else { *b = q }
                    }
                }
            }
            Shape::Radial { center, radius, angle, feather } => {
                let (s, c) = angle.to_radians().sin_cos();
                let at = |x: f32, y: f32| {
                    let (dx, dy) = (x * radius[0] * src[0], y * radius[1] * src[0]);
                    to_screen([center[0] + (c * dx - s * dy) / src[0], center[1] + (s * dx + c * dy) / src[1]])
                };
                for (k, w) in [(1.0, white), (1.0 - feather.clamp(0.0, 1.0), Stroke::new(1.0, Color32::from_white_alpha(110)))] {
                    let pts: Vec<Pos2> = (0..=64).map(|t| {
                        let th = t as f32 / 64.0 * std::f32::consts::TAU;
                        at(k * th.cos(), k * th.sin())
                    }).collect();
                    p.add(egui::Shape::line(pts.clone(), dark));
                    p.add(egui::Shape::line(pts, w));
                }
                let (pc, px, py) = (to_screen(center), at(1.0, 0.0), at(0.0, 1.0));
                dot(pc, true);
                dot(px, false);
                dot(py, false);
                let handles = [(Handle::Center, pc), (Handle::RadX, px), (Handle::RadY, py)];
                if started {
                    self.tool_drag = handle_at(&handles, origin);
                }
                if let (Some(h), true, Some(pos)) = (self.tool_drag, primary, pointer) {
                    let q = to_src(pos);
                    if let Shape::Radial { center, radius, .. } = &mut self.adj.locals[i].shape {
                        let d = ((q[0] - center[0]) * src[0]).hypot((q[1] - center[1]) * src[1]) / src[0];
                        match h {
                            Handle::Center => *center = q,
                            Handle::RadX => radius[0] = d.max(0.005),
                            Handle::RadY => radius[1] = d.max(0.005),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    fn locals_ui(&mut self, ui: &mut egui::Ui) {
        let full = self.adj.locals.len() >= MAX_LOCALS;
        ui.horizontal(|ui| {
            if ui.add_enabled(!full, egui::Button::new(t!("+ Pinceau"))).clicked() {
                self.add_local(Shape::Brush { strokes: vec![] }, t!("Pinceau"));
            }
            if ui.add_enabled(!full, egui::Button::new(t!("+ Linéaire"))).on_hover_text(t!("Dégradé : effet plein au point plein, nul au point creux")).clicked() {
                self.add_local(Shape::Linear { a: [0.5, 0.15], b: [0.5, 0.5] }, t!("Dégradé"));
            }
            if ui.add_enabled(!full, egui::Button::new(t!("+ Radial"))).clicked() {
                self.add_local(Shape::Radial { center: [0.5, 0.5], radius: [0.2, 0.2], angle: 0.0, feather: 0.5 }, t!("Radial"));
            }
        });
        ui.horizontal(|ui| {
            for (kind, label, name, tip) in [
                (AiKind::Sky, t!("+ Ciel IA"), t!("Ciel"), t!("Masque du ciel détecté automatiquement")),
                (AiKind::Subject, t!("+ Sujet IA"), t!("Sujet"), t!("Masque du sujet principal (personne, objet…)")),
                (AiKind::Background, t!("+ Arrière-plan IA"), t!("Arrière-plan"), t!("Tout sauf le sujet principal")),
            ] {
                if ui.add_enabled(!full, egui::Button::new(label)).on_hover_text(tip).clicked() {
                    self.add_local(Shape::Ai { kind, strokes: vec![] }, name);
                }
            }
        });
        ui.label(t!("Masques de la scène (IA)"));
        ui.horizontal_wrapped(|ui| {
            for (kind, name) in [
                (AiKind::People, t!("Personnes")),
                (AiKind::Buildings, t!("Bâtiments")),
                (AiKind::Vegetation, t!("Végétation")),
                (AiKind::Water, t!("Eau")),
                (AiKind::Mountains, t!("Montagnes")),
                (AiKind::Ground, t!("Sol")),
            ] {
                if ui.add_enabled(!full, egui::Button::new(format!("+ {name}"))).clicked() {
                    self.add_local(Shape::Ai { kind, strokes: vec![] }, name);
                }
            }
        });
        if self.adj.locals.is_empty() {
            ui.small(t!("Ajoute un pinceau ou un dégradé, puis règle-le ci-dessous. Huit au maximum."));
            return;
        }
        let mut remove = None;
        for (k, l) in self.adj.locals.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.checkbox(&mut l.enabled, "");
                if ui.selectable_label(self.local_sel == Some(k), &l.name).clicked() {
                    self.local_sel = if self.local_sel == Some(k) { None } else { Some(k) };
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(t!("Supprimer")).clicked() {
                        remove = Some(k);
                    }
                });
            });
        }
        if let Some(k) = remove {
            self.adj.locals.remove(k);
            self.masks_uploaded.clear();
            self.local_sel = None;
        }
        let Some(i) = self.local_sel.filter(|i| *i < self.adj.locals.len()) else {
            ui.small(t!("Clique sur un réglage pour le modifier."));
            return;
        };
        ui.separator();
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_mask, t!("Voir le masque (M)"));
            ui.checkbox(&mut self.adj.locals[i].invert, t!("Inverser"));
        });
        slider(ui, t!("Opacité"), &mut self.adj.locals[i].amount, 0.0..=1.0, 1.0);
        let l = &mut self.adj.locals[i];
        match &mut l.shape {
            Shape::Brush { strokes } | Shape::Ai { strokes, .. } => {
                slider(ui, t!("Taille"), &mut self.brush.radius, BRUSH_MIN..=0.3, 0.04);
                slider(ui, t!("Dureté"), &mut self.brush.hardness, 0.0..=1.0, 0.5);
                slider(ui, t!("Débit"), &mut self.brush.flow, 0.05..=1.0, 0.7);
                ui.horizontal(|ui| {
                    ui.toggle_value(&mut self.brush.erase, t!("Gomme")).on_hover_text(t!("Ou maintenir Alt en peignant"));
                    if ui.button(t!("Effacer les traits")).clicked() {
                        strokes.clear();
                    }
                });
                if let Shape::Ai { kind, .. } = l.shape {
                    let model = Model::of(kind);
                    if self.ai_job.as_ref().is_some_and(|j| j.model == model) {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.small(tf!("Détection {0} en cours…", model.of_what()));
                        });
                    } else if self.ai_failed.contains(&model) {
                        ui.colored_label(Color32::from_rgb(230, 120, 90), t!("Le masque IA n'a pas pu être calculé (voir la barre d'état)."));
                        if ui.small_button(t!("Réessayer")).clicked() {
                            self.ai_failed.remove(&model);
                        }
                    }
                    ui.small(t!("Le masque est détecté automatiquement. Peins pour l'étendre, Alt (ou Gomme) pour le réduire."));
                } else {
                    ui.small(t!("Peins sur la photo. [ et ] : taille. Clic droit glissé : déplacer la vue."));
                }
            }
            Shape::Radial { angle, feather, .. } => {
                slider(ui, t!("Adoucissement"), feather, 0.0..=1.0, 0.5);
                slider(ui, t!("Angle (°)"), angle, -90.0..=90.0, 0.0);
                ui.small(t!("Glisse le centre et les poignées sur la photo."));
            }
            Shape::Linear { .. } => {
                ui.small(t!("Glisse les deux poignées : effet plein au point plein, nul au point creux."));
            }
        }
        let a: &mut LocalAdj = &mut l.adj;
        slider(ui, t!("Exposition"), &mut a.exposure, -3.0..=3.0, 0.0);
        slider(ui, t!("Contraste"), &mut a.contrast, -1.0..=1.0, 0.0);
        slider(ui, t!("Hautes lumières"), &mut a.highlights, -1.0..=1.0, 0.0);
        slider(ui, t!("Ombres"), &mut a.shadows, -1.0..=1.0, 0.0);
        slider_with(ui, t!("Température"), &mut a.temperature, -1.0..=1.0, 0.0, &tracks::TEMPERATURE);
        slider_with(ui, t!("Teinte"), &mut a.tint, -1.0..=1.0, 0.0, &tracks::TINT);
        slider_with(ui, t!("Saturation"), &mut a.saturation, -1.0..=1.0, 0.0, &tracks::SATURATION);
        slider(ui, t!("Clarté"), &mut a.clarity, -1.0..=1.0, 0.0);
    }

    fn retouch_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
            for (tool, label, tip) in [
                (RetouchTool::Heal, t!("Correcteur"), t!("Clic sur une tache : elle est remplacée par une zone voisine (source déplaçable)")),
                (RetouchTool::Erase, t!("Gomme IA"), t!("Peins l'objet à faire disparaître, puis « Effacer »")),
                (RetouchTool::Generate, t!("Générer"), t!("Peins une zone et décris ce qui doit y apparaître (ComfyUI ou moteur local)")),
            ] {
                let on = self.retouch_tool == Some(tool);
                if ui.add(egui::Button::selectable(on, label).min_size(vec2(w, 30.0))).on_hover_text(tip).clicked() {
                    self.retouch_tool = if on { None } else { Some(tool) };
                    if self.retouch_tool.is_some() {
                        self.local_sel = None;
                        self.crop_mode = false;
                    }
                }
            }
        });
        if let Some(tool) = self.retouch_tool {
            // Correcteur : petites taches (rayon jusqu'à 8 % de la largeur) ; échelle logarithmique,
            // les petites tailles étant les plus utiles.
            let (range, default) = if tool == RetouchTool::Heal { (0.001..=0.08, 0.012) } else { (BRUSH_MIN..=0.3, 0.03) };
            self.brush.radius = self.brush.radius.clamp(*range.start(), *range.end());
            if tool != RetouchTool::Heal {
                ui.horizontal(|ui| {
                    ui.add_sized([110.0, 18.0], egui::Label::new(t!("Sélection")).truncate());
                    segmented(ui, &mut self.select_shape, &[(0, t!("Pinceau")), (1, t!("Lasso")), (2, t!("Rectangle"))]);
                });
            }
            if tool == RetouchTool::Heal || self.select_shape == 0 {
            ui.horizontal(|ui| {
                ui.add_sized([110.0, 18.0], egui::Label::new(t!("Taille")).truncate());
                let r = ui.add(egui::Slider::new(&mut self.brush.radius, range).logarithmic(true)
                    .custom_formatter(|v, _| if v < 0.005 { format!("{:.2} %", v * 200.0) } else { format!("{:.1} %", v * 200.0) })
                    .custom_parser(|t| t.trim().trim_end_matches('%').trim().replace(',', ".").parse::<f64>().ok().map(|v| v / 200.0)))
                    .on_hover_text(t!("Diamètre, en % de la largeur de la photo. Touches [ et ] pour réduire ou agrandir."));
                if r.double_clicked() {
                    self.brush.radius = default;
                }
            });
            // Le même diamètre en pixels de la photo.
            if let Some((w, _)) = self.size {
                ui.horizontal(|ui| {
                    ui.add_space(114.0);
                    ui.label(egui::RichText::new(tf!("soit {0} px sur la photo", (self.brush.radius * 2.0 * w as f32).round().max(1.0))).weak().small());
                });
            }
            }
        }
        match self.retouch_tool {
            Some(RetouchTool::Heal) => {
                ui.small(t!("Clic sur une tache. Glisse le cercle pointillé pour choisir une autre source. [ et ] : taille."));
                ui.small(t!("Chaque clic corrige aussitôt, rien à valider : « Retirer la dernière » ou Ctrl+Z pour revenir. Pour un objet long (pylône, fil), la Gomme IA convient mieux."));
            }
            Some(RetouchTool::Erase) => {
                ui.horizontal(|ui| {
                    ui.add_sized([110.0, 18.0], egui::Label::new(t!("Remplissage")).truncate());
                    segmented(ui, &mut self.erase_smooth, &[(false, t!("IA (LaMa)")), (true, t!("Fond uni (rapide)"))]);
                });
                ui.small(match (self.select_shape, self.erase_smooth) {
                    (0, false) => t!("Peins l'objet (Alt pour retirer), puis « Effacer ». Le calcul prend quelques secondes."),
                    (0, true) => t!("Peins l'objet (Alt pour retirer), puis « Effacer ». Fond uni : pour un ciel, un mur, une peau ; instantané."),
                    _ => t!("Entoure l'objet (Alt pour retirer une zone), puis « Effacer ». Plusieurs zones possibles."),
                });
                ui.horizontal(|ui| {
                    let ready = !self.erase_strokes.is_empty() && self.erase_job.is_none();
                    if ui.add_enabled(ready, egui::Button::new(t!("Effacer"))).clicked() {
                        let strokes = std::mem::take(&mut self.erase_strokes);
                        self.adj.retouch.push(Retouch::Erase { strokes, smooth: self.erase_smooth });
                    }
                    if ui.add_enabled(!self.erase_strokes.is_empty(), egui::Button::new(t!("Recommencer"))).clicked() {
                        self.erase_strokes.clear();
                    }
                });
                if self.erase_job.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let t = self.erase_job.as_ref().map(|j| duration_text(j.start.elapsed())).unwrap_or_default();
                        ui.small(format!("{} {t}", t!("Gomme en cours…")));
                    });
                }
            }
            Some(RetouchTool::Generate) => self.generate_ui(ui),
            None => {}
        }
        let count = |f: fn(&Retouch) -> bool| self.adj.retouch.iter().filter(|r| f(r)).count();
        let n_heal = count(|r| matches!(r, Retouch::Heal { .. }));
        let n_erase = count(|r| matches!(r, Retouch::Erase { .. }));
        let n_gen = count(|r| matches!(r, Retouch::Generate { .. }));
        ui.label(tf!("{n_heal} tache(s) corrigée(s), {n_erase} gomme(s), {n_gen} génération(s)", n_heal, n_erase, n_gen));
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.adj.retouch.is_empty(), egui::Button::new(t!("Retirer la dernière"))).clicked() {
                self.adj.retouch.pop();
            }
            if ui.add_enabled(!self.adj.retouch.is_empty(), egui::Button::new(t!("Tout retirer"))).clicked() {
                self.adj.retouch.clear();
            }
        });
        if !self.erase_failed.is_empty() && ui.small_button(t!("Réessayer les retouches en échec")).clicked() {
            self.erase_failed.clear();
            self.retouch_uploaded = None;
        }
    }

    fn generate_ui(&mut self, ui: &mut egui::Ui) {
        self.engine_ui(ui, false);
        if self.settings.ai_local {
            // En local, Ollama ne sert que s'il est installé sur ce PC : options repliées.
            ui.collapsing(t!("Ollama (facultatif)"), |ui| self.vram_ui(ui));
        } else {
            self.vram_ui(ui);
        }
        ui.label(if self.settings.translate { t!("Ce qui doit apparaître") } else { t!("Ce qui doit apparaître (en anglais)") });
        ui.add(egui::TextEdit::multiline(&mut self.gen_prompt).desired_rows(2).desired_width(f32::INFINITY));
        ui.collapsing(t!("À éviter"), |ui| {
            if ui.add(egui::TextEdit::multiline(&mut self.settings.comfy_negative).desired_rows(2).desired_width(f32::INFINITY)).lost_focus() {
                self.settings.save();
            }
        });
        slider(ui, t!("Force"), &mut self.gen_strength, 0.3..=1.0, 1.0);
        ui.small(t!("Peins la zone (Alt pour retirer), puis « Générer ». Force < 1 garde la forme d'origine (sauf modèle inpainting)."));
        let new_seed = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64 & 0xffff_ffff_ffff);
        ui.horizontal(|ui| {
            let ready = !self.erase_strokes.is_empty() && self.erase_job.is_none() && self.gen_ready() && !self.gen_prompt.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new(t!("Générer"))).clicked() {
                let strokes = std::mem::take(&mut self.erase_strokes);
                self.adj.retouch.push(Retouch::Generate {
                    strokes,
                    model: self.settings.gen_model(),
                    prompt: self.gen_prompt.trim().to_string(),
                    negative: self.settings.comfy_negative.clone(),
                    seed: new_seed(),
                    strength: self.gen_strength,
                });
            }
            if ui.add_enabled(!self.erase_strokes.is_empty(), egui::Button::new(t!("Recommencer"))).clicked() {
                self.erase_strokes.clear();
            }
            // Nouvelle graine pour la dernière génération (même zone, même invite).
            let last_gen = matches!(self.adj.retouch.last(), Some(Retouch::Generate { .. }));
            if ui.add_enabled(last_gen && self.erase_job.is_none(), egui::Button::new(t!("Autre variante"))).clicked() {
                if let Some(Retouch::Generate { seed, .. }) = self.adj.retouch.last_mut() {
                    *seed = new_seed();
                }
            }
        });
        if self.erase_job.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                let t = self.erase_job.as_ref().map(|j| duration_text(j.start.elapsed())).unwrap_or_default();
                ui.small(format!("{} {t}", t!("Génération en cours…")));
            });
        }
    }

    /// Connexion au serveur ComfyUI (liste des modèles au premier affichage).
    fn comfy_connect_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(rx) = &self.comfy_probe {
            match rx.try_recv() {
                Ok(r) => {
                    self.comfy_probe = None;
                    match r {
                        Ok((models, ups)) => {
                            self.comfy_msg = tf!("Connecté : {0} modèle(s).", models.len());
                            if !models.contains(&self.settings.comfy_model) {
                                self.settings.comfy_model = crate::comfy::preferred(&models).unwrap_or_default();
                                self.settings.save();
                            }
                            if !ups.contains(&self.settings.upscale_model) || crate::comfy::is_denoiser(&self.settings.upscale_model) {
                                self.settings.upscale_model = ups.iter().find(|m| !crate::comfy::is_denoiser(m)).cloned().unwrap_or_default();
                                self.settings.save();
                            }
                            self.comfy_models = models;
                            self.comfy_upscalers = ups;
                        }
                        Err(e) => self.comfy_msg = format!("{e:#}"),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.comfy_probe = None,
                Err(_) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(200)),
            }
        }
        if self.comfy_models.is_empty() && self.comfy_probe.is_none() && self.comfy_msg.is_empty() {
            self.probe_comfy(ui.ctx());
        }
        ui.horizontal(|ui| {
            ui.label(t!("Serveur"));
            let r = ui.add(egui::TextEdit::singleline(&mut self.settings.comfy_url).desired_width(170.0));
            if r.lost_focus() {
                self.settings.save();
            }
            if ui.add_enabled(self.comfy_probe.is_none(), egui::Button::new(t!("Connecter"))).clicked() {
                self.settings.save();
                self.probe_comfy(ui.ctx());
            }
        });
        if self.comfy_probe.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small(t!("Connexion à ComfyUI…"));
            });
        } else if !self.comfy_msg.is_empty() {
            ui.small(&self.comfy_msg);
        }
    }

    /// Choix du modèle de génération (checkpoint).
    fn checkpoint_ui(&mut self, ui: &mut egui::Ui) {
        if !self.comfy_models.is_empty() {
            let mut model = self.settings.comfy_model.clone();
            let items: Vec<(String, String)> = self.comfy_models.iter().map(|m| (m.clone(), m.trim_end_matches(".safetensors").trim_end_matches(".ckpt").to_string())).collect();
            choice_list(ui, "comfy_model", &mut model, &items);
            if model != self.settings.comfy_model {
                self.settings.comfy_model = model;
                self.settings.save();
            }
            if !self.settings.comfy_model.to_lowercase().contains("inpaint") {
                ui.small(t!("Conseil : un modèle « inpainting » donne des raccords plus propres."));
            }
        }
        let has_inpaint = self.comfy_models.iter().any(|m| m.to_lowercase().contains("inpaint"));
        self.comfy_help(ui, &NEED_INPAINT, !has_inpaint);
    }

    /// Option « libérer la carte graphique » : Ollama déchargé avant la génération, ComfyUI vidé après.
    fn vram_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(rx) = &self.ollama_job {
            match rx.try_recv() {
                Ok(msg) => {
                    self.ollama_msg = msg;
                    self.ollama_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.ollama_job = None,
                Err(_) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(200)),
            }
        }
        let tip = "Avant chaque génération, Luminux demande à Ollama de décharger ses modèles, \
                   puis demande à ComfyUI de libérer sa mémoire une fois l'image reçue. \
                   Ollama recharge son modèle à la requête suivante (plus lente).";
        if ui.checkbox(&mut self.settings.free_vram, t!("Libérer la carte graphique (Ollama)")).on_hover_text(tip).changed() {
            self.settings.save();
        }
        let tip_tr = "Le modèle d'image ne comprend bien que l'anglais : l'invite est traduite par un modèle d'Ollama \
                      juste avant l'envoi. La traduction s'affiche dans la barre d'état.";
        if ui.checkbox(&mut self.settings.translate, t!("Traduire l'invite en anglais (Ollama)")).on_hover_text(tip_tr).changed() {
            self.settings.save();
        }
        if !self.settings.free_vram && !self.settings.translate {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(t!("Ollama"));
            if ui.add(egui::TextEdit::singleline(&mut self.settings.ollama_url).desired_width(170.0)).lost_focus() {
                self.settings.save();
                self.ollama_models.clear();
                self.ollama_msg.clear();
            }
        });
        if self.settings.translate {
            if let Some(rx) = &self.ollama_models_job {
                match rx.try_recv() {
                    Ok(r) => {
                        self.ollama_models_job = None;
                        match r {
                            Ok(v) => self.ollama_models = v,
                            Err(e) => self.ollama_msg = format!("{e:#}"),
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => self.ollama_models_job = None,
                    Err(_) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(200)),
                }
            } else if self.ollama_models.is_empty() && self.ollama_msg.is_empty() {
                let (tx, rx) = channel();
                let (url, ctx) = (self.settings.ollama_url.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let _ = tx.send(crate::comfy::ollama_models(&url));
                    ctx.request_repaint();
                });
                self.ollama_models_job = Some(rx);
            }
            let auto = self.ollama_models.first().map_or(t!("le plus petit").to_string(), |m| tf!("{0} (le plus petit)", m.0));
            let shown = if self.settings.translate_model.is_empty() { tf!("Auto : {auto}", auto) } else { self.settings.translate_model.clone() };
            let mut chosen = self.settings.translate_model.clone();
            let _ = shown;
            ui.label(t!("Modèle de traduction"));
            let mut items = vec![(String::new(), tf!("Auto : {auto}", auto))];
            items.extend(self.ollama_models.iter().map(|(m, size)| (m.clone(), tf!("{m} ({0:.1} Go)", m, *size as f64 / (1u64 << 30) as f64))));
            choice_list(ui, "translate_model", &mut chosen, &items);
            if chosen != self.settings.translate_model {
                self.settings.translate_model = chosen;
                self.settings.save();
            }
        }
        if !self.settings.free_vram {
            if !self.ollama_msg.is_empty() {
                ui.small(&self.ollama_msg);
            }
            return;
        }
        ui.horizontal(|ui| {
            let idle = self.ollama_job.is_none();
            for (label, unload) in [(t!("État"), false), (t!("Décharger maintenant"), true)] {
                if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                    self.settings.save();
                    let (tx, rx) = channel();
                    let (url, ctx) = (self.settings.ollama_url.clone(), ui.ctx().clone());
                    std::thread::spawn(move || {
                        let msg = if unload {
                            match crate::comfy::ollama_unload(&url) {
                                Ok(v) if v.is_empty() => t!("Aucun modèle chargé.").to_string(),
                                Ok(v) => format!("Déchargé : {}", v.join(", ")),
                                Err(e) => format!("{e:#}"),
                            }
                        } else {
                            match crate::comfy::ollama_loaded(&url) {
                                Ok(v) if v.is_empty() => t!("Aucun modèle chargé : la carte est libre côté Ollama.").to_string(),
                                Ok(v) => v.iter().map(|(n, b)| format!("{n} : {:.1} Go", *b as f64 / (1u64 << 30) as f64)).collect::<Vec<_>>().join("\n"),
                                Err(e) => format!("{e:#}"),
                            }
                        };
                        let _ = tx.send(msg);
                        ctx.request_repaint();
                    });
                    self.ollama_job = Some(rx);
                }
            }
            if !idle {
                ui.spinner();
            }
        });
        if !self.ollama_msg.is_empty() {
            ui.small(&self.ollama_msg);
        }
    }

    fn probe_comfy(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let (url, ctx) = (self.settings.comfy_url.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::comfy::checkpoints(&url).map(|m| (m, crate::comfy::upscalers(&url).unwrap_or_default())));
            ctx.request_repaint();
        });
        self.comfy_msg.clear();
        self.comfy_probe = Some(rx);
    }

    /// Calque choisi sur la photo : cadre et découpe dessinés ; glisser le déplace, ou trace le lasso de découpe.
    fn layer_overlay(&mut self, ui: &egui::Ui, resp: &egui::Response, img_rect: Rect, view: Rect) {
        let i = self.layer_sel.min(self.adj.layers.len() - 1);
        let l = self.adj.layers[i].clone();
        let Some(img) = crate::layers::load(&l.image) else { return };
        // Même placement que develop.wgsl : demi-taille du calque en fraction du cadre.
        let (frame, la) = (img_rect.width() / img_rect.height(), img.w as f32 / img.h as f32);
        let (hx, hy) = if la > frame { (0.5 * l.scale * la / frame, 0.5 * l.scale) } else { (0.5 * l.scale, 0.5 * l.scale * frame / la) };
        // Calculs en pixels de l'écran (mêmes unités en x et en y) : la rotation y est exacte.
        let center = pos2(img_rect.min.x + (0.5 + l.x) * img_rect.width(), img_rect.min.y + (0.5 + l.y) * img_rect.height());
        let half = vec2(hx * img_rect.width(), hy * img_rect.height());
        let (sn, cs) = l.angle.to_radians().sin_cos();
        let to_screen = |t: [f32; 2]| {
            let tx = if l.flip { 1.0 - t[0] } else { t[0] };
            let (a, b) = ((tx - 0.5) * 2.0 * half.x, (t[1] - 0.5) * 2.0 * half.y);
            center + vec2(cs * a - sn * b, sn * a + cs * b)
        };
        let to_layer = |p: Pos2| {
            let d = p - center;
            let (a, b) = (cs * d.x + sn * d.y, cs * d.y - sn * d.x);
            let tx = (a / (2.0 * half.x) + 0.5).clamp(0.0, 1.0);
            [if l.flip { 1.0 - tx } else { tx }, (b / (2.0 * half.y) + 0.5).clamp(0.0, 1.0)]
        };
        let p = ui.painter_at(view);
        let dark = Stroke::new(3.0, Color32::from_black_alpha(140));
        let white = Stroke::new(1.2, Color32::WHITE);
        let frame_pts: Vec<Pos2> = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]].map(to_screen).to_vec();
        p.add(egui::Shape::closed_line(frame_pts.clone(), dark));
        p.add(egui::Shape::closed_line(frame_pts, Stroke::new(1.0, Color32::from_white_alpha(170))));
        if l.clip.len() >= 3 {
            let c: Vec<Pos2> = l.clip.iter().map(|t| to_screen(*t)).collect();
            p.add(egui::Shape::closed_line(c, Stroke::new(1.0, palette::ACCENT)));
        }
        if resp.drag_stopped() && self.painting {
            self.painting = false;
            if self.layer_tool == 2 {
                history::mark(ui.ctx(), t!("Calque gommé").to_string());
            } else if self.layer_tool == 1 {
                let pts = std::mem::take(&mut self.layer_lasso);
                if pts.len() >= 3 {
                    self.adj.layers[i].clip = pts;
                    history::mark(ui.ctx(), t!("Calque découpé").to_string());
                }
            } else if self.layer_rotating {
                history::mark(ui.ctx(), t!("Calque tourné").to_string());
            } else {
                history::mark(ui.ctx(), t!("Calque déplacé").to_string());
            }
            self.layer_rotating = false;
        }
        if self.layer_tool == 0 {
            // Poignée de rotation : un rond au-dessus du bord haut du calque, relié à son milieu.
            let top = to_screen([0.5, 0.0]);
            let up = (top - center).normalized();
            let up = if up.is_finite() { up } else { vec2(0.0, -1.0) };
            let knob = top + up * 28.0;
            p.line_segment([top, knob], dark);
            p.line_segment([top, knob], white);
            p.circle_filled(knob, 7.0, Color32::from_black_alpha(140));
            p.circle_stroke(knob, 6.0, white);
            let pointer = resp.hover_pos().or(resp.interact_pointer_pos());
            let on_knob = pointer.is_some_and(|q| q.distance(knob) <= 12.0);
            if resp.hovered() {
                let icon = if self.layer_rotating || (on_knob && !self.painting) {
                    egui::CursorIcon::Alias
                } else if self.painting {
                    egui::CursorIcon::Grabbing
                } else {
                    egui::CursorIcon::Grab
                };
                ui.ctx().set_cursor_icon(icon);
            }
            if resp.drag_started_by(egui::PointerButton::Primary) {
                self.painting = true;
                let origin = ui.input(|i| i.pointer.press_origin());
                self.layer_rotating = origin.is_some_and(|q| q.distance(knob) <= 12.0);
                // Point saisi dans le calque, pour un déplacement sans saut.
                self.layer_grab = origin.map(|q| q - center);
            }
            if self.painting && resp.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let l = &mut self.adj.layers[i];
                    if self.layer_rotating {
                        // Angle du pointeur autour du centre, 0 quand il est au-dessus ; Maj : pas de 15°.
                        let d = pos - center;
                        let mut a = d.x.atan2(-d.y).to_degrees();
                        if ui.input(|i| i.modifiers.shift) {
                            a = (a / 15.0).round() * 15.0;
                        }
                        l.angle = if a > 180.0 { a - 360.0 } else { a };
                    } else if let Some(g) = self.layer_grab {
                        // Position absolue (pointeur moins point saisi) : le calque suit la souris exactement.
                        let c = pos - g;
                        l.x = ((c.x - img_rect.min.x) / img_rect.width() - 0.5).clamp(-1.0, 1.0);
                        l.y = ((c.y - img_rect.min.y) / img_rect.height() - 0.5).clamp(-1.0, 1.0);
                    }
                }
            }
        } else if self.layer_tool == 2 {
            // Gomme du calque : rayon en fraction de la largeur du calque ; Alt remet ce qui a été gommé.
            let r = self.layer_brush * 2.0 * hx * img_rect.width();
            if let Some(pos) = resp.hover_pos() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                p.circle_stroke(pos, r, dark);
                p.circle_stroke(pos, r, white);
            }
            let restore = ui.input(|i| i.modifiers.alt);
            let stroke = |pts: Vec<[f32; 2]>, radius: f32| BrushStroke { points: pts, radius, hardness: 0.4, flow: 1.0, erase: !restore, fill: false };
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    self.adj.layers[i].strokes.push(stroke(vec![to_layer(pos)], self.layer_brush));
                    history::mark(ui.ctx(), t!("Calque gommé").to_string());
                }
            }
            if resp.drag_started_by(egui::PointerButton::Primary) {
                self.painting = true;
                self.adj.layers[i].strokes.push(stroke(Vec::new(), self.layer_brush));
            }
            if self.painting && resp.dragged_by(egui::PointerButton::Primary) {
                if let (Some(pos), Some(s)) = (resp.interact_pointer_pos(), self.adj.layers[i].strokes.last_mut()) {
                    let t = to_layer(pos);
                    if s.points.last().is_none_or(|q| (q[0] - t[0]).abs() + (q[1] - t[1]).abs() > s.radius * 0.2) {
                        s.points.push(t);
                    }
                }
            }
        } else {
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if resp.drag_started_by(egui::PointerButton::Primary) {
                self.painting = true;
                self.layer_lasso.clear();
            }
            if self.painting {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let t = to_layer(pos);
                    if self.layer_lasso.last().is_none_or(|q| (q[0] - t[0]).abs() + (q[1] - t[1]).abs() > 0.002) {
                        self.layer_lasso.push(t);
                    }
                }
                let pts: Vec<Pos2> = self.layer_lasso.iter().map(|t| to_screen(*t)).collect();
                if pts.len() >= 2 {
                    p.add(egui::Shape::closed_line(pts.clone(), dark));
                    p.add(egui::Shape::closed_line(pts, white));
                }
            }
        }
    }

    fn retouch_overlay(&mut self, ui: &egui::Ui, resp: &egui::Response, img_rect: Rect, view: Rect, src: [f32; 2]) {
        let Some(tool) = self.retouch_tool else { return };
        let adj = self.adj.clone();
        let lens = self.lens_geo;
        let to_screen = |uv: [f32; 2]| {
            let uv = lens.map_or(uv, |l| l.to_corrected(uv, src));
            let o = adj.src_to_out(uv, src, false);
            pos2(img_rect.min.x + o[0] * img_rect.width(), img_rect.min.y + o[1] * img_rect.height())
        };
        let to_src = |p: Pos2| {
            let s = adj.out_to_src([(p.x - img_rect.min.x) / img_rect.width(), (p.y - img_rect.min.y) / img_rect.height()], src, false);
            lens.map_or(s, |l| l.to_source(s, src))
        };
        let (ow, _) = adj.output_size(src[0] as u32, src[1] as u32);
        let px_scale = img_rect.width() / ow as f32;
        // Rien n'est dessiné hors de la photo (retouche faite avant un recadrage).
        let p = ui.painter_at(view.intersect(img_rect));
        let dark = Stroke::new(3.0, Color32::from_black_alpha(140));
        let white = Stroke::new(1.5, Color32::WHITE);
        let origin = ui.input(|i| i.pointer.press_origin());
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let primary = resp.dragged_by(egui::PointerButton::Primary);
        if resp.drag_stopped() {
            self.heal_drag = None;
            self.painting = false;
        }
        // Retouches existantes : cercle plein (tache) et pointillé (source).
        for (k, r) in adj.retouch.iter().enumerate() {
            if let Retouch::Heal { center, radius, offset } = r {
                let rr = radius * src[0] * px_scale;
                let c = to_screen(*center);
                let s = to_screen([center[0] + offset[0], center[1] + offset[1]]);
                p.circle_stroke(c, rr, dark);
                p.circle_stroke(c, rr, white);
                let pts: Vec<Pos2> = (0..=48).map(|t| { let a = t as f32 / 48.0 * std::f32::consts::TAU; s + vec2(rr * a.cos(), rr * a.sin()) }).collect();
                p.add(egui::Shape::dashed_line(&pts, Stroke::new(1.2, Color32::from_rgb(120, 200, 255)), 4.0, 4.0));
                p.line_segment([c, s], Stroke::new(1.0, Color32::from_white_alpha(90)));
                if resp.drag_started_by(egui::PointerButton::Primary) && origin.is_some_and(|o| o.distance(s) < rr.max(8.0)) && tool == RetouchTool::Heal {
                    self.heal_drag = Some(k);
                }
            }
        }
        let r_screen = self.brush.radius * src[0] * px_scale;
        if let Some(pos) = resp.hover_pos() {
            if tool != RetouchTool::Heal && self.select_shape != 0 {
                // Lasso, rectangle : croix de visée.
                for (d, s) in [(vec2(8.0, 0.0), dark), (vec2(0.0, 8.0), dark), (vec2(8.0, 0.0), white), (vec2(0.0, 8.0), white)] {
                    p.line_segment([pos - d, pos + d], s);
                }
            } else {
                p.circle_stroke(pos, r_screen, dark);
                p.circle_stroke(pos, r_screen, Stroke::new(1.0, if tool != RetouchTool::Heal { Color32::from_rgb(255, 120, 120) } else { Color32::WHITE }));
            }
        }
        match tool {
            RetouchTool::Heal => {
                if let (Some(k), true, Some(pos)) = (self.heal_drag, primary, pointer) {
                    let q = to_src(pos);
                    if let Some(Retouch::Heal { center, offset, .. }) = self.adj.retouch.get_mut(k) {
                        *offset = [q[0] - center[0], q[1] - center[1]];
                    }
                } else if resp.clicked() {
                    if let Some(pos) = pointer {
                        let c = to_src(pos);
                        let (w, h) = (src[0] as u32, src[1] as u32);
                        let r_px = self.brush.radius * src[0];
                        let o = if self.work_half.len() == (w * h * 4) as usize {
                            let canvas = crate::retouch::HalfCanvas { data: &mut self.work_half, w, h };
                            crate::retouch::auto_offset(&canvas, [c[0] * src[0], c[1] * src[1]], r_px)
                        } else {
                            [2.5 * r_px, 0.0]
                        };
                        self.adj.retouch.push(Retouch::Heal { center: c, radius: self.brush.radius, offset: [o[0] / src[0], o[1] / src[1]] });
                    }
                }
            }
            RetouchTool::Erase | RetouchTool::Generate => {
                let erase = ui.input(|i| i.modifiers.alt);
                let shape = self.select_shape;
                let started = resp.drag_started_by(egui::PointerButton::Primary);
                // Lasso et rectangle : un simple clic ne sélectionne rien.
                if started || (resp.clicked() && shape == 0) {
                    if let Some(pos) = origin.or(pointer) {
                        self.painting = started;
                        let q = to_src(pos);
                        // Lasso, rectangle : bord adouci sur 2 px d'écran environ.
                        let (radius, points) = match shape {
                            0 => (self.brush.radius, vec![q]),
                            1 => (2.0 / (src[0] * px_scale).max(1.0), vec![q]),
                            _ => (2.0 / (src[0] * px_scale).max(1.0), vec![q; 4]),
                        };
                        self.erase_strokes.push(BrushStroke { points, radius, hardness: 0.9, flow: 1.0, erase, fill: shape != 0 });
                    }
                }
                if self.painting && primary {
                    if let (Some(pos), Some(s)) = (pointer, self.erase_strokes.last_mut()) {
                        let q = to_src(pos);
                        if shape == 2 && s.points.len() == 4 {
                            let a = s.points[0];
                            s.points = vec![a, [q[0], a[1]], q, [a[0], q[1]]];
                        } else {
                            let last = *s.points.last().unwrap();
                            // Lasso : un point tous les 3 px d'écran.
                            let gap = if s.fill { 3.0 / px_scale } else { s.radius * src[0] * 0.25 };
                            if ((q[0] - last[0]) * src[0]).hypot((q[1] - last[1]) * src[1]) > gap {
                                s.points.push(q);
                            }
                        }
                    }
                }
                // Zone peinte en rouge translucide, rasterisée comme le masque de la gomme (traits
                // arrondis, Alt qui retire) : un trait épais d'egui a des bouts plats et ne
                // montrait pas la zone réellement traitée.
                let area = view.intersect(img_rect);
                if self.erase_strokes.is_empty() || !area.is_positive() {
                    self.stroke_tex = None;
                } else {
                    use std::hash::{Hash, Hasher};
                    let (bw, bh) = (((area.width() * 0.5).ceil() as u32).max(1), ((area.height() * 0.5).ceil() as u32).max(1));
                    let local: Vec<BrushStroke> = self
                        .erase_strokes
                        .iter()
                        .map(|s| BrushStroke {
                            points: s.points.iter().map(|q| { let o = to_screen(*q); [(o.x - area.min.x) / area.width(), (o.y - area.min.y) / area.height()] }).collect(),
                            radius: s.radius * src[0] * px_scale / area.width(),
                            ..s.clone()
                        })
                        .collect();
                    let mut hs = std::hash::DefaultHasher::new();
                    (bw, bh).hash(&mut hs);
                    for s in &local {
                        (s.radius.to_bits(), s.erase, s.fill, s.points.len()).hash(&mut hs);
                        s.points.iter().for_each(|q| (q[0].to_bits(), q[1].to_bits()).hash(&mut hs));
                    }
                    let key = hs.finish();
                    if self.stroke_tex.as_ref().map(|t| t.0) != Some(key) {
                        let mut m = vec![0f32; (bw * bh) as usize];
                        crate::masks::paint_strokes(&mut m, bw, bh, &local);
                        let rgba: Vec<u8> = m.iter().flat_map(|&v| [255, 60, 60, (v.clamp(0.0, 1.0) * 110.0) as u8]).collect();
                        let img = egui::ColorImage::from_rgba_unmultiplied([bw as usize, bh as usize], &rgba);
                        self.stroke_tex = Some((key, ui.ctx().load_texture("zone peinte", img, egui::TextureOptions::LINEAR)));
                    }
                    if let Some((_, tex)) = &self.stroke_tex {
                        p.image(tex.id(), area, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                }
            }
        }
    }

    fn lens_ui(&mut self, ui: &mut egui::Ui) {
        let shot = self.shot.clone().unwrap_or_default();
        let cam = crate::lens::find_camera(&shot.make, &shot.model);
        let detected = shot.lens.as_deref().and_then(|n| crate::lens::find_lens(n, cam.map(|c| c.crop)));
        let fix = &mut self.adj.lens;
        ui.checkbox(&mut fix.enabled, t!("Corriger l'objectif (base Lensfun)"));
        // Passe à la ligne : un nom d'objectif EXIF long élargissait le panneau, recouvert alors par la photo.
        let info = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(k).weak());
                ui.label(v);
            });
        };
        // Le modèle contient souvent déjà la marque (« Canon EOS 5D Mark III »).
        let mut cam_text = if shot.model.to_lowercase().starts_with(&shot.make.to_lowercase()) { shot.model.clone() } else { format!("{} {}", shot.make, shot.model).trim().to_string() };
        if cam_text.is_empty() {
            cam_text = "inconnu".into();
        }
        if let Some(c) = cam {
            cam_text += &tf!(" (×{0:.2})", c.crop);
        }
        info(ui, t!("Boîtier :"), cam_text);
        info(ui, t!("Objectif (EXIF) :"), shot.lens.clone().unwrap_or_else(|| t!("non indiqué").into()));
        let focal = shot.focal.map_or("?".into(), |f| tf!("{f:.0} mm", f));
        let ap = shot.aperture.map_or("?".into(), |a| format!("f/{a:.1}"));
        info(ui, t!("Prise de vue :"), format!("{focal} · {ap}"));
        let current = match &fix.lens {
            Some(k) => k.clone(),
            None => match detected {
                Some(l) => tf!("Auto : {0}", l.key()),
                None => t!("Auto : aucun profil trouvé").into(),
            },
        };
        egui::ComboBox::from_id_salt("objectif").width(ui.available_width() - 8.0).truncate().selected_text(current).show_ui(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.lens_search).hint_text(t!("Chercher (ex. 24-105)")));
            if ui.selectable_label(fix.lens.is_none(), t!("Automatique (EXIF)")).clicked() {
                fix.lens = None;
            }
            let q: Vec<String> = self.lens_search.to_lowercase().split_whitespace().map(String::from).collect();
            let mut shown = 0;
            for l in &crate::lens::db().lenses {
                let k = l.key();
                let low = k.to_lowercase();
                if !q.iter().all(|w| low.contains(w.as_str())) {
                    continue;
                }
                shown += 1;
                if shown > 200 {
                    ui.small(t!("… affine la recherche"));
                    break;
                }
                if ui.selectable_label(fix.lens.as_deref() == Some(k.as_str()), &k).clicked() {
                    fix.lens = Some(k);
                    fix.enabled = true;
                }
            }
        });
        if fix.enabled && fix.lens.is_none() && detected.is_none() {
            ui.small(t!("Objectif non reconnu : choisis-le dans la liste."));
        }
        if shot.focal.is_none() && fix.enabled {
            ui.small(t!("Focale absente des EXIF : la plus courte calibrée est utilisée."));
        }
        ui.add_enabled_ui(fix.enabled, |ui| {
            slider(ui, t!("Distorsion"), &mut fix.distortion, 0.0..=1.0, 1.0);
            slider(ui, t!("Vignetage"), &mut fix.vignetting, 0.0..=1.0, 1.0);
            ui.checkbox(&mut fix.tca, t!("Aberration chromatique"));
            ui.checkbox(&mut fix.autoscale, t!("Zoom automatique (pas de bords vides)"));
        });
    }

    fn layers_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::adjust::{Blend, LayerMask, MAX_LAYERS};
        let full = self.adj.layers.len() >= MAX_LAYERS;
        if ui.add_enabled(!full, egui::Button::new(t!("Ajouter une image…"))).on_hover_text(t!("Texture, logo ou autre photo (PNG transparent accepté)")).clicked() {
            self.open_layer_picker(ctx);
        }
        if self.adj.layers.is_empty() {
            ui.small(t!("Un calque pose une image sur la photo : texture, effet de lumière, logo ou autre photo. Jusqu'à trois calques."));
            return;
        }
        if full {
            ui.small(t!("Trois calques au plus."));
        }
        ui.add_space(4.0);
        // Liste, du plus haut au plus bas comme dans les logiciels de retouche.
        let n = self.adj.layers.len();
        self.layer_sel = self.layer_sel.min(n - 1);
        let mut action: Option<(usize, i32)> = None;
        for i in (0..n).rev() {
            ui.horizontal(|ui| {
                let l = &mut self.adj.layers[i];
                if ui.checkbox(&mut l.visible, "").on_hover_text(t!("Visible")).changed() {
                    history::mark(ui.ctx(), t!("Calque masqué ou affiché").to_string());
                }
                let missing = crate::layers::load(&l.image).is_none();
                let mut name = egui::RichText::new(crate::layers::name(&l.image));
                if missing {
                    name = name.color(Color32::from_rgb(230, 110, 90));
                }
                let r = ui.add(egui::Button::selectable(self.layer_sel == i, name).truncate().min_size(vec2(ui.available_width(), 24.0)));
                if missing {
                    r.clone().on_hover_text(tf!("Image introuvable : {0}", l.image));
                }
                if r.clicked() {
                    self.layer_sel = i;
                }
            });
        }
        let i = self.layer_sel;
        ui.horizontal(|ui| {
            if ui.add_enabled(i + 1 < n, egui::Button::new(t!("Monter"))).clicked() {
                action = Some((i, 1));
            }
            if ui.add_enabled(i > 0, egui::Button::new(t!("Descendre"))).clicked() {
                action = Some((i, -1));
            }
            if ui.button(t!("Retirer")).clicked() {
                action = Some((i, 0));
            }
        });
        if let Some((i, d)) = action {
            if d == 0 {
                self.adj.layers.remove(i);
                history::mark(ctx, t!("Calque retiré").to_string());
                if self.adj.layers.is_empty() {
                    return;
                }
                self.layer_sel = self.layer_sel.min(self.adj.layers.len() - 1);
            } else {
                let j = (i as i32 + d) as usize;
                self.adj.layers.swap(i, j);
                self.layer_sel = j;
                history::mark(ctx, t!("Ordre des calques").to_string());
            }
        }
        ui.separator();
        let i = self.layer_sel;
        let mut l = self.adj.layers[i].clone();
        ui.label(egui::RichText::new(crate::layers::name(&l.image)).strong());
        slider(ui, t!("Opacité"), &mut l.opacity, 0.0..=1.0, 1.0);
        ui.label(t!("Fusion"));
        let mut changed = segmented(ui, &mut l.blend, &[(Blend::Normal, t!("Normal")), (Blend::Multiply, t!("Produit")), (Blend::Screen, t!("Écran")), (Blend::Darken, t!("Assombrir"))]);
        changed |= segmented(ui, &mut l.blend, &[(Blend::Overlay, t!("Incrustation")), (Blend::SoftLight, t!("Lumière douce")), (Blend::Lighten, t!("Éclaircir"))]);
        ui.label(t!("Zone"));
        changed |= segmented(ui, &mut l.mask, &[(LayerMask::All, t!("Partout")), (LayerMask::Sky, t!("Ciel")), (LayerMask::Subject, t!("Sujet")), (LayerMask::Background, t!("Fond"))]);
        let model = match l.mask {
            LayerMask::All => None,
            LayerMask::Sky => Some(Model::Sky),
            _ => Some(Model::Subject),
        };
        if let Some(m) = model.filter(|m| !self.ai_masks.contains_key(m)) {
            ui.horizontal(|ui| {
                if self.ai_failed.contains(&m) {
                    ui.small(t!("Masque IA indisponible : le calque reste caché."));
                } else {
                    ui.spinner();
                    ui.small(t!("Détection en cours…"));
                }
            });
        }
        slider(ui, t!("Taille"), &mut l.scale, 0.05..=2.0, 1.0);
        slider(ui, t!("Position X"), &mut l.x, -1.0..=1.0, 0.0);
        slider(ui, t!("Position Y"), &mut l.y, -1.0..=1.0, 0.0);
        slider(ui, t!("Rotation"), &mut l.angle, -180.0..=180.0, 0.0);
        changed |= ui.checkbox(&mut l.flip, t!("Miroir")).changed();
        ui.small(t!("Taille 1 : le calque couvre tout le cadre. Pour un logo, réduis la taille puis place-le."));
        ui.add_space(4.0);
        ui.label(t!("Sur la photo"));
        segmented(ui, &mut self.layer_tool, &[(0, t!("Déplacer")), (1, t!("Lasso")), (2, t!("Gommer"))]);
        ui.small(match self.layer_tool {
            0 => t!("Fais glisser le calque sur la photo pour le placer ; le rond au-dessus le fait tourner (Maj : par pas de 15°)."),
            1 => t!("Entoure sur la photo la partie du calque à garder ; le reste devient transparent."),
            _ => t!("Peins sur le calque pour l'effacer, bord doux ; Alt pour remettre ce qui a été gommé."),
        });
        if self.layer_tool == 2 {
            slider(ui, t!("Taille du pinceau"), &mut self.layer_brush, 0.005..=0.3, 0.05);
        }
        ui.horizontal(|ui| {
            if !l.clip.is_empty() && ui.button(t!("Retirer la découpe")).clicked() {
                l.clip.clear();
                changed = true;
            }
            if !l.strokes.is_empty() && ui.button(t!("Retirer la gomme")).clicked() {
                l.strokes.clear();
                changed = true;
            }
        });
        ui.add_space(4.0);
        ui.label(t!("Raccord avec la photo"));
        changed |= ui.checkbox(&mut l.subject, t!("Sujet seul (détourage IA)")).on_hover_text(t!("Le fond de l'image du calque est retiré ; seul son sujet principal reste.")).changed();
        if l.subject {
            match crate::layers::subject(&l.image) {
                crate::layers::Subject::Pending => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.small(t!("Détourage du calque en cours…"));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(300));
                }
                crate::layers::Subject::Failed(e) => {
                    ui.small(tf!("Détourage impossible : {e}", e));
                }
                crate::layers::Subject::Ready(_) => {}
            }
        }
        slider(ui, t!("Harmoniser"), &mut l.harmony, 0.0..=1.0, 0.0);
        ui.small(t!("Met le calque dans l'ambiance de la photo : teinte, voile, blancs ; sa luminosité propre est gardée."));
        ui.add_space(4.0);
        if ui.add_enabled(self.file_job.is_none(), egui::Button::new(t!("Fusionner dans une nouvelle image"))).clicked() {
            self.file_request = Some(FileTool::Flatten);
        }
        self.file_job_status(ui, FileTool::Flatten);
        ui.small(t!("Photo et calques aplatis dans un nouveau fichier (Images/Luminux), ouvert aussitôt : gomme, retouche et réglages s'appliquent alors à l'ensemble."));
        if changed {
            history::mark(ctx, t!("Réglage du calque").to_string());
        }
        self.adj.layers[i] = l;
    }

    fn sky_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.skies.is_empty() {
            self.skies = sky::list();
        }
        ui.horizontal(|ui| {
            if ui.button(t!("Ajouter des ciels…")).on_hover_text(t!("Tes propres photos de ciel (JPEG, PNG, TIFF)")).clicked() {
                self.dialog(ctx, || rfd::FileDialog::new().add_filter(t!("Images"), sky::SKY_EXTS).pick_files().map(Picked::Skies));
            }
            if ui.button(t!("Dossier")).on_hover_text(sky::skies_dir().display().to_string()).clicked() {
                let _ = std::process::Command::new("xdg-open").arg(sky::skies_dir()).spawn();
            }
            if ui.add_enabled(self.adj.sky.image.is_some(), egui::Button::new(t!("Aucun"))).clicked() {
                self.adj.sky.image = None;
                self.sky_show_mask = false;
            }
        });
        // Vignettes des ciels, une décodée par image affichée.
        let mut decoded = false;
        let w = (ui.available_width() - 24.0) / 3.0;
        egui::Grid::new("ciels").spacing(vec2(6.0, 6.0)).show(ui, |ui| {
            for (k, name) in self.skies.clone().iter().enumerate() {
                if !self.sky_thumbs.contains_key(name) && !decoded {
                    decoded = true;
                    let tex = image::open(sky::resolve(name)).ok().map(|img| {
                        let t = img.thumbnail(240, 160).to_rgba8();
                        let ci = egui::ColorImage::from_rgba_unmultiplied([t.width() as usize, t.height() as usize], t.as_raw());
                        ctx.load_texture(format!("ciel:{name}"), ci, egui::TextureOptions::LINEAR)
                    });
                    self.sky_thumbs.insert(name.clone(), tex);
                    ctx.request_repaint();
                }
                let selected = self.adj.sky.image.as_deref() == Some(name.as_str());
                let label = name.rsplit_once('.').map_or(name.as_str(), |(n, _)| n);
                let resp = match self.sky_thumbs.get(name).and_then(|t| t.as_ref()) {
                    Some(t) => {
                        let size = vec2(w, w * 0.6);
                        let r = ui.add(egui::Image::new((t.id(), size)).fit_to_exact_size(size).sense(Sense::click()));
                        if selected {
                            ui.painter().rect_stroke(r.rect, 2.0, Stroke::new(2.0, Color32::from_rgb(90, 160, 255)), egui::StrokeKind::Outside);
                        }
                        r.on_hover_text(label)
                    }
                    None => ui.add_sized(vec2(w, w * 0.6), egui::Button::new(label).selected(selected)),
                };
                if resp.clicked() {
                    self.adj.sky.image = if selected { None } else { Some(name.clone()) };
                }
                if k % 3 == 2 {
                    ui.end_row();
                }
            }
        });
        if self.adj.sky.image.is_none() {
            ui.small(t!("Choisis un ciel : il remplace le ciel détecté par l'IA."));
            return;
        }
        if self.ai_job.as_ref().is_some_and(|j| j.model == Model::Sky) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small(t!("Détection du ciel en cours…"));
            });
        } else if self.ai_failed.contains(&Model::Sky) {
            ui.colored_label(Color32::from_rgb(230, 120, 90), t!("Le ciel n'a pas pu être détecté (voir la barre d'état)."));
            if ui.small_button(t!("Réessayer")).clicked() {
                self.ai_failed.remove(&Model::Sky);
            }
        }
        ui.checkbox(&mut self.sky_show_mask, t!("Voir le masque du ciel"));
        let s = &mut self.adj.sky;
        slider(ui, t!("Opacité"), &mut s.amount, 0.0..=1.0, 1.0);
        slider(ui, t!("Position verticale"), &mut s.offset, -0.5..=0.5, 0.0);
        slider(ui, t!("Zoom"), &mut s.scale, 1.0..=3.0, 1.0);
        ui.checkbox(&mut s.flip, t!("Miroir horizontal"));
        slider(ui, t!("Exposition du ciel"), &mut s.exposure, -2.0..=2.0, 0.0);
        slider(ui, t!("Rééclairer la scène"), &mut s.relight, 0.0..=1.0, 0.5);
    }

    fn crop_overlay(&mut self, ui: &egui::Ui, resp: &egui::Response, img_rect: Rect, view: Rect, src: [f32; 2]) {
        let c = self.adj.crop;
        let to_screen = |x: f32, y: f32| pos2(img_rect.left() + x * img_rect.width(), img_rect.top() + y * img_rect.height());
        let cr = Rect::from_min_max(to_screen(c[0], c[1]), to_screen(c[0] + c[2], c[1] + c[3]));
        let corners = [cr.left_top(), cr.right_top(), cr.left_bottom(), cr.right_bottom()];

        if resp.drag_started() {
            // Point de départ du clic (et non la position après le seuil de glissement).
            if let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
                self.crop_drag = corners
                    .iter()
                    .position(|k| k.distance(pos) < 20.0)
                    .map(CropDrag::Corner)
                    .or_else(|| cr.contains(pos).then_some(CropDrag::Move));
            }
        }
        if resp.drag_stopped() {
            self.crop_drag = None;
        }
        if let (Some(drag), true) = (self.crop_drag, resp.dragged()) {
            let d = resp.drag_delta() / img_rect.size();
            let mut n = c;
            match drag {
                CropDrag::Move => {
                    n[0] = (c[0] + d.x).clamp(0.0, 1.0 - c[2]);
                    n[1] = (c[1] + d.y).clamp(0.0, 1.0 - c[3]);
                    if crop_inside(n, self.adj.angle, src[0], src[1]) {
                        self.adj.crop = n;
                    }
                }
                CropDrag::Corner(i) => {
                    let (mut x0, mut y0, mut x1, mut y1) = (c[0], c[1], c[0] + c[2], c[1] + c[3]);
                    if i % 2 == 0 { x0 += d.x } else { x1 += d.x }
                    if i < 2 { y0 += d.y } else { y1 += d.y }
                    let (x0, x1) = (x0.clamp(0.0, x1 - 0.02), x1.clamp(x0 + 0.02, 1.0));
                    let (mut y0, mut y1) = (y0.clamp(0.0, y1 - 0.02), y1.clamp(y0 + 0.02, 1.0));
                    if ASPECTS[self.crop_aspect].1 != 0.0 {
                        // Ratio verrouillé : la hauteur suit la largeur, depuis le coin opposé.
                        let mut ratio = ASPECTS[self.crop_aspect].1;
                        if ratio < 0.0 {
                            ratio = src[0] / src[1];
                            if self.crop_portrait != (src[1] > src[0]) { ratio = 1.0 / ratio; }
                        } else if self.crop_portrait {
                            ratio = 1.0 / ratio;
                        }
                        let hh = (x1 - x0) * src[0] / (ratio * src[1]);
                        if i < 2 { y0 = y1 - hh } else { y1 = y0 + hh }
                    }
                    let nc = [x0, y0, x1 - x0, y1 - y0];
                    if nc[1] >= 0.0 && nc[1] + nc[3] <= 1.0 && crop_inside(nc, self.adj.angle, src[0], src[1]) {
                        self.adj.crop = nc;
                    }
                }
            }
        }

        let p = ui.painter_at(view);
        let shade = Color32::from_black_alpha(150);
        let ir = img_rect;
        p.rect_filled(Rect::from_min_max(ir.left_top(), pos2(ir.right(), cr.top())), 0.0, shade);
        p.rect_filled(Rect::from_min_max(pos2(ir.left(), cr.bottom()), ir.right_bottom()), 0.0, shade);
        p.rect_filled(Rect::from_min_max(pos2(ir.left(), cr.top()), pos2(cr.left(), cr.bottom())), 0.0, shade);
        p.rect_filled(Rect::from_min_max(pos2(cr.right(), cr.top()), pos2(ir.right(), cr.bottom())), 0.0, shade);
        let thin = Stroke::new(1.0, Color32::from_white_alpha(110));
        for k in 1..3 {
            let t = k as f32 / 3.0;
            p.line_segment([pos2(cr.left() + t * cr.width(), cr.top()), pos2(cr.left() + t * cr.width(), cr.bottom())], thin);
            p.line_segment([pos2(cr.left(), cr.top() + t * cr.height()), pos2(cr.right(), cr.top() + t * cr.height())], thin);
        }
        p.rect_stroke(cr, 0.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Middle);
        for k in corners {
            p.rect_filled(Rect::from_center_size(k, vec2(10.0, 10.0)), 2.0, Color32::WHITE);
        }
        p.text(view.left_top() + vec2(12.0, 12.0), egui::Align2::LEFT_TOP, "RECADRAGE · Entrée pour valider", egui::FontId::proportional(14.0), Color32::WHITE);
    }

    fn histogram_ui(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, Color32::from_gray(18));
        let Some(hist) = &self.histogram else { return };
        // Normalisation sur le 99e centile des bacs pour éviter qu'un pic écrase tout.
        let mut all: Vec<u32> = hist[..3].iter().flatten().copied().collect();
        all.sort_unstable();
        let top = all[(all.len() * 99) / 100].max(1) as f32;
        let colors = [
            Color32::from_rgba_unmultiplied(255, 70, 70, 90),
            Color32::from_rgba_unmultiplied(70, 255, 70, 90),
            Color32::from_rgba_unmultiplied(80, 120, 255, 110),
            Color32::from_rgba_unmultiplied(220, 220, 220, 60),
        ];
        for (k, c) in colors.iter().enumerate() {
            for (i, v) in hist[k].iter().enumerate() {
                let x = rect.left() + rect.width() * (i as f32 + 0.5) / 256.0;
                let y = rect.bottom() - rect.height() * (*v as f32 / top).min(1.0);
                p.line_segment([pos2(x, rect.bottom()), pos2(x, y)], Stroke::new(rect.width() / 256.0 + 0.5, *c));
            }
        }
    }

    /// Panneau de l'onglet `tab` ; l'histogramme en tête si `histogram`.
    fn panel(&mut self, ui: &mut egui::Ui, tab: Tab, histogram: bool) {
        if histogram {
            self.histogram_ui(ui);
            ui.add_space(6.0);
        }
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tab.title()).heading().size(17.0));
            if histogram {
                // Bouton « masquer » : chevrons vers le bord où le panneau se replie.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if panel_button(ui, self.panel_collapses_right(), t!("Masquer le panneau (P)")).clicked() {
                        self.toggle_panel();
                    }
                });
            }
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().slider_width = 160.0;
            match tab {
                Tab::Develop => self.develop_ui(ui),
                Tab::Local => self.locals_ui(ui),
                Tab::Crop => self.crop_ui(ui),
                Tab::Looks => self.looks_ui(ui, &ctx),
                Tab::Sky => self.sky_ui(ui, &ctx),
                Tab::Retouch => self.retouch_ui(ui),
                Tab::Lens => self.lens_ui(ui),
                Tab::Layers => self.layers_ui(ui, &ctx),
                Tab::Ai => {
                    self.depth_status(ui);
                    self.accordion(ui, &AI_SECTIONS, Self::ai_section);
                }
            }
        });
    }

    /// Change d'onglet ; quitter un outil le désactive (pinceau, recadrage, masque affiché).
    fn set_tab(&mut self, tab: Tab) {
        self.panel_hidden = false;
        if tab == self.tab {
            return;
        }
        match self.tab {
            Tab::Retouch => self.retouch_tool = None,
            Tab::Sky => self.sky_show_mask = false,
            Tab::Ai => {
                self.depth_show_mask = false;
                self.focus_pick = false;
            }
            Tab::Local => self.local_sel = None,
            Tab::Crop => {
                self.crop_mode = false;
                self.fit = true;
            }
            _ => {}
        }
        if tab == Tab::Crop {
            self.crop_mode = true;
            self.fit = true;
        }
        self.tab = tab;
    }

    /// Mesure netteté et empreinte des photos pas encore analysées, en arrière-plan.
    fn start_cull(&mut self, ctx: &egui::Context) {
        let Some(cat) = &self.catalogue else { return };
        if self.cull.is_some() {
            return;
        }
        let todo = cat.to_cull();
        if todo.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        let (total, ctx) = (todo.len(), ctx.clone());
        std::thread::spawn(move || {
            for p in todo {
                // Illisible : netteté 0 et empreinte nulle, pour ne pas la reprendre à chaque fois.
                let (sharp, hash) = library::decode_preview(&p, 1024).map(|img| crate::cull::analyse(&img)).unwrap_or((0.0, 0));
                if tx.send((p, sharp, hash)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.cull = Some((rx, 0, total));
    }

    /// Ctrl+F et menu Bibliothèque : champ de recherche, depuis l'éditeur aussi.
    fn open_search(&mut self) {
        if self.mode != Mode::Library {
            self.leave_photo();
            self.mode = Mode::Library;
            self.scroll_to_sel = true;
        }
        self.focus_search = true;
    }

    /// H et menu Édition : ouvre l'onglet Historique, ou revient aux réglages s'il est déjà ouvert.
    /// Ouvre ou ferme la section Historique du bloc Réglages.
    fn toggle_history(&mut self) {
        let h = k!("Historique");
        if self.open_section == Some(h) {
            self.open_section = None;
        } else {
            self.open_section = Some(h);
            self.panel_hidden = false;
            if !self.settings.split_layout {
                self.set_tab(Tab::Develop);
            }
        }
    }

    /// P et menu Affichage : masque ou réaffiche le panneau des réglages et outils.
    fn toggle_panel(&mut self) {
        self.panel_hidden = !self.panel_hidden;
        self.fit = true;
    }

    /// Sens dans lequel le panneau des réglages se replie (vers la droite ou vers la gauche).
    fn panel_collapses_right(&self) -> bool {
        // Réglages à part : le panneau est du côté opposé à la barre d'icônes.
        self.settings.panel_left == self.settings.split_layout
    }

    /// Barre d'icônes verticale à droite du panneau.
    fn rail(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let split = self.settings.split_layout;
        for tab in Tab::ALL {
            // Réglages à part : l'onglet Réglages a son propre panneau.
            if split && tab == Tab::Develop {
                continue;
            }
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
            let on = self.tab == tab;
            let p = ui.painter_at(rect);
            if on {
                p.rect_filled(rect.shrink2(vec2(4.0, 2.0)), 8.0, palette::ITEM_ON);
            } else if resp.hovered() {
                p.rect_filled(rect.shrink2(vec2(4.0, 2.0)), 8.0, palette::ITEM);
            }
            let col = if on { Color32::WHITE } else { Color32::from_gray(170) };
            let icon = Rect::from_center_size(rect.center() - vec2(0.0, 8.0), vec2(20.0, 20.0));
            tab_icon(&p, icon, tab, col);
            p.text(rect.center() + vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, tab.short(), egui::FontId::proportional(11.0), col);
            if resp.on_hover_text(tab.title()).clicked() {
                if self.panel_hidden {
                    self.panel_hidden = false;
                    self.set_tab(tab);
                } else if split {
                    // Réglages à part : re-clic sur l'outil ouvert pour replier son panneau.
                    self.set_tab(if on { Tab::Develop } else { tab });
                } else if on {
                    // Re-clic sur l'onglet ouvert : masque le panneau.
                    self.panel_hidden = true;
                } else {
                    self.set_tab(tab);
                }
            }
        }
    }

    fn crop_ui(&mut self, ui: &mut egui::Ui) {
                ui.horizontal(|ui| {
                    if ui.toggle_value(&mut self.crop_mode, t!("Recadrer")).on_hover_text(t!("Touche C, Entrée pour valider")).changed() {
                        self.fit = true;
                    }
                    if ui.button(t!("Réinitialiser")).clicked() {
                        self.adj.crop = [0.0, 0.0, 1.0, 1.0];
                        self.adj.angle = 0.0;
                    }
                });
                let before = (self.crop_aspect, self.crop_portrait);
                ui.add_space(4.0);
                ui.label(t!("Format"));
                ratio_tiles(ui, &mut self.crop_aspect, self.crop_portrait);
                let r = ASPECTS[self.crop_aspect].1;
                ui.add_enabled_ui(r != 0.0 && r != 1.0, |ui| {
                    segmented(ui, &mut self.crop_portrait, &[(false, t!("Horizontal")), (true, t!("Vertical"))]);
                });
                ui.add_space(4.0);
                if (self.crop_aspect, self.crop_portrait) != before {
                    self.apply_aspect();
                }
                let old = self.adj.angle;
                slider(ui, t!("Redresser (°)"), &mut self.adj.angle, -45.0..=45.0, 0.0);
                if self.adj.angle != old {
                    if let Some((w, h)) = self.size {
                        self.adj.crop = fit_crop(self.adj.crop, self.adj.angle, w as f32, h as f32);
                    }
                }
                    ui.small(t!("Glisse les poignées sur l'image. Entrée ou Échap : terminer. Touche C : recadrer."));
    }

    /// Réglages globaux en accordéon : une section ouverte à la fois, sauf les sections épinglées.
    fn develop_ui(&mut self, ui: &mut egui::Ui) {
        self.accordion(ui, &DEVELOP_SECTIONS, Self::develop_section);
    }

    fn accordion(&mut self, ui: &mut egui::Ui, sections: &[&'static str], body: fn(&mut Self, &mut egui::Ui, &str)) {
        for &name in sections {
            let open = self.open_section == Some(name) || self.pinned.contains(name);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
            let p = ui.painter_at(rect);
            if resp.hovered() || open {
                p.rect_filled(rect, 6.0, if open { palette::ITEM } else { palette::WINDOW });
            }
            let c = rect.left_center() + vec2(14.0, 0.0);
            let tri = if open {
                vec![c + vec2(-5.0, -3.0), c + vec2(5.0, -3.0), c + vec2(0.0, 4.0)]
            } else {
                vec![c + vec2(-3.0, -5.0), c + vec2(4.0, 0.0), c + vec2(-3.0, 5.0)]
            };
            p.add(egui::Shape::convex_polygon(tri, Color32::from_gray(200), Stroke::NONE));
            let title = p.text(rect.left_center() + vec2(28.0, 0.0), egui::Align2::LEFT_CENTER, crate::i18n::tr(name), egui::FontId::proportional(14.5), Color32::from_gray(230));
            // Section modifiée : point orange après le titre, et flèche circulaire pour la remettre à zéro.
            let mut reset_clicked = false;
            if DEVELOP_SECTIONS.contains(&name) && self.adj.section_changed(name) {
                p.circle_filled(pos2(title.right() + 8.0, rect.center().y), 2.5, palette::CHANGED);
                let r = Rect::from_center_size(rect.right_center() - vec2(42.0, 0.0), vec2(20.0, 20.0));
                let rr = ui.interact(r, egui::Id::unique(("reset", name)), Sense::click()).on_hover_text(t!("Remettre la section à zéro"));
                let col = if rr.hovered() { Color32::WHITE } else { Color32::from_gray(160) };
                let c = r.center();
                let arc: Vec<Pos2> = (0..=20).map(|k| {
                    let a = (-60.0 + 270.0 * k as f32 / 20.0f32).to_radians();
                    c + vec2(a.cos(), a.sin()) * 5.5
                }).collect();
                let tip = *arc.last().unwrap();
                p.add(egui::Shape::line(arc, Stroke::new(1.5, col)));
                p.add(egui::Shape::convex_polygon(vec![tip + vec2(-3.5, -1.5), tip + vec2(2.5, -3.5), tip + vec2(1.0, 2.5)], col, Stroke::NONE));
                reset_clicked = rr.clicked();
            }
            // Étiquette des outils qui passent par un serveur ComfyUI (verte une fois connecté).
            if COMFY_SECTIONS.contains(&name) {
                let tag = Rect::from_center_size(rect.right_center() - vec2(64.0, 0.0), vec2(58.0, 17.0));
                let local = self.settings.ai_local && name != DENOISE_SECTION;
                let on = if self.settings.ai_local && name == DENOISE_SECTION {
                    false
                } else if !local {
                    !self.comfy_models.is_empty()
                } else if name == UPSCALE_SECTION {
                    self.upscale_ready()
                } else {
                    self.gen_ready()
                };
                p.rect_filled(tag, 8.0, if on { Color32::from_rgb(38, 78, 52) } else { Color32::from_gray(52) });
                p.text(tag.center(), egui::Align2::CENTER_CENTER, if local { t!("Local") } else { "ComfyUI" }, egui::FontId::proportional(11.0), if on { Color32::from_rgb(170, 230, 180) } else { Color32::from_gray(185) });
                let tip = match (local, on) {
                    (true, true) => t!("Calcul sur ce PC par le moteur local."),
                    (true, false) => t!("Moteur local : à télécharger, voir dans la section."),
                    (false, _) if self.settings.ai_local => t!("Désactivé avec le moteur local : demande ComfyUI."),
                    (false, true) => t!("Calcul sur ton serveur ComfyUI (connecté)."),
                    (false, false) => t!("Calcul sur un serveur ComfyUI : à installer à part, voir dans la section."),
                };
                ui.interact(tag, egui::Id::unique(("comfy", name)), Sense::hover()).on_hover_text(tip);
            }
            // Épingle : un rond à droite, plein si la section reste ouverte.
            let pin = Rect::from_center_size(rect.right_center() - vec2(16.0, 0.0), vec2(18.0, 18.0));
            let pin_resp = ui.interact(pin, egui::Id::unique(("pin", name)), Sense::click()).on_hover_text(t!("Garder cette section ouverte"));
            let pinned = self.pinned.contains(name);
            if pinned {
                p.circle_filled(pin.center(), 4.5, Color32::from_rgb(240, 190, 60));
            } else if resp.hovered() || pin_resp.hovered() {
                p.circle_stroke(pin.center(), 4.5, Stroke::new(1.2, Color32::from_gray(150)));
            }
            if reset_clicked {
                self.adj.reset_section(name);
                self.edit_label = Some(tf!("{0} remis à zéro", crate::i18n::tr(name)));
            } else if pin_resp.clicked() {
                if pinned {
                    self.pinned.remove(name);
                } else {
                    self.pinned.insert(name);
                }
            } else if resp.clicked() {
                self.open_section = if open && !pinned { None } else { Some(name) };
            }
            // Le corps glisse en s'ouvrant et en se fermant (durée : style.animation_time).
            let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), egui::Id::unique(("section", name)), open);
            st.set_open(open);
            st.show_body_unindented(ui, |ui| {
                ui.indent(name, |ui| body(self, ui, name));
                ui.add_space(4.0);
            });
        }
    }

    fn develop_section(&mut self, ui: &mut egui::Ui, name: &str) {
        if name == "Historique" {
            self.history_ui(ui);
            return;
        }
        let a = &mut self.adj;
        match name {
            "Lumière" => {
                slider(ui, t!("Exposition"), &mut a.exposure, -5.0..=5.0, 0.0);
                slider(ui, t!("Contraste"), &mut a.contrast, -1.0..=1.0, 0.0);
                slider(ui, t!("Hautes lumières"), &mut a.highlights, -1.0..=1.0, 0.0);
                slider(ui, t!("Ombres"), &mut a.shadows, -1.0..=1.0, 0.0);
                slider(ui, t!("Blancs"), &mut a.whites, -1.0..=1.0, 0.0);
                slider(ui, t!("Noirs"), &mut a.blacks, -1.0..=1.0, 0.0);
            }
            "Couleur" => {
                slider_with(ui, t!("Température"), &mut a.temperature, -1.0..=1.0, 0.0, &tracks::TEMPERATURE);
                slider_with(ui, t!("Teinte"), &mut a.tint, -1.0..=1.0, 0.0, &tracks::TINT);
                slider_with(ui, t!("Vibrance"), &mut a.vibrance, -1.0..=1.0, 0.0, &tracks::SATURATION);
                slider_with(ui, t!("Saturation"), &mut a.saturation, -1.0..=1.0, 0.0, &tracks::SATURATION);
            }
            "Courbe des tons" => {
                curve_editor(ui, &mut a.curve);
                ui.small(t!("Clic : ajouter un point. Double-clic sur un point : le retirer."));
                if ui.button(t!("Réinitialiser la courbe")).clicked() {
                    a.curve = Adjustments::default().curve;
                }
            }
            "Détails" => {
                slider(ui, t!("Netteté"), &mut a.sharpen, 0.0..=1.0, 0.0);
                slider(ui, t!("Bruit luminance"), &mut a.nr_luma, 0.0..=1.0, 0.0);
                slider(ui, t!("Bruit couleur"), &mut a.nr_color, 0.0..=1.0, 0.0);
                ui.small(t!("À juger à 100 % (double-clic sur l'image)."));
            }
            "Effets" => {
                slider(ui, t!("Clarté"), &mut a.clarity, -1.0..=1.0, 0.0);
                slider(ui, t!("Texture"), &mut a.texture, -1.0..=1.0, 0.0);
                slider(ui, t!("Grain"), &mut a.grain, 0.0..=1.0, 0.0);
                slider(ui, t!("Taille du grain"), &mut a.grain_size, 0.0..=1.0, 0.25);
            }
            "Vignettage" => {
                slider(ui, t!("Intensité"), &mut a.vignette, -1.0..=1.0, 0.0);
                slider(ui, t!("Départ"), &mut a.vignette_mid, 0.0..=1.0, 0.5);
            }
            "Virage partiel" => {
                let rainbow = tracks::rainbow();
                ui.label(t!("Ombres"));
                slider_with(ui, t!("Teinte|couleur"), &mut a.split_shadow_hue, 0.0..=360.0, 0.0, &rainbow);
                let sat = [Color32::from_gray(110), hue_color(a.split_shadow_hue, 0.8, 0.9)];
                slider_with(ui, t!("Saturation"), &mut a.split_shadow_sat, 0.0..=1.0, 0.0, &sat);
                ui.label(t!("Hautes lumières"));
                slider_with(ui, t!("Teinte|couleur"), &mut a.split_highlight_hue, 0.0..=360.0, 0.0, &rainbow);
                let sat = [Color32::from_gray(110), hue_color(a.split_highlight_hue, 0.8, 0.9)];
                slider_with(ui, t!("Saturation"), &mut a.split_highlight_sat, 0.0..=1.0, 0.0, &sat);
                slider(ui, t!("Équilibre"), &mut a.split_balance, -1.0..=1.0, 0.0);
            }
            "Balance des couleurs" => {
                segmented(ui, &mut self.balance_tab, &[(0, t!("Ombres")), (1, t!("Tons moyens")), (2, t!("Hautes lum."))]);
                let v = match self.balance_tab {
                    0 => &mut a.balance_shadows,
                    1 => &mut a.balance_midtones,
                    _ => &mut a.balance_highlights,
                };
                for (k, name) in [t!("Rouge"), t!("Vert"), t!("Bleu")].iter().enumerate() {
                    slider_with(ui, name, &mut v[k], -1.0..=1.0, 0.0, &tracks::RGB[k]);
                }
            }
            "TSL" => {
                segmented(ui, &mut self.hsl_tab, &[(0, t!("Teinte|couleur")), (1, t!("Saturation")), (2, t!("Luminance"))]);
                for ((band, name), h) in a.hsl.iter_mut().zip(HSL_BANDS).zip(tracks::BAND_HUES) {
                    let (v, track) = match self.hsl_tab {
                        0 => (&mut band.hue, vec![hue_color(h - 30.0, 0.75, 0.9), hue_color(h, 0.75, 0.9), hue_color(h + 30.0, 0.75, 0.9)]),
                        1 => (&mut band.sat, vec![Color32::from_gray(110), hue_color(h, 0.85, 0.9)]),
                        _ => (&mut band.lum, vec![hue_color(h, 0.75, 0.3), hue_color(h, 0.45, 1.0)]),
                    };
                    slider_with(ui, crate::i18n::tr(name), v, -1.0..=1.0, 0.0, &track);
                }
            }
            _ => {}
        }
    }
}

impl App {
    /// Sections de l'onglet Outils IA (carte de profondeur).
    fn ai_section(&mut self, ui: &mut egui::Ui, name: &str) {
        match name {
            EXPAND_SECTION => return self.expand_ui(ui),
            UPSCALE_SECTION => return self.upscale_ui(ui),
            DENOISE_SECTION => return self.denoise_ui(ui),
            CUTOUT_SECTION => return self.cutout_ui(ui),
            ENHANCE_SECTION => {
                let e = &mut self.adj.enhance;
                slider(ui, t!("Accent"), &mut e.accent, 0.0..=1.0, 0.0);
                ui.small(t!("Un seul curseur : ombres débouchées et hautes lumières tenues selon la zone, contraste local et couleurs."));
                slider(ui, t!("Ciel"), &mut e.sky, 0.0..=1.0, 0.0);
                self.mask_state(ui, Model::Sky, self.adj.enhance.sky > 0.0);
                return;
            }
            FACE_SECTION => {
                let p = &mut self.adj.portrait;
                slider(ui, t!("Lumière du visage"), &mut p.face_light, 0.0..=1.0, 0.0);
                ui.label(egui::RichText::new(t!("Yeux")).strong());
                slider(ui, t!("Améliorer les yeux"), &mut p.eyes, 0.0..=1.0, 0.0);
                slider(ui, t!("Blanc des yeux"), &mut p.eye_whiten, 0.0..=1.0, 0.0);
                ui.label(egui::RichText::new(t!("Lèvres")).strong());
                slider(ui, t!("Saturation"), &mut p.lips_sat, -1.0..=1.0, 0.0);
                slider(ui, t!("Rougeur"), &mut p.lips_red, 0.0..=1.0, 0.0);
                ui.label(egui::RichText::new(t!("Dents")).strong());
                slider(ui, t!("Blanchir"), &mut p.teeth, 0.0..=1.0, 0.0);
                ui.label(egui::RichText::new(t!("Forme")).strong());
                slider(ui, t!("Amincir le visage"), &mut p.face_slim, 0.0..=1.0, 0.0);
                slider(ui, t!("Agrandir les yeux"), &mut p.eye_size, 0.0..=1.0, 0.0);
                ui.small(t!("Chaque visage est détecté puis découpé (peau, yeux, lèvres, dents) ; rien ne bouge hors des visages."));
                let on = self.adj.portrait.active();
                self.mask_state(ui, Model::FaceSkin, on);
                return;
            }
            SKIN_SECTION => {
                let p = &mut self.adj.portrait;
                slider(ui, t!("Lissage de la peau"), &mut p.skin_smooth, 0.0..=1.0, 0.0);
                slider(ui, t!("Réduire la brillance"), &mut p.shine, 0.0..=1.0, 0.0);
                ui.small(t!("Peau du visage, des oreilles et du cou. Le grain le plus fin est gardé pour éviter l'effet plastique."));
                let on = self.adj.portrait.active();
                self.mask_state(ui, Model::FaceSkin, on);
                return;
            }
            BODY_SECTION => {
                slider(ui, t!("Silhouette"), &mut self.adj.portrait.body, -1.0..=1.0, 0.0);
                slider(ui, t!("Ventre"), &mut self.adj.portrait.belly, 0.0..=1.0, 0.0);
                ui.small(t!("Vers la droite : silhouette affinée ; vers la gauche : élargie. La tête n'est pas touchée, le fond voisin suit la déformation."));
                ui.small(t!("Chaque personne d'une photo de groupe est traitée à part (quatre au plus) ; deux personnes qui se touchent comptent pour une."));
                let on = self.adj.portrait.body != 0.0 || self.adj.portrait.belly != 0.0;
                self.mask_state(ui, Model::Subject, on);
                self.mask_state(ui, Model::FaceSkin, on);
                return;
            }
            STRUCTURE_SECTION => {
                let e = &mut self.adj.enhance;
                slider(ui, t!("Structure"), &mut e.structure, -1.0..=1.0, 0.0);
                ui.checkbox(&mut e.protect_subject, t!("Protéger le sujet (visages, personnes)"));
                ui.small(t!("Renforce le détail sans toucher au ciel ni aux aplats, pour ne pas faire ressortir le bruit."));
                let on = self.adj.enhance.structure != 0.0;
                self.mask_state(ui, Model::Sky, on);
                self.mask_state(ui, Model::Subject, on && self.adj.enhance.protect_subject);
                return;
            }
            _ => {}
        }
        let d = &mut self.adj.depth;
        match name {
            "Relight" => {
                slider(ui, t!("Éclairage du premier plan"), &mut d.relight, -1.0..=1.0, 0.0);
                slider(ui, t!("Portée"), &mut d.reach, 0.0..=1.0, 0.5);
                slider(ui, t!("Chaleur"), &mut d.warmth, -1.0..=1.0, 0.0);
                slider(ui, t!("Assombrir l'arrière-plan"), &mut d.background, 0.0..=1.0, 0.0);
                ui.small(t!("Portée : jusqu'où la lumière porte, du tout premier plan au fond."));
            }
            "Flou d'arrière-plan" => {
                slider(ui, t!("Flou"), &mut d.bokeh, 0.0..=1.0, 0.0);
                ui.horizontal(|ui| {
                    ui.label(t!("Mise au point"));
                    if ui.selectable_label(d.focus < 0.0, t!("Auto")).on_hover_text(t!("Le sujet au centre de l'image")).clicked() {
                        d.focus = -1.0;
                    }
                    if ui.selectable_label(self.focus_pick, t!("Choisir sur l'image")).on_hover_text(t!("Clique sur ce qui doit rester net")).clicked() {
                        self.focus_pick = !self.focus_pick;
                    }
                });
                if d.focus >= 0.0 {
                    slider(ui, t!("Plan net"), &mut d.focus, 0.0..=1.0, 0.8);
                }
                slider(ui, t!("Éclat des lumières"), &mut d.bokeh_glow, 0.0..=1.0, 0.3);
            }
            "Atmosphère" => {
                slider(ui, t!("Brume"), &mut d.haze, 0.0..=1.0, 0.0);
                slider(ui, t!("Luminosité de la brume"), &mut d.haze_light, 0.2..=1.5, 1.0);
                slider(ui, t!("Chaleur"), &mut d.haze_warmth, -1.0..=1.0, 0.0);
                ui.small(t!("La brume s'épaissit avec la distance."));
            }
            _ => {}
        }
    }

    /// Marges du cadre agrandi à montrer sur l'image (section ouverte, au moins une marge).
    fn expand_preview(&self) -> Option<[f32; 4]> {
        let open = self.open_section == Some(EXPAND_SECTION) || self.pinned.contains(EXPAND_SECTION);
        (self.tab == Tab::Ai && open && !self.crop_mode && self.expand.iter().any(|m| *m > 0.0)).then_some(self.expand)
    }

    /// Taille de l'image développée (recadrage compris).
    fn out_size(&self) -> Option<(u32, u32)> {
        self.size.map(|(w, h)| self.adj.output_size(w, h))
    }

    /// GenExpand : la photo développée est prolongée par ComfyUI dans un nouveau fichier.
    fn expand_ui(&mut self, ui: &mut egui::Ui) {
        self.engine_ui(ui, false);
        let Some((ow, oh)) = self.out_size() else { return };
        ui.label(t!("Format visé"));
        ui.horizontal_wrapped(|ui| {
            for (name, r) in [("1:1", 1.0f32), ("4:5", 0.8), ("3:2", 1.5), ("16:9", 16.0 / 9.0), ("9:16", 9.0 / 16.0)] {
                if ui.button(name).clicked() {
                    let cur = ow as f32 / oh as f32;
                    self.expand = if r > cur {
                        let m = (r / cur - 1.0) / 2.0;
                        [m, 0.0, m, 0.0]
                    } else {
                        let m = (cur / r - 1.0) / 2.0;
                        [0.0, m, 0.0, m]
                    };
                }
            }
            if ui.button(t!("Tout autour")).on_hover_text(t!("15 % de chaque côté")).clicked() {
                self.expand = [0.15; 4];
            }
        });
        for (k, name) in [t!("Gauche"), t!("Haut"), t!("Droite"), t!("Bas")].iter().enumerate() {
            let mut pct = self.expand[k] * 100.0;
            slider(ui, &format!("{name} (%)"), &mut pct, 0.0..=100.0, 0.0);
            self.expand[k] = pct / 100.0;
        }
        let (fw, fh) = Self::expanded_size(ow, oh, self.expand);
        ui.small(tf!("Résultat : {fw} × {fh} px, dans un nouveau fichier (Images/Luminux).", fw, fh));
        ui.label(if self.settings.translate { t!("Ce qui doit apparaître (facultatif)") } else { t!("Ce qui doit apparaître (facultatif, en anglais)") });
        ui.add(egui::TextEdit::multiline(&mut self.expand_prompt).desired_rows(2).desired_width(f32::INFINITY).hint_text(t!("Vide : la suite naturelle de la photo")));
        let ready = self.file_job.is_none() && self.gen_ready() && self.expand.iter().any(|m| *m > 0.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new(t!("Agrandir le cadre"))).clicked() {
                self.file_request = Some(FileTool::Expand);
            }
            if ui.add_enabled(self.expand.iter().any(|m| *m > 0.0), egui::Button::new(t!("Remettre à zéro"))).clicked() {
                self.expand = [0.0; 4];
            }
        });
        self.file_job_status(ui, FileTool::Expand);
        ui.small(t!("La photo garde sa pleine résolution ; les bords ajoutés sont calculés vers 1024 px puis agrandis."));
    }

    fn expanded_size(ow: u32, oh: u32, m: [f32; 4]) -> (u32, u32) {
        ((ow as f32 * (1.0 + m[0] + m[2])).round() as u32, (oh as f32 * (1.0 + m[1] + m[3])).round() as u32)
    }

    /// Explique ce qu'il faut pour un outil ComfyUI : le serveur s'il n'est pas joint, sinon le modèle manquant.
    fn comfy_help(&self, ui: &mut egui::Ui, need: &ComfyNeed, model_missing: bool) {
        let warn = Color32::from_rgb(230, 170, 90);
        if self.comfy_models.is_empty() {
            if self.comfy_probe.is_some() {
                return;
            }
            ui.colored_label(
                warn,
                "Cet outil a besoin de ComfyUI, un serveur d'IA gratuit à installer sur ce PC ou sur une autre machine \
                 du réseau (carte AMD ou NVIDIA). Indique son adresse ci-dessus puis « Connecter ».",
            );
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to(t!("Installer ComfyUI"), "https://github.com/comfyanonymous/ComfyUI");
                ui.small("·");
                ui.hyperlink_to(tf!("Modèle : {0}", need.file), need.url);
                ui.small(tf!("(dans ComfyUI/models/{0})", need.dir));
            });
        } else if model_missing {
            ui.colored_label(warn, tf!("Modèle manquant dans ComfyUI : place {0} dans ComfyUI/models/{1}, puis « Connecter ».", need.file, need.dir));
            ui.hyperlink_to(t!("Télécharger le modèle"), need.url);
        }
    }

    /// Débruitage IA : la photo source passe dans un modèle de restauration de ComfyUI (SCUNet…), une fois
    /// par photo et par modèle ; l'intensité se règle ensuite sans nouveau calcul.
    fn denoise_ui(&mut self, ui: &mut egui::Ui) {
        if self.settings.ai_local {
            ui.label(t!("Le débruitage IA a besoin de ComfyUI : le moteur local ne le propose pas. Pour l'utiliser, choisis « ComfyUI (serveur) » dans Préférences (Ctrl+,)."));
            return;
        }
        self.comfy_connect_ui(ui);
        let models: Vec<String> = self.comfy_upscalers.iter().filter(|m| crate::comfy::is_denoiser(m)).cloned().collect();
        let mut model = self.settings.denoise_model.clone();
        if !models.is_empty() && !models.contains(&model) {
            model = if models.contains(&self.adj.denoise.model) { self.adj.denoise.model.clone() } else { models[0].clone() };
        }
        self.comfy_help(ui, &NEED_DENOISE, models.is_empty());
        if models.len() == 1 {
            ui.small(tf!("Modèle : {0}", models[0]));
        }
        if models.len() > 1 {
            let items: Vec<(String, String)> = models.iter().map(|m| (m.clone(), m.rsplit_once('.').map_or(m.as_str(), |(n, _)| n).to_string())).collect();
            choice_list(ui, "denoise_model", &mut model, &items);
        }
        if model != self.settings.denoise_model {
            self.settings.denoise_model = model.clone();
            self.settings.save();
        }
        if model.is_empty() {
            return;
        }
        let file = self.path.as_deref().and_then(|p| crate::denoise::cache_file(p, &model));
        let ready = file.as_ref().is_some_and(|f| f.exists());
        if ready {
            let mut amount = if self.adj.denoise.model == model { self.adj.denoise.amount } else { 0.0 };
            let before = amount;
            slider(ui, t!("Intensité"), &mut amount, 0.0..=1.0, 0.8);
            if amount != before {
                self.adj.denoise = crate::adjust::AiDenoise { model: model.clone(), amount };
            }
            ui.small(t!("Calculé une fois pour cette photo ; « Avant » montre l'image d'origine."));
        } else if let Some(job) = &self.denoise_job {
            ui.horizontal(|ui| {
                ui.spinner();
                let p = job.progress.load(std::sync::atomic::Ordering::Relaxed);
                let (done, total) = (p >> 16, p & 0xffff);
                ui.small(if total > 1 { tf!("Débruitage dans ComfyUI : tuile {0} sur {total}", (done + 1).min(total), total) } else { t!("Débruitage dans ComfyUI…").into() });
            });
        } else {
            if self.denoise_failed.is_some() && self.denoise_failed == file {
                ui.colored_label(Color32::from_rgb(255, 120, 120), t!("Le dernier essai a échoué (voir la barre d'état)."));
            }
            if ui.add_enabled(self.path.is_some() && self.orig_half.is_some(), egui::Button::new(t!("Débruiter"))).clicked() {
                self.start_denoise(model, ui.ctx());
            }
            ui.small(t!("La photo entière passe dans ComfyUI, par tuiles ; le résultat est gardé pour cette photo."));
        }
    }

    fn start_denoise(&mut self, model: String, ctx: &egui::Context) {
        let (Some(photo), Some(orig)) = (self.path.clone(), self.orig_half.clone()) else { return };
        let Some(file) = crate::denoise::cache_file(&photo, &model) else { return };
        let (w, h) = self.orig_dims;
        let (tx, rx) = channel();
        let progress = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (prog, ctx2, model2) = (progress.clone(), ctx.clone(), model.clone());
        std::thread::spawn(move || {
            let mut data = (*orig).clone();
            drop(orig);
            let canvas = crate::retouch::HalfCanvas { data: &mut data, w, h };
            let r = crate::denoise::compute(&canvas, &model2, &file, |done, total| {
                prog.store(((done as u32) << 16) | total as u32, std::sync::atomic::Ordering::Relaxed);
                ctx2.request_repaint();
            });
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.denoise_failed = None;
        self.status = t!("Débruitage IA dans ComfyUI…").into();
        self.denoise_job = Some(DenoiseJob { photo, model, rx, progress });
    }

    /// Upscale : la photo développée est agrandie par un modèle de ComfyUI dans un nouveau fichier.
    fn upscale_ui(&mut self, ui: &mut egui::Ui) {
        self.engine_ui(ui, true);
        if !self.settings.ai_local {
            let none = !self.upscale_ready();
            self.comfy_help(ui, &NEED_UPSCALE, none);
            if none {
                return;
            }
            let mut model = self.settings.upscale_model.clone();
            let items: Vec<(String, String)> = self.comfy_upscalers.iter().filter(|m| !crate::comfy::is_denoiser(m)).map(|m| (m.clone(), m.rsplit_once('.').map_or(m.as_str(), |(n, _)| n).to_string())).collect();
            choice_list(ui, "upscale_model", &mut model, &items);
            if model != self.settings.upscale_model {
                self.settings.upscale_model = model;
                self.settings.save();
            }
        } else if !self.upscale_ready() {
            return;
        }
        ui.label(t!("Facteur"));
        segmented(ui, &mut self.upscale_factor, &[(2, t!("× 2")), (3, t!("× 3")), (4, t!("× 4"))]);
        let mut too_big = false;
        if let Some((ow, oh)) = self.out_size() {
            let (fw, fh) = (ow * self.upscale_factor, oh * self.upscale_factor);
            let px = fw as u64 * fh as u64;
            too_big = px > crate::comfy::UPSCALE_MAX_PX;
            if too_big {
                ui.colored_label(Color32::from_rgb(255, 170, 90), tf!("{fw} × {fh} px ({0} Mpx) : trop grand, {1} Mpx au plus. Choisis un facteur plus petit ou recadre.", fw, fh, px / 1_000_000, crate::comfy::UPSCALE_MAX_PX / 1_000_000));
            } else {
                ui.small(tf!("Résultat : {fw} × {fh} px ({0:.0} Mpx), dans un nouveau fichier (Images/Luminux).", fw, fh, px as f64 / 1e6));
            }
        }
        if ui.add_enabled(self.file_job.is_none() && self.path.is_some() && !too_big, egui::Button::new(t!("Agrandir l'image"))).clicked() {
            self.file_request = Some(FileTool::Upscale);
        }
        self.file_job_status(ui, FileTool::Upscale);
        ui.small(t!("Conseil : recadre d'abord, l'agrandissement porte sur l'image développée."));
    }

    /// Export PNG avec le sujet seul, le fond transparent (masque du sujet IA).
    fn cutout_ui(&mut self, ui: &mut egui::Ui) {
        self.cutout_active = true;
        self.mask_state(ui, Model::Subject, true);
        let ready = self.ai_masks.contains_key(&Model::Subject);
        ui.add_enabled(ready, egui::Checkbox::new(&mut self.cutout_show, t!("Voir le détourage (damier = transparent)")));
        let ctx = ui.ctx().clone();
        if ui.add_enabled(ready && self.path.is_some(), egui::Button::new(t!("Exporter en PNG transparent…"))).clicked() {
            let path = self.path.clone().unwrap();
            let stem = library::export_stem(&path);
            let dir = path.parent().map(Path::to_path_buf);
            self.dialog(&ctx, move || {
                let mut d = rfd::FileDialog::new().set_file_name(format!("{stem}_detoure.png")).add_filter("PNG transparent", &["png"]);
                if let Some(dir) = dir {
                    d = d.set_directory(dir);
                }
                d.save_file().map(|p| Picked::ExportCutout(p.with_extension("png")))
            });
        }
        ui.small(t!("Le sujet principal est gardé avec tous tes réglages, le reste devient transparent."));
    }

    fn file_job_status(&self, ui: &mut egui::Ui, tool: FileTool) {
        if self.file_job.as_ref().is_some_and(|j| j.tool == tool) {
            ui.horizontal(|ui| {
                ui.spinner();
                if let Some(j) = &self.file_job {
                    ui.small(duration_text(j.start.elapsed()));
                }
                let p = self.file_job.as_ref().map_or(0, |j| j.progress.load(std::sync::atomic::Ordering::Relaxed));
                let (done, total) = (p >> 16, p & 0xffff);
                let engine = self.engine_name();
                if tool == FileTool::Flatten {
                    ui.small(t!("Fusion des calques…"));
                } else if total > 1 {
                    ui.small(tf!("Calcul dans {engine} : tuile {0} sur {total}", (done + 1).min(total), total, engine));
                } else {
                    ui.small(tf!("Calcul dans {engine}…", engine));
                }
            });
        }
    }

    /// Lance le calcul demandé : rendu complet de la photo développée ici, ComfyUI dans un fil à part.
    fn start_file_job(&mut self, tool: FileTool, rs: &egui_wgpu::RenderState, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else { return };
        let (w, h, px) = {
            let renderer = rs.renderer.read();
            let Some(dev) = renderer.callback_resources.get::<Developer>() else { return };
            match dev.render_full(&rs.device, &rs.queue, &self.adj) {
                Ok(v) => v,
                Err(e) => {
                    self.status = tf!("Rendu impossible : {e:#}", e);
                    return;
                }
            }
        };
        // Fusion : gardée en 16 bits pour la suite des retouches.
        let flat = (tool == FileTool::Flatten).then(|| {
            let d: Vec<u16> = px.iter().flat_map(|p| [0, 1, 2].map(|k| (p[k].clamp(0.0, 1.0) * 65535.0).round() as u16)).collect();
            image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_raw(w, h, d).unwrap()
        });
        let data: Vec<u8> = px.iter().flat_map(|p| [0, 1, 2].map(|k| (p[k].clamp(0.0, 1.0) * 255.0).round() as u8)).collect();
        drop(px);
        let img = image::RgbImage::from_raw(w, h, data).unwrap();
        let set = self.settings.clone();
        let (ew, eh) = Self::expanded_size(w, h, self.expand);
        // Marges en pixels ; l'arrondi va à droite et en bas.
        let (l, t) = ((w as f32 * self.expand[0]).round() as u32, (h as f32 * self.expand[1]).round() as u32);
        let margins = [l, t, ew - w - l, eh - h - t];
        let prompt = self.expand_prompt.trim().to_string();
        let factor = self.upscale_factor;
        let stem = library::export_stem(&path);
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64 & 0xffff_ffff_ffff);
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let progress = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let prog = progress.clone();
        std::thread::spawn(move || {
            let r = (|| -> anyhow::Result<(PathBuf, String)> {
                let ollama = set.free_vram.then_some(set.ollama_url.as_str());
                let (out, suffix, msg) = match tool {
                    FileTool::Flatten => {
                        let dir = Self::docs_dir();
                        std::fs::create_dir_all(&dir)?;
                        let file = (1..).map(|i| dir.join(if i == 1 { format!("{stem}_fusion.png") } else { format!("{stem}_fusion_{i}.png") })).find(|p| !p.exists()).unwrap();
                        flat.as_ref().unwrap().save(&file)?;
                        return Ok((file, t!("Calques fusionnés").to_string()));
                    }
                    FileTool::Expand => {
                        let translate = set.translate.then_some((set.ollama_url.as_str(), set.translate_model.as_str()));
                        let req = crate::comfy::Request { url: &set.comfy_url, model: &set.gen_model(), prompt: &prompt, negative: &set.comfy_negative, seed, strength: 1.0, inpaint_denoise: None, ollama, translate };
                        let (out, tr) = crate::comfy::expand(&img, margins, &req)?;
                        let msg = match tr {
                            Some(t) => tf!("Cadre agrandi (invite traduite : « {t} »)", t),
                            None => t!("Cadre agrandi").to_string(),
                        };
                        (out, "etendu".to_string(), msg)
                    }
                    FileTool::Upscale => {
                        let out = crate::comfy::upscale(&set.comfy_url, &img, &set.upscaler(), factor as f32, ollama, |done, total| {
                            prog.store(((done as u32) << 16) | total as u32, std::sync::atomic::Ordering::Relaxed);
                            ctx2.request_repaint();
                        })?;
                        (out, format!("x{factor}"), tf!("Image agrandie × {factor}", factor))
                    }
                };
                let dir = Self::docs_dir();
                std::fs::create_dir_all(&dir)?;
                let file = (1..).map(|i| dir.join(if i == 1 { format!("{stem}_{suffix}.png") } else { format!("{stem}_{suffix}_{i}.png") })).find(|p| !p.exists()).unwrap();
                out.save(&file)?;
                Ok((file, msg))
            })();
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        let engine = self.engine_name();
        self.status = match tool {
            FileTool::Expand => tf!("Agrandissement du cadre dans {engine}…", engine),
            FileTool::Upscale => tf!("Agrandissement de l'image dans {engine}…", engine),
            FileTool::Flatten => t!("Fusion des calques…").into(),
        };
        self.file_job = Some(FileJob { tool, start: std::time::Instant::now(), rx, progress });
    }

    /// Ajoute le dossier d'un fichier créé à la bibliothèque.
    fn add_doc_folder(&mut self, path: &Path, ctx: &egui::Context) {
        if let Some(cat) = &self.catalogue {
            let d = path.parent().unwrap().to_string_lossy().to_string();
            if cat.add_folder(&d).is_ok() {
                self.folders = cat.folders();
                if self.scanning.is_none() {
                    self.start_scan(ctx);
                }
            }
        }
    }

    /// Calcul en cours ou échec d'un masque IA dont dépend un outil actif.
    fn mask_state(&mut self, ui: &mut egui::Ui, model: Model, active: bool) {
        if !active {
            return;
        }
        let face = model.face_classes().is_some();
        let what = if face { t!("des visages") } else { model.of_what() };
        if self.ai_job.as_ref().is_some_and(|j| j.model == model || (face && j.model.face_classes().is_some())) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small(tf!("Détection {what} en cours…", what));
            });
        } else if self.ai_failed.contains(&model) {
            ui.horizontal(|ui| {
                let msg = if face { t!("Visages non détectés (voir la barre d'état).").to_string() } else { tf!("Masque {what} impossible.", what) };
                ui.colored_label(Color32::from_rgb(230, 120, 90), msg);
                if ui.small_button(t!("Réessayer")).clicked() {
                    self.ai_failed.remove(&model);
                    if face {
                        for m in ai::FACE_MODELS {
                            self.ai_failed.remove(&m);
                        }
                    }
                }
            });
        }
    }

    /// État de la carte de profondeur, commun aux trois outils.
    fn depth_status(&mut self, ui: &mut egui::Ui) {
        if self.ai_job.as_ref().is_some_and(|j| j.model == Model::Depth) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small(t!("Calcul de la profondeur en cours…"));
            });
        } else if self.ai_failed.contains(&Model::Depth) {
            ui.colored_label(Color32::from_rgb(230, 120, 90), t!("La profondeur n'a pas pu être calculée (voir la barre d'état)."));
            if ui.small_button(t!("Réessayer")).clicked() {
                self.ai_failed.remove(&Model::Depth);
            }
        }
        ui.checkbox(&mut self.depth_show_mask, t!("Voir la profondeur")).on_hover_text(t!("Blanc : proche, noir : loin"));
        ui.add_space(4.0);
    }
}

const AI_SECTIONS: [&str; 12] = [ENHANCE_SECTION, STRUCTURE_SECTION, DENOISE_SECTION, FACE_SECTION, SKIN_SECTION, BODY_SECTION, k!("Relight"), k!("Flou d'arrière-plan"), k!("Atmosphère"), EXPAND_SECTION, UPSCALE_SECTION, CUTOUT_SECTION];
const CUTOUT_SECTION: &str = k!("Détourer (fond transparent)");
const ENHANCE_SECTION: &str = k!("Amélioration IA");
const FACE_SECTION: &str = k!("Visage IA");
const SKIN_SECTION: &str = k!("Peau IA");
const BODY_SECTION: &str = k!("Corps IA");
const STRUCTURE_SECTION: &str = k!("Structure IA");
const EXPAND_SECTION: &str = k!("Agrandir le cadre");
const UPSCALE_SECTION: &str = k!("Agrandir l'image");
const DENOISE_SECTION: &str = k!("Débruitage IA");
/// Sections qui ont besoin d'un serveur ComfyUI.
const COMFY_SECTIONS: [&str; 3] = [DENOISE_SECTION, EXPAND_SECTION, UPSCALE_SECTION];

/// Modèle à installer dans ComfyUI pour un outil.
struct ComfyNeed {
    file: &'static str,
    dir: &'static str,
    url: &'static str,
}

const NEED_INPAINT: ComfyNeed = ComfyNeed {
    file: "sd_xl_base_1.0_inpainting_0.1.safetensors",
    dir: "checkpoints",
    url: "https://huggingface.co/wangqyqq/sd_xl_base_1.0_inpainting_0.1.safetensors",
};
const NEED_UPSCALE: ComfyNeed = ComfyNeed {
    file: "RealESRGAN_x4plus.pth",
    dir: "upscale_models",
    url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.1.0/RealESRGAN_x4plus.pth",
};
const NEED_DENOISE: ComfyNeed = ComfyNeed {
    file: "scunet_color_real_psnr.pth",
    dir: "upscale_models",
    url: "https://github.com/cszn/KAIR/releases/download/v1.0/scunet_color_real_psnr.pth",
};

const DEVELOP_SECTIONS: [&str; 10] =
    [k!("Lumière"), k!("Couleur"), k!("Courbe des tons"), k!("Détails"), k!("Effets"), k!("Vignettage"), k!("Virage partiel"), k!("Balance des couleurs"), k!("TSL"), k!("Historique")];

/// Onglets du panneau de droite.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Develop,
    Local,
    Crop,
    Looks,
    Sky,
    Retouch,
    Lens,
    Ai,
    Layers,
}

impl Tab {
    const ALL: [Tab; 9] = [Tab::Develop, Tab::Ai, Tab::Local, Tab::Crop, Tab::Looks, Tab::Sky, Tab::Layers, Tab::Retouch, Tab::Lens];

    fn title(self) -> &'static str {
        match self {
            Tab::Develop => t!("Réglages"),
            Tab::Local => t!("Réglages locaux et masques"),
            Tab::Crop => t!("Recadrage et redressement"),
            Tab::Looks => t!("Looks"),
            Tab::Sky => t!("Remplacement du ciel"),
            Tab::Retouch => t!("Retouche et génération"),
            Tab::Lens => t!("Correction d'objectif"),
            Tab::Ai => t!("Outils IA"),
            Tab::Layers => t!("Calques"),
        }
    }

    fn short(self) -> &'static str {
        match self {
            Tab::Develop => t!("Réglages|onglet"),
            Tab::Local => t!("Masques"),
            Tab::Crop => t!("Recadrer"),
            Tab::Looks => t!("Looks"),
            Tab::Sky => t!("Ciel"),
            Tab::Retouch => t!("Retouche"),
            Tab::Lens => t!("Objectif"),
            Tab::Ai => t!("IA"),
            Tab::Layers => t!("Calques"),
        }
    }
}

/// Icônes de la barre d'onglets, dessinées (la police n'a pas de pictogrammes).
/// Bouton à double chevron (» ou «) pour masquer ou afficher le panneau.
fn panel_button(ui: &mut egui::Ui, right: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 24.0), Sense::click());
    let p = ui.painter_at(rect);
    if resp.hovered() {
        p.rect_filled(rect, 6.0, palette::ITEM_HOVER);
    }
    let col = if resp.hovered() { Color32::WHITE } else { Color32::from_gray(185) };
    let d = if right { 1.0 } else { -1.0 };
    for dx in [-3.5, 3.5] {
        let c = rect.center() + vec2(dx, 0.0);
        p.add(egui::Shape::line(vec![c + vec2(-2.5 * d, -5.0), c + vec2(2.5 * d, 0.0), c + vec2(-2.5 * d, 5.0)], Stroke::new(1.8, col)));
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    resp.on_hover_text(tip)
}

fn tab_icon(p: &egui::Painter, r: Rect, tab: Tab, col: Color32) {
    let st = Stroke::new(1.6, col);
    let (c, w, h) = (r.center(), r.width(), r.height());
    let at = |x: f32, y: f32| pos2(r.left() + x * w, r.top() + y * h);
    match tab {
        Tab::Develop => {
            // Trois curseurs.
            for (k, t) in [0.25f32, 0.55, 0.8].iter().enumerate() {
                let y = 0.2 + 0.3 * k as f32;
                p.line_segment([at(0.05, y), at(0.95, y)], st);
                p.circle_filled(at(*t, y), 2.8, col);
            }
        }
        Tab::Local => {
            // Pinceau : manche en diagonale et pointe.
            p.line_segment([at(0.85, 0.1), at(0.4, 0.55)], Stroke::new(2.2, col));
            p.circle_filled(at(0.3, 0.68), 4.0, col);
            p.circle_stroke(at(0.3, 0.68), 7.5, Stroke::new(1.0, col.gamma_multiply(0.6)));
        }
        Tab::Crop => {
            p.line_segment([at(0.25, 0.0), at(0.25, 0.75)], st);
            p.line_segment([at(0.25, 0.75), at(1.0, 0.75)], st);
            p.line_segment([at(0.0, 0.25), at(0.75, 0.25)], st);
            p.line_segment([at(0.75, 0.25), at(0.75, 1.0)], st);
        }
        Tab::Looks => {
            p.rect_stroke(Rect::from_min_max(at(0.0, 0.25), at(0.7, 0.95)), 2.0, st, egui::StrokeKind::Middle);
            p.rect_stroke(Rect::from_min_max(at(0.3, 0.05), at(1.0, 0.75)), 2.0, st, egui::StrokeKind::Middle);
        }
        Tab::Sky => {
            // Nuage : trois cercles et une base.
            p.circle_stroke(at(0.35, 0.55), 0.2 * w, st);
            p.circle_stroke(at(0.6, 0.45), 0.25 * w, st);
            p.line_segment([at(0.1, 0.78), at(0.9, 0.78)], st);
        }
        Tab::Retouch => {
            // Gomme : losange incliné.
            let pts = vec![at(0.55, 0.05), at(0.95, 0.45), at(0.45, 0.95), at(0.05, 0.55), at(0.55, 0.05)];
            p.add(egui::Shape::line(pts, st));
            p.line_segment([at(0.3, 0.3), at(0.7, 0.7)], st);
        }
        Tab::Lens => {
            p.circle_stroke(c, 0.48 * w, st);
            p.circle_stroke(c, 0.22 * w, st);
        }
        Tab::Ai => {
            // Étincelles : une grande étoile à quatre branches et une petite.
            let star = |cx: f32, cy: f32, rr: f32| {
                let (o, i) = (rr * w, rr * w * 0.28);
                let ctr = at(cx, cy);
                let pts: Vec<Pos2> = (0..8)
                    .map(|k| {
                        let a = std::f32::consts::FRAC_PI_4 * k as f32 - std::f32::consts::FRAC_PI_2;
                        let r = if k % 2 == 0 { o } else { i };
                        ctr + vec2(a.cos(), a.sin()) * r
                    })
                    .collect();
                for k in 0..8 {
                    p.add(egui::Shape::convex_polygon(vec![ctr, pts[k], pts[(k + 1) % 8]], col, Stroke::NONE));
                }
            };
            star(0.42, 0.55, 0.42);
            star(0.82, 0.18, 0.18);
        }
        Tab::Layers => {
            // Trois feuilles empilées.
            for k in 0..3 {
                let y = 0.15 + 0.25 * k as f32;
                let pts = vec![at(0.5, y), at(0.95, y + 0.17), at(0.5, y + 0.34), at(0.05, y + 0.17), at(0.5, y)];
                p.add(egui::Shape::line(pts, st));
            }
        }
    }
}

/// Taille réduite (côté max `max`) de l'image produite par `adj`.
fn small_size(adj: &Adjustments, w: u32, h: u32, max: u32) -> (u32, u32) {
    let (ow, oh) = adj.output_size(w, h);
    let s = max as f32 / ow.max(oh) as f32;
    (((ow as f32 * s) as u32).max(1), ((oh as f32 * s) as u32).max(1))
}

/// Mesures de temps sur la sortie d'erreur si LUMINUX_DEBUG est défini.
fn debug(args: std::fmt::Arguments) {
    if std::env::var_os("LUMINUX_DEBUG").is_some() {
        eprintln!("[luminux] {args}");
    }
}

/// Cinq étoiles dessinées (la police par défaut n'a pas le glyphe ★).
fn paint_stars(p: &egui::Painter, top_left: Pos2, rating: u8, size: f32) {
    let gold = Color32::from_rgb(240, 190, 60);
    for k in 0..5u8 {
        let c = top_left + vec2(size * (k as f32 + 0.5), size * 0.5);
        let pts: Vec<Pos2> = (0..10)
            .map(|i| {
                let a = std::f32::consts::PI * (i as f32 / 5.0 - 0.5);
                let r = if i % 2 == 0 { size * 0.45 } else { size * 0.19 };
                c + vec2(a.cos(), a.sin()) * r
            })
            .collect();
        // Polygone concave : on le découpe en triangles depuis le centre.
        for i in 0..10 {
            let tri = vec![c, pts[i], pts[(i + 1) % 10]];
            let fill = if k < rating { gold } else { Color32::from_gray(70) };
            p.add(egui::Shape::convex_polygon(tri, fill, Stroke::NONE));
        }
    }
}

// Règle de l'interface : un choix parmi quelques options fixes se fait par boutons (segmented, ratio_tiles,
// choice_list), pas par liste déroulante. La liste déroulante est réservée aux listes longues ou qui grandissent
// (objectifs de la base Lensfun, packs de looks, plus de 8 modèles).

/// Auteur, copyright et filigrane de la fenêtre Exporter. Renvoie vrai pour choisir un logo.
fn owner_ui(ui: &mut egui::Ui, o: &mut export::Options) -> bool {
    use crate::watermark::{Kind, Place};
    let mut pick = false;
    ui.label(egui::RichText::new(t!("Auteur et copyright")).strong());
    egui::Grid::new("auteur").num_columns(2).show(ui, |ui| {
        ui.label(t!("Auteur"));
        ui.add(egui::TextEdit::singleline(&mut o.author).hint_text(t!("ex. Jean Martin")).desired_width(220.0));
        ui.end_row();
        ui.label(t!("Copyright"));
        ui.add(egui::TextEdit::singleline(&mut o.copyright).hint_text(t!("ex. © 2026 Jean Martin")).desired_width(220.0));
        ui.end_row();
    });
    ui.small(t!("Écrits dans les EXIF de chaque photo exportée, même sans « Garder les métadonnées »."));
    ui.add_space(4.0);
    ui.label(egui::RichText::new(t!("Filigrane")).strong());
    let wm = &mut o.watermark;
    segmented(ui, &mut wm.kind, &[(Kind::None, t!("Aucun")), (Kind::Text, t!("Texte")), (Kind::Image, t!("Logo"))]);
    match wm.kind {
        Kind::None => return false,
        Kind::Text => {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut wm.text).hint_text(t!("Texte du filigrane")).desired_width(200.0));
                if ui.add_enabled(!o.copyright.trim().is_empty(), egui::Button::new(t!("Copyright|reprendre"))).on_hover_text(t!("Reprendre le copyright")).clicked() {
                    wm.text = o.copyright.trim().to_string();
                }
            });
            segmented(ui, &mut wm.white, &[(true, t!("Blanc")), (false, t!("Noir"))]);
        }
        Kind::Image => {
            ui.horizontal(|ui| {
                if ui.button(t!("Choisir un logo…")).clicked() {
                    pick = true;
                }
                match &wm.image {
                    Some(p) => ui.label(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                    None => ui.weak(t!("PNG transparent conseillé")),
                };
            });
        }
    }
    ui.horizontal_wrapped(|ui| {
        for (v, label) in [
            (Place::TopLeft, t!("Haut gauche")),
            (Place::TopRight, t!("Haut droite")),
            (Place::Center, t!("Centre")),
            (Place::BottomLeft, t!("Bas gauche")),
            (Place::BottomRight, t!("Bas droite")),
        ] {
            ui.selectable_value(&mut wm.place, v, label);
        }
    });
    let mut width = wm.width * 100.0;
    ui.horizontal(|ui| {
        ui.label(t!("Largeur"));
        ui.add(egui::Slider::new(&mut width, 5.0..=60.0).suffix(" %"));
    });
    wm.width = width / 100.0;
    let mut op = wm.opacity * 100.0;
    ui.horizontal(|ui| {
        ui.label(t!("Opacité"));
        ui.add(egui::Slider::new(&mut op, 10.0..=100.0).suffix(" %"));
    });
    wm.opacity = op / 100.0;
    pick
}

/// Choix exclusif : boutons de même largeur sur toute la largeur disponible.
fn segmented<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let n = options.len().max(1) as f32;
        let w = ((ui.available_width() - ui.spacing().item_spacing.x * (n - 1.0)) / n).max(30.0);
        for (v, label) in options {
            let on = *value == *v;
            if ui.add(egui::Button::selectable(on, *label).min_size(vec2(w, 26.0))).clicked() && !on {
                *value = v.clone();
                changed = true;
            }
        }
    });
    changed
}

/// Liste de choix en boutons pleine largeur ; au-delà de 8 éléments, liste déroulante.
fn choice_list(ui: &mut egui::Ui, id: &str, value: &mut String, items: &[(String, String)]) -> bool {
    let before = value.clone();
    if items.len() > 8 {
        let shown = items.iter().find(|i| i.0 == *value).map_or(value.clone(), |i| i.1.clone());
        egui::ComboBox::from_id_salt(id).width(ui.available_width() - 8.0).selected_text(shown).show_ui(ui, |ui| {
            for (v, label) in items {
                ui.selectable_value(value, v.clone(), label);
            }
        });
    } else {
        let w = ui.available_width();
        for (v, label) in items {
            let on = *value == *v;
            if ui.add(egui::Button::selectable(on, label.as_str()).min_size(vec2(w, 26.0))).clicked() {
                *value = v.clone();
            }
        }
    }
    *value != before
}

/// Formats de recadrage en tuiles dessinées (rectangle aux proportions du format).
fn ratio_tiles(ui: &mut egui::Ui, sel: &mut usize, portrait: bool) {
    let cols = 4;
    let sp = ui.spacing().item_spacing.x;
    let tw = (ui.available_width() - sp * (cols as f32 - 1.0)) / cols as f32;
    for row in ASPECTS.chunks(cols).enumerate() {
        ui.horizontal(|ui| {
            for (j, (name, r)) in row.1.iter().enumerate() {
                let i = row.0 * cols + j;
                let (rect, resp) = ui.allocate_exact_size(vec2(tw, 58.0), Sense::click());
                let on = *sel == i;
                let p = ui.painter_at(rect);
                let bg = if on { palette::ITEM_ON } else if resp.hovered() { palette::ITEM } else { palette::WINDOW };
                p.rect_filled(rect, 8.0, bg);
                if on {
                    p.rect_stroke(rect.shrink(0.5), 8.0, Stroke::new(1.5, Color32::from_rgb(240, 175, 50)), egui::StrokeKind::Inside);
                }
                let col = if on { Color32::WHITE } else { Color32::from_gray(185) };
                let st = Stroke::new(1.6, col);
                let c = rect.center() - vec2(0.0, 8.0);
                match *r {
                    x if x == 0.0 => {
                        // Libre : quatre coins.
                        let b = Rect::from_center_size(c, vec2(22.0, 18.0));
                        for (k, d) in [(b.left_top(), vec2(1.0, 1.0)), (b.right_top(), vec2(-1.0, 1.0)), (b.left_bottom(), vec2(1.0, -1.0)), (b.right_bottom(), vec2(-1.0, -1.0))] {
                            p.line_segment([k, k + vec2(6.0 * d.x, 0.0)], st);
                            p.line_segment([k, k + vec2(0.0, 6.0 * d.y)], st);
                        }
                    }
                    x => {
                        // Original : cadenas stylisé dans le rectangle ; sinon rectangle aux proportions.
                        let mut a = if x < 0.0 { 1.5 } else { x };
                        if portrait {
                            a = 1.0 / a;
                        }
                        let (w, h) = if a >= 1.0 { (22.0, 22.0 / a) } else { (22.0 * a, 22.0) };
                        let b = Rect::from_center_size(c, vec2(w, h));
                        p.rect_stroke(b, 3.0, st, egui::StrokeKind::Middle);
                        if x < 0.0 {
                            p.circle_filled(b.center(), 2.5, col);
                        }
                    }
                }
                p.text(rect.center_bottom() - vec2(0.0, 11.0), egui::Align2::CENTER_CENTER, crate::i18n::tr(name), egui::FontId::proportional(12.0), col);
                if resp.clicked() {
                    *sel = i;
                }
            }
        });
    }
}

/// Curseur réinitialisé par un double-clic.
fn slider(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32) {
    slider_with(ui, label, v, range, default, &[]);
}

/// Curseur dont la piste est un dégradé de `track` (de gauche à droite ; vide : piste neutre).
/// Un point orange signale une valeur différente de celle par défaut (double-clic : y revenir).
fn slider_with(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, track: &[Color32]) {
    let (range_start, range_end) = (*range.start(), *range.end());
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 18.0], egui::Label::new(label).truncate());
        // Le dégradé est peint sous le curseur, dont la piste devient transparente.
        let under = ui.painter().add(egui::Shape::Noop);
        let r = ui
            .scope(|ui| {
                if track.len() >= 2 {
                    ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
                }
                ui.add(egui::Slider::new(v, range).fixed_decimals(2))
            })
            .inner;
        let y = r.rect.center().y;
        if track.len() >= 2 {
            let (x0, x1) = (r.rect.left(), r.rect.left() + ui.spacing().slider_width);
            let mut mesh = egui::Mesh::default();
            for (k, c) in track.iter().enumerate() {
                let x = x0 + (x1 - x0) * k as f32 / (track.len() - 1) as f32;
                mesh.colored_vertex(pos2(x, y - 2.5), *c);
                mesh.colored_vertex(pos2(x, y + 2.5), *c);
                if k > 0 {
                    let i = 2 * k as u32;
                    mesh.add_triangle(i - 2, i - 1, i);
                    mesh.add_triangle(i - 1, i, i + 1);
                }
            }
            ui.painter().set(under, mesh);
        }
        // Poignée pleine et claire, posée sur celle d'egui (creuse sur une piste transparente).
        let h = ui.spacing().interact_size.y.max(ui.text_style_height(&egui::TextStyle::Body));
        let hr = h / 2.5;
        let (x0, x1) = (r.rect.left() + hr, r.rect.left() + ui.spacing().slider_width - hr);
        let t = ((*v - range_start) / (range_end - range_start).max(1e-6)).clamp(0.0, 1.0);
        let c = pos2(x0 + (x1 - x0) * t, y);
        let hover = r.hovered() || r.dragged();
        ui.painter().circle(c, hr, if hover { Color32::WHITE } else { Color32::from_gray(225) }, Stroke::new(1.0, Color32::from_black_alpha(120)));
        if (*v - default).abs() > 1e-4 {
            ui.painter().circle_filled(pos2(r.rect.left() - 5.0, y), 2.5, palette::CHANGED);
        }
        if r.double_clicked() {
            *v = default;
        }
        if r.changed() || r.double_clicked() {
            history::mark(ui.ctx(), format!("{label} {:+.2}", *v));
        }
    });
}

/// Couleur vive de teinte `deg` (0 rouge, 120 vert, 240 bleu), de saturation `sat` et de valeur `val`.
fn hue_color(deg: f32, sat: f32, val: f32) -> Color32 {
    let h = deg.rem_euclid(360.0) / 60.0;
    let f = |n: f32| {
        let k = (n + h).rem_euclid(6.0);
        val - val * sat * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    Color32::from_rgb((f(5.0) * 255.0) as u8, (f(3.0) * 255.0) as u8, (f(1.0) * 255.0) as u8)
}

/// Pistes colorées des curseurs de couleur.
mod tracks {
    use super::{Color32, hue_color};
    pub const TEMPERATURE: [Color32; 2] = [Color32::from_rgb(70, 130, 230), Color32::from_rgb(235, 190, 60)];
    pub const TINT: [Color32; 2] = [Color32::from_rgb(80, 185, 90), Color32::from_rgb(205, 80, 200)];
    pub const SATURATION: [Color32; 2] = [Color32::from_gray(110), Color32::from_rgb(232, 96, 58)];
    /// Balance des couleurs : rouge, vert, bleu face à leur complémentaire.
    pub const RGB: [[Color32; 2]; 3] = [
        [Color32::from_rgb(60, 190, 200), Color32::from_rgb(225, 70, 60)],
        [Color32::from_rgb(200, 80, 190), Color32::from_rgb(80, 190, 90)],
        [Color32::from_rgb(225, 200, 60), Color32::from_rgb(70, 110, 230)],
    ];
    /// Teinte centrale des bandes TSL (ordre de `HSL_BANDS`).
    pub const BAND_HUES: [f32; 8] = [0.0, 30.0, 58.0, 120.0, 180.0, 225.0, 275.0, 315.0];

    pub fn rainbow() -> Vec<Color32> {
        (0..=12).map(|k| hue_color(k as f32 * 30.0, 0.75, 0.9)).collect()
    }
}

fn curve_editor(ui: &mut egui::Ui, pts: &mut Vec<[f32; 2]>) {
    let side = ui.available_width().min(300.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let to_screen = |p: [f32; 2]| pos2(rect.left() + p[0] * rect.width(), rect.bottom() - p[1] * rect.height());
    let to_curve = |s: Pos2| [((s.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((rect.bottom() - s.y) / rect.height()).clamp(0.0, 1.0)];
    let id = resp.id.with("selection");
    let nearest = |pos: Pos2, pts: &[[f32; 2]]| {
        pts.iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(*p).distance(pos)))
            .filter(|(_, d)| *d < 10.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(i) = nearest(pos, pts) {
                if i != 0 && i != pts.len() - 1 {
                    pts.remove(i);
                }
            }
        }
    } else if resp.drag_started() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let sel = nearest(pos, pts).unwrap_or_else(|| {
                let c = to_curve(pos);
                let x = c[0].clamp(0.01, 0.99);
                let i = pts.iter().position(|p| p[0] > x).unwrap_or(pts.len() - 1);
                pts.insert(i, [x, c[1]]);
                i
            });
            ui.data_mut(|d| d.insert_temp(id, sel));
        }
    }
    if resp.dragged() {
        if let (Some(sel), Some(pos)) = (ui.data(|d| d.get_temp::<usize>(id)), resp.interact_pointer_pos()) {
            if sel < pts.len() {
                let c = to_curve(pos);
                let n = pts.len();
                let x = if sel == 0 {
                    0.0
                } else if sel == n - 1 {
                    1.0
                } else {
                    c[0].clamp(pts[sel - 1][0] + 0.01, pts[sel + 1][0] - 0.01)
                };
                pts[sel] = [x, c[1]];
            }
        }
    }

    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, Color32::from_gray(18));
    let grid = Stroke::new(1.0, Color32::from_gray(45));
    for k in 1..4 {
        let t = k as f32 / 4.0;
        p.line_segment([to_screen([t, 0.0]), to_screen([t, 1.0])], grid);
        p.line_segment([to_screen([0.0, t]), to_screen([1.0, t])], grid);
    }
    p.line_segment([to_screen([0.0, 0.0]), to_screen([1.0, 1.0])], Stroke::new(1.0, Color32::from_gray(70)));
    let line: Vec<Pos2> = (0..=128).map(|i| i as f32 / 128.0).map(|x| to_screen([x, eval_curve(pts, x)])).collect();
    p.add(egui::Shape::line(line, Stroke::new(2.0, Color32::from_gray(230))));
    for q in pts.iter() {
        p.circle(to_screen(*q), 4.5, Color32::from_gray(20), Stroke::new(2.0, Color32::WHITE));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let rs = frame.wgpu_render_state().cloned();
        self.looks_visible = false;

        if let Some(rx) = &self.loading {
            if let Ok(r) = rx.try_recv() {
                self.loading = None;
                if let Some(rs) = &rs {
                    self.finish_load(r, rs);
                    self.preload_next();
                }
            }
        }
        if let Some(bg) = &self.bg_masks {
            use std::sync::atomic::Ordering;
            // Le travail interactif passe avant.
            let busy = self.loading.is_some() || self.ai_job.is_some() || self.erase_job.is_some() || self.exporting.is_some() || self.batch.is_some();
            bg.pause.store(busy, Ordering::Release);
            if bg.done.load(Ordering::Acquire) >= bg.total {
                self.bg_masks = None;
            }
        }
        // Autre dossier affiché dans la bibliothèque : ses photos passent en premier.
        if self.bg_scope.is_some() && self.bg_scope.as_ref() != Some(&self.folder) && self.scanning.is_none() {
            self.start_bg_masks(&ctx);
        }
        if let (Some(tool), Some(rs)) = (self.file_request.take(), &rs) {
            self.start_file_job(tool, rs, &ctx);
        }
        if let Some(job) = &self.file_job {
            match job.rx.try_recv() {
                Ok(r) => {
                    let took = duration_text(job.start.elapsed());
                    self.file_job = None;
                    match r {
                        Ok((file, msg)) => {
                            self.add_doc_folder(&file, &ctx);
                            self.pending_status = Some(format!("{msg} ({took}) : {}", file.display()));
                            self.open(file, &ctx);
                            self.expand = [0.0; 4];
                        }
                        Err(e) => self.status = tf!("Échec : {e:#}", e),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.file_job = None,
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(300)),
            }
        }
        if let Some(rx) = &self.exporting {
            if let Ok(r) = rx.try_recv() {
                self.exporting = None;
                self.status = match r {
                    Ok(p) => tf!("Exporté : {0}", p.display()),
                    Err(e) => tf!("Échec de l'export : {e:#}", e),
                };
            }
        }
        if let Some(job) = &self.ai_job {
            match job.rx.try_recv() {
                Ok(r) => {
                    let job = self.ai_job.take().unwrap();
                    // Résultat pour une photo déjà fermée : il est en cache, rien d'autre à faire.
                    if self.path.as_ref() == Some(&job.photo) {
                        match r {
                            Ok(set) => {
                                for (m, mask) in set {
                                    self.ai_masks.insert(m, Arc::new(mask));
                                }
                                self.masks_uploaded.clear();
                                self.sky_uploaded = None;
                                self.depth_uploaded = None;
                                self.subject_uploaded = None;
                                self.face_uploaded = None;
                                self.status = if job.model.face_classes().is_some() { t!("Masques du visage prêts.").into() } else { tf!("Masque {0} prêt.", job.model.of_what()) };
                            }
                            Err(e) => {
                                // Les masques du visage échouent ensemble (aucun visage, modèle absent).
                                if job.model.face_classes().is_some() {
                                    self.ai_failed.extend(ai::FACE_MODELS);
                                }
                                self.ai_failed.insert(job.model);
                                self.status = tf!("Masque IA {0} impossible : {e:#}", job.model.of_what(), e);
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let job = self.ai_job.take().unwrap();
                    if job.model.face_classes().is_some() {
                        self.ai_failed.extend(ai::FACE_MODELS);
                    }
                    self.ai_failed.insert(job.model);
                    self.status = tf!("Masque IA {0} impossible : le calcul s'est arrêté (détails dans le terminal).", job.model.of_what());
                }
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
            }
        }
        if let Some((_, rx)) = &self.lang_job {
            match rx.try_recv() {
                Ok(r) => {
                    self.lang_job = None;
                    match r {
                        Ok(code) => {
                            self.set_language(code);
                            self.lang_msg = t!("Interface traduite. Le fichier est modifiable dans ~/.local/share/luminux/lang.").into();
                        }
                        Err(e) => self.lang_msg = tf!("Traduction impossible : {e:#}", e),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.lang_job = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(job) = &self.denoise_job {
            match job.rx.try_recv() {
                Ok(r) => {
                    let job = self.denoise_job.take().unwrap();
                    let here = self.path.as_ref() == Some(&job.photo);
                    match r {
                        Ok(()) => {
                            if here {
                                // Résultat prêt : appliqué aussitôt (intensité gardée si déjà réglée).
                                let amount = if self.adj.denoise.model == job.model && self.adj.denoise.amount > 0.0 { self.adj.denoise.amount } else { 0.8 };
                                self.adj.denoise = crate::adjust::AiDenoise { model: job.model, amount };
                                self.status = t!("Débruitage IA prêt.").into();
                            }
                        }
                        Err(e) => {
                            if here {
                                self.denoise_failed = crate::denoise::cache_file(&job.photo, &job.model);
                                self.status = tf!("Débruitage IA impossible : {e:#}", e);
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.denoise_job = None,
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
            }
        }
        if let Some(job) = &self.erase_job {
            match job.rx.try_recv() {
                Ok(r) => {
                    let job = self.erase_job.take().unwrap();
                    if self.path.as_ref() == Some(&job.photo) {
                        let took = duration_text(job.start.elapsed());
                        match r {
                            Ok(msg) => {
                                self.status = format!("{msg} ({took})");
                                // Durée ajoutée à l'entrée d'historique qui a créé cette retouche.
                                let first = self.history.iter().position(|a| a.retouch.len() > job.index).filter(|i| *i > 0);
                                if let Some(label) = first.and_then(|i| self.history_labels.get_mut(i)) {
                                    if !label.contains(" · ") {
                                        *label = format!("{label} · {took}");
                                    }
                                }
                            }
                            Err(e) => {
                                self.erase_failed.insert(job.index);
                                self.status = tf!("Retouche impossible : {e:#}", e);
                            }
                        }
                        self.retouch_uploaded = None;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Calcul interrompu sans réponse (plantage) : on le dit plutôt que de ne rien faire.
                    let job = self.erase_job.take().unwrap();
                    self.erase_failed.insert(job.index);
                    self.status = t!("Retouche impossible : le calcul s'est arrêté (détails dans le terminal).").to_string();
                }
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
            }
        }
        // Retour dans l'application (ou toutes les 15 min) : relecture des dossiers, pour les photos
        // ajoutées, modifiées ou supprimées entre-temps.
        let focused = ctx.input(|i| i.focused);
        let since = self.last_scan.map_or(u64::MAX, |t| t.elapsed().as_secs());
        if self.scanning.is_none() && ((focused && !self.was_focused && since >= 60) || since >= 900 && since != u64::MAX) {
            self.start_scan(&ctx);
        }
        self.was_focused = focused;
        if let Some((rx, done, total)) = &mut self.cull {
            let before = *done;
            let mut finished = false;
            loop {
                match rx.try_recv() {
                    Ok((p, sharp, hash)) => {
                        if let Some(cat) = &self.catalogue {
                            cat.set_cull(&p, sharp, hash);
                        }
                        *done += 1;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
            let (d, t) = (*done, *total);
            if finished {
                self.cull = None;
            }
            // Liste mise à jour au fil de l'analyse (toutes les 25 photos) et à la fin.
            let _ = t;
            if matches!(self.filter, Filter::Blurry | Filter::Similar) && (finished || d / 25 != before / 25) {
                self.refresh_list();
            }
        }
        if let Some(rx) = &self.scanning {
            let mut done = false;
            while let Ok(m) = rx.try_recv() {
                match m {
                    ScanMsg::Progress(n) => self.scan_count = n,
                    ScanMsg::Done(n) => {
                        self.scan_count = n;
                        done = true;
                    }
                    ScanMsg::Error(e) => {
                        self.status = tf!("Erreur de scan : {e}", e);
                        done = true;
                    }
                }
            }
            if done {
                self.scanning = None;
                self.refresh_list();
                self.refresh_dirs();
                self.start_bg_masks(&ctx);
            } else if self.scan_count >= self.scan_listed + 200 {
                // Affichage progressif pendant un long premier scan.
                self.scan_listed = self.scan_count;
                self.refresh_list();
                self.refresh_dirs();
            }
        }
        if let Some(rx) = &self.picking {
            match rx.try_recv() {
                Ok(p) => {
                    self.picking = None;
                    match p {
                        Picked::Open(p) => self.open(p, &ctx),
                        Picked::Folder(d) => self.folder_picked(d, &ctx),
                        Picked::Import(paths) => self.start_import(paths, &ctx),
                        Picked::BatchExport(dir) => self.start_batch(dir, &ctx),
                        Picked::LangImport(f) => match crate::i18n::import(&f) {
                            Ok(code) => self.set_language(code),
                            Err(e) => self.lang_msg = tf!("Langue non importée : {e:#}", e),
                        },
                        Picked::LangTemplate(f) => {
                            self.lang_msg = match crate::i18n::export_template(&f) {
                                Ok(()) => tf!("Modèle enregistré : {0}. Traduis les textes de droite, puis « Importer… ».", f.display()),
                                Err(e) => format!("{e:#}"),
                            };
                        }
                        Picked::ScreenIcc(f) => {
                            self.settings.screen_icc = f.display().to_string();
                            self.settings.save();
                        }
                        Picked::LayerImage(f) => self.add_layer(f, &ctx),
                        Picked::LocalModel(f) => {
                            self.settings.local_model = f.display().to_string();
                            self.settings.save();
                        }
                        Picked::WatermarkImage(f) => self.picked_logo(f),
                        Picked::Skies(files) => {
                            let mut last = None;
                            for f in files {
                                match sky::import(&f) {
                                    Ok(name) => last = Some(name),
                                    Err(e) => self.status = tf!("Ciel non ajouté : {e:#}", e),
                                }
                            }
                            self.skies = sky::list();
                            if last.is_some() {
                                self.adj.sky.image = last;
                            }
                        }
                        Picked::Export(out) => {
                            if let Some(rs) = &rs {
                                self.export_to(out, rs, &ctx);
                            }
                        }
                        Picked::ExportCutout(out) => {
                            if let Some(rs) = &rs {
                                self.export_cutout(out, rs, &ctx);
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.picking = None,
                Err(_) => {}
            }
        }
        if let Some(t) = &self.thumbs {
            while let Ok((p, img)) = t.rx.try_recv() {
                let tex = match img {
                    Some(img) => Tex::Ready(ctx.load_texture(p.to_string_lossy(), img, egui::TextureOptions::LINEAR)),
                    None => Tex::Failed,
                };
                self.textures.insert(p, tex);
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        let (to_import, photos): (Vec<PathBuf>, Vec<PathBuf>) = dropped.into_iter().partition(|p| is_look_file(p));
        if !to_import.is_empty() {
            self.start_import(to_import, &ctx);
        } else if let Some(p) = photos.into_iter().next() {
            self.open(p, &ctx);
        }
        if let Some(rx) = &self.batch {
            while let Ok(m) = rx.try_recv() {
                match m {
                    BatchMsg::Progress { done, total } => self.batch_progress = (done, total),
                    BatchMsg::Done { ok, errors, dir } => {
                        self.batch = None;
                        let mut text = tf!("{ok} photo(s) exportée(s) dans {0}.", ok, dir.display());
                        if !errors.is_empty() {
                            text += &tf!("\n\n{0} échec(s) :", errors.len());
                            for e in errors.iter().take(20) {
                                text += &format!("\n• {e}");
                            }
                        }
                        self.status = text.lines().next().unwrap_or("").to_string();
                        self.import_report = Some(text);
                        break;
                    }
                }
            }
        }
        if let Some(rx) = &self.importing {
            if let Ok(r) = rx.try_recv() {
                self.importing = None;
                self.finish_import(r);
            }
        }
        self.new_doc_window(&ctx);
        self.prefs_window(&ctx);
        self.export_window(&ctx);
        self.help_windows(&ctx);
        self.trash_window(&ctx);
        self.layer_picker_window(&ctx);
        if let Some(text) = self.import_report.clone() {
            let mut open = true;
            egui::Window::new(t!("Rapport")).open(&mut open).collapsible(false).vscroll(true).default_height(ctx.content_rect().height() - 120.0).default_width(460.0).show(&ctx, |ui| {
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| ui.label(text));
            });
            if !open {
                self.import_report = None;
            }
        }
        if !self.close_window_on_escape(&ctx) {
            self.shortcuts(&ctx);
        }
        // Taille de l'interface changée au clavier (Ctrl+plus, Ctrl+moins) : gardée pour la prochaine fois.
        let z = ctx.zoom_factor();
        if (z - self.settings.ui_zoom).abs() > 1e-3 && !ctx.input(|i| i.pointer.any_down()) {
            self.settings.ui_zoom = z;
            self.settings.save();
        }

        egui::Panel::top("menus").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("barre").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("etat").show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.loading.is_some() || self.exporting.is_some() {
                    ui.spinner();
                }
                if self.mode == Mode::Library {
                    ui.label(tf!("{0} photos", self.photos.len()));
                    if !self.picked.is_empty() {
                        ui.label(tf!("· {0} sélectionnée(s)", self.picked.len()));
                    }
                    if self.batch.is_some() {
                        ui.spinner();
                        ui.label(tf!("export {0}/{1}", self.batch_progress.0, self.batch_progress.1));
                    }
                    ui.label(egui::RichText::new(&self.status).weak());
                    if self.scanning.is_some() {
                        ui.spinner();
                        ui.label(tf!("scan : {0} fichiers", self.scan_count));
                    }
                    let pending = self.thumbs.as_ref().map_or(0, |t| t.pending());
                    if pending > 0 {
                        ui.label(tf!("vignettes en attente : {pending}", pending));
                    }
                } else {
                    if self.erase_job.is_some() {
                        ui.spinner();
                        ui.label(t!("Retouche en cours…"));
                    }
                    if let Some(job) = &self.ai_job {
                        ui.spinner();
                        let (done, total) = (job.progress.done.load(std::sync::atomic::Ordering::Relaxed), job.progress.total.load(std::sync::atomic::Ordering::Relaxed));
                        if done > 0 && done < total {
                            ui.label(tf!("Téléchargement du modèle {0} : {1} / {2} Mo", crate::i18n::tr(job.model.label()), done >> 20, total >> 20));
                        } else {
                            ui.label(tf!("Détection {0}…", job.model.of_what()));
                        }
                    }
                    ui.label(&self.status);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.gpu_name);
                    if let Some((_, d, total)) = &self.cull {
                        ui.label(egui::RichText::new(tf!("Analyse des photos : {0}/{1}", d, total)).weak())
                            .on_hover_text(t!("Netteté et ressemblance, mesurées une fois par photo"));
                    }
                    if let Some(bg) = &self.bg_masks {
                        let d = bg.done.load(std::sync::atomic::Ordering::Acquire);
                        ui.label(egui::RichText::new(tf!("Masques IA en arrière-plan : {0}/{1}", d, bg.total)).weak())
                            .on_hover_text(t!("Sujet et ciel préparés pour les photos du dossier affiché, 500 au plus (réglable dans Préférences)"));
                    }
                    if let (Some((w, h)), Mode::Editor) = (self.size, self.mode) {
                        ui.label(tf!("{w}×{h} · {0:.0} %", w, h, self.zoom * 100.0));
                    }
                });
            });
        });
        if self.mode == Mode::Library {
            egui::Panel::left("dossiers").default_size(220.0).show(ui, |ui| self.library_side(ui));
            egui::CentralPanel::default().show(ui, |ui| self.library_grid(ui));
        } else {
            self.cutout_active = false;
            self.looks_visible = false;
            let left = self.settings.panel_left;
            let side = |id: &str, left: bool| if left { egui::Panel::left(id) } else { egui::Panel::right(id) };
            let has = self.size.is_some();
            side("onglets", left).resizable(false).exact_size(68.0).show(ui, |ui| {
                // Défile quand la fenêtre est basse ou l'interface agrandie.
                egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                    ui.add_enabled_ui(has, |ui| self.rail(ui));
                });
            });
            // Panneau masqué (P) : il glisse et laisse à sa place une fine bande avec le bouton pour le rouvrir.
            let mut shown = !self.panel_hidden;
            let main_side = if self.settings.split_layout { !left } else { left };
            let main_tab = if self.settings.split_layout { Tab::Develop } else { self.tab };
            let id = if self.settings.split_layout { "reglages_seuls" } else { "reglages" };
            egui::Panel::show_switched(
                ui,
                &mut shown,
                side("reglages_replies", main_side).resizable(false).exact_size(40.0),
                side(id, main_side).resizable(false).exact_size(360.0),
                |ui, expanded| {
                    if expanded {
                        ui.add_enabled_ui(has, |ui| self.panel(ui, main_tab, true));
                    } else {
                        ui.add_space(8.0);
                        ui.vertical_centered(|ui| {
                            // Chevrons vers l'endroit où le panneau va réapparaître.
                            if panel_button(ui, !self.panel_collapses_right(), t!("Afficher le panneau (P)")).clicked() {
                                self.toggle_panel();
                            }
                        });
                    }
                },
            );
            if self.settings.split_layout {
                // Sans glissement : en se refermant, il afficherait un outil déjà quitté.
                if shown && self.tab != Tab::Develop {
                    side("outil", left).resizable(false).exact_size(360.0).show(ui, |ui| {
                        ui.add_enabled_ui(has, |ui| self.panel(ui, self.tab, false));
                    });
                }
            }
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.view(ui));
        }

        // Un état stable (bouton relâché) devient une entrée d'historique.
        if self.adj != self.committed && !ctx.input(|i| i.pointer.any_down()) {
            let label = self.edit_label.take().or_else(|| history::take(&ctx)).unwrap_or_else(|| history::describe(&self.committed, &self.adj));
            self.history.truncate(self.hist_pos + 1);
            self.history_labels.truncate(self.hist_pos + 1);
            self.history.push(self.adj.clone());
            self.history_labels.push(label);
            self.hist_pos = self.history.len() - 1;
            self.committed = self.adj.clone();
            self.save();
            self.edits_since_open = true;
        }
        if let Some(rs) = &rs {
            self.sync_gpu(rs, &ctx);
        }
    }
}
