//! Réglages non destructifs : structure sauvegardée dans le fichier `.luminux`
//! et conversion vers les données envoyées au shader.

use serde::{Deserialize, Serialize};

pub const HSL_BANDS: [&str; 8] = [k!("Rouge"), k!("Orange"), k!("Jaune"), k!("Vert"), k!("Aqua"), k!("Bleu"), k!("Violet"), k!("Magenta")];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HslBand {
    /// Décalage de teinte, -1..1 (= ±30°).
    pub hue: f32,
    /// -1..1
    pub sat: f32,
    /// -1..1
    pub lum: f32,
}

/// Nombre maximal de réglages locaux (couches de la texture de masques).
pub const MAX_LOCALS: usize = 8;
/// Nombre maximal de calques.
pub const MAX_LAYERS: usize = 3;

/// Réglages appliqués dans un masque, ajoutés aux réglages globaux.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LocalAdj {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
    pub clarity: f32,
}

/// Un coup de pinceau : polyligne en coordonnées source normalisées.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushStroke {
    pub points: Vec<[f32; 2]>,
    /// Rayon en fraction de la largeur de l'image.
    pub radius: f32,
    /// 0 (bord très doux) .. 1 (bord net).
    pub hardness: f32,
    /// Opacité de chaque passage, 0..1.
    pub flow: f32,
    pub erase: bool,
    /// Lasso ou rectangle : l'intérieur de la polyligne (fermée) est rempli, le rayon adoucit le bord.
    #[serde(default)]
    pub fill: bool,
}

/// Masques reconnus par l'IA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AiKind {
    Sky,
    Subject,
    Background,
    /// Masques sémantiques (segmentation de scène ADE20K).
    People,
    Buildings,
    Vegetation,
    Water,
    Mountains,
    Ground,
}

/// Forme d'un masque, en coordonnées de la photo source (avant recadrage), normalisées 0..1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Shape {
    Brush { strokes: Vec<BrushStroke> },
    /// Effet complet côté `a`, nul côté `b`.
    Linear { a: [f32; 2], b: [f32; 2] },
    /// Ellipse : centre, rayons (fraction de la largeur), angle en degrés, adoucissement 0..1.
    Radial { center: [f32; 2], radius: [f32; 2], angle: f32, feather: f32 },
    Ai { kind: AiKind, #[serde(default)] strokes: Vec<BrushStroke> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Local {
    pub name: String,
    pub enabled: bool,
    pub invert: bool,
    /// Opacité du masque, 0..1.
    pub amount: f32,
    pub shape: Shape,
    pub adj: LocalAdj,
}

impl Default for Local {
    fn default() -> Self {
        Self { name: String::new(), enabled: true, invert: false, amount: 1.0, shape: Shape::Brush { strokes: vec![] }, adj: LocalAdj::default() }
    }
}

/// Remplacement du ciel (masque IA du ciel + image de ciel).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkyReplace {
    /// Nom du ciel dans le dossier des ciels (ou chemin absolu) ; None = désactivé.
    pub image: Option<String>,
    /// Opacité 0..1.
    pub amount: f32,
    /// Décalage vertical du ciel (-0.5..0.5) et zoom (1..3).
    pub offset: f32,
    pub scale: f32,
    pub flip: bool,
    /// Exposition du ciel en IL, par rapport à l'accord automatique sur le ciel d'origine.
    pub exposure: f32,
    /// Teinte de la scène vers la couleur du nouveau ciel, 0..1.
    pub relight: f32,
}

impl Default for SkyReplace {
    fn default() -> Self {
        Self { image: None, amount: 1.0, offset: 0.0, scale: 1.0, flip: false, exposure: 0.0, relight: 0.5 }
    }
}

/// Mode de fusion d'un calque avec la photo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blend {
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    Lighten,
    Darken,
}

/// Zone où un calque s'applique (masques IA de la photo).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayerMask {
    All,
    Sky,
    Subject,
    Background,
}

/// Calque : image posée sur la photo développée (texture, logo, autre photo).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    /// Chemin absolu de l'image.
    pub image: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend: Blend,
    pub mask: LayerMask,
    /// Taille : 1 = le calque couvre tout le cadre.
    pub scale: f32,
    /// Décalage du centre, en fraction du cadre (-0.5..0.5).
    pub x: f32,
    pub y: f32,
    pub flip: bool,
    /// Rotation en degrés, sens des aiguilles d'une montre.
    pub angle: f32,
    /// Découpe au lasso : contour gardé, en coordonnées de l'image du calque (0..1). Vide = image entière.
    pub clip: Vec<[f32; 2]>,
    /// Sujet seul : le fond de l'image du calque est retiré par le détourage IA.
    pub subject: bool,
    /// Gomme du calque : traits en coordonnées de l'image du calque (`erase` : rend transparent,
    /// sinon remet ce qui avait été gommé). Rayon en fraction de la largeur du calque.
    pub strokes: Vec<BrushStroke>,
    /// Harmoniser 0..1 : luminosité, contraste et teinte du calque rapprochés de ceux de la photo.
    pub harmony: f32,
    /// Ambiance de la photo développée sans ses calques (`layers::stats` : noir, blanc, teinte R, G, B,
    /// luminance moyenne), relevée quand le réglage change.
    pub harmony_target: [f32; 6],
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            image: String::new(),
            visible: true,
            opacity: 1.0,
            blend: Blend::Normal,
            mask: LayerMask::All,
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            flip: false,
            angle: 0.0,
            clip: Vec::new(),
            subject: false,
            strokes: Vec::new(),
            harmony: 0.0,
            harmony_target: [0.0, 1.0, 1.0, 1.0, 1.0, 0.5],
        }
    }
}

/// Amélioration IA : accent (tonalité adaptée au contenu), ciel, structure (détail hors ciel et sujet).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Enhance {
    /// Accent 0..1 : ombres débouchées et hautes lumières tenues selon la zone, contraste local, vibrance.
    pub accent: f32,
    /// Ciel 0..1 : ciel plus dense et plus coloré (masque de ciel IA).
    pub sky: f32,
    /// Structure -1..1 : détail moyen, sans toucher au ciel ni (option) au sujet.
    pub structure: f32,
    pub protect_subject: bool,
}

impl Default for Enhance {
    fn default() -> Self {
        Self { accent: 0.0, sky: 0.0, structure: 0.0, protect_subject: true }
    }
}

/// Retouche du portrait (masques du visage IA) : visage, yeux, bouche, peau.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Portrait {
    /// Lumière du visage 0..1 : visage éclairé, ombres débouchées.
    pub face_light: f32,
    /// Yeux 0..1 : iris plus nets et plus contrastés.
    pub eyes: f32,
    /// Blanc des yeux 0..1.
    pub eye_whiten: f32,
    /// Lèvres : saturation -1..1, rougeur 0..1.
    pub lips_sat: f32,
    pub lips_red: f32,
    /// Blanchiment des dents 0..1.
    pub teeth: f32,
    /// Peau : lissage 0..1 et réduction de la brillance 0..1.
    pub skin_smooth: f32,
    pub shine: f32,
    /// Remodelage : amincir le visage 0..1, agrandir les yeux 0..1, silhouette -1 (élargir) .. 1 (affiner).
    pub face_slim: f32,
    pub eye_size: f32,
    pub body: f32,
    /// Ventre 0..1 : affiné autour du nombril de chaque personne.
    pub belly: f32,
}

impl Portrait {
    pub fn active(&self) -> bool {
        *self != Portrait::default()
    }
}

/// Outils fondés sur la carte de profondeur (IA) : éclairage du premier plan, flou d'arrière-plan, brume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DepthFx {
    /// Relight : éclairage du premier plan en IL (-1..1 × 1,5), portée (0 = tout proche, 1 = jusqu'au fond),
    /// chaleur de la lumière (-1 froide .. 1 chaude), assombrissement de l'arrière-plan 0..1.
    pub relight: f32,
    pub reach: f32,
    pub warmth: f32,
    pub background: f32,
    /// Flou d'arrière-plan 0..1 ; mise au point (proximité 0 = fond .. 1 = premier plan, négatif = auto) ;
    /// éclat des hautes lumières floues 0..1.
    pub bokeh: f32,
    pub focus: f32,
    pub bokeh_glow: f32,
    /// Atmosphère : brume 0..1, luminosité de la brume, chaleur.
    pub haze: f32,
    pub haze_light: f32,
    pub haze_warmth: f32,
}

impl Default for DepthFx {
    fn default() -> Self {
        Self { relight: 0.0, reach: 0.5, warmth: 0.0, background: 0.0, bokeh: 0.0, focus: -1.0, bokeh_glow: 0.3, haze: 0.0, haze_light: 1.0, haze_warmth: 0.0 }
    }
}

impl DepthFx {
    /// La carte de profondeur est nécessaire.
    pub fn active(&self) -> bool {
        self.relight != 0.0 || self.background != 0.0 || self.bokeh > 0.0 || self.haze > 0.0
    }
}

/// Débruitage IA : modèle de débruitage de ComfyUI (dossier upscale_models) et intensité 0..1.
/// Le résultat est mis en cache par photo et par modèle, puis mêlé à l'image source avant tout le reste.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiDenoise {
    pub model: String,
    pub amount: f32,
}

impl AiDenoise {
    pub fn active(&self) -> bool {
        !self.model.is_empty() && self.amount > 0.0
    }
}

/// Retouche de l'image source (coordonnées source normalisées).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Retouch {
    /// Correcteur de taches : rayon en fraction de la largeur, décalage de la source.
    Heal { center: [f32; 2], radius: f32, offset: [f32; 2] },
    /// Gomme : zone peinte à remplir.
    /// `smooth` : remplissage rapide par le fond environnant, sans l'IA (LaMa).
    Erase { strokes: Vec<BrushStroke>, #[serde(default)] smooth: bool },
    /// Génération ComfyUI dans la zone peinte : modèle (checkpoint), invite, graine, force (denoise).
    Generate {
        strokes: Vec<BrushStroke>,
        model: String,
        prompt: String,
        negative: String,
        seed: u64,
        strength: f32,
        /// Gomme par diffusion, passe précise : objets longs découpés en tranches.
        #[serde(default)]
        zones: bool,
    },
}

/// Correction d'objectif (base Lensfun).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensFix {
    pub enabled: bool,
    /// Objectif choisi à la main (clé de la base) ; None = détection par les EXIF.
    pub lens: Option<String>,
    /// Intensités 0..1 de la distorsion et du vignetage, aberration chromatique oui/non.
    pub distortion: f32,
    pub tca: bool,
    pub vignetting: f32,
    /// Zoom automatique pour ne pas laisser de bords vides.
    pub autoscale: bool,
}

impl Default for LensFix {
    fn default() -> Self {
        Self { enabled: false, lens: None, distortion: 1.0, tca: true, vignetting: 1.0, autoscale: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Adjustments {
    pub exposure: f32,
    pub temperature: f32,
    pub tint: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
    /// Points de la courbe des tons (x, y) dans [0,1], triés par x, extrémités comprises.
    pub curve: Vec<[f32; 2]>,
    pub hsl: [HslBand; 8],
    /// Recadrage x, y, largeur, hauteur, normalisés dans l'image redressée.
    pub crop: [f32; 4],
    /// Redressement en degrés (sens horaire positif).
    pub angle: f32,
    /// Netteté 0..1.
    pub sharpen: f32,
    /// Réduction du bruit de luminance et de couleur, 0..1.
    pub nr_luma: f32,
    pub nr_color: f32,
    /// Vignettage -1 (assombrit) .. 1 (éclaircit), et point de départ 0..1.
    pub vignette: f32,
    pub vignette_mid: f32,
    /// Clarté (contraste local à grand rayon) et texture (rayon moyen), -1..1.
    pub clarity: f32,
    pub texture: f32,
    /// Virage partiel : teinte (0..360) et saturation (0..1) des ombres et des hautes lumières,
    /// équilibre -1 (ombres) .. 1 (hautes lumières).
    pub split_shadow_hue: f32,
    pub split_shadow_sat: f32,
    pub split_highlight_hue: f32,
    pub split_highlight_sat: f32,
    pub split_balance: f32,
    /// Balance des couleurs : décalage R, V, B (-1..1) des ombres, tons moyens, hautes lumières.
    pub balance_shadows: [f32; 3],
    pub balance_midtones: [f32; 3],
    pub balance_highlights: [f32; 3],
    /// Grain : quantité 0..1, taille 0..1.
    pub grain: f32,
    pub grain_size: f32,
    /// LUT 3D (.cube) relative au dossier des looks, et son intensité 0..1.
    pub lut: Option<String>,
    pub lut_amount: f32,
    /// Réglages locaux (pinceau, dégradés, masques IA).
    pub locals: Vec<Local>,
    pub sky: SkyReplace,
    pub lens: LensFix,
    pub retouch: Vec<Retouch>,
    pub depth: DepthFx,
    pub enhance: Enhance,
    pub portrait: Portrait,
    pub denoise: AiDenoise,
    /// Calques, du plus bas au plus haut.
    pub layers: Vec<Layer>,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            temperature: 0.0,
            tint: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            curve: vec![[0.0, 0.0], [1.0, 1.0]],
            hsl: [HslBand::default(); 8],
            crop: [0.0, 0.0, 1.0, 1.0],
            angle: 0.0,
            sharpen: 0.0,
            nr_luma: 0.0,
            nr_color: 0.0,
            vignette: 0.0,
            vignette_mid: 0.5,
            clarity: 0.0,
            texture: 0.0,
            split_shadow_hue: 0.0,
            split_shadow_sat: 0.0,
            split_highlight_hue: 0.0,
            split_highlight_sat: 0.0,
            split_balance: 0.0,
            balance_shadows: [0.0; 3],
            balance_midtones: [0.0; 3],
            balance_highlights: [0.0; 3],
            grain: 0.0,
            grain_size: 0.25,
            lut: None,
            lut_amount: 1.0,
            locals: Vec::new(),
            sky: SkyReplace::default(),
            lens: LensFix::default(),
            retouch: Vec::new(),
            depth: DepthFx::default(),
            enhance: Enhance::default(),
            portrait: Portrait::default(),
            denoise: AiDenoise::default(),
            layers: Vec::new(),
        }
    }
}

/// Uniformes du shader (alignement std140 : uniquement des vec4).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    /// Sous-rectangle de l'image à dessiner : x, y, largeur, hauteur en UV.
    pub uv_rect: [f32; 4],
    /// Gains R, G, B et multiplicateur d'exposition.
    pub wb: [f32; 4],
    /// Contraste, hautes lumières, ombres, blancs.
    pub tone: [f32; 4],
    /// Noirs, saturation, vibrance, inutilisé.
    pub tone2: [f32; 4],
    /// x = avant (1) / après (0), y = forcer le niveau de mip 0 (export),
    /// z = vignettage, w = point de départ du vignettage.
    pub misc: [f32; 4],
    /// Recadrage normalisé (x, y, l, h).
    pub crop: [f32; 4],
    /// Angle (radians), largeur et hauteur source en pixels, -.
    pub geo: [f32; 4],
    /// Netteté, bruit luminance, bruit couleur, -.
    pub detail: [f32; 4],
    /// Clarté, texture, grain, taille du grain.
    pub local: [f32; 4],
    /// Virage partiel : couleur des ombres (RGB centré, déjà × saturation), équilibre.
    pub split_sh: [f32; 4],
    /// Couleur des hautes lumières, -.
    pub split_hl: [f32; 4],
    /// Balance des couleurs (RGB, -) ombres, tons moyens, hautes lumières.
    pub bal_s: [f32; 4],
    pub bal_m: [f32; 4],
    pub bal_h: [f32; 4],
    /// Intensité de la LUT (0 = pas de LUT), taille N de la LUT, -, -.
    pub lut: [f32; 4],
    /// Nombre de masques, masque affiché en rouge (-1 : aucun), -, -.
    pub mask_info: [f32; 4],
    /// Par masque : (exposition, contraste, hautes lumières, ombres), (température, teinte, saturation, clarté).
    pub locals: [[f32; 4]; 2 * MAX_LOCALS],
    /// Ciel : opacité (0 = aucun), décalage, zoom, gain (exposition × accord automatique).
    pub sky: [f32; 4],
    /// Rééclairage, rapport largeur/hauteur du ciel, miroir, -.
    pub sky2: [f32; 4],
    /// Couleur moyenne du ciel (normalisée), - ; étendue utile de la texture (x, y) dans sky3.
    pub sky_avg: [f32; 4],
    pub sky3: [f32; 4],
    /// Objectif : actif, pixels -> unités Hugin, zoom, pixels -> unités du vignetage.
    pub lens: [f32; 4],
    /// Distorsion (c1..c4), aberration rouge et bleue (v, c, b), vignetage (k1..k3).
    pub lens_dist: [f32; 4],
    pub lens_tca_r: [f32; 4],
    pub lens_tca_b: [f32; 4],
    pub lens_vig: [f32; 4],
    pub hsl: [[f32; 4]; 8],
    /// Profondeur : (relight IL, portée, chaleur, arrière-plan), (flou, mise au point, éclat, -),
    /// (brume, luminosité, chaleur, carte disponible — renseigné par le Developer).
    pub depth: [f32; 4],
    pub depth2: [f32; 4],
    pub depth3: [f32; 4],
    /// Amélioration : (accent, ciel, structure, protéger le sujet), (masque de ciel, masque du sujet
    /// disponibles — renseignés par le Developer, -, -).
    pub enhance: [f32; 4],
    pub enhance2: [f32; 4],
    /// Portrait : (lumière du visage, yeux, blanc des yeux, dents), (saturation et rougeur des lèvres,
    /// lissage, brillance), (masques du visage disponibles — renseigné par le Developer, -, -, -).
    pub face: [f32; 4],
    pub face2: [f32; 4],
    pub face3: [f32; 4],
    /// Remodelage : (amincir le visage, agrandir les yeux, silhouette, ventre), (visages, yeux, nombre de
    /// personnes, -) ; repères en pixels source — renseignés par le Developer.
    pub reshape: [f32; 4],
    pub reshape2: [f32; 4],
    /// Par visage : (milieu des yeux x, y, axe x, y), (écart des yeux, -, -, -).
    pub faces: [[f32; 4]; 2 * crate::reshape::MAX_FACES],
    /// Par œil : centre x, y, rayon, -.
    pub eyes: [[f32; 4]; crate::reshape::MAX_EYES],
    /// Par bande (`PERSON_ROWS` par personne) : axe x, demi-largeur, poids, -.
    pub body: [[f32; 4]; crate::reshape::BODY_ROWS],
    /// Par personne : (haut, bas, centre du ventre y, étendue du ventre), (place libre à gauche, à droite, -, -).
    pub people: [[f32; 4]; 2 * crate::reshape::MAX_PEOPLE],
    /// Par calque : (opacité, mode de fusion, masque, taille), (décalage x, y, miroir, rapport largeur/hauteur),
    /// (étendue utile de la texture x, y, -, -) ; rapport et étendue renseignés par le Developer.
    pub layers: [[f32; 4]; 3 * MAX_LAYERS],
}

/// Couleur pure de teinte `hue_deg`, centrée sur sa luminance (décalage chromatique seul).
pub fn tint_offset(hue_deg: f32, sat: f32) -> [f32; 4] {
    let h = (hue_deg.rem_euclid(360.0)) / 60.0;
    let f = |n: f32| {
        let k = (n + h).rem_euclid(6.0);
        1.0 - k.min(4.0 - k).clamp(0.0, 1.0)
    };
    let (r, g, b) = (f(5.0), f(3.0), f(1.0));
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    [(r - y) * sat, (g - y) * sat, (b - y) * sat, 0.0]
}

impl Adjustments {
    /// Taille de l'image produite (après recadrage) pour une source `w`x`h`.
    pub fn output_size(&self, w: u32, h: u32) -> (u32, u32) {
        (((self.crop[2] * w as f32).round() as u32).max(1), ((self.crop[3] * h as f32).round() as u32).max(1))
    }

    /// `src` = taille de la source en pixels ; `full_frame` ignore le recadrage (mode recadrage).
    pub fn uniforms(&self, uv_rect: [f32; 4], before: bool, lod0: bool, src: [f32; 2], full_frame: bool) -> Uniforms {
        // Température et teinte : gains relatifs autour de la balance de prise de vue.
        let t = self.temperature * 0.5;
        let g = self.tint * 0.3;
        let wb = [(1.0 + t).max(0.05), (1.0 - g).max(0.05), (1.0 - t).max(0.05), self.exposure.exp2()];
        let mut hsl = [[0.0; 4]; 8];
        for (dst, b) in hsl.iter_mut().zip(&self.hsl) {
            *dst = [b.hue, b.sat, b.lum, 0.0];
        }
        Uniforms {
            uv_rect,
            wb,
            tone: [self.contrast, self.highlights, self.shadows, self.whites],
            tone2: [self.blacks, self.saturation, self.vibrance, 0.0],
            misc: [before as u32 as f32, lod0 as u32 as f32, self.vignette, self.vignette_mid],
            crop: if full_frame { [0.0, 0.0, 1.0, 1.0] } else { self.crop },
            geo: [self.angle.to_radians(), src[0], src[1], 0.0],
            detail: [self.sharpen, self.nr_luma, self.nr_color, 0.0],
            local: [self.clarity, self.texture, self.grain, self.grain_size],
            split_sh: {
                let mut v = tint_offset(self.split_shadow_hue, self.split_shadow_sat);
                v[3] = self.split_balance;
                v
            },
            split_hl: tint_offset(self.split_highlight_hue, self.split_highlight_sat),
            bal_s: [self.balance_shadows[0], self.balance_shadows[1], self.balance_shadows[2], 0.0],
            bal_m: [self.balance_midtones[0], self.balance_midtones[1], self.balance_midtones[2], 0.0],
            bal_h: [self.balance_highlights[0], self.balance_highlights[1], self.balance_highlights[2], 0.0],
            // Taille réelle renseignée par le Developer, qui connaît la LUT chargée.
            lut: [if self.lut.is_some() { self.lut_amount } else { 0.0 }, 0.0, 0.0, 0.0],
            mask_info: [self.locals.len().min(MAX_LOCALS) as f32, -1.0, 0.0, 0.0],
            locals: {
                let mut l = [[0.0; 4]; 2 * MAX_LOCALS];
                for (i, loc) in self.locals.iter().take(MAX_LOCALS).enumerate().filter(|(_, l)| l.enabled) {
                    let a = &loc.adj;
                    l[2 * i] = [a.exposure, a.contrast, a.highlights, a.shadows];
                    l[2 * i + 1] = [a.temperature, a.tint, a.saturation, a.clarity];
                }
                l
            },
            sky: [if self.sky.image.is_some() { self.sky.amount } else { 0.0 }, self.sky.offset, self.sky.scale.max(0.1), self.sky.exposure.exp2()],
            sky2: [self.sky.relight, 1.0, self.sky.flip as u32 as f32, 0.0],
            // Renseignés par le Developer, qui connaît le ciel et son masque.
            sky_avg: [1.0, 1.0, 1.0, 0.0],
            sky3: [1.0, 1.0, 0.0, 0.0],
            // Renseignés par le Developer, qui connaît le profil d'objectif de la photo.
            lens: [0.0; 4],
            lens_dist: [0.0; 4],
            lens_tca_r: [1.0, 0.0, 0.0, 0.0],
            lens_tca_b: [1.0, 0.0, 0.0, 0.0],
            lens_vig: [0.0; 4],
            hsl,
            depth: [self.depth.relight, self.depth.reach, self.depth.warmth, self.depth.background],
            depth2: [self.depth.bokeh, self.depth.focus, self.depth.bokeh_glow, 0.0],
            depth3: [self.depth.haze, self.depth.haze_light, self.depth.haze_warmth, 0.0],
            enhance: [self.enhance.accent, self.enhance.sky, self.enhance.structure, self.enhance.protect_subject as u32 as f32],
            enhance2: [0.0; 4],
            face: [self.portrait.face_light, self.portrait.eyes, self.portrait.eye_whiten, self.portrait.teeth],
            face2: [self.portrait.lips_sat, self.portrait.lips_red, self.portrait.skin_smooth, self.portrait.shine],
            face3: [0.0; 4],
            reshape: [self.portrait.face_slim, self.portrait.eye_size, self.portrait.body, self.portrait.belly],
            reshape2: [0.0; 4],
            faces: [[0.0; 4]; 2 * crate::reshape::MAX_FACES],
            eyes: [[0.0; 4]; crate::reshape::MAX_EYES],
            body: [[0.0; 4]; crate::reshape::BODY_ROWS],
            people: [[0.0; 4]; 2 * crate::reshape::MAX_PEOPLE],
            layers: {
                let mut v = [[0.0; 4]; 3 * MAX_LAYERS];
                for (i, l) in self.layers.iter().take(MAX_LAYERS).enumerate() {
                    let op = if l.visible { l.opacity.clamp(0.0, 1.0) } else { 0.0 };
                    v[3 * i] = [op, l.blend as u8 as f32, l.mask as u8 as f32, l.scale.max(0.02)];
                    v[3 * i + 1] = [l.x, l.y, l.flip as u32 as f32, 1.0];
                    v[3 * i + 2] = [0.0, 0.0, l.angle.to_radians(), 0.0];
                }
                v
            },
        }
    }

    /// Remet à zéro les réglages d'une section du bloc Réglages (nom de la section, en français).
    pub fn reset_section(&mut self, name: &str) {
        let d = Adjustments::default();
        match name {
            "Lumière" => {
                (self.exposure, self.contrast, self.highlights) = (d.exposure, d.contrast, d.highlights);
                (self.shadows, self.whites, self.blacks) = (d.shadows, d.whites, d.blacks);
            }
            "Couleur" => (self.temperature, self.tint, self.vibrance, self.saturation) = (d.temperature, d.tint, d.vibrance, d.saturation),
            "Courbe des tons" => self.curve = d.curve,
            "Détails" => (self.sharpen, self.nr_luma, self.nr_color) = (d.sharpen, d.nr_luma, d.nr_color),
            "Effets" => (self.clarity, self.texture, self.grain, self.grain_size) = (d.clarity, d.texture, d.grain, d.grain_size),
            "Vignettage" => (self.vignette, self.vignette_mid) = (d.vignette, d.vignette_mid),
            "Virage partiel" => {
                (self.split_shadow_hue, self.split_shadow_sat, self.split_balance) = (d.split_shadow_hue, d.split_shadow_sat, d.split_balance);
                (self.split_highlight_hue, self.split_highlight_sat) = (d.split_highlight_hue, d.split_highlight_sat);
            }
            "Balance des couleurs" => (self.balance_shadows, self.balance_midtones, self.balance_highlights) = (d.balance_shadows, d.balance_midtones, d.balance_highlights),
            "TSL" => self.hsl = d.hsl,
            _ => {}
        }
    }

    /// La section a-t-elle des réglages différents de ceux par défaut ?
    pub fn section_changed(&self, name: &str) -> bool {
        let mut d = self.clone();
        d.reset_section(name);
        d != *self
    }

    /// Copie des réglages d'aspect d'un look, en gardant la géométrie (recadrage, angle)
    /// et les réglages de détail propres à la photo.
    pub fn with_look(&self, look: &Adjustments) -> Adjustments {
        Adjustments {
            crop: self.crop,
            angle: self.angle,
            nr_luma: self.nr_luma,
            nr_color: self.nr_color,
            locals: self.locals.clone(),
            sky: self.sky.clone(),
            lens: self.lens.clone(),
            retouch: self.retouch.clone(),
            depth: self.depth.clone(),
            portrait: self.portrait.clone(),
            denoise: self.denoise.clone(),
            layers: self.layers.clone(),
            ..look.clone()
        }
    }

    /// Coordonnée de sortie (0..1 dans le recadrage) -> coordonnée source (0..1).
    /// Même calcul que `geo_map` dans le shader.
    pub fn out_to_src(&self, uv: [f32; 2], src: [f32; 2], full_frame: bool) -> [f32; 2] {
        let c = if full_frame { [0.0, 0.0, 1.0, 1.0] } else { self.crop };
        let (s, co) = self.angle.to_radians().sin_cos();
        let p = [(c[0] + uv[0] * c[2]) * src[0] - src[0] * 0.5, (c[1] + uv[1] * c[3]) * src[1] - src[1] * 0.5];
        let q = [co * p[0] + s * p[1], -s * p[0] + co * p[1]];
        [(q[0] + src[0] * 0.5) / src[0], (q[1] + src[1] * 0.5) / src[1]]
    }

    /// Inverse de `out_to_src`.
    pub fn src_to_out(&self, uv: [f32; 2], src: [f32; 2], full_frame: bool) -> [f32; 2] {
        let c = if full_frame { [0.0, 0.0, 1.0, 1.0] } else { self.crop };
        let (s, co) = self.angle.to_radians().sin_cos();
        let q = [uv[0] * src[0] - src[0] * 0.5, uv[1] * src[1] - src[1] * 0.5];
        let p = [co * q[0] - s * q[1], s * q[0] + co * q[1]];
        [((p[0] + src[0] * 0.5) / src[0] - c[0]) / c[2], ((p[1] + src[1] * 0.5) / src[1] - c[1]) / c[3]]
    }

    /// Courbe échantillonnée sur 256 valeurs (interpolation cubique monotone).
    pub fn curve_lut(&self) -> [f32; 256] {
        let mut lut = [0.0; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            *v = eval_curve(&self.curve, i as f32 / 255.0);
        }
        lut
    }
}

/// Vrai si le recadrage `c` (normalisé) tient entièrement dans l'image source
/// une fois celle-ci tournée de `angle_deg` autour de son centre.
pub fn crop_inside(c: [f32; 4], angle_deg: f32, w: f32, h: f32) -> bool {
    let (s, co) = angle_deg.to_radians().sin_cos();
    let corners = [[c[0], c[1]], [c[0] + c[2], c[1]], [c[0], c[1] + c[3]], [c[0] + c[2], c[1] + c[3]]];
    corners.iter().all(|p| {
        let (x, y) = (p[0] * w - w / 2.0, p[1] * h - h / 2.0);
        // Rotation inverse vers le repère de la source.
        let (sx, sy) = (co * x + s * y + w / 2.0, -s * x + co * y + h / 2.0);
        (-0.5..=w + 0.5).contains(&sx) && (-0.5..=h + 0.5).contains(&sy)
    })
}

/// Réduit le recadrage autour de son centre (en gardant ses proportions) jusqu'à ce
/// qu'il n'y ait plus de coin vide après redressement.
pub fn fit_crop(mut c: [f32; 4], angle_deg: f32, w: f32, h: f32) -> [f32; 4] {
    c[2] = c[2].clamp(0.02, 1.0);
    c[3] = c[3].clamp(0.02, 1.0);
    c[0] = c[0].clamp(0.0, 1.0 - c[2]);
    c[1] = c[1].clamp(0.0, 1.0 - c[3]);
    if crop_inside(c, angle_deg, w, h) {
        return c;
    }
    let center = [c[0] + c[2] / 2.0, c[1] + c[3] / 2.0];
    let at = |k: f32, center: [f32; 2]| [center[0] - c[2] * k / 2.0, center[1] - c[3] * k / 2.0, c[2] * k, c[3] * k];
    // Si même un recadrage minuscule déborde, on le recentre.
    let center = if crop_inside(at(0.02, center), angle_deg, w, h) { center } else { [0.5, 0.5] };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if crop_inside(at(mid, center), angle_deg, w, h) { lo = mid } else { hi = mid }
    }
    at(lo, center)
}

/// Interpolation de Fritsch-Carlson : passe par chaque point sans dépassement.
pub fn eval_curve(pts: &[[f32; 2]], x: f32) -> f32 {
    let n = pts.len();
    if n == 0 {
        return x;
    }
    if n == 1 || x <= pts[0][0] {
        return pts[0][1].clamp(0.0, 1.0);
    }
    if x >= pts[n - 1][0] {
        return pts[n - 1][1].clamp(0.0, 1.0);
    }
    let d: Vec<f32> = pts
        .windows(2)
        .map(|w| (w[1][1] - w[0][1]) / (w[1][0] - w[0][0]).max(1e-6))
        .collect();
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let a = m[i] / d[i];
            let b = m[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }
    let k = pts.windows(2).position(|w| x < w[1][0]).unwrap_or(n - 2);
    let (x0, y0) = (pts[k][0], pts[k][1]);
    let (x1, y1) = (pts[k + 1][0], pts[k + 1][1]);
    let h = (x1 - x0).max(1e-6);
    let t = (x - x0) / h;
    let (t2, t3) = (t * t, t * t * t);
    let y = (2.0 * t3 - 3.0 * t2 + 1.0) * y0
        + (t3 - 2.0 * t2 + t) * h * m[k]
        + (-2.0 * t3 + 3.0 * t2) * y1
        + (t3 - t2) * h * m[k + 1];
    y.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_round_trip() {
        let a = Adjustments { crop: [0.1, 0.2, 0.6, 0.5], angle: 7.0, ..Default::default() };
        let src = [6000.0, 4000.0];
        let s = a.out_to_src([0.3, 0.8], src, false);
        let o = a.src_to_out(s, src, false);
        assert!((o[0] - 0.3).abs() < 1e-4 && (o[1] - 0.8).abs() < 1e-4);
    }

    #[test]
    fn fit_crop_removes_empty_corners() {
        let c = fit_crop([0.0, 0.0, 1.0, 1.0], 10.0, 6000.0, 4000.0);
        assert!(crop_inside(c, 10.0, 6000.0, 4000.0));
        assert!(c[2] < 1.0 && c[2] > 0.6);
        assert!((c[2] - c[3]).abs() < 1e-3, "proportions gardées");
        let same = fit_crop([0.1, 0.1, 0.5, 0.5], 0.0, 6000.0, 4000.0);
        assert_eq!(same, [0.1, 0.1, 0.5, 0.5]);
    }

    #[test]
    fn layers_reach_the_shader_and_old_files_still_load() {
        let old: Adjustments = serde_json::from_str(r#"{"exposure":0.5}"#).unwrap();
        assert!(old.layers.is_empty());
        let mut a = Adjustments::default();
        a.layers.push(Layer { image: "/a.png".into(), opacity: 0.4, blend: Blend::Screen, mask: LayerMask::Background, ..Default::default() });
        a.layers.push(Layer { image: "/b.png".into(), visible: false, ..Default::default() });
        let back: Adjustments = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back.layers, a.layers);
        let u = a.uniforms([0.0, 0.0, 1.0, 1.0], false, false, [100.0, 100.0], false);
        assert_eq!(u.layers[0][..3], [0.4, 2.0, 3.0]);
        assert_eq!(u.layers[3][0], 0.0);
    }

    #[test]
    fn a_section_resets_only_its_own_settings() {
        let mut a = Adjustments { exposure: 1.0, temperature: 0.3, ..Default::default() };
        assert!(a.section_changed("Lumière") && a.section_changed("Couleur") && !a.section_changed("TSL"));
        a.reset_section("Lumière");
        assert_eq!((a.exposure, a.temperature), (0.0, 0.3));
        assert!(!a.section_changed("Lumière"));
    }

    #[test]
    fn identity_curve() {
        let a = Adjustments::default();
        for (i, v) in a.curve_lut().iter().enumerate() {
            assert!((v - i as f32 / 255.0).abs() < 1e-5);
        }
    }

    #[test]
    fn curve_passes_through_points_and_is_monotone() {
        let pts = vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.9], [1.0, 1.0]];
        assert!((eval_curve(&pts, 0.25) - 0.15).abs() < 1e-5);
        assert!((eval_curve(&pts, 0.75) - 0.9).abs() < 1e-5);
        let mut prev = -1.0;
        for i in 0..=1000 {
            let y = eval_curve(&pts, i as f32 / 1000.0);
            assert!(y >= prev - 1e-6);
            prev = y;
        }
    }
}
