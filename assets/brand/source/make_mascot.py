"""hutan mascot system: one parametric orangutan (based on the first logo) in many angles and UX states."""
import os

C = dict(hair="#E0692E", ear="#A55A3A", earIn="#7E4029", skin="#8C4F36", patch="#C8805E",
         line="#6F3824", eye="#1A0F0C", muzzle="#D99A7C", nose="#B87458", nostril="#3A1D14",
         mouth="#7A3B27", mouthIn="#4A1E14", gold="#F5B53D", ink="#17151A", cream="#FFF4E8")

os.makedirs("mascot", exist_ok=True)


def hair(dx=0):
    return (f'<g fill="{C["hair"]}" transform="translate({dx:.1f} 0)">'
            '<circle cx="256" cy="262" r="186"/><ellipse cx="256" cy="318" rx="206" ry="140"/>'
            '<g class="tufts"><path d="M212 84 Q196 44 166 32 Q214 34 238 76 Z"/>'
            '<path d="M244 78 Q242 32 222 12 Q264 30 268 76 Z"/>'
            '<path d="M282 80 Q302 40 338 32 Q306 50 300 86 Z"/></g></g>')


def star(x, y, r):
    return f'<path d="M{x} {y - r} Q{x} {y} {x + r} {y} Q{x} {y} {x} {y + r} Q{x} {y} {x - r} {y} Q{x} {y} {x} {y - r} Z" fill="{C["gold"]}"/>'


BADGE = (f'<circle cx="404" cy="420" r="58" fill="{C["ink"]}"/><circle cx="404" cy="420" r="48" fill="{C["gold"]}"/>'
         f'<path d="M386 404 L370 420 L386 436 M422 404 L438 420 L422 436 M410 398 L398 442" fill="none" '
         f'stroke="{C["ink"]}" stroke-width="8" stroke-linecap="round" stroke-linejoin="round"/>')


def ape(yaw=0.0, pitch=0.0, eyes=("open", "open"), look=(0, 0), brows="neutral", mouth="smirk",
        extras=(), detail=True):
    """Front-ish orangutan head in a 512 box. yaw -1..1 turns head, pitch -1..1 tilts it up."""
    k = 1 - 0.22 * abs(yaw)

    def T(x, y):
        return 256 + (x - 256) * k + yaw * 58, y - pitch * 28

    def P(x, y):
        X, Y = T(x, y)
        return f"{X:.1f} {Y:.1f}"

    def far(x):
        return (x < 256 and yaw > 0) or (x > 256 and yaw < 0)

    s = [hair(-yaw * 10)]
    # ears sit behind the head, so the turned-away one hides naturally
    for ex in (108, 404):
        X = ex + yaw * 44
        s.append(f'<ellipse cx="{X:.1f}" cy="{270 - pitch * 10:.1f}" rx="30" ry="40" fill="{C["ear"]}"/>'
                 f'<ellipse cx="{X + (4 if ex < 256 else -4):.1f}" cy="{272 - pitch * 10:.1f}" rx="14" ry="22" fill="{C["earIn"]}"/>')
    hx = yaw * 14
    s.append(f'<path transform="translate({hx:.1f} 0)" d="M256 88 C350 88 400 160 398 250 C396 350 340 440 256 440 '
             f'C172 440 116 350 114 250 C112 160 162 88 256 88 Z" fill="{C["skin"]}"/>')

    for bx in (200, 312):
        X, Y = T(bx, 232)
        sx = 1 - 0.3 * abs(yaw) if far(bx) else 1
        s.append(f'<ellipse cx="{X:.1f}" cy="{Y:.1f}" rx="{64 * sx:.1f}" ry="50" fill="{C["patch"]}"/>')

    # brows: [outer, control, inner] per side
    sides = [[(150, 210), (200, 178), (248, 210)], [(362, 210), (312, 178), (264, 210)]]
    uppers = [[(160, 190), (200, 166), (240, 188)], [(352, 190), (312, 166), (272, 188)]]
    for curves, w in ((sides, 6), (uppers, 5)):
        if not detail and w == 5:
            continue
        for (o, c, i) in curves:
            o, c, i = list(o), list(c), list(i)
            if brows == "raised":
                o[1] -= 14; c[1] -= 16; i[1] -= 14
            elif brows == "sad":
                i[1] -= 18; c[1] -= 6; o[1] += 8
            elif brows == "focused":
                i[1] += 12; c[1] += 4
            s.append(f'<path d="M{P(*o)} Q{P(*c)} {P(*i)}" fill="none" stroke="{C["line"]}" '
                     f'stroke-width="{w}" stroke-linecap="round"/>')

    lx, ly = look
    s.append('<g class="eyes">')
    for kind, bx in zip(eyes, (203, 309)):
        cx, cy = T(bx, 236)
        sx = 1 - 0.3 * abs(yaw) if far(bx) else 1
        if kind in ("open", "wide"):
            r = (25, 18) if kind == "open" else (23, 23)
            s.append(f'<g class="eye"><ellipse cx="{cx + lx:.1f}" cy="{cy + ly:.1f}" rx="{r[0] * sx:.1f}" ry="{r[1]}" fill="{C["eye"]}"/>')
            if detail:
                hr = 5.5 if kind == "open" else 7.5
                s.append(f'<circle cx="{cx + lx + 8 * sx:.1f}" cy="{cy + ly - 6:.1f}" r="{hr}" fill="#fff" opacity=".9"/>')
            s.append('</g>')
            if kind == "open" and detail:
                s.append(f'<path d="M{cx - 25 * sx:.1f} {cy - 4:.1f} Q{cx:.1f} {cy - 22:.1f} {cx + 25 * sx:.1f} {cy - 4:.1f}" '
                         f'fill="none" stroke="{C["line"]}" stroke-width="5" stroke-linecap="round"/>')
        elif kind == "happy":
            s.append(f'<path d="M{cx - 22 * sx:.1f} {cy + 6:.1f} Q{cx:.1f} {cy - 18:.1f} {cx + 22 * sx:.1f} {cy + 6:.1f}" '
                     f'fill="none" stroke="{C["eye"]}" stroke-width="9" stroke-linecap="round"/>')
        elif kind == "closed":
            s.append(f'<path d="M{cx - 22 * sx:.1f} {cy:.1f} Q{cx:.1f} {cy + 12:.1f} {cx + 22 * sx:.1f} {cy:.1f}" '
                     f'fill="none" stroke="{C["eye"]}" stroke-width="8" stroke-linecap="round"/>')
    s.append('</g>')

    if "glasses" in extras:
        for bx in (203, 309):
            cx, cy = T(bx, 238)
            s.append(f'<rect x="{cx - 46:.1f}" y="{cy - 34:.1f}" width="92" height="68" rx="22" fill="#fff" fill-opacity=".12" '
                     f'stroke="{C["ink"]}" stroke-width="8"/>')
        s.append(f'<path d="M{P(249, 236)} Q{P(256, 228)} {P(263, 236)}" fill="none" stroke="{C["ink"]}" stroke-width="8"/>')

    mx, my = T(256, 348)
    s.append(f'<ellipse cx="{mx:.1f}" cy="{my:.1f}" rx="{112 * k:.1f}" ry="82" fill="{C["muzzle"]}"/>')
    s.append(f'<path d="M{P(232, 296)} Q{P(256, 276)} {P(280, 296)} Q{P(284, 312)} {P(256, 314)} Q{P(228, 312)} {P(232, 296)} Z" fill="{C["nose"]}"/>')
    for nx in (243, 269):
        X, Y = T(nx, 303)
        s.append(f'<ellipse cx="{X:.1f}" cy="{Y:.1f}" rx="8" ry="6" fill="{C["nostril"]}"/>')

    stroke = f'fill="none" stroke="{C["mouth"]}" stroke-width="8" stroke-linecap="round"'
    if mouth == "smirk":
        s.append(f'<path d="M{P(196, 362)} Q{P(250, 392)} {P(318, 356)}" {stroke}/>')
    elif mouth == "smile":
        s.append(f'<path d="M{P(192, 348)} Q{P(256, 428)} {P(320, 348)} Q{P(256, 366)} {P(192, 348)} Z" fill="{C["mouthIn"]}" '
                 f'stroke="{C["mouth"]}" stroke-width="6" stroke-linejoin="round"/>')
        s.append(f'<path d="M{P(226, 384)} Q{P(256, 404)} {P(286, 384)} Q{P(256, 376)} {P(226, 384)} Z" fill="#D8615A"/>')
    elif mouth == "o":
        X, Y = T(256, 370)
        s.append(f'<ellipse cx="{X:.1f}" cy="{Y:.1f}" rx="14" ry="18" fill="{C["mouthIn"]}"/>')
    elif mouth == "frown":
        s.append(f'<path d="M{P(210, 382)} Q{P(256, 354)} {P(302, 382)}" {stroke}/>')
    elif mouth == "flat":
        s.append(f'<path d="M{P(218, 370)} Q{P(256, 374)} {P(296, 366)}" {stroke}/>')

    if "hand" in extras:
        s.append(f'<g class="hand" transform="translate({yaw * 40:.1f} 0)"><ellipse cx="322" cy="438" rx="50" ry="38" fill="{C["skin"]}"/>'
                 f'<path d="M290 418 Q300 404 314 414 M310 410 Q322 398 336 410 M332 412 Q344 402 356 416" fill="none" '
                 f'stroke="{C["line"]}" stroke-width="5" stroke-linecap="round"/></g>')
    if "headphones" in extras:
        s.append(f'<path d="M92 286 C92 120 160 52 256 52 C352 52 420 120 420 286" fill="none" stroke="#2B2930" stroke-width="24" stroke-linecap="round"/>'
                 f'<rect x="66" y="232" width="62" height="104" rx="26" fill="#2B2930"/><rect x="384" y="232" width="62" height="104" rx="26" fill="#2B2930"/>'
                 f'<rect x="78" y="252" width="14" height="64" rx="7" fill="{C["gold"]}"/><rect x="420" y="252" width="14" height="64" rx="7" fill="{C["gold"]}"/>')
    if "sparkles" in extras:
        s += [f'<g class="spark s{i}">{star(*a)}</g>' for i, a in enumerate(((424, 112, 34), (470, 196, 16), (84, 104, 22)))]
    if "sweat" in extras:
        s.append('<path d="M412 128 C426 156 438 172 438 188 A26 26 0 0 1 386 188 C386 172 398 156 412 128 Z" fill="#7CC4F5"/>'
                 '<ellipse cx="402" cy="186" rx="6" ry="10" fill="#fff" opacity=".7"/>')
    if "zzz" in extras:
        s.append(f'<g font-family="Avenir Next, Helvetica, sans-serif" font-weight="800" fill="{C["gold"]}">'
                 '<text x="382" y="128" font-size="44">z</text><text x="420" y="90" font-size="60">z</text>'
                 '<text x="462" y="44" font-size="76">Z</text></g>')
    if "thinking" in extras:
        s.append('<g class="bubble"><circle cx="388" cy="120" r="10" fill="#fff"/><circle cx="414" cy="86" r="16" fill="#fff"/>'
                 '<rect x="404" y="6" width="104" height="64" rx="32" fill="#fff"/>'
                 f'<g fill="{C["ink"]}"><circle class="dot" cx="432" cy="38" r="7"/><circle class="dot" cx="456" cy="38" r="7"/><circle class="dot" cx="480" cy="38" r="7"/></g></g>')
    if "question" in extras:
        s.append(f'<text x="396" y="150" font-family="Avenir Next, Helvetica, sans-serif" font-weight="900" font-size="130" fill="{C["gold"]}">?</text>')
    if "badge" in extras:
        s.append(BADGE)
    return "".join(s)


def profile(eye="open", mouth="smirk"):
    """Side view facing right, same parts and palette."""
    s = [f'<g fill="{C["hair"]}"><circle cx="232" cy="262" r="182"/><ellipse cx="220" cy="320" rx="190" ry="136"/>'
         '<path d="M198 86 Q176 46 140 38 Q194 38 222 78 Z"/><path d="M232 80 Q226 34 202 14 Q252 32 256 78 Z"/></g>',
         f'<path d="M300 92 C384 98 414 170 410 252 C406 350 362 430 292 432 C226 434 192 360 188 262 C186 162 222 88 300 92 Z" fill="{C["skin"]}"/>',
         f'<ellipse cx="200" cy="268" rx="28" ry="40" fill="{C["ear"]}"/><ellipse cx="204" cy="270" rx="13" ry="22" fill="{C["earIn"]}"/>',
         f'<ellipse cx="340" cy="232" rx="52" ry="46" fill="{C["patch"]}"/>',
         f'<path d="M292 206 Q342 174 392 202" fill="none" stroke="{C["line"]}" stroke-width="6" stroke-linecap="round"/>',
         f'<path d="M302 186 Q342 160 378 182" fill="none" stroke="{C["line"]}" stroke-width="5" stroke-linecap="round"/>',
         f'<path d="M314 340 C314 290 360 280 404 284 C446 288 462 316 458 348 C454 392 410 418 356 414 C330 412 314 380 314 340 Z" fill="{C["muzzle"]}"/>',
         f'<path d="M402 280 Q438 270 452 298 Q446 314 420 312 Z" fill="{C["nose"]}"/>',
         f'<ellipse cx="440" cy="304" rx="8" ry="6" fill="{C["nostril"]}"/>']
    if eye == "open":
        s.append(f'<ellipse cx="354" cy="236" rx="18" ry="17" fill="{C["eye"]}"/><circle cx="360" cy="230" r="5" fill="#fff" opacity=".9"/>'
                 f'<path d="M334 234 Q354 214 374 232" fill="none" stroke="{C["line"]}" stroke-width="5" stroke-linecap="round"/>')
    if mouth == "smirk":
        s.append(f'<path d="M352 376 Q400 388 442 360" fill="none" stroke="{C["mouth"]}" stroke-width="8" stroke-linecap="round"/>')
    return "".join(s)


def hanging():
    """Hanging by one arm from a code bracket — '404 / hang tight'."""
    return (f'<path d="M40 52 H472" stroke="{C["ink"]}" stroke-width="14" stroke-linecap="round" opacity=".85"/>'
            f'<path d="M80 52 q0 -24 -22 -24 M432 52 q0 -24 22 -24" fill="none" stroke="{C["ink"]}" stroke-width="10" stroke-linecap="round" opacity=".85"/>'
            f'<path d="M318 250 C330 170 330 110 340 60" fill="none" stroke="{C["hair"]}" stroke-width="40" stroke-linecap="round"/>'
            f'<ellipse cx="340" cy="58" rx="28" ry="22" fill="{C["skin"]}"/>'
            f'<ellipse cx="250" cy="470" rx="120" ry="70" fill="{C["hair"]}"/>'
            f'<path d="M150 420 C110 450 100 490 112 510" fill="none" stroke="{C["hair"]}" stroke-width="38" stroke-linecap="round"/>'
            f'<g transform="translate(66 104) scale(0.72)">{ape(yaw=-0.25, pitch=0.3, eyes=("happy", "open"), look=(4, -4), mouth="smile", brows="raised")}</g>')


def peek():
    """Peeking over the edge — empty states."""
    head = ape(pitch=0.45, eyes=("wide", "wide"), look=(0, -6), brows="raised", mouth="o")
    return (f'<defs><clipPath id="ledge"><rect width="512" height="372"/></clipPath></defs>'
            f'<g clip-path="url(#ledge)"><g transform="translate(36 150) scale(0.86)">{head}</g></g>'
            f'<rect x="20" y="372" width="472" height="18" rx="9" fill="{C["ink"]}" opacity=".85"/>'
            + "".join(f'<g transform="translate({x} 0)"><ellipse cx="0" cy="372" rx="46" ry="30" fill="{C["skin"]}"/>'
                      f'<path d="M-26 360 v18 M-6 356 v22 M14 358 v20" stroke="{C["line"]}" stroke-width="5" stroke-linecap="round"/></g>'
                      for x in (128, 384)))


def svg(body, bg=None, rx=112, size=512):
    b = f'<rect width="512" height="512" rx="{rx}" fill="{bg}"/>' if bg else ""
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="{size}" height="{size}">{b}{body}</svg>'


def save(name, content):
    with open(f"mascot/{name}.svg", "w") as f:
        f.write(content)
    return content


# ---------- icon tiles ----------
ICON = ape(extras=("badge",))
icons = [
    ("Тёмная", save("icon-dark", svg(ICON, C["ink"]))),
    ("Светлая", save("icon-light", svg(ICON, C["cream"]))),
    ("Акцент", save("icon-gold", svg(ICON, C["gold"]))),
]
FAV = svg(ape(detail=False), C["ink"])
save("favicon", FAV)

# ---------- angles ----------
angles = [
    ("Анфас", "логотип, аватар", ape()),
    ("¾ влево", "смотрит на контент слева", ape(yaw=-0.6, look=(-6, 0))),
    ("¾ вправо", "указывает на кнопку / CTA", ape(yaw=0.6, look=(6, 0))),
    ("Профиль", "шапка сайта, «идёт вперёд»", profile()),
    ("Взгляд вверх", "смотрит на подсказку сверху", ape(pitch=0.6, look=(0, -7), brows="raised")),
    ("Взгляд вниз", "смотрит в поле ввода", ape(pitch=-0.6, look=(0, 6), eyes=("open", "open"))),
]

# ---------- emotions as UX states ----------
states = [
    ("Успех", "тесты прошли, деплой готов", "#E6F6EA",
     ape(eyes=("happy", "happy"), mouth="smile", brows="raised", extras=("sparkles",))),
    ("Думаю…", "загрузка, агент работает", "#EEF0FA",
     ape(yaw=0.3, eyes=("open", "open"), look=(8, -8), mouth="flat", extras=("hand", "thinking"))),
    ("Ошибка", "сборка упала — сочувствие, не паника", "#FBEAEA",
     ape(eyes=("wide", "wide"), brows="sad", mouth="frown", extras=("sweat",))),
    ("Пусто", "нет проектов / файлов", "#FFF4E2", peek()),
    ("Фокус", "режим концентрации", "#ECEBEF",
     ape(eyes=("open", "open"), brows="focused", mouth="flat", extras=("headphones", "glasses"))),
    ("Подмигнул", "онбординг, подсказка дня", "#FFF1E6",
     ape(yaw=0.2, eyes=("open", "happy"), mouth="smirk", brows="raised")),
    ("Сплю", "простой, нет активности", "#EAF1F6",
     ape(pitch=-0.3, eyes=("closed", "closed"), mouth="flat", extras=("zzz",))),
    ("Висит на скобке", "404 — «держись, сейчас найдём»", "#F3EEE7", hanging()),
]
for i, (n, _, *rest) in enumerate(angles):
    save(f"angle-{i + 1}", svg(rest[-1]))
for i, (n, _, bg, body) in enumerate(states):
    save(f"state-{i + 1}", svg(body))

# ---------- lockup ----------
LOCKUP = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1400 512" width="700" height="256">
<g transform="translate(56 56) scale(0.78125)">{svg(ICON, C["ink"])}</g>
<text x="530" y="300" font-family="Avenir Next, Helvetica, sans-serif" font-weight="800" font-size="190" fill="#F4EDE6" letter-spacing="-4">hutan</text>
<rect x="1100" y="276" width="70" height="20" rx="4" fill="{C["gold"]}"><animate attributeName="opacity" values="1;1;0;0" dur="1.1s" repeatCount="indefinite"/></rect>
<text x="538" y="372" font-family="Menlo, monospace" font-size="38" fill="{C["hair"]}" letter-spacing="2">code from the canopy</text></svg>'''
save("lockup", LOCKUP)

# ---------- brand sheet ----------
CSS = """
*{box-sizing:border-box;margin:0}
body{font-family:'Avenir Next',-apple-system,Helvetica,sans-serif;background:#0F0E11;color:#F4EDE6;padding:56px 64px;width:1600px}
h2{font-size:15px;letter-spacing:.18em;text-transform:uppercase;color:#E0692E;margin:56px 0 20px;font-weight:700}
.hero{display:flex;align-items:center;justify-content:space-between;gap:40px}
.hero p{max-width:520px;color:#B9AFA6;font-size:20px;line-height:1.5}
.row{display:flex;gap:24px;align-items:flex-end}
.tile{text-align:center;color:#8F867E;font-size:14px}
.tile svg{display:block;margin-bottom:10px;border-radius:24px}
.sizes{display:flex;gap:28px;align-items:flex-end;margin-left:40px;padding-left:40px;border-left:1px solid #2A272E}
.grid{display:grid;gap:20px}
.g6{grid-template-columns:repeat(6,1fr)}.g4{grid-template-columns:repeat(4,1fr)}
.card{background:#1A181D;border-radius:24px;padding:18px 18px 20px}
.card .art{border-radius:18px;padding:8px;background:#232027}
.card svg{display:block;width:100%;height:auto}
.card h4{font-size:19px;margin:14px 2px 4px}
.card p{font-size:14px;color:#9C938A;margin:0 2px;line-height:1.4}
.why{display:grid;grid-template-columns:repeat(3,1fr);gap:20px}
.why div{background:#1A181D;border-radius:20px;padding:22px 24px}
.why b{display:block;font-size:18px;margin-bottom:8px;color:#F5B53D}
.why span{font-size:15px;color:#B9AFA6;line-height:1.5}
"""


def card(title, note, body, bg="#232027"):
    return (f'<div class="card"><div class="art" style="background:{bg}">{svg(body, size=300)}</div>'
            f'<h4>{title}</h4><p>{note}</p></div>')


why = [
    ("Детская схема", "Большая голова и крупные глаза (Kindchenschema) сразу вызывают симпатию и доверие, а ухмылка добавляет характер."),
    ("Лицо = статус", "Эмоцию на лице мозг считывает быстрее текста, поэтому по маскоту видно, что происходит: думает, готово или упало."),
    ("Ошибка без стресса", "Сочувствующая мордочка на ошибке снижает тревогу, а радость на успехе создаёт сильный финал (правило пика и конца)."),
    ("Один силуэт", "Одна и та же форма головы и палитра во всех ракурсах делают маскота узнаваемым, как у Duolingo или GitHub Octocat."),
    ("Выделяется в доке", "Один тёплый оранжевый на тёмном фоне заметен среди синих иконок инструментов разработчика (эффект фон Ресторф)."),
    ("Тренды 2026", "Маскот как основа бренда, плоский стиль с мягким объёмом, скруглённая геометрия, тёмная тема по умолчанию и живые состояния."),
]

html = f"""<!doctype html><html><head><meta charset="utf-8"><title>hutan mascot</title><style>{CSS}</style></head><body>
<div class="hero">{LOCKUP}<p>Маскот-система на основе первого логотипа: один и тот же орангутан в разных ракурсах и эмоциях, каждая эмоция отвечает за своё состояние интерфейса.</p></div>

<h2>Иконка приложения</h2>
<div class="row">
{''.join(f'<div class="tile">{svg(ICON, bg, size=180)}{n}</div>' for (n, _), bg in zip(icons, (C["ink"], C["cream"], C["gold"])))}
<div class="sizes">
{''.join(f'<div class="tile">{svg(ape(detail=s >= 64, extras=("badge",) if s >= 64 else ()), C["ink"], size=s)}{s}px</div>' for s in (128, 64, 32, 16))}
</div></div>

<h2>Ракурсы</h2>
<div class="grid g6">{''.join(card(t, n, b) for t, n, b in angles)}</div>

<h2>Эмоции = состояния интерфейса</h2>
<div class="grid g4">{''.join(card(t, n, b, bg) for t, n, bg, b in states)}</div>

<h2>Почему это работает</h2>
<div class="why">{''.join(f'<div><b>{t}</b><span>{d}</span></div>' for t, d in why)}</div>
</body></html>"""

with open("mascot-sheet.html", "w") as f:
    f.write(html)
