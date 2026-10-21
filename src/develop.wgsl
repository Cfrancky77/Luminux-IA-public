// Développement d'une image linéaire (sRGB primaires) vers sRGB encodé.
// Les réglages tonaux travaillent sur la luminance perceptuelle puis
// mettent à l'échelle les trois canaux, ce qui préserve les teintes.

struct U {
    uv_rect: vec4<f32>,
    wb: vec4<f32>,
    tone: vec4<f32>,   // contraste, hautes lumières, ombres, blancs
    tone2: vec4<f32>,  // noirs, saturation, vibrance, -
    misc: vec4<f32>,   // avant, mip 0, vignettage, départ du vignettage
    crop: vec4<f32>,   // recadrage normalisé x, y, l, h
    geo: vec4<f32>,    // angle (rad), largeur, hauteur source
    detail: vec4<f32>, // netteté, bruit luminance, bruit couleur
    local: vec4<f32>,  // clarté, texture, grain, taille du grain
    split_sh: vec4<f32>, // virage partiel : décalage ombres (rgb), équilibre
    split_hl: vec4<f32>, // décalage hautes lumières (rgb)
    bal_s: vec4<f32>,  // balance des couleurs ombres / tons moyens / hautes lumières
    bal_m: vec4<f32>,
    bal_h: vec4<f32>,
    lut: vec4<f32>,    // intensité de la LUT 3D, taille N, profil de l'écran actif
    mask_info: vec4<f32>, // nombre de masques, masque affiché (-1 : aucun)
    locals: array<vec4<f32>, 16>, // par masque : (expo, contraste, HL, ombres), (temp, teinte, sat, clarté)
    sky: vec4<f32>,     // ciel : opacité, décalage, zoom, gain
    sky2: vec4<f32>,    // rééclairage, largeur/hauteur du ciel, miroir
    sky_avg: vec4<f32>, // couleur moyenne du ciel
    sky3: vec4<f32>,    // étendue utile de la texture du ciel
    lens: vec4<f32>,       // objectif : actif, px -> unités Hugin, zoom, px -> unités du vignetage
    lens_dist: vec4<f32>,  // distorsion : r_src = r (1 + c1 r + c2 r² + c3 r³ + c4 r⁴)
    lens_tca_r: vec4<f32>, // aberration : r_canal = r (v + c r + b r²)
    lens_tca_b: vec4<f32>,
    lens_vig: vec4<f32>,   // vignetage : 1 + k1 r² + k2 r⁴ + k3 r⁶
    hsl: array<vec4<f32>, 8>,
    depth: vec4<f32>,  // relight : IL, portée, chaleur, assombrissement du fond
    depth2: vec4<f32>, // flou : quantité, mise au point (proximité), éclat
    depth3: vec4<f32>, // brume : quantité, luminosité, chaleur, carte disponible
    enhance: vec4<f32>,  // accent, ciel, structure, protéger le sujet
    enhance2: vec4<f32>, // masque de ciel disponible, masque du sujet disponible, -, export détouré
    face: vec4<f32>,  // portrait : lumière du visage, yeux, blanc des yeux, dents
    face2: vec4<f32>, // saturation des lèvres, rougeur des lèvres, lissage de la peau, brillance
    face3: vec4<f32>, // masques du visage disponibles, rayon du lissage (fraction du grand côté)
    reshape: vec4<f32>,  // amincir le visage, agrandir les yeux, silhouette, ventre
    reshape2: vec4<f32>, // nombre de visages, nombre d'yeux, nombre de personnes
    faces: array<vec4<f32>, 8>, // par visage : (milieu des yeux, axe des yeux), (écart des yeux)
    eyes: array<vec4<f32>, 8>,  // centre, rayon (pixels source)
    body: array<vec4<f32>, 256>, // 64 bandes par personne : axe x, demi-largeur, poids
    people: array<vec4<f32>, 8>, // par personne : (haut, bas, centre et étendue du ventre), (place libre à gauche, à droite)
    layers: array<vec4<f32>, 9>, // par calque : (opacité, fusion, masque, taille), (x, y, miroir, largeur/hauteur), (étendue utile)
}

// Paramètres tonaux effectifs d'un pixel : réglages globaux + réglages locaux pondérés par leurs masques.
struct P {
    gain: vec3<f32>,
    tone: vec4<f32>,
    tone2: vec4<f32>,
    clarity: f32,
    structure: f32,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var img: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@group(0) @binding(3) var lut: texture_2d<f32>;
// LUT 3D d'un look, stockée dans une texture 65³ dont seuls N³ texels servent.
@group(0) @binding(4) var lut3d: texture_3d<f32>;
const LUT3D_SIZE: f32 = 65.0;
const SCREEN_LUT: f32 = 33.0;
@group(0) @binding(5) var masks: texture_2d_array<f32>;
@group(0) @binding(6) var skytex: texture_2d<f32>;
@group(0) @binding(7) var screen_lut: texture_3d<f32>;
@group(0) @binding(8) var layertex: texture_2d_array<f32>;
const MAX_LAYERS: i32 = 3;

// Modes de fusion (ordre de `adjust::Blend`), sur des valeurs encodées sRGB comme les logiciels de retouche.
fn blend(a: vec3<f32>, b: vec3<f32>, mode: i32) -> vec3<f32> {
    switch mode {
        case 1: { return a * b; }
        case 2: { return 1.0 - (1.0 - a) * (1.0 - b); }
        case 3: { return select(1.0 - 2.0 * (1.0 - a) * (1.0 - b), 2.0 * a * b, a < vec3(0.5)); }
        case 4: { return (1.0 - 2.0 * b) * a * a + 2.0 * b * a; }
        case 5: { return max(a, b); }
        case 6: { return min(a, b); }
        default: { return b; }
    }
}
const SKY_LAYER: i32 = 8;
const DEPTH_LAYER: i32 = 9;
const SUBJECT_LAYER: i32 = 10;
// Masques du visage : peau, yeux, lèvres, dents.
const SKIN_LAYER: i32 = 11;
const EYES_LAYER: i32 = 12;
const LIPS_LAYER: i32 = 13;
const TEETH_LAYER: i32 = 14;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Triangle plein écran : couvre tout le viewport.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VOut;
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    let t = vec2<f32>(p.x, 1.0 - p.y);
    o.uv = u.uv_rect.xy + t * u.uv_rect.zw;
    return o;
}

fn encode(c: vec3<f32>) -> vec3<f32> {
    let x = max(c, vec3(0.0));
    return select(1.055 * pow(x, vec3(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3(0.0031308));
}

fn decode1(x: f32) -> f32 {
    if (x <= 0.04045) { return x / 12.92; }
    return pow((x + 0.055) / 1.055, 2.4);
}

fn encode1(x: f32) -> f32 {
    let y = max(x, 0.0);
    if (y <= 0.0031308) { return y * 12.92; }
    return 1.055 * pow(y, 1.0 / 2.4) - 0.055;
}

fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let d = mx - mn;
    var h = 0.0;
    if (d > 1e-6) {
        if (mx == c.r) { h = (c.g - c.b) / d; }
        else if (mx == c.g) { h = 2.0 + (c.b - c.r) / d; }
        else { h = 4.0 + (c.r - c.g) / d; }
        h = fract(h / 6.0 + 1.0);
    }
    var s = 0.0;
    if (mx > 1e-6) { s = d / mx; }
    return vec3(h, s, mx);
}

fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let k = vec3(1.0, 2.0 / 3.0, 1.0 / 3.0);
    let p = abs(fract(vec3(c.x) + k) * 6.0 - 3.0);
    return c.z * mix(vec3(1.0), clamp(p - 1.0, vec3(0.0), vec3(1.0)), c.y);
}

fn hue_dist(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn curve(x: f32) -> f32 {
    let t = clamp(x, 0.0, 1.0) * 255.0 / 256.0 + 0.5 / 256.0;
    return textureSampleLevel(lut, smp, vec2(t, 0.5), 0.0).r;
}

// Pixel affiché sans réglages : mode « Avant », ou moitié gauche de l'avant/après (geo.w = position du rideau).
var<private> before_px: bool;

// `big` et `mid` : moyennes locales (grand et moyen rayon) de l'image linéaire,
// pour la clarté, la texture, les ombres et les hautes lumières.
fn develop(lin: vec3<f32>, big: vec3<f32>, mid: vec3<f32>, pr: P) -> vec3<f32> {
    if (before_px) {
        return clamp(encode(lin), vec3(0.0), vec3(1.0));
    }
    let gain = pr.gain;
    var c = lin * gain;

    // Tonalité sur la luminance perceptuelle.
    let l = dot(c, vec3(0.2126, 0.7152, 0.0722));
    let p = clamp(encode1(l), 0.0, 1.5);
    var q = p;
    let x = clamp(p, 0.0, 1.0);
    // Clarté et texture : on renforce l'écart à la moyenne locale, surtout dans les tons moyens.
    let midw = 4.0 * x * (1.0 - x);
    if (pr.clarity != 0.0) {
        let pb = clamp(encode1(luma(big * gain)), 0.0, 1.5);
        q += pr.clarity * (p - pb) * midw;
    }
    if (u.local.y != 0.0) {
        let pm = clamp(encode1(luma(mid * gain)), 0.0, 1.5);
        q += 0.8 * u.local.y * (p - pm) * midw;
    }
    // Structure : détail moyen renforcé, mais pas dans les aplats (le bruit n'est pas amplifié).
    if (pr.structure != 0.0) {
        let pm = clamp(encode1(luma(mid * gain)), 0.0, 1.5);
        let pb = clamp(encode1(luma(big * gain)), 0.0, 1.5);
        let d = (p - pm) + 0.4 * (p - pb);
        q += 1.3 * pr.structure * d * smoothstep(0.004, 0.025, abs(p - pm)) * (0.35 + 0.65 * midw);
    }
    // Ombres et hautes lumières locales : le poids vient de la luminosité de la zone (moyennes locales),
    // pas du pixel seul. Toute la zone bouge d'un bloc et garde son détail ; un peu du pixel reste dans
    // le mélange pour limiter les halos le long des bords contrastés.
    if (pr.tone.z != 0.0 || pr.tone.y != 0.0) {
        let pb = clamp(encode1(luma(big * gain)), 0.0, 1.0);
        let pm = clamp(encode1(luma(mid * gain)), 0.0, 1.0);
        let xa = clamp(0.45 * pb + 0.3 * pm + 0.25 * x, 0.0, 1.0);
        let ws = smoothstep(0.75, 0.0, xa) * smoothstep(0.0, 0.08, x);
        let wh = smoothstep(0.3, 1.0, xa);
        q += 0.32 * pr.tone.z * ws;   // ombres
        q += 0.32 * pr.tone.y * wh * select(1.0, 1.0 - x * x, pr.tone.y > 0.0);   // hautes lumières, sans brûler le blanc
    }
    q += 0.15 * pr.tone.w * x * x * x;                          // blancs
    q += 0.15 * pr.tone2.x * (1.0 - x) * (1.0 - x) * (1.0 - x); // noirs
    let xc = clamp(q, 0.0, 1.0);
    q += pr.tone.x * (xc - 0.5) * (1.0 - abs(2.0 * xc - 1.0));  // contraste en S
    q = max(q, 0.0);
    if (l > 1e-6) {
        c *= decode1(q) / l;
    } else {
        c += vec3(decode1(q));
    }

    // Courbe des tons, par canal, en espace encodé.
    var e = encode(c);
    e = vec3(curve(e.r), curve(e.g), curve(e.b));

    // Saturation et vibrance.
    let y = dot(e, vec3(0.2126, 0.7152, 0.0722));
    let m = max(e.r, max(e.g, e.b)) - min(e.r, min(e.g, e.b));
    let s = (1.0 + pr.tone2.y) * (1.0 + pr.tone2.z * (1.0 - clamp(m, 0.0, 1.0)));
    e = mix(vec3(y), e, max(s, 0.0));

    // TSL par bande de teinte.
    var hsv = rgb2hsv(clamp(e, vec3(0.0), vec3(1.0)));
    var centers = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0);
    var dh = 0.0; var ds = 0.0; var dl = 0.0; var wsum = 0.0;
    for (var i = 0; i < 8; i++) {
        let w = max(0.0, 1.0 - hue_dist(hsv.x, centers[i] / 360.0) / (45.0 / 360.0));
        let b = u.hsl[i];
        dh += w * b.x; ds += w * b.y; dl += w * b.z; wsum += w;
    }
    if (wsum > 0.0) {
        let k = hsv.y; // les gris ne sont pas touchés
        dh /= wsum; ds /= wsum; dl /= wsum;
        hsv.x = fract(hsv.x + dh * k * 30.0 / 360.0 + 1.0);
        hsv.y = clamp(hsv.y * (1.0 + ds), 0.0, 1.0);
        hsv.z = clamp(hsv.z * (1.0 + 0.5 * dl * k), 0.0, 1.0);
        e = hsv2rgb(hsv);
    }

    // Virage partiel et balance des couleurs, pondérés par la luminosité.
    let yt = clamp(luma(e), 0.0, 1.0);
    let ws = (1.0 - yt) * (1.0 - yt);
    let wh = yt * yt;
    let wm = 4.0 * yt * (1.0 - yt);
    let bal = u.split_sh.w;
    e += 0.6 * (u.split_sh.rgb * ws * (1.0 - 0.5 * bal) + u.split_hl.rgb * wh * (1.0 + 0.5 * bal));
    e += 0.25 * (u.bal_s.rgb * ws + u.bal_m.rgb * wm + u.bal_h.rgb * wh);

    // LUT 3D (espace sRGB encodé, comme les LUT créatives .cube).
    if (u.lut.x > 0.0) {
        let n = u.lut.y;
        let coord = (clamp(e, vec3(0.0), vec3(1.0)) * (n - 1.0) + 0.5) / LUT3D_SIZE;
        let looked = textureSampleLevel(lut3d, smp, coord, 0.0).rgb;
        e = mix(e, looked, u.lut.x);
    }
    return clamp(e, vec3(0.0), vec3(1.0));
}

fn hash2(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2(0.1031, 0.1030));
    q += dot(q, q.yx + 33.33);
    return fract((q.x + q.y) * q.x);
}

// Bruit de valeur lissé, -0.5..0.5.
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let s = f * f * (3.0 - 2.0 * f);
    let a = hash2(i);
    let b = hash2(i + vec2(1.0, 0.0));
    let c = hash2(i + vec2(0.0, 1.0));
    let d = hash2(i + vec2(1.0, 1.0));
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y) - 0.5;
}

// Coordonnée de sortie (dans le recadrage) -> coordonnée dans la source.
// Même convention que `crop_inside` côté Rust.
fn geo_map(uv: vec2<f32>) -> vec2<f32> {
    let size = u.geo.yz;
    let p = (u.crop.xy + uv * u.crop.zw) * size - size * 0.5;
    let c = cos(u.geo.x);
    let s = sin(u.geo.x);
    let q = vec2(c * p.x + s * p.y, -s * p.x + c * p.y);
    return (q + size * 0.5) / size;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

fn fetch(uv: vec2<f32>, lod: f32) -> vec3<f32> {
    return max(textureSampleLevel(img, smp, uv, lod).rgb, vec3(0.0));
}

// Remodelage du portrait : position affichée -> position lue dans la source (pixels source).
// Chaque déformation est l'inverse d'un déplacement monotone, sans repli de l'image.
fn warp(p0: vec2<f32>) -> vec2<f32> {
    var p = p0;
    // Yeux agrandis : les points lus se rapprochent du centre de l'œil.
    let e = u.reshape.y * 0.3;
    if (e > 0.0) {
        for (var i = 0; i < i32(u.reshape2.y); i++) {
            let c = u.eyes[i].xy;
            let x = length(p - c) / u.eyes[i].z;
            if (x < 1.0) {
                let f = 1.0 - x * x;
                p = c + (p - c) * (1.0 - e * f * f);
            }
        }
    }
    // Visage aminci : les joues et la mâchoire se resserrent vers l'axe du visage.
    let s = u.reshape.x * 0.2;
    if (s > 0.0) {
        for (var i = 0; i < i32(u.reshape2.x); i++) {
            let o = u.faces[2 * i].xy;
            let ax = u.faces[2 * i].zw;
            let d = u.faces[2 * i + 1].x;
            let v = vec2(-ax.y, ax.x);
            let a = dot(p - o, ax) / d;
            let b = dot(p - o, v) / d;
            let wy = smoothstep(-0.2, 0.5, b) * (1.0 - smoothstep(1.4, 2.1, b));
            let wa = exp(-(a * a) / 1.21);
            p = o + (a * (1.0 + s * wy * wa) * ax + b * v) * d;
        }
    }
    // Silhouette et ventre : pour chaque personne, par bande, la largeur du corps change et le fond
    // voisin compense, sans dépasser la place libre (bord de l'image ou personne voisine).
    let k = u.reshape.z * 0.1;
    let kb = u.reshape.w * 0.12;
    for (var j = 0; j < i32(u.reshape2.z); j++) {
        let pp = u.people[2 * j];
        let room = u.people[2 * j + 1].xy;
        if (p.y < pp.x || p.y > pp.y) {
            continue;
        }
        let t = clamp((p.y - pp.x) / max(pp.y - pp.x, 1.0) * 64.0 - 0.5, 0.0, 63.0);
        let i0 = u32(floor(t));
        let i1 = min(i0 + 1u, 63u);
        let row = mix(u.body[64 * j + i32(i0)], u.body[64 * j + i32(i1)], fract(t));
        var a = k * row.z;
        if (kb > 0.0 && pp.w > 0.0) {
            let z = (p.y - pp.z) / pp.w;
            a += kb * exp(-z * z) * row.z;
        }
        if (a != 0.0 && row.y > 1.0) {
            let side = select(room.x, room.y, p.x > row.x);
            let outer = min(1.6 * row.y, side);
            let hw = min(row.y, outer / 1.25);
            let inner = hw * (1.0 - a);
            let dx = abs(p.x - row.x);
            var src = dx;
            if (dx <= inner) {
                src = dx / (1.0 - a);
            } else if (dx <= outer) {
                src = hw + (dx - inner) * (outer - hw) / (outer - inner);
            }
            p.x = row.x + sign(p.x - row.x) * src;
        }
    }
    return p;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    // misc.x : 0 = résultat, 1 = original, entre les deux = fondu (les deux sont calculés).
    let f = u.misc.x;
    if (u.geo.w > 0.0) {
        return develop_px(in, in.uv.x < u.geo.w);
    }
    if (f <= 0.0) {
        return develop_px(in, false);
    }
    if (f >= 1.0) {
        return develop_px(in, true);
    }
    return mix(develop_px(in, false), develop_px(in, true), f);
}

fn develop_px(in: VOut, before: bool) -> vec4<f32> {
    before_px = before;
    let dims = u.geo.yz;
    var suv = geo_map(in.uv);
    // Correction d'objectif : position dans l'image corrigée -> position dans la source.
    let lens_on = u.lens.x > 0.5 && !before_px;
    var suv_r = suv;
    var suv_b = suv;
    var vig_gain = 1.0;
    if (lens_on) {
        let q = (suv - 0.5) * dims / u.lens.z;
        let r = length(q) * u.lens.y;
        let c = u.lens_dist;
        let pd = q * (1.0 + r * (c.x + r * (c.y + r * (c.z + r * c.w))));
        let rd = length(pd) * u.lens.y;
        let tr = u.lens_tca_r;
        let tb = u.lens_tca_b;
        suv = pd / dims + 0.5;
        suv_r = pd * (tr.x + rd * (tr.y + rd * tr.z)) / dims + 0.5;
        suv_b = pd * (tb.x + rd * (tb.y + rd * tb.z)) / dims + 0.5;
        let rv2 = dot(pd, pd) * u.lens.w * u.lens.w;
        let k = u.lens_vig;
        vig_gain = 1.0 / max(1.0 + rv2 * (k.x + rv2 * (k.y + rv2 * k.z)), 0.05);
    }
    if (!before_px && (u.reshape2.x + u.reshape2.y + u.reshape2.z) > 0.0) {
        let shift = (warp(suv * dims) - suv * dims) / dims;
        suv += shift;
        suv_r += shift;
        suv_b += shift;
    }
    // Niveau de détail : 0 à l'export, sinon celui de l'affichage (effets cohérents au zoom).
    let dx = dpdx(suv * dims);
    let dy = dpdy(suv * dims);
    var lod = max(0.0, 0.5 * log2(max(dot(dx, dx), dot(dy, dy))));
    if (u.misc.y > 0.5) {
        lod = 0.0;
    }
    if (any(suv < vec2(0.0)) || any(suv > vec2(1.0))) {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }
    let texel = exp2(lod) / dims;
    var lin = fetch(suv, lod);
    // Aberration chromatique : rouge et bleu lus à leur propre position.
    var tca_d = vec3(0.0);
    if (lens_on) {
        tca_d = vec3(fetch(suv_r, lod).r - lin.r, 0.0, fetch(suv_b, lod).b - lin.b);
        lin = max(lin + tca_d, vec3(0.0));
    }

    if (!before_px && (u.detail.x > 0.0 || u.detail.y > 0.0 || u.detail.z > 0.0)) {
        // Filtre bilatéral 5x5 en espace perceptuel (racine carrée) pour le bruit,
        // et flou 3x3 pour le masque de netteté.
        let pc = sqrt(lin);
        let lc = luma(pc);
        let sl = 0.015 + 0.12 * u.detail.y;
        let sc = 0.04 + 0.35 * u.detail.z;
        var acc_l = vec3(0.0); var w_l = 0.0;
        var acc_c = vec3(0.0); var w_c = 0.0;
        var blur = vec3(0.0); var w_b = 0.0;
        for (var j = -2; j <= 2; j++) {
            for (var i = -2; i <= 2; i++) {
                let o = vec2(f32(i), f32(j));
                let q = sqrt(fetch(suv + o * texel, lod));
                let d2 = dot(o, o);
                let ws = exp(-d2 / 4.5);
                let dl = luma(q) - lc;
                let wl = ws * exp(-dl * dl / (2.0 * sl * sl));
                let wc = ws * exp(-dl * dl / (2.0 * sc * sc));
                acc_l += q * wl; w_l += wl;
                acc_c += q * wc; w_c += wc;
                if (d2 <= 2.0) {
                    let wb = exp(-d2);
                    blur += q * wb; w_b += wb;
                }
            }
        }
        let den_l = mix(pc, acc_l / w_l, u.detail.y);
        let den_c = mix(pc, acc_c / w_c, u.detail.z);
        // Luminance du premier, chrominance du second.
        var outp = den_c - vec3(luma(den_c)) + vec3(luma(den_l));
        // Masque flou sur la luminance seulement, sans halos de couleur.
        let hp = lc - luma(blur / w_b);
        outp += vec3(2.5 * u.detail.x * hp);
        lin = max(outp, vec3(0.0));
        lin = lin * lin;
        lin = max(lin + tca_d, vec3(0.0));
    }
    lin *= vig_gain;

    // Portrait : peau lissée et brillance atténuée, yeux plus nets, dans l'espace linéaire.
    let face_on = !before_px && u.face3.x > 0.5;
    var skin_m = 0.0;
    var eyes_m = 0.0;
    var lips_m = 0.0;
    var teeth_m = 0.0;
    if (face_on) {
        skin_m = textureSampleLevel(masks, smp, suv, SKIN_LAYER, 0.0).r;
        eyes_m = textureSampleLevel(masks, smp, suv, EYES_LAYER, 0.0).r;
        lips_m = textureSampleLevel(masks, smp, suv, LIPS_LAYER, 0.0).r;
        teeth_m = textureSampleLevel(masks, smp, suv, TEETH_LAYER, 0.0).r;
    }
    if (face_on && skin_m > 0.01 && (u.face2.z > 0.0 || u.face2.w > 0.0)) {
        // Rayon proportionnel à la taille des visages : efface pores et petites imperfections, garde les traits.
        let rpx = max(u.face3.y * max(dims.x, dims.y), 1.0);
        let lodb = max(lod, log2(max(rpx / 4.0, 1.0)));
        let lc = luma(sqrt(lin));
        var acc = lin;
        var wsum = 1.0;
        for (var k = 0; k < 16; k++) {
            let fk = f32(k);
            let a = fk * 2.39996;
            let o = vec2(cos(a), sin(a)) * rpx * sqrt((fk + 0.5) / 16.0);
            let c = fetch(suv + o / dims, lodb) * vig_gain;
            // Bilatéral : les sourcils, cils et contours ne bavent pas sur la peau.
            let dl = luma(sqrt(c)) - lc;
            let w = exp(-dl * dl / (2.0 * 0.05 * 0.05));
            acc += c * w;
            wsum += w;
        }
        let soft = acc / wsum;
        let a = u.face2.z * skin_m;
        if (a > 0.0) {
            // Le grain le plus fin est en partie conservé pour ne pas donner une peau de plastique.
            let fine = lin - fetch(suv, lod + 1.0) * vig_gain;
            lin = max(mix(lin, soft, 0.9 * a) + 0.35 * a * fine, vec3(0.0));
        }
        let sh = u.face2.w * skin_m;
        if (sh > 0.0) {
            let around = fetch(suv, max(lod + 1.0, log2(max(rpx * 2.0, 1.0)))) * vig_gain;
            let excess = luma(lin) / max(luma(around), 1e-4) - 1.05;
            let k = sh * smoothstep(0.0, 0.5, excess);
            lin = mix(lin, around * 1.05, 0.85 * k);
        }
    }
    if (face_on && eyes_m > 0.01 && u.face.y > 0.0) {
        lin = max(lin + 1.5 * u.face.y * eyes_m * (lin - fetch(suv, lod + 1.5) * vig_gain), vec3(0.0));
    }

    // Outils de profondeur : proximité 0 (loin) .. 1 (proche).
    let depth_on = !before_px && u.depth3.w > 0.5;
    var near = 0.0;
    if (depth_on) {
        near = textureSampleLevel(masks, smp, suv, DEPTH_LAYER, 0.0).r;
    }
    // Flou d'arrière-plan : disque de 24 échantillons dont le rayon croît avec l'écart à la mise au point.
    if (depth_on && u.depth2.x > 0.0) {
        let coc = clamp((abs(near - u.depth2.y) - 0.03) * 4.0, 0.0, 1.0) * u.depth2.x;
        if (coc > 0.005) {
            let rpx = coc * 0.025 * max(dims.x, dims.y);
            let lodb = max(lod, log2(max(rpx / 6.0, 1.0)));
            var acc = vec3(0.0);
            var wsum = 0.0;
            for (var k = 0; k < 24; k++) {
                let fk = f32(k);
                let a = fk * 2.39996;
                let o = vec2(cos(a), sin(a)) * rpx * sqrt((fk + 0.5) / 24.0);
                let p = suv + o / dims;
                let dk = textureSampleLevel(masks, smp, p, DEPTH_LAYER, 0.0).r;
                // Un premier plan net ne déborde pas sur le fond flou.
                var w = 1.0;
                if (dk > near + 0.06) {
                    w = 0.03;
                }
                let c = fetch(p, lodb) * vig_gain;
                // Éclat : les points lumineux dominent le disque, comme un vrai bokeh.
                w *= 1.0 + u.depth2.z * 8.0 * smoothstep(0.6, 2.0, luma(c) * u.wb.w);
                acc += c * w;
                wsum += w;
            }
            lin = mix(lin, acc / wsum, smoothstep(0.0, 0.2, coc));
        }
    }

    // Remplacement du ciel, dans l'espace linéaire de la scène avant développement.
    var shown = 0.0;
    var sky_a = 0.0;
    var sky_c = vec3(0.0);
    if (!before_px && u.sky.x > 0.0) {
        let m = textureSampleLevel(masks, smp, suv, SKY_LAYER, 0.0).r;
        if (u.mask_info.y == f32(SKY_LAYER)) {
            shown = m;
        }
        // Le ciel couvre la largeur de la photo ; agrandi si besoin pour descendre jusqu'au bas du ciel détecté
        // (sky3.z), sans quoi sa dernière ligne serait étirée en traînées verticales.
        let fit = max(1.0, min(1.0, u.sky3.z + 0.02) * dims.y / dims.x * u.sky2.y);
        let sc = u.sky.z * fit;
        var sx = (suv.x - 0.5) / sc + 0.5;
        if (u.sky2.z > 0.5) {
            sx = 1.0 - sx;
        }
        var sy = abs(suv.y * dims.y / dims.x * u.sky2.y / sc + u.sky.y);
        // Au-delà du bord (position décalée) : miroir plutôt qu'étirement.
        if (sy > 1.0) {
            sy = max(2.0 - sy, 0.0);
        }
        let half_texel = vec2(0.5 / 2048.0);
        let st = clamp(vec2(sx, sy) * u.sky3.xy, half_texel, u.sky3.xy - half_texel);
        let sk = max(textureSampleLevel(skytex, smp, st, 0.0).rgb, vec3(0.0)) * u.sky.w;
        let a = m * u.sky.x;
        // La scène prend un peu de la couleur du nouveau ciel.
        lin *= mix(vec3(1.0), u.sky_avg.rgb, u.sky2.x * 0.25 * u.sky.x * (1.0 - m));
        lin = mix(lin, sk, a);
        sky_a = a;
        sky_c = sk;
    }

    if (depth_on) {
        // Relight : lumière posée sur la scène jusqu'à la portée choisie, fond assombri au-delà.
        let edge = 1.0 - u.depth.y;
        let wl = smoothstep(edge - 0.2, edge + 0.2, near);
        lin *= exp2(1.5 * u.depth.x * wl - 1.5 * u.depth.w * (1.0 - wl));
        lin *= mix(vec3(1.0), vec3(1.0 + 0.25 * u.depth.z, 1.0, 1.0 - 0.25 * u.depth.z), wl * abs(sign(u.depth.x)));
        // Atmosphère : brume qui s'épaissit avec la distance.
        if (u.depth3.x > 0.0) {
            let fog = u.depth3.x * pow(1.0 - near, 1.6);
            let haze = vec3(0.78 + 0.08 * u.depth3.z, 0.80, 0.84 - 0.08 * u.depth3.z) * 0.55 * u.depth3.y;
            lin = mix(lin, haze, clamp(fog, 0.0, 0.95));
        }
    }

    // Moyennes locales lues dans les mips : rayon proportionnel à la taille de l'image,
    // donc même rendu à l'écran et à l'export.
    var pr: P;
    pr.gain = u.wb.rgb * u.wb.w;
    pr.tone = u.tone;
    pr.tone2 = u.tone2;
    pr.clarity = u.local.x;
    pr.structure = 0.0;
    let nmask = i32(u.mask_info.x);
    for (var i = 0; i < nmask; i++) {
        let m = textureSampleLevel(masks, smp, suv, i, 0.0).r;
        if (m > 0.0) {
            let a = u.locals[2 * i];
            let b = u.locals[2 * i + 1];
            let t = b.x * 0.5 * m;
            let g = b.y * 0.3 * m;
            pr.gain *= exp2(a.x * m) * vec3(max(1.0 + t, 0.05), max(1.0 - g, 0.05), max(1.0 - t, 0.05));
            pr.tone.x += a.y * m;
            pr.tone.y += a.z * m;
            pr.tone.z += a.w * m;
            pr.tone2.y += b.z * m;
            pr.clarity += b.w * m;
        }
        if (f32(i) == u.mask_info.y) {
            shown = m;
        }
    }
    // Amélioration IA : accent adapté à la luminosité de la zone, ciel, structure hors ciel et sujet.
    if (!before_px && (u.enhance.x > 0.0 || u.enhance.y > 0.0 || u.enhance.z != 0.0)) {
        var sky_m = 0.0;
        var subj_m = 0.0;
        if (u.enhance2.x > 0.5) {
            sky_m = textureSampleLevel(masks, smp, suv, SKY_LAYER, 0.0).r;
        }
        if (u.enhance2.y > 0.5 && u.enhance.w > 0.5) {
            subj_m = textureSampleLevel(masks, smp, suv, SUBJECT_LAYER, 0.0).r;
        }
        let a = u.enhance.x;
        if (a > 0.0) {
            let wide = fetch(suv, max(lod + 3.0, log2(max(dims.x, dims.y)) - 4.5));
            let lw = clamp(encode1(luma(wide * pr.gain)), 0.0, 1.0);
            pr.tone.z += a * 1.1 * (1.0 - lw) * (1.0 - lw);
            pr.tone.y -= a * 0.9 * lw * lw;
            pr.tone.x += a * 0.1;
            pr.tone2.z += a * 0.4;
            pr.clarity += a * 0.3;
        }
        let s = u.enhance.y * sky_m;
        if (s > 0.0) {
            pr.gain *= exp2(-0.45 * s);
            pr.tone.x += 0.25 * s;
            pr.tone2.y += 0.35 * s;
            pr.clarity += 0.25 * s;
        }
        pr.structure = u.enhance.z * (1.0 - sky_m) * (1.0 - subj_m);
    }
    if (face_on) {
        // Lumière du visage et yeux plus vifs.
        let f = max(max(skin_m, eyes_m), max(lips_m, teeth_m));
        let l = u.face.x * f;
        pr.gain *= exp2(0.6 * l + 0.25 * u.face.y * eyes_m);
        pr.tone.z += 0.3 * l;
        pr.tone.x += 0.3 * u.face.y * eyes_m;
        pr.tone2.y += 0.25 * u.face.y * eyes_m;
    }
    var big = lin;
    var mid = lin;
    if (pr.clarity != 0.0 || u.local.y != 0.0 || pr.structure != 0.0 || pr.tone.y != 0.0 || pr.tone.z != 0.0) {
        let base = log2(max(dims.x, dims.y));
        big = fetch(suv, max(lod + 2.0, base - 6.0));
        mid = fetch(suv, max(lod + 1.0, base - 8.5));
        // Pas de contraste local calculé contre l'ancien ciel.
        big = mix(big, sky_c, sky_a);
        mid = mix(mid, sky_c, sky_a);
    }
    var e = develop(lin, big, mid, pr);
    if (face_on) {
        // Blanc des yeux : zones claires et peu colorées de l'œil.
        let y = luma(e);
        let sat = max(max(e.r, e.g), e.b) - min(min(e.r, e.g), e.b);
        let wy = u.face.z * eyes_m * smoothstep(0.3, 0.55, y) * (1.0 - smoothstep(0.12, 0.35, sat));
        e = mix(e, vec3(y), 0.7 * wy);
        e += 0.12 * wy * (1.0 - e);
        // Dents : jaune retiré et éclaircies, sans toucher à l'ombre de la bouche.
        let wt = u.face.w * teeth_m * smoothstep(0.2, 0.45, y);
        e = mix(e, vec3(luma(e)), 0.75 * wt);
        e += 0.1 * wt * (1.0 - e);
        // Lèvres : saturation, puis rougeur.
        let yl = luma(e);
        e = max(vec3(yl) + (e - vec3(yl)) * (1.0 + u.face2.x * lips_m), vec3(0.0));
        let r = u.face2.y * lips_m;
        e = clamp(e + r * vec3(0.08, -0.04, -0.02), vec3(0.0), vec3(1.0));
    }
    if (!before_px && u.misc.z != 0.0) {
        let aspect = u.crop.z * dims.x / (u.crop.w * dims.y);
        let d = length((in.uv - 0.5) * vec2(aspect, 1.0)) / length(vec2(aspect, 1.0) * 0.5);
        let k = smoothstep(u.misc.w, 1.0 + 0.001, d);
        e = clamp(e * (1.0 + u.misc.z * k), vec3(0.0), vec3(1.0));
    }
    if (!before_px && u.local.z > 0.0) {
        // Grain ancré sur les pixels source ; atténué quand l'affichage est réduit.
        let cell = 1.0 + 3.0 * u.local.w;
        let gp = suv * dims / cell;
        let g = value_noise(gp) + 0.5 * value_noise(gp * 2.17 + 19.0);
        let fade = min(1.0, cell / exp2(lod));
        let y = luma(e);
        e = clamp(e + vec3(g * u.local.z * 0.16 * fade * (0.3 + 4.0 * y * (1.0 - y))), vec3(0.0), vec3(1.0));
    }
    // Calques, du plus bas au plus haut, placés dans le cadre de sortie (taille 1 = cadre couvert).
    if (!before_px) {
        let frame = u.crop.z * dims.x / (u.crop.w * dims.y);
        for (var i = 0; i < MAX_LAYERS; i++) {
            let a0 = u.layers[3 * i];
            let a1 = u.layers[3 * i + 1];
            let a2 = u.layers[3 * i + 2];
            if (a0.x <= 0.0) {
                continue;
            }
            // Demi-taille du calque en fraction du cadre, puis rotation dans un repère aux unités égales
            // en x et en y (hauteur du cadre).
            var h = vec2(0.5 * a0.w, 0.5 * a0.w * frame / a1.w);
            if (a1.w > frame) {
                h = vec2(0.5 * a0.w * a1.w / frame, 0.5 * a0.w);
            }
            let p = in.uv - 0.5 - a1.xy;
            let q = vec2(p.x * frame, p.y);
            let cs = vec2(cos(a2.z), sin(a2.z));
            let r = vec2(cs.x * q.x + cs.y * q.y, cs.x * q.y - cs.y * q.x);
            var t = vec2(r.x / frame, r.y) / (2.0 * h) + 0.5;
            if (a1.z > 0.5) {
                t.x = 1.0 - t.x;
            }
            if (any(t < vec2(0.0)) || any(t > vec2(1.0))) {
                continue;
            }
            let c = textureSampleLevel(layertex, smp, t * a2.xy, i, 0.0);
            var m = 1.0;
            let mk = i32(a0.z + 0.5);
            if (mk == 1) {
                m = select(0.0, textureSampleLevel(masks, smp, suv, SKY_LAYER, 0.0).r, u.enhance2.x > 0.5);
            } else if (mk >= 2) {
                let s = select(0.0, textureSampleLevel(masks, smp, suv, SUBJECT_LAYER, 0.0).r, u.enhance2.y > 0.5);
                m = select(s, select(0.0, 1.0 - s, u.enhance2.y > 0.5), mk == 3);
            }
            let k = clamp(c.a * a0.x * m, 0.0, 1.0);
            e = mix(e, clamp(blend(e, c.rgb, i32(a0.y + 0.5)), vec3(0.0), vec3(1.0)), k);
        }
    }
    if (depth_on && u.mask_info.y == f32(DEPTH_LAYER)) {
        e = vec3(near);
    }
    if (shown > 0.0) {
        e = mix(e, vec3(1.0, 0.15, 0.15), shown * 0.55);
    }
    // Détourage : aperçu sur damier, ou alpha = masque du sujet à l'export.
    var subj = 1.0;
    if (u.enhance2.y > 0.5) {
        subj = textureSampleLevel(masks, smp, suv, SUBJECT_LAYER, 0.0).r;
    } else if (u.enhance2.w > 0.5) {
        subj = 0.0;
    }
    if (u.mask_info.y == f32(SUBJECT_LAYER) && u.enhance2.y > 0.5) {
        let cell = floor(in.pos.xy / 10.0);
        let checker = select(0.62, 0.82, (i32(cell.x) + i32(cell.y)) % 2 == 0);
        e = mix(vec3(checker), e, subj);
    }
    // Profil de l'écran (affichage seulement) : sRGB -> espace de l'écran.
    if (u.lut.z > 0.5) {
        let coord = (clamp(e, vec3(0.0), vec3(1.0)) * (SCREEN_LUT - 1.0) + 0.5) / SCREEN_LUT;
        e = textureSampleLevel(screen_lut, smp, coord, 0.0).rgb;
    }
    if (u.enhance2.w > 0.5) {
        return vec4(e, subj);
    }
    return vec4(e, 1.0);
}

// Réduction d'un niveau de mip (moyenne 2x2 via le filtrage bilinéaire).
@group(0) @binding(0) var mip_src: texture_2d<f32>;
@group(0) @binding(1) var mip_smp: sampler;

@fragment
fn fs_mip(in: VOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(mip_src, mip_smp, in.uv, 0.0);
}

@vertex
fn vs_mip(@builtin(vertex_index) i: u32) -> VOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VOut;
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}
