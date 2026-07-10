'use strict';
const b = document.getElementById('b');
// Click the bubble → translate the selection that triggered it.
b.addEventListener('click', () => window.bubble.activate());
// Hovering keeps the bubble alive so it doesn't vanish while you aim for it.
b.addEventListener('mouseenter', () => window.bubble.keepalive());
