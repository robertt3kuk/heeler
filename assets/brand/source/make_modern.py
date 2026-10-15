"""Generates four modern hutan logo directions plus a comparison board."""

W, H = 1200, 600
SANS = "'Avenir Next', 'Helvetica Neue', Helvetica, Arial, sans-serif"
MONO = "'SF Mono', Menlo, Monaco, monospace"


def write(name, body, w=W, h=H):
    with open(name, "w") as f:
        f.write(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">{body}\n</svg>\n')


def place(mark, x=70, y=80, size=440):
    return f'<g transform="translate({x} {y}) scale({size / 512})">{mark}</g>'


# A. Geometric: flat face from pure circles
GEO = """
<circle cx="256" cy="256" r="220" fill="#E8641C"/>
<ellipse cx="256" cy="272" rx="162" ry="178" fill="#5E2C1A"/>
<g fill="#F2C9A8">
  <circle cx="198" cy="226" r="58"/><circle cx="314" cy="226" r="58"/>
  <circle cx="256" cy="352" r="112"/>
</g>
<circle cx="204" cy="230" r="17" fill="#140C09"/><circle cx="308" cy="230" r="17" fill="#140C09"/>
<circle cx="242" cy="316" r="8" fill="#5E2C1A"/><circle cx="270" cy="316" r="8" fill="#5E2C1A"/>
<path d="M214 380 Q258 402 304 374" fill="none" stroke="#5E2C1A" stroke-width="12" stroke-linecap="round"/>"""

write("modern-a-geo.svg", f"""
<rect width="{W}" height="{H}" fill="#FFF6EC"/>
{place(GEO)}
<text x="580" y="335" font-family="{SANS}" font-weight="800" font-size="150" letter-spacing="-6" fill="#1B1714">hutan</text>
<text x="588" y="395" font-family="{SANS}" font-weight="500" font-size="30" fill="#E8641C">the friendly code environment</text>
""")

# B. Brackets: a flanged male orangutan whose cheek pads are { } braces
BRACKETS = """
<g fill="none" stroke="#F3EADF" stroke-width="22" stroke-linecap="round" stroke-linejoin="round">
  <path d="M150 90 C100 90 104 150 104 200 C104 236 86 256 56 256 C86 256 104 276 104 312 C104 362 100 422 150 422"/>
  <path d="M362 90 C412 90 408 150 408 200 C408 236 426 256 456 256 C426 256 408 276 408 312 C408 362 412 422 362 422"/>
</g>
<g fill="none" stroke="#E8641C" stroke-width="12" stroke-linecap="round">
  <path d="M226 92 Q220 62 202 48 M256 88 Q258 58 250 38 M286 92 Q296 62 314 52"/>
  <path d="M182 196 Q214 176 244 194 M268 194 Q298 176 330 196"/>
</g>
<circle cx="212" cy="236" r="20" fill="#E8641C"/><circle cx="300" cy="236" r="20" fill="#E8641C"/>
<circle cx="242" cy="306" r="8" fill="#F3EADF"/><circle cx="270" cy="306" r="8" fill="#F3EADF"/>
<path d="M200 362 Q252 392 316 352" fill="none" stroke="#F3EADF" stroke-width="14" stroke-linecap="round"/>"""

write("modern-b-brackets.svg", f"""
<rect width="{W}" height="{H}" fill="#0E0E10"/>
{place(BRACKETS)}
<text x="580" y="330" font-family="{MONO}" font-weight="700" font-size="128" letter-spacing="-4" fill="#F3EADF">hutan</text>
<text x="586" y="392" font-family="{MONO}" font-size="28" fill="#8C857C"><tspan fill="#E8641C">{{</tspan> ape-grade code editor <tspan fill="#E8641C">}}</tspan></text>
""")

# C. Pixel: 16x16 sprite, terminal / agent-CLI energy
SPRITE = [
    "......o..o......",
    "....oooooooo....",
    "...oobbbbbboo...",
    "..oobbbbbbbboo..",
    ".oobccbbbbccboo.",
    ".obbckbbbbkcbbo.",
    "oobbccbbbbccbboo",
    "oobbbbbbbbbbbboo",
    "oobccccccccccboo",
    "oobccckcckcccboo",
    ".obccccccccccbo.",
    ".obcccccccckcbo.",
    "..obcckkkkkcbo..",
    "...obccccccbo...",
    "....oobbbboo....",
    "......oooo......",
]
PAL = {"o": "#FF7A1A", "b": "#7A3E28", "c": "#EDB38F", "k": "#0B0B0C"}
cell = 512 / 16
PIXEL = "".join(
    f'<rect x="{x * cell:.0f}" y="{y * cell:.0f}" width="{cell + .5:.1f}" height="{cell + .5:.1f}" fill="{PAL[ch]}"/>'
    for y, row in enumerate(SPRITE) for x, ch in enumerate(row) if ch in PAL)

write("modern-c-pixel.svg", f"""
<rect width="{W}" height="{H}" fill="#0B0B0C"/>
{place(PIXEL, 90, 100, 400)}
<text x="580" y="320" font-family="{MONO}" font-weight="700" font-size="120" fill="#F2F2F2"><tspan fill="#FF7A1A">&gt;</tspan>hutan<tspan fill="#FF7A1A">_</tspan></text>
<text x="586" y="384" font-family="{MONO}" font-size="26" letter-spacing="3" fill="#6E6E73">v1.0 · agentic dev environment</text>
""")

# D. Gradient: overlapping translucent circles, modern-startup glyph
GRAD = """
<defs>
  <linearGradient id="g1" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#FFB547"/><stop offset="1" stop-color="#FF4D2E"/></linearGradient>
  <linearGradient id="g2" x1="1" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FF8A3D"/><stop offset="1" stop-color="#E0306B"/></linearGradient>
  <linearGradient id="g3" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FF6A2B"/><stop offset="1" stop-color="#8E2DE2"/></linearGradient>
</defs>
<g style="mix-blend-mode:multiply" opacity=".92">
  <circle cx="186" cy="200" r="120" fill="url(#g1)"/>
  <circle cx="326" cy="200" r="120" fill="url(#g2)"/>
  <circle cx="256" cy="338" r="150" fill="url(#g3)"/>
</g>
<circle cx="196" cy="212" r="20" fill="#FFF"/><circle cx="316" cy="212" r="20" fill="#FFF"/>
<circle cx="200" cy="216" r="10" fill="#120B14"/><circle cx="312" cy="216" r="10" fill="#120B14"/>
<path d="M214 392 Q258 414 304 386" fill="none" stroke="#FFF" stroke-width="12" stroke-linecap="round"/>"""

write("modern-d-gradient.svg", f"""
<rect width="{W}" height="{H}" fill="#FFFFFF"/>
{place(GRAD)}
<text x="580" y="330" font-family="{SANS}" font-weight="600" font-size="140" letter-spacing="-5" fill="#120B14">Hutan</text>
<text x="588" y="390" font-family="{SANS}" font-weight="500" font-size="30" fill="#7B6F85">Build wild. Ship calm.</text>
""")
