---
name: scribe-design
description: Use this skill to generate well-branded interfaces and assets for the scribe app (a self-hosted Audible mirror that strips DRM, converts to M4B, and hands the result to audiobookshelf), either for production or throwaway prototypes/mocks. Contains essential design guidelines, colors, type, fonts, assets, and UI kit components for prototyping. Sibling product to halo and chat — same design language, different glyph.
user-invocable: true
---

Read `README.md` in this skill, plus `colors_and_type.css` and `assets/`.

For production code, the source of truth lives in the host repo:
- Components: `frontend/src/components/`
- Theme tokens: `frontend/src/themes.ts` (copied verbatim from halo)
- Routes: `frontend/src/routes/` (TanStack Router, file-based)
- Wordmark: `frontend/src/components/Wordmark.tsx`

If creating visual artifacts (slides, mocks, throwaway prototypes, etc), copy
assets out and create static HTML files for the user to view. If working on
production code, refer to existing components first; do not recreate them as
JSX prototypes.

If the user invokes this skill without any other guidance, ask them what they
want to build or design, ask some questions, and act as an expert designer
who outputs HTML artifacts _or_ production code, depending on the need.

Key things to keep in mind for scribe:

- **Sibling of halo and chat.** Identical color palette, fonts, shadow,
  radius. Anyone who has used halo or chat should immediately recognize
  the family. The only visual divergence is the wordmark glyph.
- **Wordmark is "the path of the righteous scribe."** Inter 600, lowercase,
  `letter-spacing: -0.04em`, accent period. Pulp Fiction reference —
  Jules' Ezekiel 25:17 speech ("the path of the righteous man" with
  "man" swapped for "scribe"). On narrow screens (≤600px) the `the path
  of the righteous ` prefix collapses, leaving `scribe.` alone. Same
  pattern as chat's `royale with chat.` → `chat.`.
- **Glyph: closed book outline + audio ripples + warm dot.** A `currentColor`
  rounded-rect book with a spine groove on the left. Inside the front cover,
  a warm dot (`#f78f08`) stands in for a speaker cone, with two
  `currentColor` arcs opening to the right as audio ripples. Reads as
  "audiobook" at favicon size. Same 3px stroke weight family as halo's ring
  and chat's bubble.
- **Voice.** Lowercase, quiet, archival/literary. The app does work in
  the background and surfaces results plainly. Empty states allowed one
  quiet line ("no books yet. log in to begin."). No marketing voice. No
  exclamation marks. No emoji.
- **Single column, sparse.** Library view is a grid of book covers with
  small caption strips. Job queue is a thin list. Settings are env vars.
  No top nav bar, no breadcrumbs.
- **Cards: 6px radius, soft shadow** in light theme; shadow off in dark.
  Same as halo and chat.
- **No emoji, no hero imagery.** Cover art on book cards is the only
  imagery — Material Icons Outlined for any other glyph.
- **Touch-friendly.** Tap targets large. Long-press a job row to cancel.

## Differences from halo and chat

| Aspect | halo | chat | scribe |
|---|---|---|---|
| Wordmark glyph | thin ring + warm centre | chat bubble + warm centre | closed book + warm dot + audio ripples |
| Wordmark text | `halo.` | `royale with chat.` (collapses to `chat.`) | `the path of the righteous scribe.` (collapses to `scribe.`) |
| Layout | fixed 720px column with nav rail | full-width sidebar + thread | full-width grid + thin job list |
| Locale | Finnish, lowercase | English, lowercase, Pulp Fiction flavor | English, lowercase, archival |
| Density | data-dense (clock, charts, cards) | sparse (one column of bubbles) | grid of covers + thin progress lines |
| Motion | drawer unfold, breathing bulbs | none (yet) — stream is the motion | tiny progress fill on active jobs |

Everything else — colors, fonts, shadow, radius, accent — is identical.
Copy forward from halo's `themes.ts` whenever it changes.
