# Electron UI reference

Regenerate from the repository root: `node test/reference-ui.js`.

Reference source: Electron v1.3.0, commit `898942df944d9c463a5cfd7c849eec90cf14a165`.

All PNGs are captured in-process through the ghost capture-page route. The generator
uses a fresh temporary profile, fixture-only settings/history, test/fake-screen.png
and synthetic recording sources. It restores the clipboard and removes the profile
afterwards. It does not read the real history/settings or capture desktop pixels/audio.

The main pages are shown in both native themes. Small windows keep their actual
reference styling (some remain dark regardless of the main theme). PNG sizes reflect
the renderer scale of the monitor layout; compare layout in DIP, not PNG dimensions.
The GIF export completion currently uses the video presentation in the product;
toast-gif additionally records the explicitly configured gif kind accepted by showToast.

| PNG | Pixel size | State |
|---|---|---|
| [main-shots-light.png](main-shots-light.png) | 1120×741 | shots page, light theme, three cropped fake screenshots. |
| [main-text-light.png](main-text-light.png) | 1120×741 | text page, light theme, fake input and stub translation. |
| [main-claude-light.png](main-claude-light.png) | 1120×741 | claude page, light theme. |
| [main-settings-light.png](main-settings-light.png) | 1120×741 | settings page, light theme. |
| [main-shots-dark.png](main-shots-dark.png) | 1120×741 | shots page, dark theme, three cropped fake screenshots. |
| [main-text-dark.png](main-text-dark.png) | 1120×741 | text page, dark theme, fake input and stub translation. |
| [main-claude-dark.png](main-claude-dark.png) | 1120×741 | claude page, dark theme. |
| [main-settings-dark.png](main-settings-dark.png) | 1120×741 | settings page, dark theme. |
| [overlay-idle.png](overlay-idle.png) | 2560×1440 | Idle overlay over fake-screen.png, with hint chips. |
| [overlay-selection.png](overlay-selection.png) | 2560×1440 | Fake window selected, resize handles, dimensions and action/tool toolbar. |
| [overlay-annotations.png](overlay-annotations.png) | 2560×1440 | Same selection with rectangle, arrow, numbered marker and text annotation. |
| [overlay-live-text.png](overlay-live-text.png) | 2560×1440 | Live Text: real OCR of fake EN/RU fixture text, all word boxes selected and text toolbar. |
| [toast-color.png](toast-color.png) | 380×92 | Color copied: actual blue color swatch and hexadecimal value. |
| [toast-image.png](toast-image.png) | 380×92 | Image copied, with thumbnail made from fake-screen.png. |
| [toast-claude.png](toast-claude.png) | 380×92 | Copy for Claude completed, with fake image thumbnail. |
| [toast-repeat.png](toast-repeat.png) | 380×92 | Repeat capture completed, with thumbnail of the fake remembered region. |
| [toast-text.png](toast-text.png) | 380×92 | Text notification with fixture-only title and body. |
| [toast-gif.png](toast-gif.png) | 380×92 | Configured gif notification kind, default checkmark icon and fake GIF name. |
| [pin.png](pin.png) | 1101×639 | Pinned cropped fake window in idle state; hover controls are hidden. |
| [selection-bubble.png](selection-bubble.png) | 64×64 | Selection translation bubble raised for fake text. |
| [translate-loading.png](translate-loading.png) | 380×182 | Translate popup loading, original fixture text and loading indicator. |
| [translate-result.png](translate-result.png) | 380×182 | Translate popup with explicit fake EN→RU result, language pair and Copy control. |
| [recording-controls.png](recording-controls.png) | 492×59 | Active screen recording controls: timer, synthetic system/microphone meters, pause, stop, cancel. |
| [recording-border.png](recording-border.png) | 491×279 | Transparent 3px recording border around the fake default region. |
| [toast-video.png](toast-video.png) | 380×92 | Finished synthetic screen recording notification. |
| [toast-audio.png](toast-audio.png) | 380×92 | Finished synthetic audio-only recording notification. |

Coverage: 8 main-page/theme images, 4 overlay/Live Text images, 8 toast kinds,
pin, selection bubble, 2 translation states, recording controls and border (26 PNGs).
