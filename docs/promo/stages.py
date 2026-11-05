"""The staged screens of the promo film: made-up applications, drawn by Edge in headless mode.

Each fills a whole display, because a fake screen is scaled to fit one, and the film shows its top left 1120x630. Nothing
here looks like a real product, and the token on the checkout page is obviously not one.
"""
import os
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageChops

TERMINAL_BOX = (70, 92, 1050, 538)

BASE = """<!doctype html><meta charset="utf-8"><style>
*{box-sizing:border-box;margin:0}
html,body{width:100%;height:100%;overflow:hidden;background:#232838;font-family:'Segoe UI Variable Text','Segoe UI',sans-serif;color:#1c1f26}
.win{position:absolute;background:#fff;border-radius:10px;overflow:hidden;box-shadow:0 18px 50px rgba(0,0,0,.45)}
.bar{height:42px;background:#f3f4f6;border-bottom:1px solid #e3e5ea;display:flex;align-items:center;padding:0 16px;font-size:14px;color:#50545f}
.bar .btns{margin-left:auto;display:flex;gap:22px;color:#7a7e89;font-size:13px}
</style>"""

COPY = {
    "en": {
        "checkoutTitle": "Acme Store: checkout",
        "nav": ["Cart", "Delivery", "Payment", "Review"],
        "summary": "Order summary",
        "rows": [("Wireless keyboard", "$39.00"), ("Delivery", "$5.00"), ("Tax", "$4.00")],
        "total": ("Total", "$48.00"),
        "pay": "Pay $48.00 now",
        "postTitle": "Morning digest",
        "author": "Nina Volkova",
        "when": "Today, 09:41",
        "post": ["Вышло большое обновление: поиск теперь работает", "без интернета, а папки можно закреплять сверху."],
        "postRest": "Подробности и список изменений в закреплённом сообщении.",
        "gameTitle": "Star Harbor",
        "callTitle": "Weekly sync",
        "picture": ["Миссия выполнена", "Награда: 120 кристаллов"],
    },
    "ru": {
        "checkoutTitle": "Магазин Acme: оформление заказа",
        "nav": ["Корзина", "Доставка", "Оплата", "Проверка"],
        "summary": "Ваш заказ",
        "rows": [("Беспроводная клавиатура", "3 900 ₽"), ("Доставка", "500 ₽"), ("Налог", "400 ₽")],
        "total": ("Итого", "4 800 ₽"),
        "pay": "Оплатить 4 800 ₽ сейчас",
        "postTitle": "Утренний дайджест",
        "author": "Nina Volkova",
        "when": "Сегодня, 09:41",
        "post": ["A big update is out: search now works without", "the internet, and folders can be pinned to the top."],
        "postRest": "Details and the full list of changes are in the pinned message.",
        "gameTitle": "Star Harbor",
        "callTitle": "Созвон команды",
        "picture": ["Mission complete", "Reward: 120 crystals"],
    },
}


def checkout(copy):
    nav = "".join(f'<div style="padding:11px 14px;border-radius:8px;font-size:15px;{"background:#eef2ff;color:#3346c8;font-weight:600" if i == 2 else "color:#50545f"}">{name}</div>' for i, name in enumerate(copy["nav"]))
    rows = "".join(f'<div style="display:flex;justify-content:space-between;padding:9px 0;font-size:15.5px;color:#3a3e49"><span>{a}</span><span>{b}</span></div>' for a, b in copy["rows"])
    return BASE + f"""
<div class="win" style="left:60px;top:40px;width:1000px;height:550px">
  <div class="bar">{copy['checkoutTitle']}<div class="btns"><span>&#8212;</span><span>&#9633;</span><span>&#10005;</span></div></div>
  <div style="position:absolute;left:18px;top:62px;width:190px">{nav}</div>
  <div style="position:absolute;left:280px;top:92px;width:420px;height:256px;background:#fff;border:1px solid #e3e5ea;border-radius:12px;padding:20px 24px;box-shadow:0 2px 8px rgba(20,25,40,.06)">
    <div style="font-size:19px;font-weight:600;margin-bottom:8px">{copy['summary']}</div>
    {rows}
    <div style="display:flex;justify-content:space-between;border-top:1px solid #e3e5ea;margin-top:6px;padding-top:10px;font-size:17px;font-weight:600"><span>{copy['total'][0]}</span><span>{copy['total'][1]}</span></div>
  </div>
  <div style="position:absolute;left:380px;top:328px;width:420px;height:44px;background:#2f5be0;border-radius:9px;color:#fff;font-weight:600;font-size:16px;display:flex;align-items:center;justify-content:center">{copy['pay']}</div>
  <div style="position:absolute;left:280px;top:386px;width:420px;font:14px Consolas,monospace;color:#5b6070;background:#f6f7f9;border:1px solid #e3e5ea;border-radius:7px;padding:10px 14px">api_token = demo-8f3a21c7d94e60b5</div>
</div>"""


def post(copy, selected):
    lines = copy["post"]
    highlight = "background:#b6d4fe;" if selected else ""
    rows = "".join(f'<div style="position:absolute;left:{LINE_X}px;top:{LINE_Y + i * LINE_H}px;height:{LINE_H}px;line-height:{LINE_H}px;font-size:21px;white-space:nowrap"><span class="line" style="{highlight}">{line}</span></div>' for i, line in enumerate(lines))
    return BASE + f"""
<div class="win" style="left:60px;top:40px;width:1000px;height:550px">
  <div class="bar">{copy['postTitle']}<div class="btns"><span>&#8212;</span><span>&#9633;</span><span>&#10005;</span></div></div>
</div>
<div style="position:absolute;left:150px;top:128px;width:48px;height:48px;border-radius:24px;background:#6d7fe8;color:#fff;display:flex;align-items:center;justify-content:center;font-weight:600;font-size:18px">NV</div>
<div style="position:absolute;left:{LINE_X}px;top:122px;font-size:17px;font-weight:600">{copy['author']}</div>
<div style="position:absolute;left:{LINE_X}px;top:148px;font-size:13.5px;color:#7a7e89">{copy['when']}</div>
{rows}
<div style="position:absolute;left:{LINE_X}px;top:{LINE_Y + 2 * LINE_H + 216}px;font-size:21px;color:#3a3e49">{copy['postRest']}</div>"""


LINE_X = 214
LINE_Y = 192
LINE_H = 36


def picture(copy):
    a, b = copy["picture"]
    return BASE + f"""
<div class="win" style="left:60px;top:40px;width:1000px;height:550px;background:#0d1020">
  <div class="bar" style="background:#171a2c;border-color:#23273d;color:#aab0c8">{copy['gameTitle']}<div class="btns" style="color:#7f86a3"><span>&#8212;</span><span>&#9633;</span><span>&#10005;</span></div></div>
  <div style="position:absolute;inset:42px 0 0 0;background:radial-gradient(circle at 70% 30%,#3a2d78 0,#141735 55%,#0d1020 100%)"></div>
  <div style="position:absolute;left:62px;top:196px;color:#ffffff;font-weight:600;font-size:40px">{a}</div>
  <div style="position:absolute;left:62px;top:262px;color:#e6e8f8;font-weight:400;font-size:26px">{b}</div>
  <div style="position:absolute;left:690px;top:120px;width:220px;height:220px;border-radius:50%;background:radial-gradient(circle at 35% 30%,#ffd98a,#d9663a 60%,#5b2448)"></div>
</div>"""


def call(copy):
    tile = lambda name, initials, color: f'<div style="flex:1;background:#232735;border-radius:12px;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:12px;color:#d9dcf2;font-size:15px"><div style="width:84px;height:84px;border-radius:42px;background:{color};display:flex;align-items:center;justify-content:center;color:#fff;font-size:30px;font-weight:600">{initials}</div>{name}</div>'
    return BASE + f"""
<div class="win" style="left:60px;top:40px;width:1000px;height:550px;background:#171a24">
  <div class="bar" style="background:#1d2130;border-color:#2a2e3e;color:#a8adba">{copy['callTitle']}<div class="btns" style="color:#7d828f"><span>&#8212;</span><span>&#9633;</span><span>&#10005;</span></div></div>
  <div style="position:absolute;inset:60px 18px 78px 18px;display:flex;gap:14px">{tile('Nina Volkova', 'NV', '#6d7fe8')}{tile('Alex Moreno', 'AM', '#d9663a')}</div>
</div>"""


def terminal():
    x0, y0, x1, y1 = TERMINAL_BOX
    return BASE + f"""
<div class="win" style="left:{x0}px;top:{y0}px;width:{x1 - x0}px;height:{y1 - y0}px;background:#14161c">
  <div class="bar" style="background:#1d2028;border-color:#2a2e38;color:#a8adba">claude<div class="btns" style="color:#7d828f"><span>&#8212;</span><span>&#9633;</span><span>&#10005;</span></div></div>
</div>"""


def end():
    return BASE + "<style>html,body{background:#14171f}</style>"


def edge():
    for root in (os.environ.get("ProgramFiles(x86)", ""), os.environ.get("ProgramFiles", "")):
        path = Path(root) / "Microsoft/Edge/Application/msedge.exe"
        if path.exists():
            return path
    raise RuntimeError("Microsoft Edge is needed to draw the stages")


def draw(html, name, folder, size):
    source = folder / f"{name}.html"
    source.write_text(html, encoding="utf-8")
    out = folder / f"{name}.png"
    with tempfile.TemporaryDirectory(prefix="edge-") as profile:
        subprocess.run([str(edge()), "--headless=new", "--disable-gpu", "--hide-scrollbars", f"--user-data-dir={profile}",
                        "--force-device-scale-factor=1", f"--window-size={size[0]},{size[1]}", f"--screenshot={out}", source.as_uri()],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
    if not out.exists():
        raise RuntimeError(f"Edge drew nothing for {name}")
    # A fake screen is scaled to fit the display, and every coordinate of the film would then miss.
    with Image.open(out) as image:
        if image.size != tuple(size):
            raise RuntimeError(f"{name} is {image.size}, the display is {size}")
    return out


def render_stages(lang, folder, size):
    copy = COPY[lang]
    stages = {
        "lang": lang,
        "checkout": draw(checkout(copy), "checkout", folder, size),
        "post": draw(post(copy, False), "post", folder, size),
        "post_selected": draw(post(copy, True), "post_selected", folder, size),
        "picture": draw(picture(copy), "picture", folder, size),
        "terminal": draw(terminal(), "terminal", folder, size),
        "call": draw(call(copy), "call", folder, size),
        "end": draw(end(), "end", folder, size),
        "post_text": " ".join(copy["post"]),
        "picture_rect": (96, 222, 560, 366),
        "picture_text": copy["picture"],
    }
    # Where the selected lines are, measured from the two drawings: where they differ.
    plain = Image.open(stages["post"]).convert("RGB")
    selected = Image.open(stages["post_selected"]).convert("RGB")
    lines = []
    for index in range(len(copy["post"])):
        y = LINE_Y + index * LINE_H
        box = ImageChops.difference(plain, selected).crop((0, y, plain.width, y + LINE_H)).getbbox()
        if box is None:
            raise RuntimeError(f"line {index} of the post is not selected in the drawing")
        lines.append((box[0], y + box[1], box[2] - box[0], box[3] - box[1]))
    stages["post_lines"] = lines
    return stages
