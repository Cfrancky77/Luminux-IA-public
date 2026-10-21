//! Luminux IA : éditeur photo non destructif, rendu GPU (wgpu/Vulkan).
//!
//! Usage : luminux [photo]
//!         luminux --export <photo> <sortie.jpg|png|tif>   (sans fenêtre, réglages du .luminux)
//!         luminux --detourer <photo> <sortie.png>   (PNG au fond transparent, masque du sujet)
//!         luminux --batch <dossier> <jpg|webp|avif|png|tif> <photos…>   (export par lots)
//!         luminux --import <packs…>   (LUT .cube, presets .xmp/.lrtemplate/.lnpc, .zip)

#[macro_use]
mod i18n;
mod adjust;
mod ai;
mod app;
mod batch;
mod comfy;
mod cull;
mod denoise;
mod export;
mod gpu;
mod history;
mod icc;
mod image_io;
mod inpaint;
mod layers;
mod lens;
mod library;
mod looks;
mod masks;
mod reshape;
mod retouch;
mod sdlocal;
mod settings;
mod sky;
mod watermark;

use anyhow::{Context, Result};
use eframe::{egui, egui_wgpu, wgpu};
use std::path::PathBuf;
use std::sync::Arc;

fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    // Les photos dépassent souvent 8192 px et les buffers par défaut (256 Mo) :
    // on demande les limites réelles de la carte.
    wgpu::DeviceDescriptor { label: Some("luminux"), required_limits: adapter.limits(), ..Default::default() }
}

fn backends() -> wgpu::Backends {
    // Vulkan seul évite les avertissements EGL de Mesa sur les cartes NVIDIA.
    wgpu::Backends::from_env().unwrap_or(wgpu::Backends::VULKAN)
}

fn headless_gpu() -> Result<(wgpu::Device, wgpu::Queue)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends();
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .context("aucun GPU Vulkan")?;
    Ok(pollster::block_on(adapter.request_device(&device_descriptor(&adapter)))?)
}

fn export_headless(input: PathBuf, output: PathBuf, cutout: bool) -> Result<()> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends();
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .context("aucun GPU Vulkan")?;
    println!("GPU : {}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&device_descriptor(&adapter)))?;
    let t = std::time::Instant::now();
    let img = image_io::load(&input)?;
    println!("chargé {}x{} en {:?}", img.width, img.height, t.elapsed());
    let adj = app::load_sidecar(&input).unwrap_or_default();
    let mut img = img;
    let (w, h) = (img.width, img.height);
    let mut canvas = retouch::F32Canvas { data: &mut img.pixels, w, h };
    if !denoise::apply(&mut canvas, Some(&input), &adj.denoise, true) {
        eprintln!("Débruitage IA indisponible : export sans débruitage.");
    }
    retouch::apply_all(&mut canvas, Some(&input), &adj.retouch, true);
    let mut dev = gpu::Developer::new(&device, &queue, wgpu::TextureFormat::Bgra8Unorm, &img)?;
    let extra: &[ai::Model] = if cutout { &[ai::Model::Subject] } else { &[] };
    let masks = ai::masks_with(&input, &adj, extra, &dev, &device, &queue, true);
    dev.apply_assets_with(&device, &queue, &adj, &|m| masks.get(&m).cloned());
    if cutout {
        dev.set_subject(&queue, masks.get(&ai::Model::Subject).map(|m| m.as_ref()));
        anyhow::ensure!(masks.contains_key(&ai::Model::Subject), "masque du sujet indisponible");
    }
    lens::setup(&mut dev, &input, &adj.lens);
    let t = std::time::Instant::now();
    let (w, h, px) = dev.render_full_with(&device, &queue, &adj, cutout)?;
    println!("rendu en {:?}", t.elapsed());
    if cutout {
        image_io::export_rgba(&output, w, h, &px)?;
    } else {
        export::write(&output, Some(&input), w, h, &px, &settings::Settings::load().export)?;
    }
    println!("écrit {}", output.display());
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = i18n::set_language(&settings::Settings::load().language) {
        eprintln!("langue : {e:#}");
    }
    if args.first().map(String::as_str) == Some("--import") {
        anyhow::ensure!(args.len() >= 2, "usage : luminux --import <fichiers ou dossiers…>");
        let paths: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
        let r = looks::import(&paths);
        for a in &r.added {
            println!("+ {a}");
        }
        for (k, n) in &r.ignored {
            println!("  sans équivalent ({n}) : {k}");
        }
        for e in &r.errors {
            eprintln!("erreur : {e}");
        }
        println!("{} -> {}", r.summary(), looks::looks_dir().display());
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--batch") {
        anyhow::ensure!(args.len() >= 4, "usage : luminux --batch <dossier> <jpg|webp|avif|png|tif> <photos…>");
        let (device, queue) = headless_gpu()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let photos = args[3..].iter().map(PathBuf::from).collect();
        batch::run(photos, args[1].clone().into(), args[2].clone(), settings::Settings::load().export, device, queue, tx, egui::Context::default());
        for m in rx.try_iter() {
            if let batch::BatchMsg::Done { ok, errors, dir } = m {
                println!("{ok} photo(s) exportée(s) dans {}", dir.display());
                for e in errors {
                    eprintln!("erreur : {e}");
                }
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--mask-from") {
        // Remplace le masque IA d'une photo par une image en niveaux de gris (blanc = dans le masque).
        anyhow::ensure!(args.len() == 4 && ["ciel", "sujet"].contains(&args[2].as_str()), "usage : luminux --mask-from <photo> <ciel|sujet> <masque.png>");
        let photo = PathBuf::from(&args[1]);
        let img = image_io::load(&photo)?;
        let (w, h) = masks::mask_size(img.width, img.height);
        let m = image::open(&args[3])?.resize_exact(w, h, image::imageops::FilterType::Triangle).to_luma32f();
        let data = m.into_raw();
        let lin = img.pixels.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]);
        // Luminance moyenne sous le masque, mesurée sur la source réduite au même pas.
        let (mut num, mut den) = (0f64, 0f64);
        let (sx, sy) = (img.width as f32 / w as f32, img.height as f32 / h as f32);
        let lum: Vec<f32> = lin.collect();
        for y in 0..h {
            for x in 0..w {
                let v = data[(y * w + x) as usize] as f64;
                let l = lum[((y as f32 * sy) as u32 * img.width + (x as f32 * sx) as u32) as usize].min(1.0) as f64;
                num += v * l;
                den += v;
            }
        }
        let model = if args[2] == "ciel" { ai::Model::Sky } else { ai::Model::Subject };
        let mean_luma = if den > 1.0 { (num / den) as f32 } else { 0.0 };
        ai::cache_store(&photo, model, &ai::AiMask { w, h, data, mean_luma })?;
        println!("masque {} enregistré pour {}", args[2], photo.display());
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--export") {
        anyhow::ensure!(args.len() == 3, "usage : luminux --export <photo> <sortie>");
        return export_headless(args[1].clone().into(), args[2].clone().into(), false);
    }
    if args.first().map(String::as_str) == Some("--detourer") {
        anyhow::ensure!(args.len() == 3, "usage : luminux --detourer <photo> <sortie.png>");
        return export_headless(args[1].clone().into(), args[2].clone().into(), true);
    }
    let initial = args.first().map(PathBuf::from);

    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = backends();
        setup.power_preference = wgpu::PowerPreference::HighPerformance;
        setup.device_descriptor = Arc::new(device_descriptor);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 900.0]).with_title("Luminux IA"),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native("Luminux IA", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, initial)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}
