#!/usr/bin/env python3
"""Generates packaging/haze.svg: Mist's dusk scene + glass card, with an amp knob as the mark."""
import math, pathlib

cx, cy = 256, 236
def pt(a, r):
    a = math.radians(a)
    return cx + r * math.sin(a), cy - r * math.cos(a)

ticks = []
for i in range(11):
    a = -135 + i * 27
    x1, y1 = pt(a, 96); x2, y2 = pt(a, 108)
    lit = a <= 60
    ticks.append(f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" stroke="{"#f3a35c" if lit else "#f5efe9"}" '
                 f'stroke-opacity="{0.95 if lit else 0.35}" stroke-width="5" stroke-linecap="round"/>')
ax, ay = pt(-135, 82); bx, by = pt(60, 82)
px1, py1 = pt(60, 18); px2, py2 = pt(60, 50)

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#140f1c"/>
      <stop offset="0.55" stop-color="#2b1d26"/>
      <stop offset="1" stop-color="#5a3826"/>
    </linearGradient>
    <radialGradient id="sun" cx="0.5" cy="0.85" r="0.6">
      <stop offset="0" stop-color="#f3a35c" stop-opacity="0.9"/>
      <stop offset="0.5" stop-color="#c46a3c" stop-opacity="0.3"/>
      <stop offset="1" stop-color="#c46a3c" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="dune" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#3d241c"/>
      <stop offset="1" stop-color="#180e0c"/>
    </linearGradient>
    <linearGradient id="glass" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.22"/>
      <stop offset="1" stop-color="#ffffff" stop-opacity="0.06"/>
    </linearGradient>
    <linearGradient id="arc" x1="0" y1="1" x2="1" y2="0">
      <stop offset="0" stop-color="#c46a3c"/>
      <stop offset="1" stop-color="#f6b86e"/>
    </linearGradient>
    <radialGradient id="cap" cx="0.42" cy="0.35" r="0.75">
      <stop offset="0" stop-color="#fbf6f0"/>
      <stop offset="0.7" stop-color="#e6dbcf"/>
      <stop offset="1" stop-color="#bfae9d"/>
    </radialGradient>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="0" dy="8" stdDeviation="16" flood-color="#000000" flood-opacity="0.45"/>
    </filter>
    <filter id="knobshadow" x="-30%" y="-30%" width="160%" height="160%">
      <feDropShadow dx="0" dy="6" stdDeviation="7" flood-color="#000000" flood-opacity="0.5"/>
    </filter>
    <filter id="glow" x="-30%" y="-30%" width="160%" height="160%">
      <feGaussianBlur stdDeviation="5"/>
    </filter>
  </defs>

  <!-- Background (Mist dusk) -->
  <rect width="512" height="512" fill="url(#bg)"/>
  <circle cx="256" cy="380" r="240" fill="url(#sun)"/>
  <path d="M 0,380 Q 140,320 280,360 T 512,340 L 512,512 L 0,512 Z" fill="url(#dune)"/>

  <!-- Glass card -->
  <rect x="96" y="96" width="320" height="320" rx="72" fill="url(#glass)" stroke="#ffffff" stroke-opacity="0.3" stroke-width="2.5" filter="url(#shadow)"/>

  <!-- Scale: ticks + value arc (cranked past 7) -->
  {chr(10).join("  " + t for t in ticks)}
  <path d="M {ax:.1f},{ay:.1f} A 82 82 0 1 1 {bx:.1f},{by:.1f}" fill="none" stroke="url(#arc)" stroke-width="9" stroke-linecap="round" opacity="0.55" filter="url(#glow)"/>
  <path d="M {ax:.1f},{ay:.1f} A 82 82 0 1 1 {bx:.1f},{by:.1f}" fill="none" stroke="url(#arc)" stroke-width="7" stroke-linecap="round"/>

  <!-- Knob -->
  <circle cx="{cx}" cy="{cy}" r="60" fill="url(#cap)" filter="url(#knobshadow)"/>
  <circle cx="{cx}" cy="{cy}" r="60" fill="none" stroke="#ffffff" stroke-opacity="0.55" stroke-width="1.5"/>
  <line x1="{px1:.1f}" y1="{py1:.1f}" x2="{px2:.1f}" y2="{py2:.1f}" stroke="#2b1d26" stroke-width="9" stroke-linecap="round"/>

  <!-- Glass pill: amp status LEDs -->
  <rect x="186" y="345" width="140" height="24" rx="12" fill="#000000" fill-opacity="0.5" stroke="#ffffff" stroke-opacity="0.15"/>
  <circle cx="204" cy="357" r="4" fill="#22c55e"/>
  <circle cx="308" cy="357" r="4" fill="#f5b942"/>
</svg>
'''
out = pathlib.Path(__file__).resolve().parent.parent / "packaging" / "haze.svg"
out.write_text(svg)
print(out)
