"""hutan logos rooted in the orangutan's homeland: Dayak (Borneo), Batik (Java/Sumatra), Wayang (shadow play)."""
import math

W, H = 1200, 600
SANS = "'Avenir Next', 'Helvetica Neue', Helvetica, Arial, sans-serif"
SERIF = "Didot, 'Bodoni 72', 'Times New Roman', serif"


def write(name, body, w=W, h=H):
    with open(name, "w") as f:
        f.write(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">{body}\n</svg>\n')


def place(mark, x=70, y=50, size=500):
    return f'<g transform="translate({x} {y}) scale({size / 512})">{mark}</g>'


def spiral(cx, cy, r, turns, a0, cw=True):
    """Inward spiral from radius r to ~0, starting at angle a0 (degrees)."""
    n, sgn = 60, (1 if cw else -1)
    pts = []
    for i in range(n + 1):
        t = i / n
        a = math.radians(a0) + sgn * t * turns * 2 * math.pi
        rr = r * (1 - 0.85 * t)
        pts.append(f"{cx + rr * math.cos(a):.1f} {cy + rr * math.sin(a):.1f}")
    return "M" + " L".join(pts)


def mirror(d_left, mark):
    return f'{d_left}<g transform="translate(512 0) scale(-1 1)">{d_left}</g>' if mark else d_left


def scallop(cx, cy, r, bumps, depth):
    d = ""
    for i in range(bumps + 1):
        a = 2 * math.pi * i / bumps - math.pi / 2
        x, y = cx + r * math.cos(a), cy + r * math.sin(a)
        d += f"M{x:.1f} {y:.1f} " if i == 0 else f"A{depth} {depth} 0 0 1 {x:.1f} {y:.1f} "
    return d + "Z"


# ---------- 1. Dayak shield (Borneo) ----------
INK, RED, BONE, OCHRE = "#141110", "#B3261E", "#EFE3CC", "#D9A13B"
half_face = f"""
<path d="M256 168 Q206 138 168 158" />
<path d="{spiral(150, 182, 30, 1.25, -60, cw=False)}"/>
<path d="{spiral(126, 300, 44, 1.4, -90, cw=False)}"/>
<path d="M176 282 C176 350 210 400 256 404"/>
<path d="M150 412 L178 440 L206 412" stroke="{RED}"/>
"""
DAYAK = f"""
<path d="M256 14 L412 92 L412 420 L256 498 L100 420 L100 92 Z" fill="{BONE}" stroke="{INK}" stroke-width="14" stroke-linejoin="round"/>
<path d="M256 40 L390 106 L390 406 L256 472 L122 406 L122 106 Z" fill="none" stroke="{RED}" stroke-width="6" stroke-linejoin="round"/>
<g fill="none" stroke="{INK}" stroke-width="11" stroke-linecap="round" stroke-linejoin="round">
  {half_face}
  <g transform="translate(512 0) scale(-1 1)">{half_face}</g>
</g>
<g fill="none" stroke="{RED}" stroke-width="11" stroke-linecap="round">
  <path d="M256 128 L256 70"/>
  <path d="{spiral(214, 92, 22, 1.1, 90, cw=True)}"/>
  <path d="{spiral(298, 92, 22, 1.1, 90, cw=False)}"/>
</g>
<g>
  <circle cx="208" cy="234" r="30" fill="{OCHRE}" stroke="{INK}" stroke-width="9"/>
  <circle cx="304" cy="234" r="30" fill="{OCHRE}" stroke="{INK}" stroke-width="9"/>
  <circle cx="208" cy="234" r="12" fill="{INK}"/><circle cx="304" cy="234" r="12" fill="{INK}"/>
  <path d="M234 296 Q244 280 252 300 Q244 312 234 296 Z M278 296 Q268 280 260 300 Q268 312 278 296 Z" fill="{INK}"/>
  <path d="M206 352 Q256 330 306 352 Q256 384 206 352 Z" fill="{RED}" stroke="{INK}" stroke-width="7" stroke-linejoin="round"/>
</g>"""

tumpal = "".join(f'<path d="M{586 + i * 34} 426 l17 -26 l17 26 Z" fill="{RED if i % 2 else INK}"/>' for i in range(14))
write("nusantara-dayak.svg", f"""
<rect width="{W}" height="{H}" fill="#E6D7BC"/>
{place(DAYAK)}
<text x="580" y="350" font-family="{SANS}" font-weight="900" font-size="132" letter-spacing="6" fill="{INK}">HUTAN</text>
{tumpal}
<text x="586" y="478" font-family="{SANS}" font-weight="600" font-size="22" letter-spacing="6" fill="{RED}">FROM THE FORESTS OF BORNEO</text>
""")

# ---------- 2. Batik / Mega Mendung (Java, Sumatra) ----------
INDIGO, SOGAN, CREAM = "#17233D", "#8B5A2B", "#EAD9B8"
clouds = "".join(
    f'<path d="{scallop(256, 262, r, 16, d)}" fill="{c}"/>'
    for r, d, c in [(236, 40, "#24365C"), (214, 36, "#2F4C7E"), (192, 32, "#4A6FA5"), (172, 28, "#7FA2CC")])
BATIK = f"""
{clouds}
<path d="M256 104 C344 104 388 170 386 252 C384 344 334 424 256 424 C178 424 128 344 126 252 C124 170 168 104 256 104 Z" fill="{SOGAN}"/>
<g fill="{CREAM}">
  <ellipse cx="202" cy="232" rx="58" ry="46"/><ellipse cx="310" cy="232" rx="58" ry="46"/>
  <ellipse cx="256" cy="344" rx="108" ry="74"/>
</g>
<g fill="none" stroke="{SOGAN}" stroke-width="7" stroke-linecap="round">
  <path d="M158 206 Q202 182 246 206 M266 206 Q310 182 354 206"/>
  <path d="M180 258 Q204 270 228 258 M284 258 Q308 270 332 258" stroke-width="5"/>
  <path d="M206 362 Q252 388 310 356" stroke-width="9"/>
</g>
<circle cx="206" cy="236" r="14" fill="#2A160C"/><circle cx="306" cy="236" r="14" fill="#2A160C"/>
<ellipse cx="244" cy="312" rx="7" ry="5" fill="#2A160C"/><ellipse cx="268" cy="312" rx="7" ry="5" fill="#2A160C"/>
<g fill="{SOGAN}">{''.join(f'<circle cx="{256 + 92 * math.cos(a / 10)}" cy="{344 + 60 * math.sin(a / 10)}" r="3.2"/>' for a in range(0, 63, 3))}</g>"""

write("nusantara-batik.svg", f"""
<defs>
  <pattern id="kawung" width="80" height="80" patternUnits="userSpaceOnUse">
    <g fill="none" stroke="#24345A" stroke-width="3">
      <ellipse cx="40" cy="20" rx="11" ry="18"/><ellipse cx="40" cy="60" rx="11" ry="18"/>
      <ellipse cx="20" cy="40" rx="18" ry="11"/><ellipse cx="60" cy="40" rx="18" ry="11"/>
    </g>
    <circle cx="40" cy="40" r="3" fill="#24345A"/>
  </pattern>
</defs>
<rect width="{W}" height="{H}" fill="{INDIGO}"/>
<rect width="{W}" height="{H}" fill="url(#kawung)"/>
<rect x="540" y="0" width="{W - 540}" height="{H}" fill="{INDIGO}" opacity=".88"/>
{place(BATIK)}
<text x="590" y="330" font-family="{SERIF}" font-size="160" letter-spacing="6" fill="{CREAM}">hutan</text>
<g fill="#D9A13B">{''.join(f'<circle cx="{596 + i * 18}" cy="372" r="3"/>' for i in range(30))}</g>
<text x="594" y="430" font-family="{SERIF}" font-style="italic" font-size="34" fill="#9FB5D6">code, hand-drawn like batik</text>
""")

# ---------- 3. Wayang kulit shadow puppet ----------
GOLD = "#D9A441"
holes = []
for r, n in [(196, 40), (170, 34)]:
    for i in range(n):
        a = 2 * math.pi * i / n
        holes.append(f'<circle cx="{256 + r * math.cos(a):.1f}" cy="{262 + r * math.sin(a):.1f}" r="5" fill="black"/>')
for y, w_ in [(130, 5), (150, 7)]:
    for i in range(w_):
        holes.append(f'<circle cx="{256 + (i - (w_ - 1) / 2) * 22}" cy="{y}" r="4.5" fill="black"/>')
WAYANG = f"""
<defs>
  <mask id="cut">
    <rect width="512" height="512" fill="white"/>
    {''.join(holes)}
    <path d="M170 238 Q204 212 238 238 Q204 252 170 238 Z M274 238 Q308 212 342 238 Q308 252 274 238 Z" fill="black"/>
    <path d="M160 196 Q204 168 246 192 L242 200 Q204 182 164 204 Z M352 196 Q308 168 266 192 L270 200 Q308 182 348 204 Z" fill="black"/>
    <path d="M238 312 Q246 300 252 316 Z M274 312 Q266 300 260 316 Z" fill="black"/>
    <path d="M204 356 Q254 384 312 350 L310 360 Q254 396 204 364 Z" fill="black"/>
  </mask>
</defs>
<line x1="256" y1="400" x2="256" y2="512" stroke="{GOLD}" stroke-width="10" stroke-linecap="round"/>
<g mask="url(#cut)">
  <path d="{scallop(256, 262, 214, 24, 30)}" fill="#0A0605"/>
  <path d="M256 60 Q244 28 222 18 Q252 26 256 44 Q260 26 290 18 Q268 28 256 60 Z" fill="#0A0605"/>
</g>
<path d="{scallop(256, 262, 214, 24, 30)}" fill="none" stroke="{GOLD}" stroke-width="3"/>
<circle cx="207" cy="238" r="7" fill="{GOLD}"/><circle cx="305" cy="238" r="7" fill="{GOLD}"/>"""

write("nusantara-wayang.svg", f"""
<defs>
  <radialGradient id="lamp" cx="22%" cy="45%" r="60%">
    <stop offset="0" stop-color="#F6B652"/><stop offset=".35" stop-color="#B4471C"/><stop offset="1" stop-color="#1A0907"/>
  </radialGradient>
</defs>
<rect width="{W}" height="{H}" fill="url(#lamp)"/>
{place(WAYANG, 70, 40, 520)}
<text x="600" y="330" font-family="{SERIF}" font-size="124" letter-spacing="16" fill="{GOLD}">HUTAN</text>
<path d="M606 372 H1150" stroke="{GOLD}" stroke-width="2"/>
<path d="M878 360 l12 12 l-12 12 l-12 -12 Z" fill="{GOLD}"/>
<text x="606" y="430" font-family="{SERIF}" font-style="italic" font-size="34" fill="#F1D9A8">a shadow play for code</text>
""")
