import {
  type GlyphEnv,
  glyphTile,
  mix,
  type Palette,
} from "@anarkisti/igyb/core";
import { type Theme, useTheme } from "@emotion/react";
import { useEffect, useRef } from "react";

/**
 * A whisper-subtle repeating book-mark field, drawn only behind the empty
 * library / no-accounts states — the bare "no books yet" line becomes an
 * empty shelf waiting to fill. It renders solely in that branch, so it never
 * sits behind the populated cover grid: once real books arrive the component
 * unmounts and the field is destroyed.
 *
 * The tiling + shimmer come from @anarkisti/igyb (glyphTile); scribe owns the
 * glyph — we reproduce the Wordmark book/audio-ripple mark as the draw callback
 * (the dice precedent draws real pips the same way). Colours are read from the
 * live Emotion theme, so the field tracks the light/dark flip.
 */

// Build the igyb palette from scribe's Emotion theme object (not CSS vars, so no
// paletteFromCSS here). glyphTile paints `bg` across the whole canvas, so it must
// match the surface the layer sits on — the panel's `background.main`. The glyph
// is tinted a few steps off that surface toward `text.muted` (the dice raise/
// lighten trick) so the books read as faint ghosts, and the warm accent seeds the
// speaker dot.
function buildPalette(theme: Theme): Palette {
  const bg = theme.colors.background.main;
  return {
    bg,
    fg: mix(bg, theme.colors.text.muted, 0.14),
    accents: [theme.colors.activity.on],
  };
}

/**
 * Reproduces scribe's Wordmark glyph (a closed-book outline + spine, a warm
 * speaker dot, two audio ripples) centred in a `size`×`size` cell. Coordinates
 * map the 0–64 SVG viewBox onto the cell at 72% fill.
 */
function drawBook(
  ctx: CanvasRenderingContext2D,
  size: number,
  _index: number,
  env: GlyphEnv,
): void {
  const u = (size / 64) * 0.72; // viewBox unit → cell px
  const X = (x: number): number => (x - 32) * u; // 32,32 is the viewBox centre
  const Y = (y: number): number => (y - 32) * u;
  const S = (n: number): number => n * u; // scale a length (stroke width, radius)

  // Book outline, spine and ripples in the near-body foreground tint. igyb
  // pre-sets stroke/fill to the cell's accent, so override it here — otherwise
  // the whole book would draw in the loud accent instead of a whisper.
  ctx.strokeStyle = env.palette.fg;
  ctx.lineJoin = "round";
  ctx.lineCap = "round";

  // Closed book outline (SVG rect 12,10 40×44 r2, stroke 3).
  ctx.lineWidth = S(3);
  ctx.beginPath();
  ctx.roundRect(X(12), Y(10), S(40), S(44), S(2));
  ctx.stroke();

  // Spine groove (18,12 → 18,52, stroke 2.5).
  ctx.lineWidth = S(2.5);
  ctx.beginPath();
  ctx.moveTo(X(18), Y(12));
  ctx.lineTo(X(18), Y(52));
  ctx.stroke();

  // Two audio ripples opening right from the dot. Centres + half-angles derive
  // from the SVG arc endpoints (M34 27 A6 → 34 37, and M40 23 A11 → 40 41) so
  // the curvature matches the wordmark.
  ctx.beginPath();
  ctx.arc(X(30.68), Y(32), S(6), -0.985, 0.985);
  ctx.stroke();
  ctx.beginPath();
  ctx.arc(X(33.68), Y(32), S(11), -0.958, 0.958);
  ctx.stroke();

  // Warm dot — the speaker cone, a faint hint of the family accent over the
  // surface so it stays a whisper rather than a loud orange.
  ctx.fillStyle = mix(env.palette.bg, env.palette.accent(0), 0.4);
  ctx.beginPath();
  ctx.arc(X(27), Y(32), S(3.5), 0, Math.PI * 2);
  ctx.fill();
}

export default function EmptyShelfBackground() {
  const theme = useTheme();
  const hostRef = useRef<HTMLDivElement>(null);
  const bgRef = useRef<ReturnType<typeof glyphTile>>(undefined);
  // The palette is read live via a thunk (below) so a light/dark flip re-themes
  // the field in place instead of tearing it down; keep the latest in a ref.
  const paletteRef = useRef<Palette>(buildPalette(theme));

  // Create once. Started on mount, destroyed on unmount — so nothing lingers
  // behind the grid, since this only mounts in the empty state.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const bg = glyphTile(host, {
      glyph: drawBook,
      size: 84,
      layout: "grid",
      jitterRotate: 6, // a gentle lean, like books resting on a shelf
      speed: 0.25, // a barely-there drift; frozen under prefers-reduced-motion
      themeTransition: 0.3, // crossfade the palette on a light/dark flip
      autoPause: true, // idle while the tab is hidden or scrolled offscreen
      theme: () => paletteRef.current,
    });
    bgRef.current = bg;
    bg.start();
    return () => {
      bg.destroy();
      bgRef.current = undefined;
    };
  }, []);

  // Re-theme in place on a light/dark flip: refresh the palette ref, then let
  // igyb crossfade to it. `theme` is a stable module object per mode, so this
  // only fires on an actual flip.
  useEffect(() => {
    paletteRef.current = buildPalette(theme);
    bgRef.current?.refresh();
  }, [theme]);

  return (
    <div
      ref={hostRef}
      aria-hidden="true"
      css={{
        position: "absolute",
        inset: 0,
        pointerEvents: "none",
        opacity: 0.8, // an extra touch of restraint on top of the near-body tint
      }}
    />
  );
}
