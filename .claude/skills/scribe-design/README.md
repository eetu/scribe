# Handoff: scribe

## Overview

**scribe** (full wordmark: _scribe._) is a self-hosted Audible library
mirror that polls your purchased audiobooks, strips DRM (within your
rights as the owner), and writes clean M4B files to a NAS share where
audiobookshelf can index them. Sibling product to
[halo](../../../../halo) and [chat](../../../../chat) — same design
tokens, fonts, and warm orange accent. Only the wordmark glyph and the
layout density differ.

- Polls Audible every 5 min (configurable), auto-downloads new purchases
- Streams via the press worker (ffmpeg) so scribe never holds a full file
- Keeps original AAXC alongside as cold backup (separate NAS tree)
- Notifies audiobookshelf to rescan on completion
- One row per job; tap-and-hold to cancel
- No top navbar, no settings panel — just header + outlet

It runs headless on the Pi; the UI is touch-first for phones and tablets
on the LAN, optionally over VPN.

## Visual language

Identical to halo. See [`halo-design`](../../../../halo/.claude/skills/halo_design/README.md)
for the full reference. Below is the scribe-specific delta.

### Wordmark + glyph

- **Glyph.** 64×64 SVG. Outlined closed book (rounded rect with spine
  groove) housing a speaker-like motif on the front cover: a warm dot
  (`#f78f08`, r=3.5) where a speaker cone would sit, and two
  `currentColor` arcs opening rightward as audio ripples. Stroke
  weight 3 on the book + 2.5 on the spine and ripples, all
  `stroke-linecap: round` / `stroke-linejoin: round`, all
  `currentColor` so the outline inherits theme text color.
  - Reference shapes:
    - book: `<rect x="12" y="10" width="40" height="44" rx="2"/>`
    - spine: `<line x1="18" y1="12" x2="18" y2="52"/>`
    - dot:   `<circle cx="27" cy="32" r="3.5" fill="#f78f08"/>`
    - ripples: `M34 27 A 6 6 0 0 1 34 37` and `M40 23 A 11 11 0 0 1 40 41`

- **Wordmark.** `the path of the righteous scribe` in Inter 600 lowercase,
  `letter-spacing: -0.04em`, followed by an accent period. Pulp Fiction
  reference — Jules' Ezekiel 25:17 speech with "man" swapped for
  "scribe". Below ~600px width, the `the path of the righteous `
  prefix collapses, leaving `scribe.` alone. Treat the wordmark as a
  single line — never wrap. Short form (`scribe.` only) available via
  the `short` prop for tight spaces.

- **Sizing.** Default 22px (header), 28–32px on full-screen states
  (login, empty landing). 10px gap between glyph and wordmark.

### Layout

- **Single column, header above outlet.** No sidebar. The job queue
  appears inline above the library grid when there's active work.
- **Library grid.** Auto-fit, min 160px per cover. Each card: cover art
  + 2-line title-and-author caption. Hover (desktop) reveals a small
  status chip (`new`, `queued`, `done`, `failed`).
- **Job row.** Thin horizontal — cover thumbnail, title, progress bar,
  status text, cancel icon (long-press confirms on touch).
- **No floating actions.** Refresh is a button in the header next to the
  wordmark; "log in" lives on the empty state.

### Voice

Lowercase, quiet, archival. Examples:

- empty library: `no books yet. log in to begin.`
- empty job queue: `nothing in progress.`
- finished job: `done.`
- failed job: `failed. retry?`
- new account screen heading: `link an audible account.`
- 2FA wait: `approve the push notification on your phone, then paste the redirect URL.`

No exclamation marks. No emoji. The app does work in the background and
surfaces results plainly.

### Motion

- Tiny progress fill animates inside an active job's bar.
- New covers fade in over 200ms when polling discovers them.
- That's it — nothing else animates.
