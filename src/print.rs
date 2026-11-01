//! Impression : mise en page de la photo sur une feuille (format, orientation, marges, ajuster ou remplir),
//! signature, puis envoi à CUPS (`lp`) sous forme de PDF à la taille exacte du papier, ou enregistrement du PDF.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use image::{ImageBuffer, Rgb};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Paper {
    A3,
    A4,
    A5,
    Letter,
    Legal,
    /// 10 × 15 cm (4 × 6 pouces).
    P10x15,
    /// 13 × 18 cm (5 × 7 pouces).
    P13x18,
    /// 20 × 25 cm (8 × 10 pouces).
    P20x25,
}

impl Paper {
    pub const ALL: [Paper; 8] = [Paper::A4, Paper::A3, Paper::A5, Paper::Letter, Paper::Legal, Paper::P10x15, Paper::P13x18, Paper::P20x25];

    /// Largeur et hauteur en mm, en portrait.
    pub fn mm(self) -> (f32, f32) {
        match self {
            Paper::A3 => (297.0, 420.0),
            Paper::A4 => (210.0, 297.0),
            Paper::A5 => (148.0, 210.0),
            Paper::Letter => (215.9, 279.4),
            Paper::Legal => (215.9, 355.6),
            Paper::P10x15 => (101.6, 152.4),
            Paper::P13x18 => (127.0, 177.8),
            Paper::P20x25 => (203.2, 254.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Paper::A3 => "A3",
            Paper::A4 => "A4",
            Paper::A5 => "A5",
            Paper::Letter => "Letter",
            Paper::Legal => "Legal",
            Paper::P10x15 => "10×15",
            Paper::P13x18 => "13×18",
            Paper::P20x25 => "20×25",
        }
    }

    /// Nom du format pour CUPS (`-o media=`).
    fn cups(self) -> &'static str {
        match self {
            Paper::A3 => "A3",
            Paper::A4 => "A4",
            Paper::A5 => "A5",
            Paper::Letter => "Letter",
            Paper::Legal => "Legal",
            Paper::P10x15 => "4x6",
            Paper::P13x18 => "5x7",
            Paper::P20x25 => "8x10",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    /// Celle de la photo.
    Auto,
    Portrait,
    Landscape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    /// Toute la photo, avec des bandes blanches si les proportions diffèrent.
    Fit,
    /// Remplit la zone imprimable, quitte à rogner la photo.
    Fill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Draft,
    Normal,
    High,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// Imprimante CUPS ; vide : celle par défaut du système.
    pub printer: String,
    pub paper: Paper,
    pub orientation: Orientation,
    pub layout: Layout,
    /// Tourner la photo d'un quart de tour quand elle remplit mieux la feuille ainsi.
    pub rotate_to_fill: bool,
    /// Marge blanche autour de la photo, en mm.
    pub margin: f32,
    pub copies: u32,
    pub quality: Quality,
    pub gray: bool,
    /// Netteté d'impression.
    pub sharpen: bool,
    /// Résolution de la page envoyée, en points par pouce.
    pub ppi: u32,
    pub signature: crate::watermark::Watermark,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            printer: String::new(),
            paper: Paper::A4,
            orientation: Orientation::Auto,
            layout: Layout::Fit,
            rotate_to_fill: true,
            margin: 5.0,
            copies: 1,
            quality: Quality::Normal,
            gray: false,
            sharpen: true,
            ppi: 300,
            signature: Default::default(),
        }
    }
}

/// Place de la photo sur la feuille.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Feuille, en mm, dans le sens d'impression choisi.
    pub page: (f32, f32),
    /// Photo tournée d'un quart de tour (sens horaire).
    pub rotate: bool,
    /// Rectangle de la photo sur la feuille, en mm : x, y, largeur, hauteur.
    pub dest: [f32; 4],
    /// Partie de la photo (tournée) imprimée, en coordonnées 0..1 : x, y, largeur, hauteur.
    pub src: [f32; 4],
}

pub fn place(iw: u32, ih: u32, o: &Options) -> Placement {
    let (pw, ph) = o.paper.mm();
    let photo_wide = iw > ih;
    let landscape = match o.orientation {
        Orientation::Auto => photo_wide,
        Orientation::Portrait => false,
        Orientation::Landscape => true,
    };
    let page = if landscape { (ph, pw) } else { (pw, ph) };
    let rotate = o.rotate_to_fill && o.orientation != Orientation::Auto && iw != ih && photo_wide != landscape;
    let (iw, ih) = if rotate { (ih as f32, iw as f32) } else { (iw as f32, ih as f32) };
    let m = o.margin.clamp(0.0, page.0.min(page.1) / 2.0 - 1.0);
    let (aw, ah) = (page.0 - 2.0 * m, page.1 - 2.0 * m);
    match o.layout {
        Layout::Fit => {
            let k = (aw / iw).min(ah / ih);
            let (w, h) = (iw * k, ih * k);
            Placement { page, rotate, dest: [(page.0 - w) / 2.0, (page.1 - h) / 2.0, w, h], src: [0.0, 0.0, 1.0, 1.0] }
        }
        Layout::Fill => {
            let k = (aw / iw).max(ah / ih);
            let (sw, sh) = (aw / (iw * k), ah / (ih * k));
            Placement { page, rotate, dest: [m, m, aw, ah], src: [(1.0 - sw) / 2.0, (1.0 - sh) / 2.0, sw, sh] }
        }
    }
}

/// Feuille prête à imprimer (RGB sRGB 0..1, fond blanc), signature comprise : largeur, hauteur en pixels, valeurs.
/// `rgb` : photo développée, encodée sRGB.
pub fn compose(w: u32, h: u32, rgb: &[f32], o: &Options) -> (u32, u32, Vec<f32>) {
    let p = place(w, h, o);
    let mut photo = rgb.to_vec();
    let (mut w, mut h) = (w, h);
    if p.rotate {
        photo = rotate_cw(w, h, &photo);
        (w, h) = (h, w);
    }
    let ppmm = o.ppi.clamp(72, 1200) as f32 / 25.4;
    let px = |mm: f32| (mm * ppmm).round() as u32;
    let (pw, ph) = (px(p.page.0).max(1), px(p.page.1).max(1));
    let (dx, dy) = (px(p.dest[0]), px(p.dest[1]));
    let (dw, dh) = (px(p.dest[2]).clamp(1, pw), px(p.dest[3]).clamp(1, ph));
    let img = ImageBuffer::<Rgb<f32>, _>::from_raw(w, h, photo).expect("taille de la photo");
    let (sx, sy) = ((p.src[0] * w as f32) as u32, (p.src[1] * h as f32) as u32);
    let (sw, sh) = (((p.src[2] * w as f32).round() as u32).clamp(1, w - sx), ((p.src[3] * h as f32).round() as u32).clamp(1, h - sy));
    let crop = image::imageops::crop_imm(&img, sx, sy, sw, sh).to_image();
    let filter = if dw < sw { image::imageops::FilterType::Lanczos3 } else { image::imageops::FilterType::CatmullRom };
    let mut fitted = image::imageops::resize(&crop, dw, dh, filter).into_raw();
    if o.sharpen {
        crate::export::sharpen(dw, dh, &mut fitted, crate::export::Sharpen::Print);
    }
    // Signature sur la partie imprimée, droite sur la feuille : ni tournée avec la photo, ni rognée.
    crate::watermark::apply(&o.signature, dw, dh, &mut fitted);
    let mut page = vec![1.0f32; (pw * ph * 3) as usize];
    for y in 0..dh.min(ph.saturating_sub(dy)) {
        for x in 0..dw.min(pw.saturating_sub(dx)) {
            let (s, d) = (((y * dw + x) * 3) as usize, (((dy + y) * pw + dx + x) * 3) as usize);
            page[d..d + 3].copy_from_slice(&fitted[s..s + 3]);
        }
    }
    (pw, ph, page)
}

fn rotate_cw(w: u32, h: u32, rgb: &[f32]) -> Vec<f32> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0f32; rgb.len()];
    for y in 0..h {
        for x in 0..w {
            // (x, y) -> (h - 1 - y, x) dans une image de largeur h.
            let d = (x * h + (h - 1 - y)) * 3;
            out[d..d + 3].copy_from_slice(&rgb[(y * w + x) * 3..(y * w + x) * 3 + 3]);
        }
    }
    out
}

/// PDF d'une page à la taille `mm`, couverte par l'image `w`×`h` (JPEG intégré).
pub fn pdf(w: u32, h: u32, rgb: &[f32], mm: (f32, f32)) -> Result<Vec<u8>> {
    let data: Vec<u8> = rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode(&data, w, h, image::ExtendedColorType::Rgb8)?;
    let (pw, ph) = (mm.0 / 25.4 * 72.0, mm.1 / 25.4 * 72.0);
    let content = format!("q {pw:.2} 0 0 {ph:.2} 0 0 cm /Im0 Do Q");
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    let mut obj = |out: &mut Vec<u8>, head: String, stream: Option<&[u8]>| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{head}\n", offsets.len()).as_bytes());
        if let Some(s) = stream {
            out.extend_from_slice(b"stream\n");
            out.extend_from_slice(s);
            out.extend_from_slice(b"\nendstream\n");
        }
        out.extend_from_slice(b"endobj\n");
    };
    obj(&mut out, "<< /Type /Catalog /Pages 2 0 R >>".into(), None);
    obj(&mut out, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(), None);
    obj(&mut out, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>"), None);
    obj(&mut out, format!("<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>", jpeg.len()), Some(&jpeg));
    obj(&mut out, format!("<< /Length {} >>", content.len()), Some(content.as_bytes()));
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1).as_bytes());
    Ok(out)
}

/// Met la photo en page et écrit le PDF.
pub fn write_pdf(out: &Path, w: u32, h: u32, rgb: &[f32], o: &Options) -> Result<()> {
    let (pw, ph, page) = compose(w, h, rgb, o);
    let mm = place(w, h, o).page;
    let bytes = pdf(pw, ph, &page, mm)?;
    std::fs::File::create(out).and_then(|mut f| f.write_all(&bytes)).with_context(|| format!("écriture de {}", out.display()))
}

/// Arguments de `lp` pour imprimer `file`.
pub fn lp_args(o: &Options, file: &Path, title: &str) -> Vec<String> {
    let mut a = Vec::new();
    if !o.printer.trim().is_empty() {
        a.extend(["-d".to_string(), o.printer.trim().to_string()]);
    }
    a.extend(["-n".to_string(), o.copies.clamp(1, 99).to_string(), "-t".to_string(), title.to_string()]);
    let q = match o.quality {
        Quality::Draft => 3,
        Quality::Normal => 4,
        Quality::High => 5,
    };
    for opt in [
        format!("media={}", o.paper.cups()),
        format!("print-quality={q}"),
        format!("print-color-mode={}", if o.gray { "monochrome" } else { "color" }),
        // La page est déjà à la taille du papier : on la cale dans la zone imprimable de l'imprimante.
        "fit-to-page".to_string(),
    ] {
        a.extend(["-o".to_string(), opt]);
    }
    a.push(file.display().to_string());
    a
}

/// Imprimantes CUPS installées et celle par défaut.
pub fn printers() -> (Vec<String>, Option<String>) {
    let run = |args: &[&str]| Command::new("lpstat").args(args).env("LC_ALL", "C").output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let mut list: Vec<String> = run(&["-e"]).map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default();
    if list.is_empty() {
        list = run(&["-a"]).map(|s| s.lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect()).unwrap_or_default();
    }
    let default = run(&["-d"]).and_then(|s| s.split_once(':').map(|(_, n)| n.trim().to_string())).filter(|n| !n.is_empty() && !n.contains(' '));
    (list, default)
}

/// Met en page, écrit un PDF temporaire et l'envoie à l'imprimante. Renvoie la réponse de CUPS.
pub fn print(w: u32, h: u32, rgb: &[f32], o: &Options, title: &str) -> Result<String> {
    let file = std::env::temp_dir().join(format!("luminux-impression-{}.pdf", std::process::id()));
    write_pdf(&file, w, h, rgb, o)?;
    let out = Command::new("lp").args(lp_args(o, &file, title)).output();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            bail!("impossible de lancer « lp » (CUPS installé ?) : {e}");
        }
    };
    // CUPS a copié le fichier dans sa file d'attente.
    let _ = std::fs::remove_file(&file);
    let msg = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string();
    if !out.status.success() {
        bail!("{msg}");
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn fit_keeps_the_whole_photo_centered() {
        let o = Options { margin: 10.0, ..Default::default() };
        // Photo 3:2 en paysage, orientation auto : feuille A4 en paysage.
        let p = place(3000, 2000, &o);
        assert_eq!(p.page, (297.0, 210.0));
        assert!(!p.rotate);
        assert_eq!(p.src, [0.0, 0.0, 1.0, 1.0]);
        // Largeur limitante : 277 mm sur 184,7 mm, centrée en hauteur.
        assert!(close(p.dest[2], 277.0) && close(p.dest[3], 184.667) && close(p.dest[0], 10.0) && close(p.dest[1], 12.667), "{:?}", p.dest);
    }

    #[test]
    fn fill_crops_and_rotate_turns_the_photo() {
        let o = Options { margin: 0.0, layout: Layout::Fill, orientation: Orientation::Portrait, rotate_to_fill: false, ..Default::default() };
        let p = place(3000, 2000, &o);
        assert_eq!(p.dest, [0.0, 0.0, 210.0, 297.0]);
        // Photo paysage sur feuille portrait : on n'en garde qu'une bande verticale centrée.
        assert!(close(p.src[3], 1.0) && p.src[2] < 0.5 && close(p.src[0] * 2.0 + p.src[2], 1.0), "{:?}", p.src);
        let p = place(3000, 2000, &Options { rotate_to_fill: true, ..o });
        assert!(p.rotate);
        assert!(p.src[2] > 0.9, "{:?}", p.src);
    }

    #[test]
    fn page_has_paper_size_and_white_margins() {
        let (w, h) = (300u32, 200u32);
        let rgb = vec![0.2f32; (w * h * 3) as usize];
        let o = Options { paper: Paper::P10x15, ppi: 100, margin: 10.0, sharpen: false, ..Default::default() };
        let (pw, ph, page) = compose(w, h, &rgb, &o);
        // 152,4 × 101,6 mm à 100 ppp.
        assert_eq!((pw, ph), (600, 400));
        assert_eq!(&page[0..3], &[1.0, 1.0, 1.0]);
        let mid = ((ph / 2 * pw + pw / 2) * 3) as usize;
        assert!((page[mid] - 0.2).abs() < 0.01);
        // Sans marge ni bandes : la photo touche le bord gauche de la feuille.
        let (_, _, full) = compose(w, h, &rgb, &Options { margin: 0.0, ..o.clone() });
        let left = ((ph / 2 * pw) * 3) as usize;
        assert!((full[left] - 0.2).abs() < 0.01);
        let pdf = pdf(pw, ph, &page, place(w, h, &o).page).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
        assert!(String::from_utf8_lossy(&pdf).contains("/MediaBox [0 0 432.00 288.00]"));
    }

    #[test]
    fn signature_stays_upright_and_inside_the_print() {
        // Photo paysage tournée sur une feuille portrait remplie : la signature reste en haut à gauche de la feuille.
        let (w, h) = (300u32, 200u32);
        let rgb = vec![0.0f32; (w * h * 3) as usize];
        let signature = crate::watermark::Watermark { kind: crate::watermark::Kind::Text, text: "Signature".into(), opacity: 1.0, place: crate::watermark::Place::TopLeft, ..Default::default() };
        let o = Options { paper: Paper::P10x15, ppi: 100, margin: 0.0, sharpen: false, layout: Layout::Fill, orientation: Orientation::Portrait, signature, ..Default::default() };
        assert!(place(w, h, &o).rotate);
        let (pw, ph, page) = compose(w, h, &rgb, &o);
        let lit: Vec<(u32, u32)> = (0..pw * ph).filter(|i| page[*i as usize * 3] > 0.5).map(|i| (i % pw, i / pw)).collect();
        assert!(!lit.is_empty());
        let (maxx, maxy) = (lit.iter().map(|p| p.0).max().unwrap(), lit.iter().map(|p| p.1).max().unwrap());
        let (minx, miny) = (lit.iter().map(|p| p.0).min().unwrap(), lit.iter().map(|p| p.1).min().unwrap());
        // Plus large que haute (texte droit), dans le quart haut gauche.
        assert!(maxx < pw / 2 && maxy < ph / 4, "{maxx} {maxy}");
        assert!(maxx - minx > 2 * (maxy - miny), "{minx}..{maxx} {miny}..{maxy}");
    }

    #[test]
    fn rotation_is_clockwise() {
        // 2×1 : rouge à gauche, bleu à droite -> 1×2 : rouge en haut.
        let r = rotate_cw(2, 1, &[1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        assert_eq!(r, vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        // 1×2 : rouge en haut, bleu en bas -> 2×1 : bleu à gauche.
        let r = rotate_cw(1, 2, &[1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        assert_eq!(r, vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn lp_command() {
        let o = Options { printer: "HP_DeskJet".into(), copies: 2, gray: true, quality: Quality::High, paper: Paper::P10x15, ..Default::default() };
        let a = lp_args(&o, Path::new("/tmp/x.pdf"), "photo");
        assert_eq!(a[..6], ["-d", "HP_DeskJet", "-n", "2", "-t", "photo"]);
        assert!(a.contains(&"media=4x6".to_string()) && a.contains(&"print-color-mode=monochrome".to_string()) && a.contains(&"print-quality=5".to_string()));
        assert_eq!(a.last().unwrap(), "/tmp/x.pdf");
        assert!(!lp_args(&Options::default(), Path::new("f"), "t").contains(&"-d".to_string()));
    }
}
