'use strict';
// The whole window is the click target (not just the 32px circle) so a click
// near the bubble still activates it. Hover anywhere keeps it alive.
document.body.addEventListener('click', () => window.bubble.activate());
document.body.addEventListener('mouseenter', () => window.bubble.keepalive());
