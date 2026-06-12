<p align="center">
  <img src="assets/icon.png" width="96" alt="Shotik">
</p>

<h1 align="center">Shotik</h1>

<p align="center">
  <b>Скриншоты для людей и AI</b> · красивый open-source скриншотер для Windows<br>
  со встроенным MCP-сервером — Claude может видеть твой экран
</p>

<p align="center">
  <a href="https://github.com/gorka2354/shotik/releases/latest"><img src="https://img.shields.io/github/v/release/gorka2354/shotik?label=release&color=2ea44f" alt="Release"></a>
  <a href="https://github.com/gorka2354/shotik/releases"><img src="https://img.shields.io/github/downloads/gorka2354/shotik/total?color=blue" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-0078d4" alt="Platform">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-lightgrey" alt="MIT"></a>
  <img src="https://img.shields.io/badge/MCP-built--in-7c5cff" alt="MCP">
</p>

---

<p align="center">
  <img src="docs/demo.gif" width="88%" alt="Демо: hover-snap окна, аннотации, копирование">
</p>
<p align="center"><i>наведение подсвечивает окно → клик → стрелка, рамка, пикселизация, маркеры, текст → Enter → в буфере</i></p>

<p align="center">
  <img src="docs/app-dark.png" width="46%" alt="Тёмная тема">
  <img src="docs/app-light.png" width="46%" alt="Светлая тема">
</p>
<p align="center"><i>UI следует системной теме и акцентному цвету Windows</i></p>

## Зачем ещё один скриншотер?

Shotik — это ShareX-стиль захвата областей с аннотациями, но построенный вокруг двух идей:

1. **AI-first.** Скриншоты сегодня чаще показывают не людям, а Claude/ChatGPT в консоли.
   Shotik делает это нулевым трением.
2. **Скорость без диалогов.** Снял → уже в буфере, уже на диске, уже можно вставлять.

## Фишки

### ✦ Smart Clipboard
После каждого снимка в буфере обмена лежат **оба формата сразу**: PNG-картинка и текстовый путь к файлу.
`Ctrl+V` в Claude Code вставит изображение, `Ctrl+V` в обычный терминал — путь. Никаких переключений.

### ✦ Встроенный MCP-сервер
Подключи один раз:

```bash
claude mcp add shotik --transport http http://127.0.0.1:7464/mcp
```

и Claude получает инструменты:

| Инструмент | Что делает |
|---|---|
| `take_screenshot` | посмотреть на экран прямо сейчас |
| `ask_user_to_select_region` | открыть оверлей, чтобы **ты** показал область |
| `get_last_screenshot` | взять твой последний снимок |
| `take_screenshot_region` | снять конкретный прямоугольник |
| `list_screenshots`, `list_displays` | история и мониторы |

Скажи «посмотри на мой экран» — и Claude видит. Скажи «посмотри сюда» — и просто обведи место мышкой.

### ✦ Переснять область (`Shift+PrtSc`)
Та же область, свежие пиксели, мгновенно, без оверлея. Идеально для итераций:
поправил CSS → `Shift+PrtSc` → `Ctrl+V` в Claude → «теперь видишь?». Работает даже после перезапуска.

### ✦ Оверлей с аннотациями (Flameshot-style)
Заморозка экрана, лупа с пикселями и HEX-цветом, рамки, стрелки, перо, маркер,
**пикселизация секретов**, текст, нумерованные маркеры — всё прямо в оверлее, до сохранения.

### ✦ И остальное
- **Pin** — прилепить снимок поверх всех окон точно там, где вырезал (колесо = зум, правый клик = меню)
- **OCR** — распознать текст из выделения через встроенный Windows OCR (русский + английский, офлайн)
- **Пипетка** — `C` в оверлее копирует HEX цвета пикселя
- История с галереей, тосты с превью, мульти-монитор, HiDPI
- **Родной вид**: светлая/тёмная тема и акцентный цвет берутся из системы и меняются на лету

## Горячие клавиши

| Действие | По умолчанию |
|---|---|
| Снимок области | `PrtSc` |
| Весь экран (монитор под курсором) | `Ctrl+PrtSc` |
| Переснять последнюю область | `Shift+PrtSc` |

В оверлее: `Enter`/двойной клик — копировать · `Ctrl+S` — сохранить как · `P` — pin · `T` — OCR ·
`A` — копировать для Claude · `F` — весь экран · `1..0` — инструменты · `[` `]` — толщина ·
`Ctrl+Z` — отмена · стрелки — сдвиг выделения · `Esc` — выход.

## Установка

**Готовые сборки:** [Releases](https://github.com/gorka2354/shotik/releases) →
`Shotik-Setup-x.x.x.exe` (установщик с ярлыками) или `Shotik-x.x.x-portable.exe` (один файл, без установки).

**Из исходников:**

```bash
git clone https://github.com/gorka2354/shotik && cd shotik
npm install
npm start
```

Требуется Windows 10/11 (для сборки из исходников — Node.js ≥ 20). Приложение живёт в трее; закрытие окна сворачивает его, выход — через меню трея.

**macOS (experimental):** в Releases собираются `.dmg` (arm64 и x64). Хоткеи по умолчанию — `⌘⇧2` (область), `⌘⇧1` (экран), `⌘⇧7` (переснять); OCR — через Apple Vision; hover-snap окон пока только на Windows. Сборка не подписана: при первом запуске — правый клик → «Открыть», и выдай разрешение «Запись экрана» в настройках конфиденциальности. Маков у мейнтейнеров нет — фидбек и PR очень welcome.

**Мониторы:** мультимонитор и разные разрешения поддерживаются полностью (захват и оверлей считаются per-display, HiDPI учитывается). Известное ограничение: на конфигурациях со *смешанным* DPI-масштабом подсветка окон (hover-snap) может смещаться на пару пикселей.

CLI: `electron . --capture region|full` — снять из командной строки; `--hidden` — тихий старт в трей.

## Для контрибьюторов: ghost-режим

E2E-тесты не трогают рабочий стол: окна уезжают за экран, вместо рабочего стола подставляется фикстура,
глобальные хоткеи не регистрируются, а управление идёт через HTTP-эндпоинты `/test/*`:

```powershell
$env:SHOTIK_TEST='1'; $env:SHOTIK_GHOST='1'
$env:SHOTIK_FAKE_SCREEN='test\fake-screen.png'
npm start -- --hidden
# POST http://127.0.0.1:7464/test/trigger {"mode":"region"}
# POST http://127.0.0.1:7464/test/input {"events":[...]}   — синтетический ввод
# POST http://127.0.0.1:7464/test/capture-page              — снимок UI окна
```

Демо-GIF для README тоже снимается роботом: `node test/film-demo.js` (раскадровка через
тест-эндпоинты) + `node test/make-gif.js` (сборка кадров).

## Архитектура

```
src/
  main/        главный процесс: захват (desktopCapturer), история, OCR (WinRT),
               MCP-сервер (zero-deps streamable HTTP), окна, настройки
  overlay/     оверлей выделения: canvas-рендер, аннотации, тулбар
  app/         главное окно: галерея, Claude-страница, настройки
  pin/ toast/  закреплённые снимки и уведомления
```

Единственная зависимость — Electron. Без сборщиков, без фреймворков, без нативных модулей.

## Лицензия

MIT
