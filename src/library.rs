//! Bibliothèque : catalogue SQLite des dossiers suivis, scan en tâche de fond,
//! métadonnées EXIF et vignettes en cache.

use crate::adjust::Adjustments;
use crate::app::{load_sidecar, sidecar_path};
use anyhow::{Context, Result};
use eframe::{egui, wgpu};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader};
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::UNIX_EPOCH;

pub const RAW_EXTS: &[&str] = &[
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "dng", "srw", "3fr", "iiq", "mos",
];
pub const IMAGE_EXTS: &[&str] = &[
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "dng", "srw", "3fr", "iiq", "mos",
    "jpg", "jpeg", "png", "tif", "tiff", "webp",
];
pub const THUMB_SIZE: u32 = 360;

// ---------- Copies virtuelles ----------
// Une copie virtuelle n'a pas de fichier image : c'est le chemin de la photo suivi de « #n » (n ≥ 2),
// avec ses propres réglages (`photo.jpg#2.luminux`), sa vignette et sa ligne du catalogue.

/// Numéro de la copie virtuelle, `None` pour une vraie photo.
pub fn copy_number(p: &Path) -> Option<u32> {
    let name = p.file_name()?.to_str()?;
    let (base, n) = name.rsplit_once('#')?;
    if base.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// Fichier image d'une photo ou d'une copie virtuelle.
pub fn source(p: &Path) -> PathBuf {
    match copy_number(p) {
        Some(_) => {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            p.with_file_name(name.rsplit_once('#').map_or(name, |(b, _)| b))
        }
        None => p.to_path_buf(),
    }
}

pub fn virtual_copy(p: &Path, n: u32) -> PathBuf {
    let src = source(p);
    let mut s = src.into_os_string();
    s.push(format!("#{n}"));
    PathBuf::from(s)
}

/// Montre le fichier dans le gestionnaire de fichiers (sélectionné si possible), sinon ouvre son dossier.
pub fn show_in_folder(p: &Path) {
    let file = source(p);
    std::thread::spawn(move || {
        let mut uri = String::from("file://");
        for b in file.as_os_str().as_encoded_bytes() {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(b) {
                uri.push(*b as char);
            } else {
                uri.push_str(&format!("%{b:02X}"));
            }
        }
        let shown = std::process::Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.FileManager1",
                "--object-path",
                "/org/freedesktop/FileManager1",
                "--method",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("['{uri}']"),
                "",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !shown {
            if let Some(dir) = file.parent() {
                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
            }
        }
    });
}

/// Nom de base d'un export : « IMG_0002 », ou « IMG_0002_2 » pour la copie virtuelle 2.
pub fn export_stem(p: &Path) -> String {
    let stem = source(p).file_stem().and_then(|s| s.to_str()).unwrap_or("photo").to_string();
    match copy_number(p) {
        Some(n) => format!("{stem}_{n}"),
        None => stem,
    }
}

fn ext_of(p: &Path) -> String {
    source(p)
        .extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_image(p: &Path) -> bool {
    IMAGE_EXTS.contains(&ext_of(p).as_str())
}

pub fn is_raw(p: &Path) -> bool {
    RAW_EXTS.contains(&ext_of(p).as_str())
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(source(p))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Une photo modifiée = un fichier .luminux non neutre à côté.
pub fn is_edited(p: &Path) -> bool {
    load_sidecar(p).is_some_and(|a| a != Adjustments::default())
}

#[derive(Clone, Debug)]
pub struct Photo {
    pub path: PathBuf,
    pub name: String,
    pub taken: i64,
    pub camera: String,
    pub rating: u8,
    pub edited: bool,
    /// Netteté mesurée (-1 : pas encore analysée) et empreinte d'image, voir `cull`.
    pub sharp: f32,
    pub hash: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    DateDesc,
    DateAsc,
    Name,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Edited,
    MinRating(u8),
    /// Photos probablement floues, les moins nettes d'abord.
    Blurry,
    /// Doublons et rafales, groupe par groupe, la plus nette en tête.
    Similar,
}

pub struct Catalogue {
    conn: Connection,
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("luminux")
}

impl Catalogue {
    pub fn open() -> Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        Self::with(Connection::open(dir.join("catalogue.db")).context("ouverture du catalogue")?)
    }

    fn with(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS folders (path TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS photos (
                 path   TEXT PRIMARY KEY,
                 folder TEXT NOT NULL,
                 name   TEXT NOT NULL,
                 mtime  INTEGER NOT NULL,
                 taken  INTEGER NOT NULL,
                 camera TEXT NOT NULL DEFAULT '',
                 rating INTEGER NOT NULL DEFAULT 0,
                 edited INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS photos_folder ON photos(folder);
             CREATE TABLE IF NOT EXISTS keywords (
                 path TEXT NOT NULL,
                 word TEXT NOT NULL COLLATE NOCASE,
                 PRIMARY KEY (path, word)
             );",
        )?;
        // Colonnes ajoutées en 0.17 (erreur ignorée si elles existent déjà).
        for col in ["sharp REAL NOT NULL DEFAULT -1", "dhash INTEGER NOT NULL DEFAULT 0", "cull_mtime INTEGER NOT NULL DEFAULT 0"] {
            let _ = conn.execute(&format!("ALTER TABLE photos ADD COLUMN {col}"), []);
        }
        Ok(Self { conn })
    }

    pub fn folders(&self) -> Vec<String> {
        let mut st = self.conn.prepare("SELECT path FROM folders ORDER BY path").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Sous-dossiers d'un dossier de la bibliothèque qui contiennent des photos, avec le nombre de
    /// photos de chacun (sous-dossiers compris) ; le dossier lui-même en premier.
    pub fn subfolders(&self, root: &str) -> Vec<(String, usize)> {
        let mut st = self.conn.prepare("SELECT path FROM photos WHERE folder = ?1").unwrap();
        let paths: Vec<String> = st.query_map([root], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect();
        let root = root.trim_end_matches('/');
        let mut count: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        count.insert(root.to_string(), 0);
        for p in &paths {
            let mut d = Path::new(p).parent();
            while let Some(dir) = d {
                let k = dir.to_string_lossy();
                if !k.starts_with(root) {
                    break;
                }
                *count.entry(k.to_string()).or_default() += 1;
                if k.len() <= root.len() {
                    break;
                }
                d = dir.parent();
            }
        }
        count.into_iter().collect()
    }

    pub fn add_folder(&self, path: &str) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO folders(path) VALUES (?1)", [path])?;
        Ok(())
    }

    /// Retire le dossier du catalogue (les fichiers ne sont pas touchés).
    pub fn remove_folder(&self, path: &str) -> Result<()> {
        self.conn.execute("DELETE FROM folders WHERE path = ?1", [path])?;
        self.conn.execute("DELETE FROM photos WHERE folder = ?1", [path])?;
        Ok(())
    }

    pub fn list(&self, folder: Option<&str>, filter: Filter, sort: Sort, keyword: Option<&str>) -> Vec<Photo> {
        let order = match sort {
            Sort::DateDesc => "taken DESC, name",
            Sort::DateAsc => "taken ASC, name",
            Sort::Name => "name COLLATE NOCASE, taken",
        };
        let (edited, min) = match filter {
            Filter::Edited => (true, 0),
            Filter::MinRating(r) => (false, r),
            _ => (false, 0),
        };
        let blurry = filter == Filter::Blurry;
        let order = if blurry { "sharp ASC, name" } else { order };
        let sql = format!(
            "SELECT path, name, taken, camera, rating, edited, sharp, dhash FROM photos
             WHERE (?1 IS NULL OR substr(path, 1, length(?5)) = ?5) AND rating >= ?2 AND (?3 = 0 OR edited = 1)
               AND (?4 IS NULL OR path IN (SELECT path FROM keywords WHERE word = ?4))
               AND (?6 = 0 OR (sharp >= 0 AND sharp < ?7))
             ORDER BY {order}"
        );
        let mut st = self.conn.prepare(&sql).unwrap();
        let map = |r: &rusqlite::Row| {
            Ok(Photo {
                path: PathBuf::from(r.get::<_, String>(0)?),
                name: r.get(1)?,
                taken: r.get(2)?,
                camera: r.get(3)?,
                rating: r.get::<_, i64>(4)? as u8,
                edited: r.get::<_, i64>(5)? != 0,
                sharp: r.get::<_, f64>(6)? as f32,
                hash: r.get::<_, i64>(7)? as u64,
            })
        };
        // Le dossier et tous ses sous-dossiers.
        let prefix = folder.map(|f| format!("{}/", f.trim_end_matches('/')));
        let list: Vec<Photo> = st.query_map(params![folder, min, edited, keyword, prefix, blurry, crate::cull::BLUR_LIMIT], map).unwrap().filter_map(|r| r.ok()).collect();
        if filter != Filter::Similar {
            return list;
        }
        // Copies virtuelles écartées : elles ont la même image que leur photo.
        let list: Vec<Photo> = list.into_iter().filter(|p| p.sharp >= 0.0 && copy_number(&p.path).is_none()).collect();
        let hashes: Vec<u64> = list.iter().map(|p| p.hash).collect();
        let mut groups = crate::cull::groups(&hashes);
        for g in &mut groups {
            g.sort_by(|a, b| list[*b].sharp.total_cmp(&list[*a].sharp));
        }
        groups.sort_by_key(|g| g.iter().map(|i| list[*i].taken).min());
        let mut slots: Vec<Option<Photo>> = list.into_iter().map(Some).collect();
        groups.into_iter().flatten().filter_map(|i| slots[i].take()).collect()
    }

    /// Photos à analyser pour le tri (jamais analysées, ou modifiées depuis), copies virtuelles exclues.
    pub fn to_cull(&self) -> Vec<PathBuf> {
        let mut st = self.conn.prepare("SELECT path FROM photos WHERE cull_mtime != mtime").unwrap();
        st.query_map([], |r| r.get::<_, String>(0)).unwrap().filter_map(|r| r.ok()).map(PathBuf::from).filter(|p| copy_number(p).is_none()).collect()
    }

    pub fn set_cull(&self, path: &Path, sharp: f32, hash: u64) {
        let _ = self.conn.execute(
            "UPDATE photos SET sharp = ?2, dhash = ?3, cull_mtime = mtime WHERE path = ?1",
            params![path.to_string_lossy(), sharp as f64, hash as i64],
        );
    }

    /// Mots-clés de toutes les photos, par chemin (pour la recherche).
    pub fn keyword_map(&self) -> std::collections::HashMap<PathBuf, Vec<String>> {
        let mut st = self.conn.prepare("SELECT path, word FROM keywords").unwrap();
        let mut map: std::collections::HashMap<PathBuf, Vec<String>> = Default::default();
        for (p, w) in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap().filter_map(|r| r.ok()) {
            map.entry(PathBuf::from(p)).or_default().push(w);
        }
        map
    }

    pub fn set_rating(&self, path: &Path, rating: u8) {
        let _ = self.conn.execute("UPDATE photos SET rating = ?2 WHERE path = ?1", params![path.to_string_lossy(), rating]);
    }

    pub fn rating(&self, path: &Path) -> u8 {
        self.conn
            .query_row("SELECT rating FROM photos WHERE path = ?1", [path.to_string_lossy()], |r| r.get::<_, i64>(0))
            .map(|v| v as u8)
            .unwrap_or(0)
    }

    /// Crée une copie virtuelle de `p` (photo ou copie) avec ses réglages actuels ; renvoie son chemin.
    pub fn create_copy(&self, p: &Path) -> Result<PathBuf> {
        let src = source(p);
        let mut n = 2;
        let copy = loop {
            let c = virtual_copy(&src, n);
            let known: bool = self.conn.query_row("SELECT COUNT(*) > 0 FROM photos WHERE path = ?1", [c.to_string_lossy()], |r| r.get(0))?;
            if !known && !sidecar_path(&c).exists() {
                break c;
            }
            n += 1;
        };
        let adj = load_sidecar(p).unwrap_or_default();
        crate::app::save_sidecar(&copy, &adj).context("écriture des réglages de la copie")?;
        let added = self.conn.execute(
            "INSERT INTO photos(path, folder, name, mtime, taken, camera, rating, edited)
             SELECT ?1, folder, ?2, mtime, taken, camera, rating, ?3 FROM photos WHERE path = ?4",
            params![
                copy.to_string_lossy(),
                format!("{} ({n})", src.file_name().unwrap_or_default().to_string_lossy()),
                adj != Adjustments::default(),
                p.to_string_lossy()
            ],
        )?;
        anyhow::ensure!(added == 1, "photo absente du catalogue");
        self.conn.execute("INSERT OR IGNORE INTO keywords(path, word) SELECT ?1, word FROM keywords WHERE path = ?2", params![copy.to_string_lossy(), p.to_string_lossy()])?;
        Ok(copy)
    }

    /// Supprime une copie virtuelle (ligne, mots-clés, réglages, vignette). Sans effet sur une vraie photo.
    pub fn delete_copy(&self, p: &Path) -> Result<()> {
        anyhow::ensure!(copy_number(p).is_some(), "ce n'est pas une copie virtuelle");
        let key = p.to_string_lossy();
        self.conn.execute("DELETE FROM photos WHERE path = ?1", [&key])?;
        self.conn.execute("DELETE FROM keywords WHERE path = ?1", [&key])?;
        let _ = std::fs::remove_file(sidecar_path(p));
        let _ = std::fs::remove_file(thumb_path(p));
        Ok(())
    }

    /// Met une photo à la corbeille avec ses réglages, et l'enlève du catalogue avec ses copies
    /// virtuelles (lignes, mots-clés, réglages, vignettes). Une copie virtuelle seule est supprimée.
    pub fn trash_photo(&self, p: &Path) -> Result<()> {
        if copy_number(p).is_some() {
            return self.delete_copy(p);
        }
        trash(p)?;
        let sc = sidecar_path(p);
        if sc.exists() {
            let _ = trash(&sc);
        }
        self.forget_photo(p)
    }

    /// Enlève une photo du catalogue avec ses copies virtuelles (lignes, mots-clés, réglages des
    /// copies, vignettes), sans toucher au fichier image.
    fn forget_photo(&self, p: &Path) -> Result<()> {
        let key = p.to_string_lossy();
        let copies: Vec<String> = {
            let mut st = self.conn.prepare("SELECT path FROM photos WHERE path = ?1 OR (path > ?2 AND path < ?3)")?;
            // Copies : « photo#n », toutes entre « photo# » et « photo$ ».
            st.query_map(params![key, format!("{key}#"), format!("{key}$")], |r| r.get(0))?.filter_map(|r| r.ok()).collect()
        };
        for c in &copies {
            let c = Path::new(c);
            if c != p && copy_number(c).is_none() {
                continue;
            }
            self.conn.execute("DELETE FROM photos WHERE path = ?1", [c.to_string_lossy()])?;
            self.conn.execute("DELETE FROM keywords WHERE path = ?1", [c.to_string_lossy()])?;
            if c != p {
                let _ = std::fs::remove_file(sidecar_path(c));
            }
            let _ = std::fs::remove_file(thumb_path(c));
        }
        Ok(())
    }

    pub fn keywords(&self, path: &Path) -> Vec<String> {
        let mut st = self.conn.prepare("SELECT word FROM keywords WHERE path = ?1 ORDER BY word").unwrap();
        st.query_map([path.to_string_lossy()], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Tous les mots-clés du catalogue, avec le nombre de photos.
    pub fn all_keywords(&self) -> Vec<(String, usize)> {
        let mut st = self.conn.prepare("SELECT word, COUNT(*) FROM keywords GROUP BY word ORDER BY word").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize))).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn add_keyword(&self, path: &Path, word: &str) {
        let _ = self.conn.execute("INSERT OR IGNORE INTO keywords(path, word) VALUES (?1, ?2)", params![path.to_string_lossy(), word]);
    }

    pub fn remove_keyword(&self, path: &Path, word: &str) {
        let _ = self.conn.execute("DELETE FROM keywords WHERE path = ?1 AND word = ?2", params![path.to_string_lossy(), word]);
    }

    pub fn set_edited(&self, path: &Path, edited: bool) {
        let _ = self.conn.execute("UPDATE photos SET edited = ?2 WHERE path = ?1", params![path.to_string_lossy(), edited]);
    }
}

pub enum ScanMsg {
    Progress(usize),
    Done(usize),
    Error(String),
}

/// Parcourt les dossiers, ajoute les nouvelles photos, met à jour les modifiées,
/// retire celles qui ont disparu. Tourne dans son propre thread.
pub fn scan(folders: Vec<String>, tx: Sender<ScanMsg>, ctx: egui::Context) {
    let run = || -> Result<usize> {
        let cat = Catalogue::open()?;
        let mut seen = 0;
        for folder in &folders {
            // Dossier absent (disque ou partage réseau non monté) : ses photos restent au catalogue.
            if !Path::new(folder).is_dir() {
                continue;
            }
            let known: HashMap<String, i64> = {
                let mut st = cat.conn.prepare("SELECT path, mtime FROM photos WHERE folder = ?1")?;
                st.query_map([folder], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect()
            };
            let mut present = HashSet::new();
            let tx_batch = cat.conn.unchecked_transaction()?;
            for entry in walkdir::WalkDir::new(folder).follow_links(true).into_iter().filter_map(|e| e.ok()) {
                let p = entry.path();
                if !entry.file_type().is_file() || !is_image(p) {
                    continue;
                }
                let key = p.to_string_lossy().to_string();
                present.insert(key.clone());
                let mt = mtime(p);
                if known.get(&key) == Some(&mt) {
                    cat.set_edited(p, is_edited(p));
                } else {
                    let (taken, camera) = read_meta(p);
                    tx_batch.execute(
                        "INSERT INTO photos(path, folder, name, mtime, taken, camera, edited)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT(path) DO UPDATE SET mtime = ?4, taken = ?5, camera = ?6, edited = ?7",
                        params![
                            key,
                            folder,
                            p.file_name().unwrap_or_default().to_string_lossy(),
                            mt,
                            taken.unwrap_or(mt),
                            camera,
                            is_edited(p)
                        ],
                    )?;
                }
                seen += 1;
                if seen % 25 == 0 {
                    let _ = tx.send(ScanMsg::Progress(seen));
                    ctx.request_repaint();
                }
            }
            // Une copie virtuelle reste tant que sa photo est là.
            let gone = |k: &String| {
                let src = source(Path::new(k)).to_string_lossy().to_string();
                !present.contains(&src)
            };
            for k in known.keys().filter(|k| gone(k)) {
                tx_batch.execute("DELETE FROM photos WHERE path = ?1", [k])?;
                tx_batch.execute("DELETE FROM keywords WHERE path = ?1", [k])?;
            }
            for k in known.keys().filter(|k| copy_number(Path::new(k.as_str())).is_some() && !gone(k)) {
                cat.set_edited(Path::new(k.as_str()), is_edited(Path::new(k.as_str())));
            }
            tx_batch.commit()?;
        }
        Ok(seen)
    };
    let _ = tx.send(match run() {
        Ok(n) => ScanMsg::Done(n),
        Err(e) => ScanMsg::Error(format!("{e:#}")),
    });
    ctx.request_repaint();
}

/// Date de prise de vue (secondes Unix, heure locale traitée comme UTC) et appareil.
fn read_meta(p: &Path) -> (Option<i64>, String) {
    let p = &source(p);
    if is_raw(p) {
        let meta = (|| {
            let src = rawler::rawsource::RawSource::new(p).ok()?;
            let dec = rawler::get_decoder(&src).ok()?;
            dec.raw_metadata(&src, &Default::default()).ok()
        })();
        if let Some(m) = meta {
            let date = m.exif.date_time_original.or(m.exif.create_date);
            return (date.as_deref().and_then(parse_exif_date), format!("{} {}", m.make, m.model).trim().to_string());
        }
        return (None, String::new());
    }
    let exif = std::fs::File::open(p)
        .ok()
        .and_then(|f| exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)).ok());
    let Some(exif) = exif else { return (None, String::new()) };
    let field = |t| exif.get_field(t, exif::In::PRIMARY).map(|f| f.display_value().to_string().trim_matches('"').to_string());
    let date = field(exif::Tag::DateTimeOriginal).and_then(|d| parse_exif_date(&d.replace('-', ":")));
    (date, field(exif::Tag::Model).unwrap_or_default())
}

/// Détails d'un fichier pour le panneau Informations de la bibliothèque.
#[derive(Clone, Debug, Default)]
pub struct Details {
    pub bytes: u64,
    /// Largeur et hauteur (lues dans l'en-tête ; inconnues pour les RAW).
    pub size: Option<(u32, u32)>,
    pub lens: Option<String>,
    /// « 1/250 s · f/8 · ISO 200 · 35 mm » (ce qui est connu).
    pub exposure: String,
}

/// Lit la taille du fichier et les EXIF utiles (sans décoder l'image).
pub fn details(p: &Path) -> Details {
    let p = &source(p);
    let bytes = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let shot = crate::lens::read_shot(p);
    let (mut time, mut iso) = (None, None);
    let mut size = None;
    if is_raw(p) {
        let meta = (|| {
            let src = rawler::rawsource::RawSource::new(p).ok()?;
            let dec = rawler::get_decoder(&src).ok()?;
            dec.raw_metadata(&src, &Default::default()).ok()
        })();
        if let Some(m) = meta {
            time = m.exif.exposure_time.filter(|r| r.d != 0 && r.n != 0).map(|r| r.n as f64 / r.d as f64);
            iso = m.exif.iso_speed_ratings.map(u32::from).or(m.exif.iso_speed);
        }
    } else {
        size = image::image_dimensions(p).ok();
        if let Some(exif) = std::fs::File::open(p).ok().and_then(|f| exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)).ok()) {
            let field = |t| exif.get_field(t, exif::In::PRIMARY);
            time = field(exif::Tag::ExposureTime).and_then(|f| match &f.value {
                exif::Value::Rational(v) => v.first().filter(|r| r.denom != 0 && r.num != 0).map(|r| r.num as f64 / r.denom as f64),
                _ => None,
            });
            iso = field(exif::Tag::PhotographicSensitivity).and_then(|f| f.value.get_uint(0));
        }
    }
    let mut parts = Vec::new();
    if let Some(t) = time {
        parts.push(if t < 0.5 { format!("1/{} s", (1.0 / t).round()) } else { format!("{} s", (t * 10.0).round() / 10.0) });
    }
    if let Some(a) = shot.aperture {
        parts.push(format!("f/{}", (a * 10.0).round() / 10.0));
    }
    if let Some(i) = iso {
        parts.push(format!("ISO {i}"));
    }
    if let Some(f) = shot.focal {
        parts.push(format!("{} mm", f.round()));
    }
    Details { bytes, size, lens: shot.lens, exposure: parts.join(" · ") }
}

/// Taille de fichier lisible (« 24,3 Mo »).
pub fn format_bytes(b: u64) -> String {
    let n = |v: f64| format!("{v:.1}").replace('.', t!(",|séparateur décimal"));
    match b {
        b if b >= 1 << 30 => tf!("{0} Go", n(b as f64 / (1u64 << 30) as f64)),
        b if b >= 1 << 20 => tf!("{0} Mo", n(b as f64 / (1u64 << 20) as f64)),
        b => tf!("{0} ko", (b as f64 / 1024.0).ceil()),
    }
}

/// « 2023:05:14 10:22:33 » → secondes depuis 1970.
pub fn parse_exif_date(s: &str) -> Option<i64> {
    let n: Vec<i64> = s.split(|c: char| !c.is_ascii_digit()).filter(|x| !x.is_empty()).filter_map(|x| x.parse().ok()).collect();
    if n.len() < 3 || n[0] < 1900 {
        return None;
    }
    let (y, m, d) = (n[0], n[1], n[2]);
    let (hh, mm, ss) = (*n.get(3).unwrap_or(&0), *n.get(4).unwrap_or(&0), *n.get(5).unwrap_or(&0));
    Some(days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Secondes Unix → « 14/05/2023 ».
pub fn format_date(ts: i64) -> String {
    let z = ts.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{d:02}/{m:02}/{y}")
}

// ---------- Recherche ----------

/// Minuscules sans accents, pour comparer « Été » et « ete ».
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

const MONTHS: [[&str; 2]; 12] = [
    ["janvier", "january"],
    ["fevrier", "february"],
    ["mars", "march"],
    ["avril", "april"],
    ["mai", "may"],
    ["juin", "june"],
    ["juillet", "july"],
    ["aout", "august"],
    ["septembre", "september"],
    ["octobre", "october"],
    ["novembre", "november"],
    ["decembre", "december"],
];

/// Vrai si la photo répond à chaque mot de `query` : année (2024), mois (juillet), date (14/07/2024 ou
/// 07/2024), sinon un morceau du nom, de l'appareil, d'un mot-clé ou du dossier.
pub fn matches(query: &str, photo: &Photo, keywords: &[String]) -> bool {
    let date = format_date(photo.taken);
    let (d, m, y) = (&date[0..2], &date[3..5], &date[6..]);
    let text = fold(&format!("{} {} {} {}", photo.name, photo.camera, keywords.join(" "), photo.path.parent().map(|p| p.to_string_lossy()).unwrap_or_default()));
    fold(query).split_whitespace().all(|tok| {
        let digits = tok.chars().all(|c| c.is_ascii_digit() || c == '/');
        if digits && tok.contains('/') {
            let parts: Vec<&str> = tok.split('/').collect();
            let pad = |v: &str| format!("{v:0>2}");
            return match parts.as_slice() {
                [dd, mm, yy] => pad(dd) == d && pad(mm) == m && *yy == y,
                [mm, yy] if yy.len() == 4 => pad(mm) == m && *yy == y,
                [dd, mm] => pad(dd) == d && pad(mm) == m,
                _ => false,
            };
        }
        if tok.len() == 4 && digits && (tok.starts_with("19") || tok.starts_with("20")) {
            return tok == y || text.contains(tok);
        }
        if let Some(k) = MONTHS.iter().position(|names| names.contains(&tok)) {
            return m == format!("{:02}", k + 1);
        }
        text.contains(tok)
    })
}

// ---------- Corbeille ----------

/// Corbeille de l'utilisateur (freedesktop) : $XDG_DATA_HOME/Trash.
fn trash_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("Trash")
}

/// Met un fichier à la corbeille du bureau (spécification freedesktop.org), d'où le gestionnaire de
/// fichiers peut le restaurer. Sur un autre disque (NAS, clé), le fichier y est copié puis effacé.
pub fn trash(p: &Path) -> Result<()> {
    trash_into(&trash_dir(), p)
}

fn trash_into(dir: &Path, p: &Path) -> Result<()> {
    let p = std::path::absolute(p)?;
    anyhow::ensure!(p.is_file(), "{} introuvable", p.display());
    let (files, info) = (dir.join("files"), dir.join("info"));
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;
    let name = p.file_name().context("nom de fichier")?.to_string_lossy().into_owned();
    // Nom libre dans la corbeille : « photo.jpg », « photo.2.jpg »…
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut n = 1;
    let (dest, info_file) = loop {
        let cand = if n == 1 { name.clone() } else { format!("{stem}.{n}{ext}") };
        let inf = info.join(format!("{cand}.trashinfo"));
        // Le .trashinfo créé en exclusif réserve le nom.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&inf) {
            Ok(mut f) => {
                use std::io::Write;
                let mut path = String::new();
                for b in p.as_os_str().as_encoded_bytes() {
                    if b.is_ascii_alphanumeric() || b"/-_.~".contains(b) {
                        path.push(*b as char);
                    } else {
                        path.push_str(&format!("%{b:02X}"));
                    }
                }
                write!(f, "[Trash Info]\nPath={path}\nDeletionDate={}\n", local_now())?;
                break (files.join(&cand), inf);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e.into()),
        }
    };
    let moved = std::fs::rename(&p, &dest).or_else(|_| {
        // Autre système de fichiers : copie puis effacement.
        std::fs::copy(&p, &dest).and_then(|_| std::fs::remove_file(&p)).inspect_err(|_| {
            let _ = std::fs::remove_file(&dest);
        })
    });
    if let Err(e) = moved {
        let _ = std::fs::remove_file(&info_file);
        return Err(anyhow::Error::from(e).context(format!("mise à la corbeille de {}", p.display())));
    }
    Ok(())
}

/// Date et heure locales au format AAAA-MM-JJTHH:MM:SS (heure locale via `date`, UTC à défaut).
fn local_now() -> String {
    if let Ok(o) = std::process::Command::new("date").arg("+%Y-%m-%dT%H:%M:%S").output() {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && s.len() == 19 {
            return s;
        }
    }
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, sec) = (t.div_euclid(86400), t.rem_euclid(86400));
    // Jours depuis 1970 -> date civile (algorithme de H. Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", sec / 3600, sec / 60 % 60, sec % 60)
}

// ---------- Vignettes ----------

pub fn thumb_path(photo: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    photo.hash(&mut h);
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("luminux/vignettes")
        .join(format!("{:016x}.jpg", h.finish()))
}

/// Écrit une vignette déjà développée (RGB encodé, 0..1).
pub fn save_thumb(photo: &Path, w: u32, h: u32, rgba: &[[f32; 4]]) -> Result<()> {
    let tp = thumb_path(photo);
    std::fs::create_dir_all(tp.parent().unwrap())?;
    crate::image_io::export(&tp, w, h, rgba)
}

/// Aperçu d'au plus `size` px (aperçu intégré pour un RAW).
pub fn decode_preview(p: &Path, size: u32) -> Result<DynamicImage> {
    let p = &source(p);
    if is_raw(p) {
        let src = rawler::rawsource::RawSource::new(p)?;
        let dec = rawler::get_decoder(&src)?;
        let params = Default::default();
        let orient = dec.raw_metadata(&src, &params).ok().and_then(|m| m.exif.orientation);
        let img = match dec.preview_image(&src, &params).ok().flatten() {
            Some(i) => i,
            None => dec.thumbnail_image(&src, &params).ok().flatten().context("pas d'aperçu intégré")?,
        };
        let mut img = img.thumbnail(size, size);
        if let Some(o) = orient.and_then(|o| Orientation::from_exif(o as u8)) {
            img.apply_orientation(o);
        }
        Ok(img)
    } else {
        let mut dec = ImageReader::open(p)?.with_guessed_format()?.into_decoder()?;
        let o = dec.orientation().unwrap_or(Orientation::NoTransforms);
        let mut img = DynamicImage::from_decoder(dec)?.thumbnail(size, size);
        img.apply_orientation(o);
        Ok(img)
    }
}

/// Vignette JPEG rangée dans les données EXIF par l'appareil (160x120 chez Canon), si elle fait au moins
/// `min` px de côté : bien plus rapide que de décoder une photo de 20 Mpx.
fn exif_thumbnail(p: &Path, min: u32) -> Option<DynamicImage> {
    let f = std::fs::File::open(p).ok()?;
    let ex = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)).ok()?;
    let at = |t| ex.get_field(t, exif::In::THUMBNAIL).and_then(|f| f.value.get_uint(0)).map(|v| v as usize);
    let (off, len) = (at(exif::Tag::JPEGInterchangeFormat)?, at(exif::Tag::JPEGInterchangeFormatLength)?);
    let mut img = image::load_from_memory_with_format(ex.buf().get(off..off.checked_add(len)?)?, image::ImageFormat::Jpeg).ok()?;
    if img.width().max(img.height()) < min {
        return None;
    }
    // Bandes noires (vignette 4:3 d'une photo 3:2) : recadrée aux proportions de la photo.
    let dim = |t| ex.get_field(t, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).filter(|v| *v > 0);
    if let (Some(pw), Some(ph)) = (dim(exif::Tag::PixelXDimension), dim(exif::Tag::PixelYDimension)) {
        let (tw, th) = (img.width(), img.height());
        let want = pw as f32 / ph as f32;
        if (tw as f32 / th as f32 - want).abs() > 0.02 {
            let (cw, ch) = if tw as f32 / th as f32 > want { ((th as f32 * want).round() as u32, th) } else { (tw, (tw as f32 / want).round() as u32) };
            img = img.crop_imm((tw - cw) / 2, (th - ch) / 2, cw.max(1), ch.max(1));
        }
    }
    let o = ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0));
    if let Some(o) = o.and_then(|o| Orientation::from_exif(o as u8)) {
        img.apply_orientation(o);
    }
    Some(img)
}

/// Aperçu rapide pour les fenêtres de choix : vignette EXIF d'un JPEG si elle suffit, sinon `decode_preview`.
pub fn decode_quick(p: &Path, size: u32) -> Result<DynamicImage> {
    let jpeg = p.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "jpg" | "jpeg"));
    if jpeg {
        if let Some(img) = exif_thumbnail(p, size * 3 / 4) {
            return Ok(img.thumbnail(size, size));
        }
    }
    decode_preview(p, size)
}

/// Fabrique la vignette : aperçu intégré, développé avec les réglages si la photo est modifiée.
fn make_thumb(p: &Path, gpu: Option<&(wgpu::Device, wgpu::Queue)>) -> Result<egui::ColorImage> {
    let img = decode_preview(p, THUMB_SIZE)?;
    let adj = load_sidecar(p).filter(|a| *a != Adjustments::default());
    let rgb = match (adj, gpu) {
        (Some(adj), Some((device, queue))) => {
            let mut lin = img.into_rgba32f();
            for px in lin.pixels_mut() {
                for c in &mut px.0[..3] {
                    *c = crate::image_io::srgb_to_linear(*c);
                }
            }
            let (w, h) = lin.dimensions();
            let mut li = crate::image_io::LinearImage { width: w, height: h, pixels: bytemuck::cast_slice(lin.as_raw()).to_vec() };
            let mut canvas = crate::retouch::F32Canvas { data: &mut li.pixels, w, h };
            crate::retouch::apply_all(&mut canvas, Some(p), &adj.retouch, false);
            let mut dev = crate::gpu::Developer::new(device, queue, wgpu::TextureFormat::Bgra8Unorm, &li)?;
            // Masques IA seulement s'ils sont déjà en cache (pas de calcul pour une vignette).
            let masks = crate::ai::masks_for(p, &adj, &dev, device, queue, false);
            dev.apply_assets_with(device, queue, &adj, &|m| masks.get(&m).cloned());
            crate::lens::setup(&mut dev, p, &adj.lens);
            let (ow, oh) = adj.output_size(w, h);
            let u = adj.uniforms([0.0, 0.0, 1.0, 1.0], false, true, [w as f32, h as f32], false);
            let px = dev.render_to_cpu(device, queue, &u, ow, oh)?;
            save_thumb(p, ow, oh, &px)?;
            image::open(thumb_path(p))?
        }
        _ => {
            let tp = thumb_path(p);
            std::fs::create_dir_all(tp.parent().unwrap())?;
            img.to_rgb8().save(&tp)?;
            img
        }
    };
    Ok(to_color_image(&rgb))
}

fn to_color_image(img: &DynamicImage) -> egui::ColorImage {
    let rgba = img.to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
}

fn cached_thumb(p: &Path) -> Option<egui::ColorImage> {
    let tp = thumb_path(p);
    let fresh = mtime(&tp) >= mtime(p).max(mtime(&sidecar_path(p)));
    if !fresh {
        return None;
    }
    image::open(&tp).ok().map(|i| to_color_image(&i))
}

struct Queue {
    items: VecDeque<PathBuf>,
    queued: HashSet<PathBuf>,
}

/// Pool de threads qui produit les vignettes ; les dernières demandées passent en premier.
pub struct Thumbnailer {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    pub rx: Receiver<(PathBuf, Option<egui::ColorImage>)>,
}

impl Thumbnailer {
    pub fn new(ctx: egui::Context, gpu: Option<(wgpu::Device, wgpu::Queue)>) -> Self {
        let shared = Arc::new((Mutex::new(Queue { items: VecDeque::new(), queued: HashSet::new() }), Condvar::new()));
        let (tx, rx) = channel();
        let gpu = Arc::new(gpu);
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 6);
        for _ in 0..n {
            let (shared, tx, ctx, gpu) = (shared.clone(), tx.clone(), ctx.clone(), gpu.clone());
            std::thread::spawn(move || loop {
                let path = {
                    let (lock, cv) = &*shared;
                    let mut q = lock.lock().unwrap();
                    while q.items.is_empty() {
                        q = cv.wait(q).unwrap();
                    }
                    let p = q.items.pop_front().unwrap();
                    q.queued.remove(&p);
                    p
                };
                let img = cached_thumb(&path).or_else(|| make_thumb(&path, gpu.as_ref().as_ref()).ok());
                if tx.send((path, img)).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        Self { shared, rx }
    }

    pub fn request(&self, p: &Path) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        if q.queued.insert(p.to_path_buf()) {
            q.items.push_front(p.to_path_buf());
            cv.notify_one();
        }
    }

    pub fn pending(&self) -> usize {
        self.shared.0.lock().unwrap().items.len()
    }

    /// Oublie les demandes qui ne sont plus visibles.
    pub fn retain(&self, keep: &HashSet<PathBuf>) {
        let mut q = self.shared.0.lock().unwrap();
        q.items.retain(|p| keep.contains(p));
        q.queued.retain(|p| keep.contains(p));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_copy_paths() {
        let p = Path::new("/photos/IMG_0002.CR2");
        let c = virtual_copy(p, 3);
        assert_eq!(c, Path::new("/photos/IMG_0002.CR2#3"));
        assert_eq!(copy_number(&c), Some(3));
        assert_eq!(source(&c), p);
        assert!(is_raw(&c));
        assert_eq!(copy_number(p), None);
        assert_eq!(copy_number(&sidecar_path(&c)), None);
        assert_eq!(copy_number(Path::new("/x/#2")), None);
    }

    #[test]
    fn search_by_camera_date_month_and_keyword() {
        // 14/07/2024 à midi.
        let p = Photo { path: PathBuf::from("/photos/Été/img_1.jpg"), name: "img_1.jpg".into(), taken: 1_720_958_400, camera: "Canon EOS 600D".into(), rating: 0, edited: false, sharp: -1.0, hash: 0 };
        let kw = vec!["Plage".to_string()];
        for q in ["canon", "600d", "2024", "juillet", "July", "14/07/2024", "7/2024", "plage", "ete", "canon 2024 plage", ""] {
            assert!(matches(q, &p, &kw), "{q}");
        }
        for q in ["nikon", "2023", "août", "15/07/2024", "canon 2023", "montagne"] {
            assert!(!matches(q, &p, &kw), "{q}");
        }
    }

    #[test]
    fn subfolders_and_folder_filter() {
        let cat = Catalogue::with(Connection::open_in_memory().unwrap()).unwrap();
        for p in ["/ph/a.jpg", "/ph/2024/b.jpg", "/ph/2024/noel/c.jpg", "/ph/2024/noel/c.jpg#2", "/ph/2025/d.jpg", "/ph2/e.jpg"] {
            let root = if p.starts_with("/ph2") { "/ph2" } else { "/ph" };
            cat.conn.execute("INSERT INTO photos(path, folder, name, mtime, taken) VALUES (?1, ?2, ?1, 0, 0)", [p, root]).unwrap();
        }
        let dirs = cat.subfolders("/ph");
        assert_eq!(dirs, vec![("/ph".into(), 5), ("/ph/2024".into(), 3), ("/ph/2024/noel".into(), 2), ("/ph/2025".into(), 1)]);
        let n = |f: &str| cat.list(Some(f), Filter::All, Sort::Name, None).len();
        assert_eq!((n("/ph"), n("/ph/2024"), n("/ph/2024/noel"), n("/ph2")), (5, 3, 2, 1));
        assert_eq!(cat.list(None, Filter::All, Sort::Name, None).len(), 6);
    }

    #[test]
    fn dates() {
        let ts = parse_exif_date("2023:05:14 10:22:33").unwrap();
        assert_eq!(ts, 1684059753);
        assert_eq!(format_date(ts), "14/05/2023");
        assert_eq!(format_date(0), "01/01/1970");
        assert!(parse_exif_date("0000:00:00 00:00:00").is_none());
    }

    #[test]
    fn trashing_moves_the_file_and_forgets_its_copies() {
        let dir = std::env::temp_dir().join(format!("luminux-corbeille-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ph")).unwrap();
        let photo = dir.join("ph/été 1.jpg");
        for _ in 0..2 {
            std::fs::write(&photo, b"x").unwrap();
            trash_into(&dir.join("Trash"), &photo).unwrap();
            assert!(!photo.exists());
        }
        assert!(dir.join("Trash/files/été 1.jpg").exists() && dir.join("Trash/files/été 1.2.jpg").exists());
        let info = std::fs::read_to_string(dir.join("Trash/info/été 1.2.jpg.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/") && info.contains("/ph/%C3%A9t%C3%A9%201.jpg\nDeletionDate=20"), "{info}");
        assert!(trash_into(&dir.join("Trash"), &photo).is_err());
        let _ = std::fs::remove_dir_all(&dir);

        let cat = Catalogue::with(Connection::open_in_memory().unwrap()).unwrap();
        for p in ["/ph/a.jpg", "/ph/a.jpg#2", "/ph/a.jpg.png", "/ph/a.jpg#x", "/ph/b.jpg"] {
            cat.conn.execute("INSERT INTO photos(path, folder, name, mtime, taken) VALUES (?1, '/ph', ?1, 0, 0)", [p]).unwrap();
        }
        cat.add_keyword(Path::new("/ph/a.jpg#2"), "mer");
        cat.forget_photo(Path::new("/ph/a.jpg")).unwrap();
        let left: Vec<_> = cat.list(None, Filter::All, Sort::Name, None).into_iter().map(|p| p.path).collect();
        assert_eq!(left, ["/ph/a.jpg#x", "/ph/a.jpg.png", "/ph/b.jpg"].map(PathBuf::from));
        assert_eq!(cat.all_keywords().len(), 0);
    }
}
