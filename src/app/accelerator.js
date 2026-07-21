'use strict';
// Builds an Electron accelerator string from a KeyboardEvent-like object.
// Used by the settings hotkey recorder; exported UMD-style so node unit tests
// can require() the same logic the renderer runs.
(function (root, factory) {
  if (typeof module === 'object' && module.exports) module.exports = factory();
  else root.ShotikAccelerator = factory();
})(typeof self !== 'undefined' ? self : this, function () {
  // e: { ctrlKey, altKey, shiftKey, metaKey, key, code }; platform: process.platform value
  function toAccelerator(e, platform) {
    const mods = [];
    // Electron accepts 'Super' everywhere, but the app displays hotkeys verbatim —
    // on a Mac the key is called Cmd and leads, matching the Cmd+… defaults.
    if (e.metaKey && platform === 'darwin') mods.push('Cmd');
    if (e.ctrlKey) mods.push('Ctrl');
    if (e.altKey) mods.push('Alt');
    if (e.shiftKey) mods.push('Shift');
    if (e.metaKey && platform !== 'darwin') mods.push('Super');
    if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return null;

    let key = null;
    const code = e.code; // layout-independent
    if (/^Key[A-Z]$/.test(code)) key = code.slice(3);
    else if (/^Digit\d$/.test(code)) key = code.slice(5);
    else if (/^F\d+$/.test(code)) key = code;
    else {
      const map = {
        PrintScreen: 'PrintScreen', Space: 'Space', Home: 'Home', End: 'End',
        PageUp: 'PageUp', PageDown: 'PageDown', Insert: 'Insert',
        ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right',
        Backquote: '`', Minus: '-', Equal: '=',
      };
      key = map[code] || null;
    }
    if (!key) return null;
    // PrintScreen / F-keys can be used alone; other keys need a modifier
    if (!mods.length && key !== 'PrintScreen' && !/^F\d+$/.test(key)) return null;
    return [...mods, key].join('+');
  }
  return { toAccelerator };
});
