import { useTheme } from "@emotion/react";
import { useRef } from "react";

import type { Book, Job } from "../api";
import { coverUrl } from "../api";

type Props = {
  book: Book;
  job: Job | null;
  onDownload: () => void;
  onReconvert: () => void;
  /** Drop the book from scribe's tracking. Files on disk are kept; the
   * caller confirms before this fires. */
  onRemove: () => void;
  /** ASINs of other library rows sharing this title + primary author.
   * Surfaced as a small "dupe" badge so the user can decide whether to
   * download both editions or skip one. Empty when no overlap. */
  duplicateOf?: string[];
  /** If a dupe sibling has a higher bitrate, the better value (kbps);
   * tints this card's quality badge so the worse copy stands out. */
  dupeBetterKbps?: number;
  /** Marketplace locale of the account this row belongs to ("us", "uk").
   * Rendered as a small badge so users with multi-region accounts can
   * tell editions apart at a glance. Undefined hides the badge. */
  region?: string | null;
  /** Whether the in-UI preview player is available for this book (shelf
   * configured + the book is done). Shows a play/pause overlay on the
   * cover. Preview only — no playback position is stored. */
  canPlay?: boolean;
  isPlaying?: boolean;
  /** 0..1 playback progress of the active book; drives the arc around
   * the play/pause button. Only meaningful while isPlaying. */
  progress?: number;
  onTogglePlay?: () => void;
  /** Seek the active book to a 0..1 fraction. Wired to the ring as a
   * mouse-only scrub (the ring expands on hover); touch never scrubs. */
  onScrub?: (fraction: number) => void;
  /** Re-fetch this book's cover + re-probe quality. Provided for done
   * books; renders a small "refresh" action. */
  onRefresh?: () => void;
  /** Cache-buster appended to the cover URL so a just-refreshed cover
   * reloads past the browser's long-lived image cache. */
  coverBust?: number;
};

export default function BookCard({
  book,
  job,
  onDownload,
  onReconvert,
  onRemove,
  duplicateOf = [],
  dupeBetterKbps,
  region,
  canPlay = false,
  isPlaying = false,
  progress = 0,
  onTogglePlay,
  onScrub,
  onRefresh,
  coverBust,
}: Props) {
  const theme = useTheme();
  const status = jobStatus(job);
  const coverSrc = coverBust
    ? `${coverUrl(book.asin)}?v=${coverBust}`
    : coverUrl(book.asin);
  const isDuplicate = duplicateOf.length > 0;

  // Scrub: map a pointer position on the ring to a 0..1 fraction by its
  // angle from the ring centre (top = 0, clockwise). Radius doesn't
  // matter — the hover- (desktop) or always- (touch) expanded ring just
  // gives a bigger, more precise target. Works for mouse + touch + pen.
  const ringRef = useRef<SVGSVGElement | null>(null);
  const scrubbingRef = useRef(false);
  const fractionFromEvent = (e: React.PointerEvent<SVGSVGElement>) => {
    const svg = ringRef.current;
    if (!svg) return 0;
    const r = svg.getBoundingClientRect();
    const dx = e.clientX - (r.left + r.width / 2);
    const dy = e.clientY - (r.top + r.height / 2);
    const f = (Math.atan2(dy, dx) + Math.PI / 2) / (2 * Math.PI);
    return ((f % 1) + 1) % 1;
  };
  const onRingDown = (e: React.PointerEvent<SVGSVGElement>) => {
    if (!onScrub) return;
    scrubbingRef.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
    onScrub(fractionFromEvent(e));
  };
  const onRingMove = (e: React.PointerEvent<SVGSVGElement>) => {
    if (!scrubbingRef.current) return;
    onScrub?.(fractionFromEvent(e));
  };
  const onRingUp = (e: React.PointerEvent<SVGSVGElement>) => {
    scrubbingRef.current = false;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      // pointer already released — nothing to do
    }
  };

  return (
    <article
      css={{
        background: theme.colors.background.main,
        borderRadius: theme.border.radius,
        boxShadow: theme.shadows.main,
        overflow: "hidden",
        display: "flex",
        flexDirection: "column",
      }}
    >
      <div
        css={{
          position: "relative",
          background: theme.colors.background.light,
          aspectRatio: "1 / 1",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          // Play button reveals on hover; pause (no .play-overlay class)
          // stays put. Touch devices have no hover, so show it always
          // there — otherwise the preview is unreachable on mobile.
          "& .play-overlay": { opacity: 0 },
          "&:hover .play-overlay": { opacity: 1 },
          // Hover expands the ring into a bigger scrub target on desktop.
          "&:hover .scrub-ring": {
            transform: "translate(-50%, -50%) rotate(-90deg) scale(2.3)",
          },
          // Touch has no hover, so always show the play button and keep
          // the ring at its expanded size — phones get the same scrub
          // area without needing to hover-then-tap.
          "@media (hover: none)": {
            "& .play-overlay": { opacity: 1 },
            "& .scrub-ring": {
              transform: "translate(-50%, -50%) rotate(-90deg) scale(2.3)",
            },
          },
        }}
      >
        {book.cover_url ? (
          <img
            src={coverSrc}
            alt=""
            css={{ width: "100%", height: "100%", objectFit: "cover" }}
            loading="lazy"
          />
        ) : (
          <span css={{ color: theme.colors.text.muted, fontSize: 12 }}>
            no cover
          </span>
        )}
        {region && (
          <span
            title={`account region: ${region.toUpperCase()}`}
            css={{
              position: "absolute",
              top: 6,
              left: 6,
              fontFamily: theme.fonts.heading,
              fontSize: 10,
              fontWeight: 600,
              padding: "1px 6px",
              borderRadius: 3,
              letterSpacing: "0.04em",
              background: "rgba(0, 0, 0, 0.55)",
              color: "#fff",
              textTransform: "uppercase",
              pointerEvents: "none",
            }}
          >
            {region}
          </span>
        )}
        {book.bitrate_kbps != null && (
          <span
            title={`${book.bitrate_kbps} kbps${
              book.channels === 1
                ? " · mono"
                : book.channels === 2
                  ? " · stereo"
                  : ""
            }${
              dupeBetterKbps
                ? ` — a higher-quality copy is in your library (${dupeBetterKbps} kbps)`
                : ""
            }`}
            css={{
              position: "absolute",
              top: 6,
              right: 6,
              fontFamily: theme.fonts.heading,
              fontSize: 10,
              fontWeight: 600,
              padding: "1px 6px",
              borderRadius: 3,
              letterSpacing: "0.04em",
              background: "rgba(0, 0, 0, 0.55)",
              color: dupeBetterKbps ? theme.colors.activity.on : "#fff",
              pointerEvents: "none",
            }}
          >
            {dupeBetterKbps ? "↓" : ""}
            {book.bitrate_kbps}k
          </span>
        )}
        {canPlay && onTogglePlay && (
          <>
            {isPlaying && (
              <svg
                ref={ringRef}
                className="scrub-ring"
                width="56"
                height="56"
                viewBox="0 0 56 56"
                onPointerDown={onRingDown}
                onPointerMove={onRingMove}
                onPointerUp={onRingUp}
                css={{
                  position: "absolute",
                  top: "50%",
                  left: "50%",
                  transform: "translate(-50%, -50%) rotate(-90deg)",
                  transition: "transform 150ms ease",
                  // Mouse scrub captures pointer events on the ring band;
                  // the centre play/pause button sits on top (later in the
                  // DOM) so its clicks still toggle. Touch falls through to
                  // the guard in onRingDown and does nothing.
                  pointerEvents: onScrub ? "auto" : "none",
                  cursor: onScrub ? "pointer" : "default",
                }}
              >
                {/* Dark backing disc: scales with the ring (it's inside the
                    scaled svg) so the white arc keeps contrast on a light
                    cover, and doubles as the full-area pointer target — a
                    click anywhere maps to its angle, not just the 3px
                    stroke. The centre button sits on top for its clicks. */}
                <circle cx="28" cy="28" r="26" fill="rgba(0, 0, 0, 0.5)" />
                {/* Lighter progress wedge filling the disc between the
                    play button and the outer ring. A circle at r=12.5
                    with stroke 25 spans 0..25 in svg units (full inner
                    disc); the dasharray sweep reveals it clockwise from
                    top. Stroke scales with the svg so the wedge grows
                    proportionally when the ring expands on hover/touch. */}
                <circle
                  cx="28"
                  cy="28"
                  r="12.5"
                  fill="none"
                  stroke="rgba(255, 255, 255, 0.16)"
                  strokeWidth="25"
                  strokeDasharray={WEDGE_CIRCUMFERENCE}
                  strokeDashoffset={
                    WEDGE_CIRCUMFERENCE *
                    (1 - Math.min(1, Math.max(0, progress)))
                  }
                  css={{ transition: "stroke-dashoffset 250ms linear" }}
                />
                <circle
                  cx="28"
                  cy="28"
                  r="25"
                  fill="none"
                  stroke="rgba(255, 255, 255, 0.3)"
                  strokeWidth="3"
                  vectorEffect="non-scaling-stroke"
                />
                <circle
                  cx="28"
                  cy="28"
                  r="25"
                  fill="none"
                  stroke="#fff"
                  strokeWidth="3"
                  strokeLinecap="round"
                  vectorEffect="non-scaling-stroke"
                  strokeDasharray={RING_CIRCUMFERENCE}
                  strokeDashoffset={
                    RING_CIRCUMFERENCE *
                    // Tiny 1% floor: the round linecap renders it as a
                    // small nub at the top so the ring reads as "started"
                    // without overstating progress on a multi-hour book.
                    (1 - Math.min(1, Math.max(0.01, progress)))
                  }
                  css={{ transition: "stroke-dashoffset 250ms linear" }}
                />
              </svg>
            )}
            <button
              onClick={onTogglePlay}
              className={isPlaying ? undefined : "play-overlay"}
              title={isPlaying ? "pause preview" : "play preview"}
              aria-label={isPlaying ? "pause preview" : "play preview"}
              css={{ ...playOverlay, ...(isPlaying ? { opacity: 1 } : {}) }}
            >
              {isPlaying ? (
                <svg
                  width="20"
                  height="20"
                  viewBox="0 0 24 24"
                  fill="currentColor"
                >
                  <path d="M6 5h4v14H6zM14 5h4v14h-4z" />
                </svg>
              ) : (
                <svg
                  width="20"
                  height="20"
                  viewBox="0 0 24 24"
                  fill="currentColor"
                >
                  <path d="M8 5v14l11-7z" />
                </svg>
              )}
            </button>
          </>
        )}
      </div>
      <div
        css={{
          padding: 12,
          display: "flex",
          flexDirection: "column",
          gap: 6,
          flex: 1,
        }}
      >
        <h3
          title={book.title}
          css={{
            margin: 0,
            fontFamily: theme.fonts.heading,
            fontSize: 14,
            fontWeight: 500,
            lineHeight: 1.25,
            color: theme.colors.text.main,
            display: "-webkit-box",
            WebkitLineClamp: 2,
            WebkitBoxOrient: "vertical",
            overflow: "hidden",
            minHeight: "calc(14px * 1.25 * 2)",
          }}
        >
          {book.title}
        </h3>
        <p
          title={book.authors.join(", ")}
          css={{
            margin: 0,
            fontFamily: theme.fonts.body,
            fontSize: 12,
            color: theme.colors.text.muted,
            lineHeight: 1.3,
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
          }}
        >
          {book.authors.join(", ") || "unknown author"}
        </p>
        <div
          css={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            marginTop: "auto",
            paddingTop: 4,
            minHeight: 28,
            gap: 8,
            flexWrap: "nowrap",
          }}
        >
          <div
            css={{
              display: "flex",
              gap: 6,
              alignItems: "center",
              // Let the left group shrink so the chip's ellipsis kicks in
              // before the right-side action buttons get clipped.
              minWidth: 0,
              flex: "1 1 auto",
              overflow: "hidden",
            }}
          >
            <StatusChip
              label={status.label}
              tone={status.tone}
              title={status.tooltip}
            />
            {isDuplicate && (
              <span
                title={`Another copy in your library: ${duplicateOf.join(", ")}. Likely same recording across regions, but Audible doesn't guarantee — content may differ.`}
                css={{
                  fontFamily: theme.fonts.heading,
                  fontSize: 10,
                  padding: "2px 6px",
                  borderRadius: 999,
                  border: `1px solid ${theme.colors.text.muted}`,
                  color: theme.colors.text.muted,
                  cursor: "help",
                }}
              >
                dupe
              </span>
            )}
          </div>
          <div css={{ display: "flex", gap: 6, alignItems: "center" }}>
            {onRefresh && (
              <button
                onClick={onRefresh}
                css={mutedButton(theme)}
                title="re-fetch cover + re-probe quality"
              >
                refresh
              </button>
            )}
            <button
              onClick={onRemove}
              css={removeButton(theme)}
              title="remove from scribe — files on disk are kept"
            >
              remove
            </button>
            {status.canEnqueue && (
              <button onClick={onDownload} css={cardActionButton(theme)}>
                download
              </button>
            )}
            {status.canReconvert && (
              <button
                onClick={onReconvert}
                css={cardActionButton(theme)}
                title="rebuild m4b from the cached encrypted source"
              >
                re-convert
              </button>
            )}
          </div>
        </div>
      </div>
    </article>
  );
}

function cardActionButton(theme: ReturnType<typeof useTheme>) {
  return {
    background: "transparent",
    border: `1px solid ${theme.colors.border}`,
    borderRadius: 4,
    fontFamily: theme.fonts.heading,
    fontSize: 11,
    padding: "3px 8px",
    color: theme.colors.text.main,
    cursor: "pointer",
    // Keep the label on a single line even when the bottom row gets
    // crowded (status chip + dupe badge + button). Without this the
    // hyphen in "re-convert" lets the browser break the word and the
    // button grows to two lines.
    whiteSpace: "nowrap",
    flexShrink: 0,
    "&:hover": {
      borderColor: theme.colors.activity.on,
      color: theme.colors.activity.on,
    },
  } as const;
}

// Circumference of the r=25 progress ring (2πr), for stroke-dash maths.
const RING_CIRCUMFERENCE = 2 * Math.PI * 25;
// Inner "wedge" — a circle whose stroke is wide enough to fill the disc
// from the play-button edge out to the ring. Drawn with a progress-driven
// dash so it sweeps clockwise from the top, a lighter shade behind the
// crisp outer arc.
const WEDGE_CIRCUMFERENCE = 2 * Math.PI * 12.5;

const playOverlay = {
  position: "absolute",
  top: "50%",
  left: "50%",
  transform: "translate(-50%, -50%)",
  width: 48,
  height: 48,
  borderRadius: "50%",
  border: "none",
  padding: 0,
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  background: "rgba(0, 0, 0, 0.55)",
  color: "#fff",
  cursor: "pointer",
  transition: "opacity 120ms ease, background 120ms ease",
  "&:hover": { background: "rgba(0, 0, 0, 0.78)" },
} as const;

function removeButton(theme: ReturnType<typeof useTheme>) {
  return {
    background: "transparent",
    border: "none",
    padding: "3px 4px",
    fontFamily: theme.fonts.heading,
    fontSize: 11,
    color: theme.colors.text.muted,
    cursor: "pointer",
    whiteSpace: "nowrap",
    flexShrink: 0,
    "&:hover": {
      color: theme.colors.error,
    },
  } as const;
}

function mutedButton(theme: ReturnType<typeof useTheme>) {
  return {
    background: "transparent",
    border: "none",
    padding: "3px 4px",
    fontFamily: theme.fonts.heading,
    fontSize: 11,
    color: theme.colors.text.muted,
    cursor: "pointer",
    whiteSpace: "nowrap",
    flexShrink: 0,
    "&:hover": {
      color: theme.colors.activity.on,
    },
  } as const;
}

function StatusChip({
  label,
  tone,
  title,
}: {
  label: string;
  tone: "muted" | "active" | "ok" | "err";
  title?: string;
}) {
  const theme = useTheme();
  const color =
    tone === "ok"
      ? theme.colors.connected
      : tone === "err"
        ? theme.colors.error
        : tone === "active"
          ? theme.colors.activity.on
          : theme.colors.text.muted;
  return (
    <span
      title={title}
      css={{
        fontFamily: theme.fonts.heading,
        fontSize: 11,
        color,
        textTransform: "lowercase",
        cursor: title ? "help" : undefined,
        // Truncate long phases ("fetching voucher", "unavailable") so a
        // chip + remove + re-convert stack never pushes past the card.
        whiteSpace: "nowrap",
        overflow: "hidden",
        textOverflow: "ellipsis",
        minWidth: 0,
      }}
    >
      {label}
    </span>
  );
}

function jobStatus(job: Job | null): {
  label: string;
  tone: "muted" | "active" | "ok" | "err";
  canEnqueue: boolean;
  canReconvert: boolean;
  tooltip?: string;
} {
  if (!job)
    return {
      label: "new",
      tone: "muted",
      canEnqueue: true,
      canReconvert: false,
    };
  if (job.status === "done") {
    // A done row with the m4b gone from disk means someone deleted the
    // file out from under scribe (ABS purge, manual cleanup, NAS
    // tinkering). Surface that distinctly and offer a reconvert from
    // the cached AAXC instead of forcing a full Audible round-trip.
    if (!job.m4b_present) {
      return {
        label: "missing",
        tone: "err",
        canEnqueue: false,
        canReconvert: job.aaxc_present,
        tooltip: job.aaxc_present
          ? "m4b deleted — re-convert from stored aaxc"
          : "m4b and aaxc both gone — re-download required",
      };
    }
    return {
      label: "done",
      tone: "ok",
      canEnqueue: false,
      canReconvert: false,
    };
  }
  if (job.status === "failed") {
    // License-denied failures are terminal: Audible has refused to issue
    // a voucher (Plus catalog rotation, region mismatch). Retrying won't
    // help, so flag the chip distinctly and keep canEnqueue off — user
    // can still force-retry manually but we don't lure them into it.
    const denied = job.error?.toLowerCase().startsWith("license denied");
    // If the encrypted source is still cached, offer a reconvert as a
    // recovery path. Cheaper than a fresh CDN round-trip and works for
    // generic ffmpeg / network failures. License-denied books skip this
    // unless the sidecar already has a stored voucher — the reconvert
    // path will short-circuit on its own if the lazy shim fetch fails.
    return {
      label: denied ? "unavailable" : "failed",
      tone: "err",
      canEnqueue: !denied,
      canReconvert: job.aaxc_present && !denied,
      tooltip: job.error ?? undefined,
    };
  }
  if (job.status === "cancelled")
    return {
      label: "cancelled",
      tone: "muted",
      canEnqueue: true,
      canReconvert: false,
    };
  return {
    label: job.status.replace("_", " "),
    tone: "active",
    canEnqueue: false,
    canReconvert: false,
  };
}
