//! Rendu GPU : l'image source reste sur la carte (texture f16 avec mips),
//! le shader de développement sert à l'affichage comme à l'export.

use crate::adjust::Uniforms;
use crate::image_io::LinearImage;
use anyhow::{Result, bail};
use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait};
use eframe::wgpu;
use half::f16;

const EXPORT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const IMAGE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub struct Developer {
    pub width: u32,
    pub height: u32,
    display_pipeline: wgpu::RenderPipeline,
    mip_pipeline: wgpu::RenderPipeline,
    mip_layout: wgpu::BindGroupLayout,
    texture: wgpu::Texture,
    export_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    display_ubuf: wgpu::Buffer,
    display_bg: wgpu::BindGroup,
    image_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    lut: wgpu::Texture,
    lut_view: wgpu::TextureView,
    lut3d: wgpu::Texture,
    lut3d_view: wgpu::TextureView,
    /// Taille N de la LUT 3D chargée (0 = aucune).
    lut3d_size: u32,
    retouch: Vec<crate::adjust::Retouch>,
    /// Repères du remodelage (visages, yeux, silhouette) et masque du sujet qui sert à la silhouette.
    shape: crate::reshape::Geometry,
    subject_map: Option<Vec<f32>>,
    /// Profil de l'écran : LUT 3D sRGB -> écran, appliquée seulement à l'affichage.
    screen_lut: wgpu::Texture,
    screen_lut_view: wgpu::TextureView,
    screen_lut_on: bool,
    masks: wgpu::Texture,
    masks_view: wgpu::TextureView,
    pub mask_w: u32,
    pub mask_h: u32,
    sky_tex: wgpu::Texture,
    sky_view: wgpu::TextureView,
    /// Ciel chargé : rapport largeur/hauteur, couleur moyenne, étendue utile, luminance moyenne.
    sky: Option<SkyInfo>,
    /// Luminance moyenne de la photo sous le masque du ciel (None : pas de masque).
    sky_mask_luma: Option<f32>,
    /// Bas du ciel détecté (fraction de la hauteur) : le nouveau ciel doit descendre au moins jusque-là.
    sky_mask_bottom: f32,
    /// Carte de profondeur chargée, et mise au point automatique (proximité du sujet principal).
    depth_ok: bool,
    depth_focus: f32,
    subject_ok: bool,
    /// Masques du visage chargés (peau, yeux, lèvres, dents).
    face_ok: bool,
    /// Rayon du lissage de la peau, en fraction du grand côté (selon la surface de peau détectée).
    skin_radius: f32,
    /// Correction d'objectif calculée pour cette image (None : aucune).
    lens: Option<crate::lens::LensUniforms>,
    /// Calques : texture (1 × 1 tant qu'aucun calque n'est posé), image chargée par emplacement,
    /// et (rapport largeur/hauteur, étendue utile x, y) de chacun.
    layer_tex: wgpu::Texture,
    layer_view: wgpu::TextureView,
    layer_paths: [String; crate::adjust::MAX_LAYERS],
    layer_info: [Option<[f32; 3]>; crate::adjust::MAX_LAYERS],
}

fn layer_texture(device: &wgpu::Device, side: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("calques"),
        size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: crate::adjust::MAX_LAYERS as u32 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
    (tex, view)
}

#[derive(Clone, Copy)]
struct SkyInfo {
    aspect: f32,
    avg: [f32; 3],
    ext: [f32; 2],
    luma: f32,
}

/// Couche de la texture de masques réservée au masque du ciel.
pub const SKY_LAYER: usize = crate::adjust::MAX_LOCALS;
/// Couche de la carte de profondeur (proximité 0..1).
pub const DEPTH_LAYER: usize = SKY_LAYER + 1;
/// Masque du sujet (protégé par Structure).
pub const SUBJECT_LAYER: usize = DEPTH_LAYER + 1;
/// Masques du visage (ordre de `ai::FACE_MODELS`) : peau, yeux, lèvres, dents.
pub const FACE_LAYER: usize = SUBJECT_LAYER + 1;
/// Nombre de couches de la texture de masques.
const MASK_LAYERS: usize = FACE_LAYER + crate::ai::FACE_MODELS.len();

/// Côté maximal d'une LUT 3D acceptée.
pub const LUT3D_MAX: u32 = 65;
/// Taille de la LUT du profil de l'écran.
pub const SCREEN_LUT: u32 = 33;

fn pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    vs: &str,
    fs: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs),
        layout: Some(layout),
        vertex: wgpu::VertexState { module, entry_point: Some(vs), buffers: &[], compilation_options: Default::default() },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            targets: &[Some(format.into())],
            compilation_options: Default::default(),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn develop_bg(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, ubuf: &wgpu::Buffer, sampler: &wgpu::Sampler, v: [&wgpu::TextureView; 7]) -> wgpu::BindGroup {
    let [image, lut, lut3d, masks, sky, screen, layers] = v;
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(image) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(lut) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(lut3d) },
            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(masks) },
            wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(sky) },
            wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(screen) },
            wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::TextureView(layers) },
        ],
    })
}

impl Developer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, target: wgpu::TextureFormat, img: &LinearImage) -> Result<Self> {
        Self::new_half(device, queue, target, img.width, img.height, &img.to_half())
    }

    /// Variante qui reçoit les pixels déjà convertis en f16 (conversion faite hors du thread de l'interface).
    pub fn new_half(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: wgpu::TextureFormat,
        width: u32,
        height: u32,
        half: &[f16],
    ) -> Result<Self> {
        let max = device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            bail!("image {width}x{height} plus grande que la limite GPU {max}");
        }
        let module = device.create_shader_module(wgpu::include_wgsl!("develop.wgsl"));
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let tex3d_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        };
        let smp_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("develop"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                smp_entry(2),
                tex_entry(3),
                tex_entry(6),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                tex3d_entry(4),
                tex3d_entry(7),
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let mip_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mip"),
            entries: &[tex_entry(0), smp_entry(1)],
        });
        let pl = |l: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(l)],
                immediate_size: 0,
            })
        };
        let develop_pl = pl(&layout);
        let display_pipeline = pipeline(device, &module, &develop_pl, "vs", "fs", target);
        let export_pipeline = pipeline(device, &module, &develop_pl, "vs", "fs", EXPORT_FORMAT);
        let mip_pipeline = pipeline(device, &module, &pl(&mip_layout), "vs_mip", "fs_mip", IMAGE_FORMAT);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        // Texture source avec chaîne de mips pour un affichage réduit sans moiré.
        let levels = 32 - width.max(height).leading_zeros();
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size,
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: IMAGE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            bytemuck::cast_slice(half),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 8), rows_per_image: None },
            size,
        );
        Self::build_mips(device, queue, &texture, &mip_layout, &mip_pipeline, &sampler);
        let image_view = texture.create_view(&Default::default());

        let lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("courbe"),
            size: wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: IMAGE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lut_view = lut.create_view(&Default::default());
        let lut3d = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lut 3d"),
            size: wgpu::Extent3d { width: LUT3D_MAX, height: LUT3D_MAX, depth_or_array_layers: LUT3D_MAX },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: IMAGE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lut3d_view = lut3d.create_view(&Default::default());
        let screen_lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("profil écran"),
            size: wgpu::Extent3d { width: SCREEN_LUT, height: SCREEN_LUT, depth_or_array_layers: SCREEN_LUT },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: IMAGE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let screen_lut_view = screen_lut.create_view(&Default::default());
        let (mask_w, mask_h) = crate::masks::mask_size(width, height);
        let masks = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("masques"),
            size: wgpu::Extent3d { width: mask_w, height: mask_h, depth_or_array_layers: MASK_LAYERS as u32 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let masks_view = masks.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let sky_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ciel"),
            size: wgpu::Extent3d { width: crate::sky::SKY_MAX, height: crate::sky::SKY_MAX, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: IMAGE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sky_view = sky_tex.create_view(&Default::default());

        let display_ubuf = Self::ubuf(device);
        let (layer_tex, layer_view) = layer_texture(device, 1);
        let mut dev = Self {
            width,
            height,
            display_bg: develop_bg(device, &layout, &display_ubuf, &sampler, [&image_view, &lut_view, &lut3d_view, &masks_view, &sky_view, &screen_lut_view, &layer_view]),
            display_pipeline,
            mip_pipeline,
            mip_layout,
            texture,
            export_pipeline,
            layout,
            display_ubuf,
            image_view,
            sampler,
            lut,
            lut_view,
            lut3d,
            lut3d_view,
            lut3d_size: 0,
            retouch: Vec::new(),
            screen_lut,
            screen_lut_view,
            screen_lut_on: false,
            masks,
            masks_view,
            mask_w,
            mask_h,
            sky_tex,
            sky_view,
            sky: None,
            sky_mask_luma: None,
            sky_mask_bottom: 0.75,
            depth_ok: false,
            depth_focus: 0.8,
            subject_ok: false,
            face_ok: false,
            shape: Default::default(),
            subject_map: None,
            skin_radius: 0.004,
            lens: None,
            layer_tex,
            layer_view,
            layer_paths: Default::default(),
            layer_info: [None; crate::adjust::MAX_LAYERS],
        };
        let identity: Vec<f32> = (0..256).map(|i| i as f32 / 255.0).collect();
        dev.set_curve(queue, identity.as_slice().try_into().unwrap());
        Ok(dev)
    }

    /// Recalcule la chaîne de mips à partir du niveau 0.
    fn build_mips(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, layout: &wgpu::BindGroupLayout, pipeline: &wgpu::RenderPipeline, sampler: &wgpu::Sampler) {
        let levels = texture.mip_level_count();
        let level_view = |l| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: l,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let mut enc = device.create_command_encoder(&Default::default());
        for l in 1..levels {
            let src = level_view(l - 1);
            let dst = level_view(l);
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mip"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([enc.finish()]);
    }

    /// Remplace les pixels de l'image (retouches) et recalcule les mips.
    pub fn upload_image(&self, device: &wgpu::Device, queue: &wgpu::Queue, half: &[f16]) {
        if half.len() != (self.width * self.height * 4) as usize {
            return;
        }
        queue.write_texture(
            self.texture.as_image_copy(),
            bytemuck::cast_slice(half),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(self.width * 8), rows_per_image: None },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        Self::build_mips(device, queue, &self.texture, &self.mip_layout, &self.mip_pipeline, &self.sampler);
    }

    fn ubuf(device: &wgpu::Device) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("réglages"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn set_curve(&mut self, queue: &wgpu::Queue, lut: &[f32; 256]) {
        let data: Vec<f16> = lut.iter().flat_map(|v| [f16::from_f32(*v); 4]).collect();
        queue.write_texture(
            self.lut.as_image_copy(),
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256 * 8), rows_per_image: None },
            wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
        );
    }

    /// Charge la LUT 3D d'un look (ou aucune).
    pub fn set_lut(&mut self, queue: &wgpu::Queue, lut: Option<&crate::looks::Lut3d>) {
        let Some(lut) = lut.filter(|l| l.size >= 2 && l.size <= LUT3D_MAX) else {
            self.lut3d_size = 0;
            return;
        };
        let n = lut.size;
        let data: Vec<f16> = lut.data.iter().flat_map(|c| [f16::from_f32(c[0]), f16::from_f32(c[1]), f16::from_f32(c[2]), f16::ONE]).collect();
        queue.write_texture(
            self.lut3d.as_image_copy(),
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(n * 8), rows_per_image: Some(n) },
            wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
        );
        self.lut3d_size = n;
    }

    /// Charge la conversion vers le profil de l'écran (N = SCREEN_LUT), ou la retire.
    pub fn set_screen_lut(&mut self, queue: &wgpu::Queue, lut: Option<&crate::looks::Lut3d>) {
        let Some(lut) = lut.filter(|l| l.size == SCREEN_LUT) else {
            self.screen_lut_on = false;
            return;
        };
        let n = lut.size;
        let data: Vec<f16> = lut.data.iter().flat_map(|c| [f16::from_f32(c[0]), f16::from_f32(c[1]), f16::from_f32(c[2]), f16::ONE]).collect();
        queue.write_texture(
            self.screen_lut.as_image_copy(),
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(n * 8), rows_per_image: Some(n) },
            wgpu::Extent3d { width: n, height: n, depth_or_array_layers: n },
        );
        self.screen_lut_on = true;
    }

    /// Envoie le masque `layer` (mask_w x mask_h octets).
    pub fn set_mask(&self, queue: &wgpu::Queue, layer: usize, data: &[u8]) {
        if layer >= MASK_LAYERS || data.len() != (self.mask_w * self.mask_h) as usize {
            return;
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.masks,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer as u32 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(self.mask_w), rows_per_image: Some(self.mask_h) },
            wgpu::Extent3d { width: self.mask_w, height: self.mask_h, depth_or_array_layers: 1 },
        );
    }

    /// Ciel de remplacement (ou aucun).
    pub fn set_sky(&mut self, queue: &wgpu::Queue, sky: Option<&crate::sky::SkyImage>) {
        let Some(sk) = sky.filter(|s| s.w <= crate::sky::SKY_MAX && s.h <= crate::sky::SKY_MAX) else {
            self.sky = None;
            return;
        };
        queue.write_texture(
            self.sky_tex.as_image_copy(),
            bytemuck::cast_slice(&sk.half),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(sk.w * 8), rows_per_image: None },
            wgpu::Extent3d { width: sk.w, height: sk.h, depth_or_array_layers: 1 },
        );
        let m = crate::sky::SKY_MAX as f32;
        self.sky = Some(SkyInfo { aspect: sk.w as f32 / sk.h as f32, avg: sk.avg, ext: [sk.w as f32 / m, sk.h as f32 / m], luma: sk.mean_luma });
    }

    /// Images des calques (chargées seulement si elles ont changé). La texture passe à sa taille
    /// pleine au premier calque.
    pub fn set_layers(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, layers: &[crate::adjust::Layer]) {
        let max = crate::layers::LAYER_MAX;
        let wanted = |i: usize| layers.get(i).map_or(String::new(), crate::layers::key);
        if (0..crate::adjust::MAX_LAYERS).any(|i| !wanted(i).is_empty()) && self.layer_tex.width() < max {
            (self.layer_tex, self.layer_view) = layer_texture(device, max);
            self.layer_paths = Default::default();
            self.display_bg = develop_bg(device, &self.layout, &self.display_ubuf, &self.sampler, [&self.image_view, &self.lut_view, &self.lut3d_view, &self.masks_view, &self.sky_view, &self.screen_lut_view, &self.layer_view]);
        }
        for i in 0..crate::adjust::MAX_LAYERS {
            if self.layer_paths[i] == wanted(i) {
                continue;
            }
            self.layer_paths[i] = wanted(i);
            self.layer_info[i] = None;
            let Some(img) = layers.get(i).filter(|l| !l.image.is_empty()).and_then(crate::layers::load_layer) else { continue };
            if img.w > self.layer_tex.width() || img.h > self.layer_tex.height() {
                continue;
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.layer_tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: i as u32 }, aspect: wgpu::TextureAspect::All },
                &img.rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.w * 4), rows_per_image: None },
                wgpu::Extent3d { width: img.w, height: img.h, depth_or_array_layers: 1 },
            );
            self.layer_info[i] = Some([img.w as f32 / img.h as f32, img.w as f32 / max as f32, img.h as f32 / max as f32]);
        }
    }

    /// Correction d'objectif (profil de la photo et réglages), ou aucune.
    /// Correction d'objectif en cours (pour placer les tracés au bon endroit de la source).
    pub fn lens(&self) -> Option<crate::lens::LensUniforms> {
        self.lens
    }

    pub fn set_lens(&mut self, profile: Option<&crate::lens::Profile>, fix: &crate::adjust::LensFix) {
        self.lens = profile.filter(|_| fix.enabled).map(|p| p.uniforms(fix, self.width, self.height));
    }

    /// Retouches de la photo, reportées sur les masques IA (calculés sur la photo d'origine).
    pub fn set_retouch(&mut self, ops: &[crate::adjust::Retouch]) {
        self.retouch = ops.to_vec();
    }

    /// Masque IA à la taille des masques, avec les retouches reportées : une zone gommée ou corrigée
    /// prend le masque de ce qui l'a remplacée.
    fn mask_data(&self, m: &crate::ai::AiMask) -> Vec<f32> {
        let mut d = m.at(self.mask_w, self.mask_h);
        crate::retouch::apply_to_mask(&mut d, self.mask_w, self.mask_h, &self.retouch);
        d
    }

    /// Masque du ciel (ou aucun : le remplacement est alors inactif).
    pub fn set_sky_mask(&mut self, queue: &wgpu::Queue, mask: Option<&crate::ai::AiMask>) {
        match mask {
            Some(m) => {
                let data: Vec<u8> = self.mask_data(m).iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
                self.set_mask(queue, SKY_LAYER, &data);
                self.sky_mask_luma = Some(m.mean_luma);
                self.sky_mask_bottom = sky_bottom(&data, self.mask_w, self.mask_h);
            }
            None => self.sky_mask_luma = None,
        }
    }

    /// Carte de profondeur (ou aucune : les outils de profondeur sont alors inactifs).
    pub fn set_depth(&mut self, queue: &wgpu::Queue, depth: Option<&crate::ai::AiMask>) {
        match depth {
            Some(m) => {
                let d = self.mask_data(m);
                let data: Vec<u8> = d.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
                self.set_mask(queue, DEPTH_LAYER, &data);
                self.depth_focus = auto_focus(&d, self.mask_w, self.mask_h);
                self.depth_ok = true;
            }
            None => self.depth_ok = false,
        }
    }

    /// Masque du sujet pour Structure (ou aucun : le sujet n'est pas protégé).
    pub fn set_subject(&mut self, queue: &wgpu::Queue, mask: Option<&crate::ai::AiMask>) {
        self.subject_ok = mask.is_some();
        self.subject_map = None;
        if let Some(m) = mask {
            let d = self.mask_data(m);
            let data: Vec<u8> = d.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
            self.set_mask(queue, SUBJECT_LAYER, &data);
            self.subject_map = Some(d);
        }
        self.update_body();
    }

    /// Profil de la silhouette (masque du sujet, têtes situées par les visages trouvés).
    fn update_body(&mut self) {
        let scale = self.width as f32 / self.mask_w as f32;
        self.shape.body = match &self.subject_map {
            Some(d) => crate::reshape::body_profiles(d, self.mask_w as usize, self.mask_h as usize, scale, &self.shape.faces),
            None => Vec::new(),
        };
    }

    /// Masques du visage (ordre de `ai::FACE_MODELS`) ; tous présents ou aucun.
    pub fn set_face(&mut self, queue: &wgpu::Queue, masks: Option<[&crate::ai::AiMask; 4]>) {
        self.face_ok = masks.is_some();
        self.shape.faces.clear();
        self.shape.eyes.clear();
        for (i, m) in masks.into_iter().flatten().enumerate() {
            let d = self.mask_data(m);
            if i == 1 {
                let scale = self.width as f32 / self.mask_w as f32;
                (self.shape.faces, self.shape.eyes) = crate::reshape::eyes_and_faces(&d, self.mask_w as usize, self.mask_h as usize, scale);
            }
            if i == 0 {
                // Surface de peau ≈ 0,5 × côté du visage² : rayon ≈ 1/40 du visage (surestimé s'il y a plusieurs visages).
                let area: f32 = d.iter().sum();
                self.skin_radius = (0.035 * area.sqrt() / self.mask_w.max(self.mask_h) as f32).clamp(0.0005, 0.02);
            }
            let data: Vec<u8> = d.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
            self.set_mask(queue, FACE_LAYER + i, &data);
        }
        self.update_body();
    }

    /// Masque rasterisé d'un réglage local, avec le masque IA éventuel.
    pub fn upload_local(&self, queue: &wgpu::Queue, i: usize, l: &crate::adjust::Local, ai: Option<&crate::ai::AiMask>) {
        let data = ai.map(|m| self.mask_data(m));
        self.set_mask(queue, i, &crate::masks::rasterize(l, self.mask_w, self.mask_h, data.as_deref()));
    }

    /// Courbe, LUT, masques locaux et ciel d'un jeu de réglages (LUT et ciel lus sur disque, avec cache).
    /// `ai` fournit les masques IA déjà calculés.
    pub fn apply_assets_with(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, adj: &crate::adjust::Adjustments, ai: &dyn Fn(crate::ai::Model) -> Option<std::sync::Arc<crate::ai::AiMask>>) {
        self.set_retouch(&adj.retouch);
        self.set_layers(device, queue, &adj.layers);
        self.set_curve(queue, &adj.curve_lut());
        let lut = adj.lut.as_deref().and_then(crate::looks::load_lut);
        self.set_lut(queue, lut.as_deref());
        for (i, l) in adj.locals.iter().take(crate::adjust::MAX_LOCALS).enumerate() {
            let m = match l.shape {
                crate::adjust::Shape::Ai { kind, .. } => ai(crate::ai::Model::of(kind)),
                _ => None,
            };
            self.upload_local(queue, i, l, m.as_deref());
        }
        let sky = adj.sky.image.as_deref().and_then(crate::sky::load);
        self.set_sky(queue, sky.as_deref());
        let needed = crate::ai::needed(adj);
        let mask = if needed.contains(&crate::ai::Model::Sky) { ai(crate::ai::Model::Sky) } else { None };
        self.set_sky_mask(queue, mask.as_deref());
        let depth = if adj.depth.active() { ai(crate::ai::Model::Depth) } else { None };
        self.set_depth(queue, depth.as_deref());
        let subject = if needed.contains(&crate::ai::Model::Subject) { ai(crate::ai::Model::Subject) } else { None };
        self.set_subject(queue, subject.as_deref());
        let face: Vec<_> = if adj.portrait.active() { crate::ai::FACE_MODELS.iter().filter_map(|m| ai(*m)).collect() } else { Vec::new() };
        self.set_face(queue, <[_; 4]>::try_from(face.iter().map(|m| m.as_ref()).collect::<Vec<_>>()).ok());
    }

    /// Complète les uniformes avec ce que seul le Developer connaît (taille de la LUT).
    fn patch(&self, u: &Uniforms) -> Uniforms {
        let mut u = *u;
        u.lut[1] = self.lut3d_size as f32;
        if self.lut3d_size == 0 {
            u.lut[0] = 0.0;
        }
        match (self.sky, self.sky_mask_luma) {
            (Some(sk), Some(luma)) => {
                // Accord automatique : le nouveau ciel prend la luminosité moyenne de l'ancien.
                u.sky[3] *= (luma / sk.luma.max(1e-4)).clamp(0.05, 20.0);
                u.sky2[1] = sk.aspect;
                u.sky_avg = [sk.avg[0], sk.avg[1], sk.avg[2], 0.0];
                u.sky3 = [sk.ext[0], sk.ext[1], self.sky_mask_bottom, 0.0];
            }
            _ => u.sky[0] = 0.0,
        }
        for (i, info) in self.layer_info.iter().enumerate() {
            match info {
                Some([aspect, ex, ey]) => {
                    u.layers[3 * i + 1][3] = *aspect;
                    u.layers[3 * i + 2][0] = *ex;
                    u.layers[3 * i + 2][1] = *ey;
                }
                None => u.layers[3 * i][0] = 0.0,
            }
        }
        u.depth3[3] = self.depth_ok as u32 as f32;
        u.enhance2[0] = self.sky_mask_luma.is_some() as u32 as f32;
        u.enhance2[1] = self.subject_ok as u32 as f32;
        u.face3[0] = self.face_ok as u32 as f32;
        u.face3[1] = self.skin_radius;
        if u.depth2[1] < 0.0 {
            u.depth2[1] = self.depth_focus;
        }
        let g = &self.shape;
        let nf = if u.reshape[0] > 0.0 { g.faces.len().min(crate::reshape::MAX_FACES) } else { 0 };
        let ne = if u.reshape[1] > 0.0 { g.eyes.len().min(crate::reshape::MAX_EYES) } else { 0 };
        let np = if u.reshape[2] != 0.0 || u.reshape[3] != 0.0 { g.body.len().min(crate::reshape::MAX_PEOPLE) } else { 0 };
        u.reshape2 = [nf as f32, ne as f32, np as f32, 0.0];
        for (i, f) in g.faces.iter().take(nf).enumerate() {
            u.faces[2 * i] = [f.o[0], f.o[1], f.u[0], f.u[1]];
            u.faces[2 * i + 1] = [f.d, 0.0, 0.0, 0.0];
        }
        for (i, e) in g.eyes.iter().take(ne).enumerate() {
            u.eyes[i] = [e.c[0], e.c[1], e.r, 0.0];
        }
        for (j, person) in g.body.iter().take(np).enumerate() {
            for (i, r) in person.rows.iter().take(crate::reshape::PERSON_ROWS).enumerate() {
                u.body[j * crate::reshape::PERSON_ROWS + i] = [r[0], r[1], r[2], 0.0];
            }
            u.people[2 * j] = [person.top, person.bottom, person.belly[0], person.belly[1]];
            u.people[2 * j + 1] = [person.room[0], person.room[1], 0.0, 0.0];
        }
        if let Some(l) = &self.lens {
            u.lens = l.info;
            u.lens_dist = l.dist;
            u.lens_tca_r = l.tca_r;
            u.lens_tca_b = l.tca_b;
            u.lens_vig = l.vig;
        }
        u
    }

    /// Rend `uniforms` dans une image `w`x`h` relue sur le CPU (RGB encodé sRGB).
    /// Sert à l'export (par bandes, mip 0) et à l'histogramme (vignette).
    pub fn render_to_cpu(&self, device: &wgpu::Device, queue: &wgpu::Queue, u: &Uniforms, w: u32, h: u32) -> Result<Vec<[f32; 4]>> {
        let ubuf = Self::ubuf(device);
        queue.write_buffer(&ubuf, 0, bytemuck::bytes_of(&self.patch(u)));
        let bg = develop_bg(device, &self.layout, &ubuf, &self.sampler, [&self.image_view, &self.lut_view, &self.lut3d_view, &self.masks_view, &self.sky_view, &self.screen_lut_view, &self.layer_view]);
        let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rendu"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: EXPORT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let row = (w * 16).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: row as u64 * h as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("export"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.export_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        enc.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            size,
        );
        queue.submit([enc.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely())?;
        let mapped = slice.get_mapped_range()?;
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h as usize {
            let line = &mapped[y * row as usize..][..w as usize * 16];
            out.extend_from_slice(bytemuck::cast_slice::<u8, [f32; 4]>(line));
        }
        drop(mapped);
        readback.unmap();
        Ok(out)
    }

    /// Image complète développée (recadrée) à pleine résolution, rendue par bandes.
    /// Renvoie largeur, hauteur et pixels.
    pub fn render_full(&self, device: &wgpu::Device, queue: &wgpu::Queue, adj: &crate::adjust::Adjustments) -> Result<(u32, u32, Vec<[f32; 4]>)> {
        self.render_full_with(device, queue, adj, false)
    }

    /// Rendu complet ; `cutout` : l'alpha de sortie est le masque du sujet (fond transparent).
    pub fn render_full_with(&self, device: &wgpu::Device, queue: &wgpu::Queue, adj: &crate::adjust::Adjustments, cutout: bool) -> Result<(u32, u32, Vec<[f32; 4]>)> {
        let (w, h) = adj.output_size(self.width, self.height);
        let src = [self.width as f32, self.height as f32];
        let band = ((64u32 << 20) / (w * 16)).clamp(1, h);
        let mut out = Vec::with_capacity((w * h) as usize);
        let mut y = 0;
        while y < h {
            let n = band.min(h - y);
            let mut u = adj.uniforms([0.0, y as f32 / h as f32, 1.0, n as f32 / h as f32], false, true, src, false);
            u.enhance2[3] = cutout as u32 as f32;
            out.extend(self.render_to_cpu(device, queue, &u, w, n)?);
            y += n;
        }
        Ok((w, h, out))
    }
}

/// Dessin de l'image développée dans la fenêtre egui.
pub struct ImageCallback {
    pub uniforms: Uniforms,
}

impl CallbackTrait for ImageCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(dev) = resources.get::<Developer>() {
            let mut u = dev.patch(&self.uniforms);
            u.lut[2] = dev.screen_lut_on as u32 as f32;
            queue.write_buffer(&dev.display_ubuf, 0, bytemuck::bytes_of(&u));
        }
        Vec::new()
    }

    fn paint(&self, _info: eframe::egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        if let Some(dev) = resources.get::<Developer>() {
            pass.set_pipeline(&dev.display_pipeline);
            pass.set_bind_group(0, &dev.display_bg, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// Dernière ligne (fraction de la hauteur) où le masque du ciel occupe encore une part notable de la largeur.
fn sky_bottom(mask: &[u8], w: u32, h: u32) -> f32 {
    let min = (w as usize / 200).max(1);
    (0..h as usize)
        .rev()
        .find(|&y| mask[y * w as usize..(y + 1) * w as usize].iter().filter(|&&v| v > 127).count() >= min)
        .map_or(0.75, |y| (y + 1) as f32 / h as f32)
}

/// Mise au point automatique : proximité typique de la zone centrale de l'image (le sujet y est le plus souvent),
/// pondérée vers le premier plan (75e centile).
fn auto_focus(d: &[f32], w: u32, h: u32) -> f32 {
    let (w, h) = (w as usize, h as usize);
    let mut v: Vec<f32> = (h / 4..h * 3 / 4).flat_map(|y| (w / 4..w * 3 / 4).map(move |x| d[y * w + x])).collect();
    if v.is_empty() {
        return 0.8;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() * 3 / 4]
}
