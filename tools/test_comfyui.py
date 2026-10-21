#!/usr/bin/env python3
"""Test des requêtes de Luminux IA contre ton vrai ComfyUI et ton Ollama.

Lancer :  python3 test_comfyui.py [photo.jpg]
Serveurs : COMFY=http://machine:8188 OLLAMA=http://machine:11434 python3 test_comfyui.py
(par défaut : ce PC).
Avec une photo, le test d'agrandissement du cadre se fait aussi sur elle.
Rien n'est installé ni modifié sur le serveur. Les images de test et le rapport
(rapport_luminux.txt) sont écrits dans le dossier luminux_test/ à côté du script.
"""
import json, os, struct, sys, time, urllib.request, urllib.error, uuid, zlib

COMFY = os.environ.get("COMFY", "http://127.0.0.1:8188").rstrip("/")
OLLAMA = os.environ.get("OLLAMA", "http://127.0.0.1:11434").rstrip("/")
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "luminux_test")
os.makedirs(OUT, exist_ok=True)
LOG = open(os.path.join(OUT, "rapport_luminux.txt"), "w", encoding="utf-8")


def log(*a):
    s = " ".join(str(x) for x in a)
    print(s)
    LOG.write(s + "\n")
    LOG.flush()


def req(method, url, body=None, ctype="application/json", timeout=120):
    data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    r = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        r.add_header("Content-Type", ctype)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def png(w, h, rows, rgba):
    """PNG minimal (8 bits, RGB ou RGBA) sans dépendance."""
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6 if rgba else 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")


def scene(x, y, w, h):
    """Paysage simple : ciel en dégradé, collines, sol."""
    hill = 0.55 * h + 0.08 * h * __import__("math").sin(x / w * 6.0)
    if y < hill:
        t = y / hill
        return (int(90 + 80 * t), int(140 + 70 * t), int(230 - 20 * t))
    t = (y - hill) / (h - hill)
    return (int(60 + 40 * t), int(120 - 30 * t), int(50 + 10 * t))


def upload(data, name):
    b = "----luminux" + uuid.uuid4().hex
    body = (f"--{b}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n").encode()
    body += data + f"\r\n--{b}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\ntrue\r\n--{b}--\r\n".encode()
    st, r = req("POST", COMFY + "/upload/image", body, "multipart/form-data; boundary=" + b)
    if st != 200:
        raise RuntimeError(f"envoi refusé ({st}) : {r[:500]!r}")
    v = json.loads(r)
    return (v["subfolder"] + "/" + v["name"]) if v.get("subfolder") else v["name"]


def run(graph, outname):
    st, r = req("POST", COMFY + "/prompt", {"prompt": graph, "client_id": "luminux"})
    if st != 200:
        log(f"  /prompt refusé ({st}) :", r.decode(errors="replace")[:2000])
        return None
    pid = json.loads(r)["prompt_id"]
    t0 = time.time()
    while time.time() - t0 < 900:
        time.sleep(1)
        _, h = req("GET", f"{COMFY}/history/{pid}")
        e = json.loads(h).get(pid)
        if not e:
            continue
        s = e.get("status", {})
        if s.get("status_str") == "error":
            msgs = [m[1].get("exception_message") for m in s.get("messages", []) if m[0] == "execution_error"]
            log("  ÉCHEC d'exécution :", msgs)
            return None
        imgs = e.get("outputs", {}).get("9", {}).get("images")
        if imgs:
            i = imgs[0]
            q = urllib.parse.urlencode({"filename": i["filename"], "subfolder": i.get("subfolder", ""), "type": i.get("type", "output")})
            _, data = req("GET", f"{COMFY}/view?{q}")
            path = os.path.join(OUT, outname)
            open(path, "wb").write(data)
            w, hh = struct.unpack(">II", data[16:24])
            log(f"  OK en {time.time() - t0:.1f} s : {w} x {hh} -> {path}")
            return data
        if s.get("completed"):
            log("  terminé sans image")
            return None
    log("  pas de résultat après 15 min")
    return None


import urllib.parse


def combo(inp):
    """Ancien format [[a, b], {}] ou nouveau ["COMBO", {"options": [a, b]}]."""
    if isinstance(inp[0], list):
        return inp[0]
    return inp[1].get("options", []) if inp[0] == "COMBO" else []


def sampler(ck, inp, pixels, mask, variant):
    """Graphe d'inpainting : A = VAEEncodeForInpaint (ancienne version), B = InpaintModelConditioning (nouvelle)."""
    g = {
        "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": ck}},
        "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "a natural continuation of the landscape, photo", "clip": ["1", 1]}},
        "4": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry, deformed, lowres, text, watermark, frame, border", "clip": ["1", 1]}},
        "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["1", 2]}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "luminux"}},
    }
    ks = {"model": ["1", 0], "seed": 12345, "steps": 28, "cfg": 6.5, "sampler_name": "dpmpp_2m", "scheduler": "karras", "denoise": 1.0}
    if variant == "A":
        g["6"] = ({"class_type": "VAEEncodeForInpaint", "inputs": {"pixels": pixels, "vae": ["1", 2], "mask": mask, "grow_mask_by": 8}} if inp
                  else {"class_type": "SetLatentNoiseMask", "inputs": {"samples": ["5", 0], "mask": mask}})
        g["5"] = {"class_type": "VAEEncode", "inputs": {"pixels": pixels, "vae": ["1", 2]}}
        ks.update(positive=["3", 0], negative=["4", 0], latent_image=["6", 0])
    else:
        g["6"] = {"class_type": "InpaintModelConditioning", "inputs": {"positive": ["3", 0], "negative": ["4", 0], "vae": ["1", 2],
                                                                      "pixels": pixels, "mask": mask, "noise_mask": True}}
        ks.update(positive=["6", 0], negative=["6", 1], latent_image=["6", 2])
    g["7"] = {"class_type": "KSampler", "inputs": ks}
    return g

# Photo vérifiée avant tout essai, pour ne pas tester sans elle.
if len(sys.argv) > 1 and not os.path.isfile(os.path.expanduser(sys.argv[1])):
    sys.exit(f"Photo introuvable : {sys.argv[1]} (exemple : python3 test_comfyui.py ~/Images/IMG_1234.jpg)")
log("script de test Luminux v4")
log("=== ComfyUI", COMFY)
try:
    st, r = req("GET", COMFY + "/system_stats", timeout=10)
    s = json.loads(r)
    log("version :", s.get("system", {}).get("comfyui_version"), "| GPU :", [(d.get("name"), round(d.get("vram_total", 0) / 2**30, 1)) for d in s.get("devices", [])])
except Exception as e:
    log("ComfyUI injoignable :", e)
    sys.exit(1)
_, r = req("GET", COMFY + "/object_info/CheckpointLoaderSimple")
ckpts = combo(json.loads(r)["CheckpointLoaderSimple"]["input"]["required"]["ckpt_name"])
log("checkpoints :", ckpts)
_, r = req("GET", COMFY + "/object_info/UpscaleModelLoader")
try:
    ups = combo(json.loads(r)["UpscaleModelLoader"]["input"]["required"]["model_name"])
except Exception:
    ups = []
log("modèles d'agrandissement :", ups or "aucun")

# 1. Agrandir le cadre : bandes latérales transparentes à remplir.
W, H, L, R = 1024, 768, 154, 153
rows = []
for y in range(H):
    row = []
    for x in range(W):
        c = scene(x, y, W, H)
        row += [*c, 0 if (x < L or x >= W - R) else 255]
    rows.append(row)
name = upload(png(W, H, rows, True), "luminux_test.png")
inp = [c for c in ckpts if "inpaint" in c.lower()]
ck = inp[0] if inp else ckpts[0]
log(f"\n--- Agrandir le cadre avec {ck} (inpainting : {'oui' if inp else 'non'})")
for v in "AB":
    log(f"  variante {v} :")
    g = sampler(ck, bool(inp), ["2", 0], ["2", 1], v)
    g["2"] = {"class_type": "LoadImage", "inputs": {"image": name}}
    run(g, f"agrandir_cadre_{v}.png")

# Même test sur une vraie photo, réduite à 0,6 Mpx, 256 px ajoutés de chaque côté.
# B : bandes grises (ImagePadForOutpaint), comme la v3.
# C : bandes pré-remplies par la photo étirée et floutée, modèle d'inpainting, denoise 0,85.
# D : mêmes bandes pré-remplies, modèle classique (turbo) avec masque de bruit, denoise 0,9.
def image_size(data):
    """Largeur, hauteur d'un JPEG ou d'un PNG (sans bibliothèque)."""
    if data[:8] == b"\x89PNG\r\n\x1a\n":
        return int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")
    i = 2
    while i < len(data) - 9:
        if data[i] != 0xFF:
            i += 1
            continue
        m = data[i + 1]
        if m in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
            return int.from_bytes(data[i + 7:i + 9], "big"), int.from_bytes(data[i + 5:i + 7], "big")
        i += 2 + int.from_bytes(data[i + 2:i + 4], "big")
    raise ValueError("format d'image non reconnu (JPEG ou PNG attendu)")


if len(sys.argv) > 1:
    photo = os.path.expanduser(sys.argv[1])
    data = open(photo, "rb").read()
    pw, ph = image_size(data)
    k = (0.6e6 / (pw * ph)) ** 0.5
    w0, h0 = max(64, round(pw * k / 8) * 8), max(64, round(ph * k / 8) * 8)
    log(f"\n--- Agrandir le cadre sur {photo} ({pw} x {ph}, réduite à {w0} x {h0})")
    ext = os.path.splitext(photo)[1].lower() or ".jpg"
    b = "----luminux" + uuid.uuid4().hex
    body = (f"--{b}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"luminux_photo{ext}\"\r\nContent-Type: application/octet-stream\r\n\r\n").encode()
    body += data + f"\r\n--{b}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\ntrue\r\n--{b}--\r\n".encode()
    st, r = req("POST", COMFY + "/upload/image", body, "multipart/form-data; boundary=" + b)
    pname = json.loads(r)["name"]
    turbo = [c for c in ckpts if "inpaint" not in c.lower()]
    for v in "BCD":
        if v == "D" and not turbo:
            log("  variante D : ignorée (aucun modèle classique)")
            continue
        log(f"  variante {v} :")
        if v == "D":
            g = sampler(turbo[0], False, ["24", 0], ["21", 1], "A")
            g["7"]["inputs"].update(steps=8, cfg=1.5, sampler_name="euler_ancestral", scheduler="normal", denoise=0.9)
        else:
            g = sampler(ck, bool(inp), ["21", 0] if v == "B" else ["24", 0], ["21", 1], "B")
            if v == "C":
                g["7"]["inputs"]["denoise"] = 0.85
        g["2"] = {"class_type": "LoadImage", "inputs": {"image": pname}}
        g["20"] = {"class_type": "ImageScale", "inputs": {"image": ["2", 0], "upscale_method": "lanczos", "width": w0, "height": h0, "crop": "disabled"}}
        g["21"] = {"class_type": "ImagePadForOutpaint", "inputs": {"image": ["20", 0], "left": 256, "top": 0, "right": 256, "bottom": 0, "feathering": 24}}
        if v != "B":
            g["22"] = {"class_type": "ImageScale", "inputs": {"image": ["20", 0], "upscale_method": "bilinear", "width": w0 + 512, "height": h0, "crop": "disabled"}}
            g["23"] = {"class_type": "ImageBlur", "inputs": {"image": ["22", 0], "blur_radius": 31, "sigma": 10.0}}
            g["24"] = {"class_type": "ImageCompositeMasked", "inputs": {"destination": ["23", 0], "source": ["20", 0], "x": 256, "y": 0, "resize_source": False}}
        run(g, f"photo_agrandie_{v}.png")

# 2. Agrandir l'image (si un modèle est installé).
log("\n--- Agrandir l'image")
if not ups:
    log("  ignoré : ComfyUI ne voit aucun modèle dans models/upscale_models (le fichier y est-il ? ComfyUI redémarré depuis ?)")
else:
    m = ups[0]
    rows = [[v for x in range(512) for v in scene(x, y, 512, 384)] for y in range(384)]
    name = upload(png(512, 384, rows, False), "luminux_up_test.png")
    x4 = "x2" not in m.lower() and "2x" not in m.lower()
    g = {
        "2": {"class_type": "LoadImage", "inputs": {"image": name}},
        "10": {"class_type": "UpscaleModelLoader", "inputs": {"model_name": m}},
        "11": {"class_type": "ImageUpscaleWithModel", "inputs": {"upscale_model": ["10", 0], "image": ["2", 0]}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["12" if x4 else "11", 0], "filename_prefix": "luminux_up"}},
    }
    if x4:
        g["12"] = {"class_type": "ImageScaleBy", "inputs": {"image": ["11", 0], "upscale_method": "lanczos", "scale_by": 0.5}}
    log(f"  modèle {m}, attendu 1024 x 768")
    run(g, "agrandir_image.png")

st, _ = req("POST", COMFY + "/free", {"unload_models": True, "free_memory": True})
log("\n/free :", st)

# 3. Ollama : traduction de l'invite.
log("\n=== Ollama", OLLAMA)
try:
    _, r = req("GET", OLLAMA + "/api/tags", timeout=10)
    models = [(m["name"], m.get("size", 0)) for m in json.loads(r)["models"]]
    log("modèles :", [(n, round(s / 1e9, 1)) for n, s in models])
    _, r = req("GET", OLLAMA + "/api/ps", timeout=10)
    log("chargés :", [(m["name"], round(m.get("size_vram", 0) / 1e9, 1)) for m in json.loads(r)["models"]])
    text = [(n, s) for n, s in models if "embed" not in n.lower()]
    small = min(text, key=lambda t: t[1])[0]
    t0 = time.time()
    st, r = req("POST", OLLAMA + "/api/generate", {
        "model": small, "stream": False, "think": False, "keep_alive": 0, "options": {"temperature": 0, "seed": 1},
        "prompt": "Translate the following image-generation prompt into English. Answer with the translation only, no quotes, no explanation.\n\nun canard jaune sur l'herbe"}, timeout=300)
    log(f"traduction avec {small} ({st}, {time.time() - t0:.1f} s) :", repr(json.loads(r).get("response", r.decode(errors='replace'))))
except Exception as e:
    log("Ollama :", e)

log("\nFini. Rapport :", os.path.join(OUT, "rapport_luminux.txt"))
