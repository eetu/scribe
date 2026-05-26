import { useTheme } from "@emotion/react";

import type { Book, Job } from "../api";

type Props = {
  book: Book;
  job: Job | null;
  onDownload: () => void;
  onReconvert: () => void;
  /** ASINs of other library rows sharing this title + primary author.
   * Surfaced as a small "dupe" badge so the user can decide whether to
   * download both editions or skip one. Empty when no overlap. */
  duplicateOf?: string[];
  /** Marketplace locale of the account this row belongs to ("us", "uk").
   * Rendered as a small badge so users with multi-region accounts can
   * tell editions apart at a glance. Undefined hides the badge. */
  region?: string | null;
};

export default function BookCard({
  book,
  job,
  onDownload,
  onReconvert,
  duplicateOf = [],
  region,
}: Props) {
  const theme = useTheme();
  const status = jobStatus(job);
  const isDuplicate = duplicateOf.length > 0;

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
        }}
      >
        {book.cover_url ? (
          <img
            src={book.cover_url}
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
          }}
        >
          <div css={{ display: "flex", gap: 6, alignItems: "center" }}>
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
