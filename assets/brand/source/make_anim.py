"""Animated hutan mascot: idle (blink), thinking (loading), success (joy) + a loading→success demo page."""
import os
import random

from make_mascot import ape, C

os.makedirs("anim", exist_ok=True)

BASE_CSS = """
.body{transform-box:view-box;transform-origin:256px 470px}
.eye{transform-box:fill-box;transform-origin:center;animation:blink 4.2s infinite}
@keyframes blink{0%,90%,100%{transform:scaleY(1)}93%{transform:scaleY(.08)}96%{transform:scaleY(1)}}
.tufts path{transform-box:fill-box;transform-origin:50% 100%;animation:sway 2.6s ease-in-out infinite alternate}
.tufts path:nth-child(2){animation-delay:-.9s}.tufts path:nth-child(3){animation-delay:-1.7s}
@keyframes sway{from{transform:rotate(-7deg)}to{transform:rotate(7deg)}}
.shadow{transform-box:fill-box;transform-origin:center}
@media (prefers-reduced-motion:reduce){*{animation:none!important}}
"""

IDLE_CSS = """
.body{animation:breathe 3.2s ease-in-out infinite}
@keyframes breathe{0%,100%{transform:translateY(0)}50%{transform:translateY(-6px) scale(1.01)}}
"""

THINK_CSS = """
.body{animation:tilt 3.2s ease-in-out infinite}
@keyframes tilt{0%,100%{transform:rotate(-3deg)}50%{transform:rotate(3deg)}}
.eyes{animation:look 3.2s ease-in-out infinite}
@keyframes look{0%,35%{transform:translateX(-7px)}50%,85%{transform:translateX(8px)}100%{transform:translateX(-7px)}}
.bubble{animation:float 2s ease-in-out infinite}
@keyframes float{0%,100%{transform:translateY(0)}50%{transform:translateY(-6px)}}
.dot{transform-box:fill-box;transform-origin:center;animation:dot 1s ease-in-out infinite}
.dot:nth-child(2){animation-delay:.15s}.dot:nth-child(3){animation-delay:.3s}
@keyframes dot{0%,60%,100%{transform:translateY(0);opacity:.35}30%{transform:translateY(-9px);opacity:1}}
.hand{transform-box:fill-box;transform-origin:30% 80%;animation:tap 1.6s ease-in-out infinite}
@keyframes tap{0%,100%{transform:rotate(0)}20%{transform:rotate(-6deg)}40%{transform:rotate(0)}}
"""

SUCCESS_CSS = """
.body{animation:jump 1.4s cubic-bezier(.3,.7,.4,1) {it} both}
@keyframes jump{
  0%{transform:translateY(0) scale(1,1)}
  14%{transform:translateY(0) scale(1.07,.9)}
  38%{transform:translateY(-46px) scale(.95,1.06)}
  60%{transform:translateY(0) scale(1.05,.94)}
  74%{transform:translateY(-8px) scale(1,1)}
  86%,100%{transform:translateY(0) scale(1,1)}}
.shadow{animation:shadow 1.4s cubic-bezier(.3,.7,.4,1) {it} both}
@keyframes shadow{0%,14%,60%,86%,100%{transform:scaleX(1);opacity:.35}38%{transform:scaleX(.6);opacity:.15}}
.spark{transform-box:fill-box;transform-origin:center;animation:pop 1.4s ease-out {it} both}
.s1{animation-delay:.12s}.s2{animation-delay:.24s}
@keyframes pop{0%{transform:scale(0) rotate(-40deg);opacity:0}30%{transform:scale(1.3) rotate(10deg);opacity:1}
  50%{transform:scale(1) rotate(0)}80%{opacity:1}100%{transform:scale(1);opacity:{end}}}
.confetti rect{transform-box:fill-box;transform-origin:center;animation:fall 1.8s linear {it} both}
@keyframes fall{0%{transform:translateY(-60px) rotate(0);opacity:0}10%{opacity:1}100%{transform:translateY(560px) rotate(540deg);opacity:1}}
"""


def confetti():
    rnd = random.Random(7)
    colors = [C["gold"], C["hair"], "#7CC4F5", "#8ED1A0", "#F28AB2"]
    out = []
    for i in range(18):
        x = rnd.randint(20, 490)
        out.append(f'<rect x="{x}" y="-20" width="{rnd.randint(8, 14)}" height="{rnd.randint(14, 22)}" rx="3" '
                   f'fill="{colors[i % len(colors)]}" style="animation-delay:{rnd.uniform(0, 1.2):.2f}s;'
                   f'animation-duration:{rnd.uniform(1.4, 2.2):.2f}s"/>')
    return f'<g class="confetti">{"".join(out)}</g>'


def animated(name, css, body, extra_front="", shadow=True):
    sh = f'<ellipse class="shadow" cx="256" cy="478" rx="150" ry="16" fill="#000" opacity=".35"/>' if shadow else ""
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">'
           f'<style>{BASE_CSS}{css}</style>{sh}'
           f'<g class="body"><g transform="translate(31 22) scale(.88)">{body}</g></g>{extra_front}</svg>')
    with open(f"anim/{name}.svg", "w") as f:
        f.write(svg)


animated("hutan-idle", IDLE_CSS, ape())
animated("hutan-thinking", THINK_CSS,
         ape(yaw=0.25, look=(0, -7), mouth="flat", extras=("hand", "thinking")))
joy = ape(eyes=("happy", "happy"), mouth="smile", brows="raised", extras=("sparkles",))
animated("hutan-success", SUCCESS_CSS.replace("{it}", "infinite").replace("{end}", "0"), joy, confetti())
animated("hutan-success-once", SUCCESS_CSS.replace("{it}", "1").replace("{end}", "1"), joy, confetti())

DEMO = """<!doctype html><html lang="ru"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>hutan — анимации</title>
<style>
*{box-sizing:border-box;margin:0}
body{font-family:'Avenir Next',-apple-system,Helvetica,sans-serif;background:#0F0E11;color:#F4EDE6;
  min-height:100vh;display:flex;flex-direction:column;align-items:center;padding:48px 16px;gap:48px}
.card{width:min(420px,100%);background:#1A181D;border-radius:28px;padding:28px;text-align:center}
.card img{width:220px;height:220px}
h1{font-size:26px;margin-top:8px}
.sub{color:#9C938A;margin-top:6px;min-height:1.4em}
.bar{height:8px;background:#2A272E;border-radius:99px;margin:22px 0;overflow:hidden}
.bar i{display:block;height:100%;width:0;background:linear-gradient(90deg,#E0692E,#F5B53D);border-radius:99px;transition:width .4s ease}
button{font:inherit;font-weight:700;font-size:17px;border:0;border-radius:14px;padding:14px 28px;
  background:#F5B53D;color:#17151A;cursor:pointer}
button:disabled{opacity:.4;cursor:default}
.loops{display:flex;gap:20px;flex-wrap:wrap;justify-content:center}
.loops figure{background:#1A181D;border-radius:22px;padding:14px;text-align:center;color:#9C938A;font-size:14px}
.loops img{width:180px;height:180px;display:block;margin-bottom:6px}
.loops b{color:#F4EDE6;display:block;font-size:16px}
</style></head><body>
<div class="card">
  <img id="ape" src="hutan-idle.svg" alt="маскот hutan">
  <h1 id="title">Готов к деплою</h1>
  <p class="sub" id="sub">Нажми кнопку — орангутан всё сделает</p>
  <div class="bar"><i id="bar"></i></div>
  <button id="go">Задеплоить</button>
</div>
<div class="loops">
  <figure><img src="hutan-idle.svg" alt=""><b>Ожидание</b>моргает и дышит</figure>
  <figure><img src="hutan-thinking.svg" alt=""><b>Загрузка</b>думает, точки бегут</figure>
  <figure><img src="hutan-success.svg" alt=""><b>Успех</b>прыжок и конфетти</figure>
</div>
<script>
const $ = id => document.getElementById(id);
const steps = ["Собираю проект…", "Гоняю тесты…", "Выкатываю на сервер…"];
let busy = false;
function setApe(name){ $("ape").src = name + ".svg?t=" + Date.now(); }
$("go").onclick = () => {
  if (busy) return;
  if ($("go").dataset.done) { reset(); return; }
  busy = true; $("go").disabled = true;
  setApe("hutan-thinking"); $("title").textContent = "Думаю…";
  const t0 = performance.now(); let i = 0;
  const tick = setInterval(() => {
    $("sub").textContent = steps[Math.min(i, steps.length - 1)];
    $("bar").style.width = Math.min(100, (i + 1) * 34) + "%"; i++;
    if (i > steps.length) {
      clearInterval(tick);
      const sec = ((performance.now() - t0) / 1000).toFixed(1);
      setApe("hutan-success-once"); $("title").textContent = "Готово!";
      $("sub").textContent = "Деплой прошёл за " + sec + " с";
      $("go").textContent = "Ещё раз"; $("go").disabled = false; $("go").dataset.done = 1; busy = false;
    }
  }, 1100);
};
function reset(){
  delete $("go").dataset.done; setApe("hutan-idle"); $("bar").style.width = "0";
  $("title").textContent = "Готов к деплою"; $("sub").textContent = "Нажми кнопку — орангутан всё сделает";
  $("go").textContent = "Задеплоить";
}
</script></body></html>"""

with open("anim/demo.html", "w") as f:
    f.write(DEMO)
