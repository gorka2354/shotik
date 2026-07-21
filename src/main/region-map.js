'use strict';
// Virtual-desktop region mapping for the MCP region tool. Coordinates are
// global DIP — exactly the space screen.getAllDisplays() bounds live in,
// including negative origins and per-display Y offsets. Pure module (no
// Electron) so the geometry is unit-testable.

const fmt = (r) => `${r.width}x${r.height} at (${r.x}, ${r.y})`;

function describeDisplays(displays) {
  return displays.map((d) => `id ${d.id}: ${fmt(d.bounds)}`).join('; ');
}

// Coerce and validate {x,y,width,height}. Throws a clear error instead of
// letting NaN slip into a crop (which yields an empty image downstream).
function validateRegion(args) {
  const out = {};
  for (const k of ['x', 'y', 'width', 'height']) {
    const v = Number(args ? args[k] : undefined);
    if (!Number.isFinite(v)) {
      throw new Error(
        `Invalid region: "${k}" must be a finite number, got ${JSON.stringify(args ? args[k] : undefined)}. ` +
        'Pass x, y, width, height in global virtual-desktop DIP coordinates (same space as list_displays bounds).');
    }
    out[k] = Math.round(v);
  }
  if (out.width < 1 || out.height < 1) {
    throw new Error(`Invalid region: width and height must be >= 1, got ${out.width}x${out.height}`);
  }
  return out;
}

function intersect(a, b) {
  const x1 = Math.max(a.x, b.x), y1 = Math.max(a.y, b.y);
  const x2 = Math.min(a.x + a.width, b.x + b.width);
  const y2 = Math.min(a.y + a.height, b.y + b.height);
  if (x2 - x1 < 1 || y2 - y1 < 1) return null;
  return { x: x1, y: y1, width: x2 - x1, height: y2 - y1 };
}

// displays: [{ id, bounds: {x,y,width,height} }] (DIP), rect: global DIP,
// displayId: optional restriction. A region spanning several displays is
// clipped to the one holding the largest share (screens are captured one at a
// time — stitching mixed-DPI monitors has no honest pixel space).
// -> { displayId, relRect (display-relative DIP), globalRect (what will
//      actually be captured), clipped }
function mapGlobalRegion(displays, rect, displayId = null) {
  const pool = displayId != null ? displays.filter((d) => d.id === displayId) : displays;
  if (displayId != null && !pool.length) {
    throw new Error(`Unknown display_id ${displayId}. Displays: ${describeDisplays(displays)}`);
  }
  let best = null;
  for (const d of pool) {
    const inter = intersect(rect, d.bounds);
    if (!inter) continue;
    const area = inter.width * inter.height;
    if (!best || area > best.area) best = { d, inter, area };
  }
  if (!best) {
    throw new Error(
      `Region ${fmt(rect)} does not intersect ${displayId != null ? `display ${displayId}` : 'any display'}. ` +
      `Displays: ${describeDisplays(pool.length ? pool : displays)}`);
  }
  const { d, inter } = best;
  return {
    displayId: d.id,
    relRect: { x: inter.x - d.bounds.x, y: inter.y - d.bounds.y, width: inter.width, height: inter.height },
    globalRect: inter,
    clipped: inter.width !== rect.width || inter.height !== rect.height,
  };
}

module.exports = { validateRegion, mapGlobalRegion, describeDisplays };
