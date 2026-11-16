#!/usr/bin/env python3
"""Una sola vez: crea ~/.eslabon a partir de ~/.codex sin romper el Codex oficial.

Copia (el oficial los sigue usando): config.toml, auth.json, skills de sistema y perfiles del Codex oficial.
Mueve (solo sirven a eslabon): perfiles con wire_api = "chat" o [eslabon], catalogs/, skills migrados, eslabon-phi.json.
Sube a ~/.eslabon/config.toml los [model_providers] definidos en esos perfiles, para que /model cambie entre ellos.
Idempotente: si ~/.eslabon/config.toml ya existe, no copia de nuevo."""
import os, re, shutil, sys, tomllib

H = os.path.expanduser("~")
SRC, DST = os.path.join(H, ".codex"), os.path.join(H, ".eslabon")
# eslabon: skills to move with the home; override with ESLABON_MIGRATE_SKILLS.
ESLABON_SKILLS = os.environ.get("ESLABON_MIGRATE_SKILLS", "").split()
os.makedirs(DST, exist_ok=True)
hecho = []

cfg_dst = os.path.join(DST, "config.toml")
if not os.path.exists(cfg_dst) and os.path.exists(os.path.join(SRC, "config.toml")):
    shutil.copy2(os.path.join(SRC, "config.toml"), cfg_dst); hecho.append("copiado config.toml")
if os.path.exists(os.path.join(SRC, "auth.json")) and not os.path.exists(os.path.join(DST, "auth.json")):
    shutil.copy2(os.path.join(SRC, "auth.json"), os.path.join(DST, "auth.json"))
    os.chmod(os.path.join(DST, "auth.json"), 0o600); hecho.append("copiado auth.json")

# Perfiles: los de eslabon se mueven; los del oficial se copian.
providers = {}
for name in sorted(os.listdir(SRC)):
    if not name.endswith(".config.toml"):
        continue
    src = os.path.join(SRC, name)
    text = open(src).read()
    try:
        data = tomllib.loads(text)
    except Exception:
        continue
    es_eslabon = "eslabon" in data or any(
        p.get("wire_api") == "chat" for p in data.get("model_providers", {}).values())
    for pid, info in data.get("model_providers", {}).items():
        providers.setdefault(pid, info)
    dst = os.path.join(DST, name)
    if es_eslabon:
        shutil.move(src, dst); hecho.append(f"movido {name}")
    elif not os.path.exists(dst):
        shutil.copy2(src, dst); hecho.append(f"copiado {name}")

# Catálogos y skills de eslabon se mueven.
cat_src = os.path.join(SRC, "catalogs")
if os.path.isdir(cat_src):
    os.makedirs(os.path.join(DST, "catalogs"), exist_ok=True)
    for f in os.listdir(cat_src):
        shutil.move(os.path.join(cat_src, f), os.path.join(DST, "catalogs", f)); hecho.append(f"movido catalogs/{f}")
    os.rmdir(cat_src)
os.makedirs(os.path.join(DST, "skills"), exist_ok=True)
for s in ESLABON_SKILLS:
    p = os.path.join(SRC, "skills", s)
    if os.path.isdir(p):
        shutil.move(p, os.path.join(DST, "skills", s)); hecho.append(f"movido skills/{s}")
sys_src = os.path.join(SRC, "skills", ".system")
if os.path.isdir(sys_src) and not os.path.isdir(os.path.join(DST, "skills", ".system")):
    shutil.copytree(sys_src, os.path.join(DST, "skills", ".system")); hecho.append("copiado skills/.system")
phi = os.path.join(SRC, "eslabon-phi.json")
if os.path.exists(phi):
    shutil.move(phi, os.path.join(DST, "eslabon-phi.json")); hecho.append("movido eslabon-phi.json")

# Rutas internas ~/.codex → ~/.eslabon en lo movido.
for root, _, files in os.walk(DST):
    if "/sessions" in root or "/.system" in root:
        continue
    for f in files:
        if not f.endswith((".toml", ".md", ".py", ".sh", ".json")) or f in ("auth.json",) or root == DST and f == "config.toml":
            continue
        p = os.path.join(root, f)
        try:
            t = open(p).read()
        except Exception:
            continue
        n = t.replace(f"{H}/.codex/", f"{H}/.eslabon/").replace("~/.codex/", "~/.eslabon/")
        if n != t:
            open(p, "w").write(n); hecho.append(f"rutas → .eslabon en {os.path.relpath(p, DST)}")

# Proveedores de los perfiles → config.toml de eslabon (si faltan).
cfg = open(cfg_dst).read() if os.path.exists(cfg_dst) else ""
existentes = tomllib.loads(cfg).get("model_providers", {}) if cfg else {}
def q(v): return '"' + str(v).replace("\\", "\\\\").replace('"', '\\"') + '"'
bloques = []
for pid, info in providers.items():
    if pid in existentes:
        continue
    lines = [f"\n[model_providers.{q(pid)}]"]
    sub = []
    for k, v in info.items():
        if isinstance(v, dict):
            sub.append((k, v))
        elif isinstance(v, (list, tuple)):
            lines.append(f"{k} = [" + ", ".join(q(x) for x in v) + "]")
        elif isinstance(v, bool):
            lines.append(f"{k} = {str(v).lower()}")
        elif isinstance(v, (int, float)):
            lines.append(f"{k} = {v}")
        else:
            lines.append(f"{k} = {q(v)}")
    for k, v in sub:
        lines.append(f"\n[model_providers.{q(pid)}.{k}]")
        for kk, vv in v.items():
            if isinstance(vv, list):
                lines.append(f"{kk} = [" + ", ".join(q(x) for x in vv) + "]")
            elif isinstance(vv, (int, float)) and not isinstance(vv, bool):
                lines.append(f"{kk} = {vv}")
            else:
                lines.append(f"{kk} = {q(vv)}")
    bloques.append("\n".join(lines)); hecho.append(f"proveedor {pid} → config.toml")
if bloques:
    open(cfg_dst, "a").write("\n# eslabon: proveedores subidos desde los perfiles\n" + "\n".join(bloques) + "\n")
    tomllib.loads(open(cfg_dst).read())  # valida

for h in hecho: print("·", h)
print(f"listo: {DST}")
