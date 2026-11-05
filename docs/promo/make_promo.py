#!/usr/bin/env python3
"""Makes docs/promo.gif and docs/promo.mp4 (and their .ru versions) from a built binary.

Every window on the film is drawn by Shotik itself: a ghost instance with a staged screen, driven through its test
routes, with each state rendered to a file. The stages are ours, so nothing real can end up on a picture, the desktop
and the clipboard stay untouched, and the timeline does not depend on how long anything takes. The cursor, the captions
and the terminal are drawn here.

    python docs/promo/make_promo.py <shotik.exe> [--lang en,ru] [--only sheet]

Needs Pillow, ffmpeg and Microsoft Edge (for the stages). Translation is real, so it needs the network.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

from stages import render_stages, TERMINAL_BOX

HERE = Path(__file__).resolve().parent
DOCS = HERE.parent

SCREEN = (1120, 630)  # the part of the display the film shows, at full size
BAND = 70
SIZE = (SCREEN[0], SCREEN[1] + BAND)
FPS = 30
BAND_COLOR = (14, 17, 23)
INK = (240, 240, 236)
ACCENT = (58, 135, 229)

FONTS = "C:/Windows/Fonts/"


def font(name, size):
    return ImageFont.truetype(FONTS + name, size)


TEXT = {
    "en": {
        "press": "Press PrtSc. The screen freezes in 150 ms.",
        "markup": "Select. Mark it up. Hide your secrets.",
        "claude": "One key, and it is in Claude.",
        "toastTitle": "Ready for Claude",
        "toastBody": "Shot_2026-11-05_10-42-18.png",
        "mcp": "Or Claude looks for itself: built-in MCP.",
        "translate": "Select text anywhere, get a translation.",
        "livetext": "Even text inside pictures.",
        "record": "Record the screen or a call.",
        "end": "Free and open source. Rewritten in Rust.",
        "was": "was",
        "wasInstaller": "71 MB",
        "wasTray": "288 MB",
        "wasOverlay": "2 s and more",
        "statInstaller": "installer",
        "statTray": "in the tray",
        "statOverlay": "to the overlay",
        "mb": "MB",
        "ms": "ms",
        "ask": "something is off on this page, see it?",
        "image": "[Image #1]",
        "answer": ["I see it: the total is $48.00, but the button charges $84.00.",
                   "The digits are swapped. And your token is hidden, so that is safe."],
        "askMcp": "now check my screen yourself",
        "answerMcp": ["The button still says $84.00, so it is not fixed yet."],
        "toolCall": "shotik · take_screenshot",
    },
    "ru": {
        "press": "Нажми PrtSc. Экран замирает за 150 мс.",
        "markup": "Выдели. Отметь. Скрой секреты.",
        "claude": "Одна клавиша, и это уже в Claude.",
        "toastTitle": "Готово для Claude",
        "toastBody": "Shot_2026-11-05_10-42-18.png",
        "mcp": "Или Claude посмотрит сам: встроенный MCP.",
        "translate": "Выдели текст где угодно и получи перевод.",
        "livetext": "Даже текст на картинках.",
        "record": "Запись экрана и созвонов.",
        "end": "Бесплатно и с открытым кодом. Переписан на Rust.",
        "was": "было",
        "wasInstaller": "71 МБ",
        "wasTray": "288 МБ",
        "wasOverlay": "2 с и больше",
        "statInstaller": "установщик",
        "statTray": "в трее",
        "statOverlay": "до оверлея",
        "mb": "МБ",
        "ms": "мс",
        "ask": "что-то не так на этой странице, видишь?",
        "image": "[Image #1]",
        "answer": ["Вижу: итого 4 800 ₽, а кнопка списывает 8 400 ₽.",
                   "Цифры перепутаны. А токен скрыт, так что всё безопасно."],
        "askMcp": "теперь проверь мой экран сам",
        "answerMcp": ["Кнопка всё ещё показывает 8 400 ₽, пока не исправлено."],
        "toolCall": "shotik · take_screenshot",
    },
}


ICON = Image.open(DOCS.parent / "assets" / "icon-64.png").convert("RGBA").resize((36, 36), Image.Resampling.LANCZOS)


def ease(t):
    return t * t * (3 - 2 * t)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class App:
    """A private ghost instance on a staged screen."""

    def __init__(self, exe, screen, lang, temp, translate=False, env=None, settings=None, display=None, ui_scale=1):
        self.display = display
        self.ui_scale = ui_scale
        self.temp = Path(tempfile.mkdtemp(prefix="app-", dir=temp))
        self.port = free_port()
        profile = self.temp / "appdata" / "Shotik-Test"
        profile.mkdir(parents=True)
        saved = self.temp / "saved"
        saved.mkdir()
        (self.temp / "tmp").mkdir()
        stored = {"saveDir": str(saved), "language": lang, "checkUpdates": False, "autoSave": False,
                  "translate": {"target": lang}}
        stored.update(settings or {})
        (profile / "settings.json").write_text(json.dumps(stored), encoding="utf-8")
        (profile / "history.json").write_text("[]", encoding="utf-8")
        child_env = {
            **os.environ,
            "APPDATA": str(self.temp / "appdata"), "TEMP": str(self.temp / "tmp"), "TMP": str(self.temp / "tmp"),
            "SHOTIK_TEST": "1", "SHOTIK_GHOST": "1", "SHOTIK_PORT": str(self.port), "SHOTIK_SAVE_DIR": str(saved),
            "SHOTIK_FAKE_SCREEN": str(screen), "SHOTIK_REAL_CAPTURE": "0", **(env or {}),
        }
        if not translate:
            child_env["SHOTIK_TRANSLATE_STUB"] = "1"
        self.child = subprocess.Popen([str(exe), "--hidden", "--test"], env=child_env,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.time() + 30
        while True:
            try:
                if self.post("ping") == "pong":
                    break
            except (OSError, urllib.error.URLError):
                if time.time() > deadline:
                    raise RuntimeError("the application did not answer")
                time.sleep(0.05)
        self.index = 0

    def post(self, route, timeout=60, **body):
        data = json.dumps(body).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.port}/test/{route}", data=data,
                                         headers={"content-type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                reply = json.load(response)
        except urllib.error.HTTPError as e:
            reply = json.load(e)
        if not reply.get("ok"):
            raise RuntimeError(f"{route}: {reply.get('error')}")
        return reply["result"]

    def capture(self, target, index=0):
        path = self.temp / "frame.png"
        self.post("capture-page", target=target, index=index, path=str(path))
        with Image.open(path) as image:
            return image.convert("RGB")

    def small(self, target, scale=None):
        """A small window, keeping its transparency. Windows that open on the primary display come at its scale, and on
        the film everything is at 100 %."""
        path = self.temp / "small.png"
        self.post("capture-page", target=target, path=str(path))
        with Image.open(path) as image:
            image = image.convert("RGBA")
            factor = self.ui_scale if scale is None else scale
            if factor != 1:
                image = image.resize((round(image.width / factor), round(image.height / factor)), Image.Resampling.LANCZOS)
            return image

    def overlays(self):
        """The open overlay windows, one per display."""
        found = []
        for index in range(8):
            try:
                found.append((index, self.post("ui-state", target="overlay", index=index)))
            except RuntimeError:
                break
        return found

    def open_overlay(self, mode="region"):
        self.post("trigger", mode=mode)
        for index, state in self.overlays():
            viewport = state["viewport"]
            if (int(viewport["width"]), int(viewport["height"])) == self.display:
                self.index = index
                return state
        raise RuntimeError(f"no overlay on the {self.display} display")

    def act(self, kind, **fields):
        return self.post("ui-action", target="overlay", index=self.index, action={"type": kind, **fields})

    def shot(self):
        return self.capture("overlay", self.index).crop((0, 0, *SCREEN))

    def stop(self):
        try:
            self.post("quit", timeout=3)
        except (OSError, RuntimeError, urllib.error.URLError):
            pass
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.kill()
        shutil.rmtree(self.temp, ignore_errors=True)


def arrow_sprite():
    """The usual pointer, drawn large and scaled down so its edges are smooth."""
    scale = 4
    points = [(0, 0), (0, 17), (4, 13), (7, 20), (10, 19), (7, 12), (13, 12)]
    sprite = Image.new("RGBA", (22 * scale, 24 * scale), (0, 0, 0, 0))
    draw = ImageDraw.Draw(sprite)
    scaled = [(x * scale + 4, y * scale + 4) for x, y in points]
    draw.polygon(scaled, fill=(255, 255, 255, 255), outline=(0, 0, 0, 255), width=scale + 1)
    inner = [(x * scale + 4, y * scale + 4) for x, y in points]
    draw.line(inner + [inner[0]], fill=(0, 0, 0, 255), width=scale + 1, joint="curve")
    return sprite.resize((22, 24), Image.Resampling.LANCZOS), (1, 1)


def cross_sprite():
    scale = 4
    size = 23 * scale
    sprite = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(sprite)
    middle = size // 2
    for color, width in (((0, 0, 0, 200), 3 * scale), ((255, 255, 255, 255), scale)):
        draw.line([(middle, 0), (middle, middle - 3 * scale)], fill=color, width=width)
        draw.line([(middle, middle + 3 * scale), (middle, size)], fill=color, width=width)
        draw.line([(0, middle), (middle - 3 * scale, middle)], fill=color, width=width)
        draw.line([(middle + 3 * scale, middle), (size, middle)], fill=color, width=width)
    return sprite.resize((23, 23), Image.Resampling.LANCZOS), (11, 11)


def glow_of(image, radius=7, opacity=110, offset=(0, 3)):
    """A soft shadow for a window with its own shape: the bubble is white, and so is the page under it."""
    pad = radius * 2
    canvas = Image.new("RGBA", (image.width + pad * 2, image.height + pad * 2), (0, 0, 0, 0))
    shadow = Image.new("L", canvas.size, 0)
    shadow.paste(image.getchannel("A").point([v * opacity // 255 for v in range(256)]), (pad + offset[0], pad + offset[1]))
    canvas.putalpha(shadow.filter(ImageFilter.GaussianBlur(radius)))
    canvas.alpha_composite(image, (pad, pad))
    return canvas, pad


def shadow_of(image, radius=14, opacity=120, offset=(0, 6), corner=8):
    """A window on the stage: rounded, with a soft shadow, ready to paste."""
    w, h = image.size
    pad = radius * 2
    canvas = Image.new("RGBA", (w + pad * 2, h + pad * 2), (0, 0, 0, 0))
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, w - 1, h - 1), corner, fill=255)
    shadow = Image.new("L", canvas.size, 0)
    shadow.paste(mask.point([v * opacity // 255 for v in range(256)]), (pad + offset[0], pad + offset[1]))
    canvas.putalpha(shadow.filter(ImageFilter.GaussianBlur(radius)))
    canvas.alpha_composite(Image.merge("RGBA", (*image.convert("RGB").split(), mask)), (pad, pad))
    return canvas, pad


class Film:
    def __init__(self, sink):
        self.sink = sink
        self.screen = Image.new("RGB", SCREEN, (20, 22, 30))
        self.cursor = None
        self.pos = (560.0, 315.0)
        self.caption = ""
        self.caption_alpha = 1.0
        self.layers = []
        self.chip = None
        self.frames = 0

    def band(self):
        band = Image.new("RGB", (SIZE[0], BAND), BAND_COLOR)
        band.paste(ICON, (20, 17), ICON)
        if self.caption:
            ink = tuple(int(BAND_COLOR[i] + (INK[i] - BAND_COLOR[i]) * self.caption_alpha) for i in range(3))
            face = font("seguisb.ttf", 27)
            width = face.getlength(self.caption)
            ImageDraw.Draw(band).text(((SIZE[0] - width) / 2, 17), self.caption, font=face, fill=ink)
        return band

    def frame(self):
        canvas = Image.new("RGB", SIZE)
        canvas.paste(self.band(), (0, 0))
        screen = self.screen.copy().convert("RGBA")
        for image, pos in self.layers:
            screen.alpha_composite(image, (max(int(pos[0]), 0), max(int(pos[1]), 0)))
        if self.chip:
            screen.alpha_composite(self.chip, ((SCREEN[0] - self.chip.width) // 2, SCREEN[1] - self.chip.height - 26))
        if self.cursor:
            sprite, hot = self.cursor
            x, y = int(self.pos[0] - hot[0]), int(self.pos[1] - hot[1])
            if 0 <= x < SCREEN[0] and 0 <= y < SCREEN[1]:
                screen.alpha_composite(sprite, (x, y))
        canvas.paste(screen.convert("RGB"), (0, BAND))
        return canvas

    def emit(self, image=None):
        self.sink.write((image or self.frame()).tobytes())
        self.frames += 1

    def hold(self, seconds):
        frame = self.frame()
        for _ in range(int(round(seconds * FPS))):
            self.emit(frame)

    def move(self, to, seconds):
        start = self.pos
        count = max(int(seconds * FPS), 1)
        for i in range(1, count + 1):
            t = ease(i / count)
            self.pos = (start[0] + (to[0] - start[0]) * t, start[1] + (to[1] - start[1]) * t)
            self.emit()

    def say(self, text):
        if self.caption:
            for i in range(5):
                self.caption_alpha = 1 - (i + 1) / 5
                self.emit()
        self.caption = text
        for i in range(6):
            self.caption_alpha = (i + 1) / 6
            self.emit()
        self.caption_alpha = 1.0

    def fade_to(self, screen, seconds=0.3):
        old = self.frame()
        self.screen = screen
        new = self.frame()
        count = max(int(seconds * FPS), 1)
        for i in range(1, count + 1):
            self.emit(Image.blend(old, new, ease(i / count)))


def key_chip(text):
    face = font("seguisb.ttf", 24)
    width = int(face.getlength(text)) + 44
    chip = Image.new("RGBA", (width, 54), (0, 0, 0, 0))
    draw = ImageDraw.Draw(chip)
    draw.rounded_rectangle((0, 3, width - 1, 53), 10, fill=(30, 32, 40, 255))
    draw.rounded_rectangle((0, 0, width - 1, 48), 10, fill=(58, 62, 74, 255))
    draw.text((22, 6), text, font=face, fill=(255, 255, 255, 255))
    return chip


def scene_capture(film, exe, stages, t, temp):
    """PrtSc, select, annotate, hide the token."""
    app = App(exe, stages["checkout"], stages["lang"], temp, display=stages["size"])
    try:
        film.screen = Image.open(stages["checkout"]).convert("RGB").crop((0, 0, *SCREEN))
        arrow, cross = arrow_sprite(), cross_sprite()
        film.cursor = arrow
        film.pos = (980, 560)
        film.say(t["press"])
        film.hold(0.3)
        film.chip = key_chip("PrtSc")
        film.hold(0.3)
        app.open_overlay()
        film.screen = app.shot()
        film.cursor = cross
        film.hold(0.4)
        film.chip = None

        start, end = (300, 104), (900, 520)
        film.say(t["markup"])
        film.move(start, 0.32)
        app.act("pointer-down", x=start[0], y=start[1])
        steps = 11
        for i in range(1, steps + 1):
            f = ease(i / steps)
            x, y = start[0] + (end[0] - start[0]) * f, start[1] + (end[1] - start[1]) * f
            app.act("pointer-move", x=x, y=y)
            film.screen = app.shot()
            film.pos = (x, y)
            film.emit()
            film.emit()
        app.act("pointer-up", x=end[0], y=end[1])
        film.screen = app.shot()
        film.hold(0.2)

        def stroke(tool, a, b, frames=12):
            app.act("set-tool", tool=tool)
            film.screen = app.shot()
            film.move(a, 0.26)
            app.act("pointer-down", x=a[0], y=a[1])
            for i in range(1, frames + 1):
                f = ease(i / frames)
                x, y = a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f
                app.act("pointer-move", x=x, y=y)
                film.screen = app.shot()
                film.pos = (x, y)
                film.emit()
                film.emit()
            app.act("pointer-up", x=b[0], y=b[1])
            film.screen = app.shot()
            film.hold(0.1)

        stroke("rect", (358, 362), (742, 418), frames=9)
        stroke("arrow", (850, 268), (752, 354), frames=9)
        stroke("blur", (442, 452), (624, 476), frames=8)
        film.hold(0.4)
        return film.screen
    finally:
        app.post("overlay-cancel")
        app.stop()


TERMINAL_FONT = "CascadiaMono.ttf" if os.path.exists(FONTS + "CascadiaMono.ttf") else "consola.ttf"


def terminal_lines(lines):
    """The terminal's text, on the stage's empty terminal."""
    def paint(stage):
        image = stage.copy()
        draw = ImageDraw.Draw(image)
        face = font(TERMINAL_FONT, 21)
        x, y = TERMINAL_BOX[0] + 28, TERMINAL_BOX[1] + 66
        for line in lines:
            if line is None:
                y += 14
                continue
            text, color = line
            draw.text((x, y), text, font=face, fill=color)
            y += 33
        return image
    return paint


def scene_claude(film, exe, stages, t, temp, annotated):
    """The real toast, then the paste into a terminal. The clipboard stays out of it: the user is working, so the
    action itself is not run, and the toast gets the picture it would have shown."""
    app = App(exe, stages["checkout"], stages["lang"], temp, display=stages["size"], ui_scale=stages["ui_scale"])
    try:
        film.say(t["claude"])
        film.chip = key_chip("A")
        film.hold(0.35)
        app.post("show-toast", kind="claude", title=t["toastTitle"], body=t["toastBody"])
        time.sleep(0.25)
        window = app.small("toast").convert("RGB")
        thumb = annotated.crop((300, 104, 900, 520)).resize((86, 60), Image.Resampling.LANCZOS).crop((1, 0, 85, 60))
        corners = Image.new("L", thumb.size, 0)
        ImageDraw.Draw(corners).rounded_rectangle((0, 0, thumb.width - 1, thumb.height - 1), 5, fill=255)
        window.paste(thumb, (16, 16), corners)
        toast, pad = shadow_of(window)
    finally:
        app.stop()
    film.chip = None
    film.cursor = None
    backdrop = Image.open(stages["checkout"]).convert("RGB").crop((0, 0, *SCREEN))
    film.fade_to(backdrop, 0.14)
    film.layers = [(toast, (SCREEN[0] - toast.width + pad - 16, SCREEN[1] - toast.height + pad - 16))]
    film.hold(0.85)
    film.layers = []

    stage = Image.open(stages["terminal"]).convert("RGB").crop((0, 0, *SCREEN))
    prompt, dim, text, tool = (132, 200, 255), (130, 134, 146), (232, 232, 228), (230, 170, 120)
    film.fade_to(stage, 0.24)
    lines = []
    typed = ""
    for char in t["ask"]:
        typed += char
        film.screen = terminal_lines([(("> " + typed + "▌"), prompt)])(stage)
        film.emit()
    film.chip = key_chip("Ctrl+V")
    film.hold(0.3)
    film.chip = None
    film.screen = terminal_lines([(f"> {t['ask']} {t['image']}▌", prompt)])(stage)
    film.hold(0.4)
    lines = [(f"> {t['ask']} {t['image']}", prompt), None]
    for line in t["answer"]:
        lines = lines + [("● " + line if line == t["answer"][0] else "  " + line, text)]
        film.screen = terminal_lines(lines)(stage)
        film.hold(0.25)
    film.hold(0.85)

    film.say(t["mcp"])
    lines = lines + [None]
    typed = ""
    for char in t["askMcp"]:
        typed += char
        film.screen = terminal_lines(lines + [("> " + typed + "▌", prompt)])(stage)
        film.emit()
    film.hold(0.12)
    lines = lines + [("> " + t["askMcp"], prompt), None]
    film.screen = terminal_lines(lines + [("● " + t["toolCall"], tool)])(stage)
    film.hold(0.45)
    lines = lines + [("● " + t["toolCall"], tool)]
    for line in t["answerMcp"]:
        lines = lines + [("● " + line, text)]
    film.screen = terminal_lines(lines)(stage)
    film.hold(1.1)


def scene_translate(film, exe, stages, t, temp):
    """Select text in any application: the bubble, a click, the real translation."""
    app = App(exe, stages["post"], stages["lang"], temp, translate=True, display=stages["size"], ui_scale=stages["ui_scale"])
    try:
        plain = Image.open(stages["post"]).convert("RGB").crop((0, 0, *SCREEN))
        selected = Image.open(stages["post_selected"]).convert("RGB").crop((0, 0, *SCREEN))
        film.fade_to(plain, 0.3)
        film.say(t["translate"])
        film.cursor = arrow_sprite()
        lines = stages["post_lines"]
        film.pos = (900, 560)
        first = lines[0]
        film.move((first[0], first[1] + first[3] / 2), 0.38)
        total = sum(line[2] for line in lines)
        steps = 26
        for i in range(1, steps + 1):
            reach = total * ease(i / steps)
            image = plain.copy()
            where = film.pos
            for x, y, w, h in lines:
                part = min(reach, w)
                if part > 0:
                    image.paste(selected.crop((x, y, int(x + part), y + h)), (x, y))
                    where = (x + part, y + h / 2)
                reach -= part
            film.screen = image
            film.pos = where
            film.emit()
        film.hold(0.12)
        text = stages["post_text"]
        end = lines[-1]
        anchor = (end[0] + end[2], end[1] + end[3])
        app.post("selection-bubble", text=text, x=1000, y=500)
        bubble = app.small("bubble")
        lifted, lift = glow_of(bubble)
        film.layers = [(lifted, (anchor[0] - lift + 4, anchor[1] - lift + 4))]
        film.hold(0.28)
        film.move((anchor[0] + 4 + bubble.width / 2, anchor[1] + 4 + bubble.height / 2), 0.24)
        film.hold(0.08)
        app.post("bubble-activate", wait=30)
        deadline = time.time() + 20
        loading = None
        while time.time() < deadline:
            state = app.post("ui-state", target="tp")
            if state.get("loading") and loading is None:
                loading = app.small("tp").convert("RGB")
            if not state.get("loading") and state.get("translated"):
                break
            time.sleep(0.03)
        else:
            raise RuntimeError("the translation did not arrive")
        print("translated:", state.get("translated"))
        result = app.small("tp").convert("RGB")
        pasted, pad = shadow_of(result)
        where = (min(anchor[0] - 150, SCREEN[0] - result.width - 40) - pad, min(anchor[1] + 22, SCREEN[1] - result.height - 14) - pad)
        if stages["lang"] != "ru" and state.get("translated") == text:
            raise RuntimeError("the post was not translated")
        if loading is not None:
            film.layers = [(shadow_of(loading)[0], (where[0], where[1]))]
            film.hold(0.22)
        film.layers = [(pasted, where)]
        film.move((where[0] + pad + result.width + 34, where[1] + pad + 40), 0.3)
        film.hold(1.5)
    finally:
        film.layers = []
        app.stop()


def scene_livetext(film, exe, stages, t, temp):
    """Live Text on a picture, and the real translation of what it read."""
    app = App(exe, stages["picture"], stages["lang"], temp, translate=True, display=stages["size"], ui_scale=stages["ui_scale"])
    try:
        film.fade_to(Image.open(stages["picture"]).convert("RGB").crop((0, 0, *SCREEN)), 0.3)
        film.say(t["livetext"])
        film.cursor = cross_sprite()
        film.pos = (980, 560)
        film.chip = key_chip("Ctrl+Shift+PrtSc")
        film.hold(0.35)
        app.open_overlay("text")
        film.screen = app.shot()
        film.hold(0.2)
        film.chip = None
        start, end = tuple(stages["picture_rect"][:2]), tuple(stages["picture_rect"][2:])
        film.move(start, 0.4)
        app.act("pointer-down", x=start[0], y=start[1])
        steps = 14
        for i in range(1, steps + 1):
            f = ease(i / steps)
            x, y = start[0] + (end[0] - start[0]) * f, start[1] + (end[1] - start[1]) * f
            app.act("pointer-move", x=x, y=y)
            film.screen = app.shot()
            film.pos = (x, y)
            film.emit()
            film.emit()
        state = app.act("pointer-up", x=end[0], y=end[1])
        if not state.get("textMode"):
            state = app.act("live-text")
        deadline = time.time() + 15
        while time.time() < deadline and not state.get("wordCount"):
            time.sleep(0.05)
            state = app.post("ui-state", target="overlay", index=app.index)
        print("words:", state.get("wordCount"))
        film.screen = app.shot()
        film.hold(0.32)
        state = app.act("text-select-all")
        film.screen = app.shot()
        film.hold(0.32)
        state = app.act("text-translate")
        deadline = time.time() + 20
        while time.time() < deadline:
            state = app.post("ui-state", target="overlay", index=app.index)
            shown = state.get("translate")
            if shown and shown.get("body") and not shown.get("loading"):
                break
            time.sleep(0.05)
        print("overlay translate:", state.get("translate"))
        deadline = time.time() + 6
        while time.time() < deadline and state.get("flash"):
            time.sleep(0.1)
            state = app.post("ui-state", target="overlay", index=app.index)
        film.screen = app.shot()
        film.hold(1.5)
    finally:
        film.cursor = None
        try:
            app.post("overlay-cancel")
        except RuntimeError:
            pass
        app.stop()


def scene_record(film, exe, stages, t, temp):
    """The real recording controls and border over the stage, with the real timer."""
    app = App(exe, stages["checkout"], stages["lang"], temp, env={"SHOTIK_FORCE_CONTROLS": "1"}, display=stages["size"], ui_scale=stages["ui_scale"],
              settings={"recording": {"countdown": False}})
    try:
        backdrop = Image.open(stages["checkout"]).convert("RGB").crop((0, 0, *SCREEN))
        film.fade_to(backdrop, 0.24)
        film.say(t["record"])
        rect = {"x": 300, "y": 104, "w": 600, "h": 416}
        target = next(d for d in app.post("state")["displays"] if (int(d["bounds"]["width"]), int(d["bounds"]["height"])) == stages["size"])
        box = {"x": rect["x"], "y": rect["y"], "width": rect["w"], "height": rect["h"]}
        app.post("record-start", displayId=target["id"], rectPhys=box, rectDip=box)
        time.sleep(0.4)
        border = app.small("rec-border", scale=1)
        border_at = (rect["x"] + (rect["w"] - border.width) // 2, rect["y"] + (rect["h"] - border.height) // 2)
        for _ in range(2):
            controls, pad = shadow_of(app.small("rec-controls", scale=1).convert("RGB"), corner=10)
            film.layers = [(border, border_at), (controls, ((SCREEN[0] - controls.width) // 2, rect["y"] + rect["h"] + 6 - pad))]
            film.hold(0.7)
            time.sleep(0.7)
        film.layers = []
        app.post("record-stop")
        deadline = time.time() + 15
        while time.time() < deadline and app.post("record-status").get("recording"):
            time.sleep(0.1)

        # A call: sound only, from synthetic sources in a test instance, with the level meters moving.
        film.fade_to(Image.open(stages["call"]).convert("RGB").crop((0, 0, *SCREEN)), 0.2)
        app.post("trigger", mode="audio", timeout=30)
        time.sleep(0.8)
        for _ in range(9):
            controls, pad = shadow_of(app.small("rec-controls", scale=1).convert("RGB"), corner=10)
            film.layers = [(controls, ((SCREEN[0] - controls.width) // 2, SCREEN[1] - controls.height + pad - 24))]
            film.hold(0.2)
            time.sleep(0.2)
    finally:
        film.layers = []
        try:
            app.post("record-stop")
        except RuntimeError:
            pass
        app.stop()


def scene_end(film, stages, t, lang):
    stage = Image.open(stages["end"]).convert("RGB").crop((0, 0, *SCREEN))
    window = Image.open(DOCS / ("app-dark.png" if lang == "en" else "app-dark.ru.png")).convert("RGB")
    scaled = window.resize((650, 430), Image.Resampling.LANCZOS)
    pasted, pad = shadow_of(scaled, radius=22, corner=10)
    image = stage.convert("RGBA")
    image.alpha_composite(pasted, (40 - pad, 66 - pad))
    draw = ImageDraw.Draw(image)
    stats = [("7", t["mb"], t["statInstaller"], t["wasInstaller"]), ("6", t["mb"], t["statTray"], t["wasTray"]),
             ("150", t["ms"], t["statOverlay"], t["wasOverlay"])]
    y = 58
    number = font("seguisb.ttf", 60)
    for value, unit, label, was in stats:
        draw.text((740, y), value, font=number, fill=(255, 255, 255, 255))
        draw.text((740 + number.getlength(value) + 8, y + 31), unit, font=font("seguisb.ttf", 25), fill=(205, 208, 218, 255))
        draw.text((740, y + 80), label, font=font("segoeui.ttf", 19), fill=(205, 208, 218, 255))
        draw.text((740, y + 106), f"{t['was']} {was}", font=font("segoeui.ttf", 16), fill=(140, 145, 158, 255))
        y += 150
    url = "github.com/gorka2354/shotik"
    face = font("seguisb.ttf", 24)
    draw.text(((SCREEN[0] - face.getlength(url)) / 2, 546), url, font=face, fill=(255, 255, 255, 255))
    film.fade_to(image.convert("RGB"), 0.3)
    film.say(t["end"])
    film.hold(2.8)


def find_display(exe, temp):
    """The smallest display at 100 %: there a pixel of the film is a pixel of the UI, and the stages are drawn to fit it."""
    app = App(exe, DOCS.parent / "test" / "fake-screen.png", "en", temp)
    try:
        app.post("trigger", mode="region")
        sizes = []
        ui_scale = 1.0
        for _, state in app.overlays():
            viewport, image = state["viewport"], state["image"]
            ui_scale = max(ui_scale, image["width"] / viewport["width"])
            if viewport["width"] == image["width"] and viewport["width"] >= SCREEN[0] and viewport["height"] >= SCREEN[1]:
                sizes.append((int(viewport["width"]), int(viewport["height"])))
        app.post("overlay-cancel")
    finally:
        app.stop()
    if not sizes:
        raise RuntimeError("the film needs a display at 100 % scale, at least 1120x630")
    return min(sizes, key=lambda size: size[0] * size[1]), ui_scale


def build(exe, lang, temp, out_dir):
    t = TEXT[lang]
    size, ui_scale = find_display(exe, temp)
    print("filming on a display of", size, "with small windows at", ui_scale)
    stages = render_stages(lang, Path(tempfile.mkdtemp(prefix="stages-", dir=temp)), size)
    stages["size"] = size
    stages["ui_scale"] = ui_scale
    video = out_dir / f"promo-{lang}.master.mp4"
    encoder = subprocess.Popen(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24",
         "-s", f"{SIZE[0]}x{SIZE[1]}", "-r", str(FPS), "-i", "-", "-c:v", "libx264", "-preset", "slow", "-crf", "12",
         "-pix_fmt", "yuv444p", str(video)], stdin=subprocess.PIPE)
    assert encoder.stdin is not None
    film = Film(encoder.stdin)
    annotated = scene_capture(film, exe, stages, t, temp)
    annotated.save(out_dir / f"annotated-{lang}.png")
    scene_claude(film, exe, stages, t, temp, annotated)
    scene_translate(film, exe, stages, t, temp)
    scene_livetext(film, exe, stages, t, temp)
    scene_record(film, exe, stages, t, temp)
    scene_end(film, stages, t, lang)
    encoder.stdin.close()
    if encoder.wait() != 0:
        raise RuntimeError("ffmpeg failed")
    print(f"{lang}: {film.frames} frames, {film.frames / FPS:.1f} s")
    return video


def publish(video, lang, gif_width):
    suffix = "" if lang == "en" else f".{lang}"
    mp4 = DOCS / f"promo{suffix}.mp4"
    gif = DOCS / f"promo{suffix}.gif"
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(video), "-c:v", "libx264",
                    "-preset", "slow", "-crf", "22", "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(mp4)], check=True)
    palette = (f"fps=20,scale={gif_width}:-1:flags=lanczos,split[a][b];"
               "[a]palettegen=max_colors=224:stats_mode=diff[p];"
               "[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle")
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(video), "-filter_complex", palette,
                    "-loop", "0", str(gif)], check=True)
    print(mp4.name, mp4.stat().st_size // 1024, "KB;", gif.name, gif.stat().st_size // 1024, "KB")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("exe")
    parser.add_argument("--lang", default="en,ru")
    parser.add_argument("--out", help="keep the master videos here instead of publishing them to docs/")
    parser.add_argument("--gif-width", type=int, default=SIZE[0])
    args = parser.parse_args()
    temp = Path(tempfile.mkdtemp(prefix="shotik-promo-"))
    out_dir = Path(args.out) if args.out else temp
    out_dir.mkdir(parents=True, exist_ok=True)
    try:
        for lang in args.lang.split(","):
            video = build(Path(args.exe), lang, temp, out_dir)
            if not args.out:
                publish(video, lang, args.gif_width)
    finally:
        shutil.rmtree(temp, ignore_errors=True)


if __name__ == "__main__":
    main()
