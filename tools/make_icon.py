#!/usr/bin/env python3
"""Generates the Haze app icon in the Mist tile family (Files / Player / Settings):
superellipse tile, saturated gradient, one glossy lifted object, top sheen, glass rim.

Mark: a cream guitar pick with the Haze wave pressed into it, on an ember-orange tile.
Writes packaging/haze.svg, packaging/haze-{32..512}.png and ui/art/icon.png (window icon).
Needs rsvg-convert. Run from anywhere: python3 tools/make_icon.py
"""
import math, pathlib, subprocess

root = pathlib.Path(__file__).resolve().parent.parent

# Same tile as the Mist icons: superellipse n=5, r=412 around (512,512) on a 1024 canvas.
def squircle(n=5.0, r=412.0, c=512.0, steps=176):
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        ct, st = math.cos(t), math.sin(t)
        x = c + r * math.copysign(abs(ct) ** (2 / n), ct)
        y = c + r * math.copysign(abs(st) ** (2 / n), st)
        pts.append(f"{x:.1f},{y:.1f}")
    return "M" + " L".join(pts) + " Z"

tile = squircle()

# Classic 351-shape pick, point down.
pick = ("M512,268 C640,268 764,288 772,372 C780,456 642,642 562,732 "
        "C536,760 488,760 462,732 C382,642 244,456 252,372 C260,288 384,268 512,268 Z")
# Haze wave: three soft crests, same rhythm as the in-app brand glyph.
wave = "M366,462 Q402,392 439,462 T512,462 T585,462 T658,462"

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <title>Haze</title>
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0.35" y2="1">
      <stop offset="0" stop-color="#f59a4a"/><stop offset="0.55" stop-color="#d4602c"/><stop offset="1" stop-color="#8e3219"/>
    </linearGradient>
    <linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.30"/>
      <stop offset="0.45" stop-color="#ffffff" stop-opacity="0.04"/>
      <stop offset="1" stop-color="#ffffff" stop-opacity="0"/>
    </linearGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.55"/>
      <stop offset="0.5" stop-color="#ffffff" stop-opacity="0.08"/>
      <stop offset="1" stop-color="#ffffff" stop-opacity="0.22"/>
    </linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.95" r="0.75">
      <stop offset="0" stop-color="#ffd08a" stop-opacity="0.40"/><stop offset="1" stop-color="#ffd08a" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="edge" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#d8b88e"/><stop offset="1" stop-color="#a87a4e"/>
    </linearGradient>
    <linearGradient id="face" x1="0.2" y1="0" x2="0.6" y2="1">
      <stop offset="0" stop-color="#fffaf1"/><stop offset="0.6" stop-color="#f4e6cf"/><stop offset="1" stop-color="#e6cfac"/>
    </linearGradient>
    <linearGradient id="gloss" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.85"/><stop offset="1" stop-color="#ffffff" stop-opacity="0"/>
    </linearGradient>
    <filter id="drop" x="-10%" y="-10%" width="120%" height="125%">
      <feDropShadow dx="0" dy="14" stdDeviation="16" flood-color="#000000" flood-opacity="0.32"/>
    </filter>
    <filter id="lift" x="-20%" y="-20%" width="140%" height="150%">
      <feDropShadow dx="0" dy="18" stdDeviation="22" flood-color="#4a1406" flood-opacity="0.50"/>
    </filter>
    <clipPath id="tile"><path d="{tile}"/></clipPath>
    <clipPath id="pickclip"><path d="{pick}"/></clipPath>
  </defs>
  <path d="{tile}" fill="url(#bg)" filter="url(#drop)"/>
  <g clip-path="url(#tile)">
    <rect x="100" y="100" width="824" height="824" fill="url(#glow)"/>
    <g filter="url(#lift)" transform="translate(512 528) rotate(-8) scale(1.2) translate(-512 -512)">
      <!-- pick thickness, then the face slightly above it -->
      <path d="{pick}" fill="url(#edge)" transform="translate(0 14)"/>
      <path d="{pick}" fill="url(#face)"/>
      <!-- pressed-in wave: dark groove with a light lower lip -->
      <path d="{wave}" fill="none" stroke="#ffffff" stroke-opacity="0.9" stroke-width="50" stroke-linecap="round" transform="translate(0 6)"/>
      <path d="{wave}" fill="none" stroke="#c9562a" stroke-width="46" stroke-linecap="round"/>
      <path d="{wave}" fill="none" stroke="#7e2b12" stroke-opacity="0.55" stroke-width="14" stroke-linecap="round" transform="translate(0 -12)"/>
      <!-- top gloss -->
      <g clip-path="url(#pickclip)">
        <ellipse cx="470" cy="300" rx="230" ry="70" fill="url(#gloss)" opacity="0.7"/>
      </g>
      <path d="{pick}" fill="none" stroke="#ffffff" stroke-opacity="0.6" stroke-width="4"/>
    </g>
    <path d="M100,100 H924 V560 Q512,470 100,560 Z" fill="url(#sheen)"/>
  </g>
  <path d="{tile}" fill="none" stroke="url(#rim)" stroke-width="5"/>
</svg>
'''

out = root / "packaging/haze.svg"
out.write_text(svg)

def render(size, dest):
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), str(out), "-o", str(dest)], check=True)

for s in (32, 48, 64, 128, 256, 512):
    render(s, root / f"packaging/haze-{s}.png")
render(128, root / "crates/katana-studio/ui/art/icon.png")
print("wrote", out)
