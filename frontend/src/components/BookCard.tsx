import { useTheme } from "@emotion/react";

import type { Book, Job } from "../api";

type Props = {
  book: Book;
  job: Job | null;
  onDownload: () => void;
  /** ASINs of other library rows sharing this title + primary author.
   * Surfaced as a small "dupe" badge so the user can decide whether to
   * download both editions or skip one. Empty when no overlap. */
  duplicateOf?: string[];
};

export default function BookCard({
  book,
  job,
  onDownload,
  duplicateOf = [],
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
            <button
              onClick={onDownload}
              css={{
                background: "transparent",
                border: `1px solid ${theme.colors.border}`,
                borderRadius: 4,
                fontFamily: theme.fonts.heading,
                fontSize: 11,
                padding: "3px 8px",
                color: theme.colors.text.main,
                cursor: "pointer",
                "&:hover": {
                  borderColor: theme.colors.activity.on,
                  color: theme.colors.activity.on,
                },
              }}
            >
              download
            </button>
          )}
        </div>
      </div>
    </article>
  );
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
  tooltip?: string;
} {
  if (!job) return { label: "new", tone: "muted", canEnqueue: true };
  if (job.status === "done")
    return { label: "done", tone: "ok", canEnqueue: false };
  if (job.status === "failed") {
    // License-denied failures are terminal: Audible has refused to issue
    // a voucher (Plus catalog rotation, region mismatch). Retrying won't
    // help, so flag the chip distinctly and keep canEnqueue off — user
    // can still force-retry manually but we don't lure them into it.
    const denied = job.error?.toLowerCase().startsWith("license denied");
    return {
      label: denied ? "unavailable" : "failed",
      tone: "err",
      canEnqueue: !denied,
      tooltip: job.error ?? undefined,
    };
  }
  if (job.status === "cancelled")
    return { label: "cancelled", tone: "muted", canEnqueue: true };
  return {
    label: job.status.replace("_", " "),
    tone: "active",
    canEnqueue: false,
  };
}
