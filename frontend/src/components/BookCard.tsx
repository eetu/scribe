import { useTheme } from "@emotion/react";

import type { Book, Job } from "../api";

type Props = {
  book: Book;
  job: Job | null;
  onDownload: () => void;
};

export default function BookCard({ book, job, onDownload }: Props) {
  const theme = useTheme();
  const status = jobStatus(job);

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
        css={{ padding: 12, display: "flex", flexDirection: "column", gap: 6 }}
      >
        <h3
          css={{
            margin: 0,
            fontFamily: theme.fonts.heading,
            fontSize: 14,
            fontWeight: 500,
            lineHeight: 1.25,
            color: theme.colors.text.main,
          }}
        >
          {book.title}
        </h3>
        <p
          css={{
            margin: 0,
            fontFamily: theme.fonts.body,
            fontSize: 12,
            color: theme.colors.text.muted,
            lineHeight: 1.3,
          }}
        >
          {book.authors.join(", ") || "unknown author"}
        </p>
        <div
          css={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            marginTop: 4,
          }}
        >
          <StatusChip label={status.label} tone={status.tone} />
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
}: {
  label: string;
  tone: "muted" | "active" | "ok" | "err";
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
      css={{
        fontFamily: theme.fonts.heading,
        fontSize: 11,
        color,
        textTransform: "lowercase",
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
} {
  if (!job) return { label: "new", tone: "muted", canEnqueue: true };
  if (job.status === "done")
    return { label: "done", tone: "ok", canEnqueue: false };
  if (job.status === "failed")
    return { label: "failed", tone: "err", canEnqueue: true };
  if (job.status === "cancelled")
    return { label: "cancelled", tone: "muted", canEnqueue: true };
  return {
    label: job.status.replace("_", " "),
    tone: "active",
    canEnqueue: false,
  };
}
