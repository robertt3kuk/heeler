"""Generates the minimal, luxury-style hutan logos (line-art emblem + serif wordmark)."""
import math

# Scalloped hair ring around the head
def scallop(cx, cy, r, bumps, depth):
    pts = []
    for i in range(bumps):
        a0 = 2 * math.pi * i / bumps - math.pi / 2
        a1 = 2 * math.pi * (i + 1) / bumps - math.pi / 2
        pts.append((cx + r * math.cos(a0), cy + r * math.sin(a0), cx + r * math.cos(a1), cy + r * math.sin(a1)))
    d = f"M{pts[0][0]:.1f} {pts[0][1]:.1f} "
    for _, _, x1, y1 in pts:
        d += f"A{depth} {depth} 0 0 1 {x1:.1f} {y1:.1f} "
    return d + "Z"

HAIR = scallop(256, 262, 178, 18, 34)

# Line-art emblem, drawn with currentColor so each variant can recolor it
EMBLEM = f"""
<g fill="none" stroke="currentColor" stroke-width="7" stroke-linecap="round" stroke-linejoin="round">
  <path d="{HAIR}"/>
  <path d="M236 70 Q230 46 214 34 M256 66 Q258 40 250 22 M276 70 Q286 46 304 36"/>
  <path d="M256 104 C344 104 388 170 386 252 C384 344 334 424 256 424 C178 424 128 344 126 252 C124 170 168 104 256 104 Z"/>
  <path d="M168 196 Q206 168 244 190 M268 190 Q306 168 344 196"/>
  <path d="M182 258 Q204 270 228 258 M284 258 Q308 270 330 258"/>
  <path d="M182 230 Q203 216 226 230 M286 230 Q309 216 330 230"/>
  <path d="M162 330 C160 290 200 282 256 284 C312 282 352 290 350 330 C348 372 310 398 256 398 C202 398 164 372 162 330 Z"/>
  <path d="M240 312 Q256 302 272 312"/>
  <path d="M208 362 Q252 384 310 356"/>
</g>
<g fill="currentColor">
  <ellipse cx="204" cy="238" rx="13" ry="9"/>
  <ellipse cx="308" cy="238" rx="13" ry="9"/>
  <ellipse cx="245" cy="322" rx="6" ry="4.5"/>
  <ellipse cx="267" cy="322" rx="6" ry="4.5"/>
</g>"""

SERIF = "Didot, 'Bodoni 72', 'Playfair Display', 'Times New Roman', serif"
MONO = "Menlo, Monaco, monospace"


def write(name, body, w, h):
    with open(name, "w") as f:
        f.write(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">{body}\n</svg>\n')


def emblem(x, y, size, color):
    s = size / 512
    return f'<g color="{color}" transform="translate({x} {y}) scale({s})">{EMBLEM}</g>'


# 1. App icon: Hermès-style orange tile, cream line art
write("luxe-icon.svg",
      '<rect width="512" height="512" rx="112" fill="#E8641C"/>' + emblem(56, 52, 400, "#FBF3E6"),
      512, 512)

# 2. Stacked logo on cream (classic house-brand lockup)
write("luxe-logo-light.svg", f"""
<rect width="1200" height="1200" fill="#F6EFE4"/>
{emblem(390, 170, 420, "#1C1A17")}
<text x="600" y="790" text-anchor="middle" font-family="{SERIF}" font-size="150" letter-spacing="42" fill="#1C1A17">HUTAN</text>
<line x1="470" y1="850" x2="730" y2="850" stroke="#E8641C" stroke-width="3"/>
<text x="600" y="910" text-anchor="middle" font-family="{SERIF}" font-style="italic" font-size="44" letter-spacing="4" fill="#1C1A17">code from the canopy</text>
<text x="600" y="1040" text-anchor="middle" font-family="{MONO}" font-size="22" letter-spacing="10" fill="#8A7F72">EST. 2026 · DEVELOPMENT ENVIRONMENT</text>
""", 1200, 1200)

# 3. Horizontal dark logo
write("luxe-logo-dark.svg", f"""
<rect width="1600" height="560" fill="#0E0D0C"/>
<circle cx="300" cy="280" r="196" fill="none" stroke="#E8641C" stroke-width="3"/>
{emblem(140, 116, 320, "#F6EFE4")}
<text x="580" y="300" font-family="{SERIF}" font-size="170" letter-spacing="48" fill="#F6EFE4">HUTAN</text>
<line x1="586" y1="352" x2="1470" y2="352" stroke="#E8641C" stroke-width="2"/>
<text x="586" y="410" font-family="{MONO}" font-size="26" letter-spacing="12" fill="#B7AC9E">CODE FROM THE CANOPY</text>
""", 1600, 560)
