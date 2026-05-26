import { useTheme } from "@emotion/react";

import type { Book, Job } from "../api";
import { useJobSse } from "../hooks/useJobSse";

type Props = {
  job: Job;
  book?: Book;
  onCancel: () => void;
};

const ACTIVE_PHASES = new Set([
  "queued",
  "fetching_voucher",
  "downloading",
  "converting",
  "streaming",
]);

export default function JobRow({ job, book, onCancel }: Props) {
  const theme = useTheme();
  const active = ACTIVE_PHASES.has(job.status);
  const { status: ev, progress: liveProgress } = useJobSse(
    active ? job.id : null,
  );

  // Effective status. Priority order:
  //   1. Live Progress event phase — press's actual sub-state (downloading
  //      vs converting). The queue's coarse `downloading` covers the
  //      whole press round-trip, so without this the chip lies during
  //      the ffmpeg pass.
  //   2. Most recent Phase event — fires on queue lifecycle transitions.
  //   3. The DB row's last-saved status.
  const status =
    liveProgress?.phase ??
    (ev?.kind === "phase" ? ev.phase : null) ??
    job.status;
  // Each phase fills 0→100% on its own — chip label tells the user which
  // phase they're in (downloading vs converting vs streaming). No global
  // % across phases: we don't actually know the relative weights between
  // CDN download, ffmpeg remux, and the LAN copy, and inventing them
  // would just shift the lie. When bytes_total isn't known yet (queued,
  // fetching_voucher), bar sits at 0 and the chip carries the story.
  const progress =
    liveProgress && liveProgress.bytes_total
      ? Math.min(
          100,
          Math.round(
            (liveProgress.bytes_done / liveProgress.bytes_total) * 100,
          ),
        )
      : status === "done"
        ? 100
        : status === "failed" || status === "cancelled"
          ? 100
          : 0;
  const bytesLabel = formatBytesLabel(liveProgress);

  return (
    <div
      css={{
        display: "grid",
        gridTemplateColumns: "1fr auto auto",
        alignItems: "center",
        gap: 16,
        padding: "10px 12px",
        background: theme.colors.background.main,
        borderRadius: theme.border.radius,
        boxShadow: active ? theme.shadows.main : "none",
        opacity: active ? 1 : 0.85,
      }}
    >
      <div
        css={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0 }}
      >
        <span
          css={{
            fontFamily: theme.fonts.heading,
            fontSize: 14,
            fontWeight: 500,
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
            color: theme.colors.text.main,
          }}
        >
          {book?.title ?? job.asin}
        </span>
        {/* Bar is only meaningful while a job is moving. Once it's
            done/failed/cancelled, hide the fill but keep the 4px
            slot reserved so completion doesn't reflow the row height
            under the user. */}
        <div
          css={{
            position: "relative",
            height: 4,
            background: active
              ? theme.colors.activity.offBackground
              : "transparent",
            borderRadius: 2,
            overflow: "hidden",
          }}
        >
          {active && (
            <div
              css={{
                position: "absolute",
                left: 0,
                top: 0,
                bottom: 0,
                width: `${progress}%`,
                background:
                  status === "failed"
                    ? theme.colors.error
                    : theme.colors.activity.on,
                transition: "width 0.4s ease",
              }}
            />
          )}
        </div>
      </div>
      <span
        css={{
          fontFamily: theme.fonts.heading,
          fontSize: 12,
          color:
            status === "failed" ? theme.colors.error : theme.colors.text.muted,
          textAlign: "right",
          minWidth: 100,
        }}
      >
        <div>{status.replace("_", " ")}</div>
        {bytesLabel && (
          <div
            css={{
              fontFamily: "monospace",
              fontSize: 10,
              color: theme.colors.text.muted,
              marginTop: 2,
            }}
          >
            {bytesLabel}
          </div>
        )}
      </span>
      <button
        onClick={onCancel}
        disabled={!active}
        css={{
          background: "transparent",
          border: `1px solid ${theme.colors.border}`,
          borderRadius: 4,
          fontFamily: theme.fonts.heading,
          fontSize: 11,
          padding: "3px 8px",
          color: theme.colors.text.muted,
          cursor: active ? "pointer" : "not-allowed",
          opacity: active ? 1 : 0.4,
          "&:hover": active
            ? {
                borderColor: theme.colors.error,
                color: theme.colors.error,
              }
            : undefined,
        }}
      >
        cancel
      </button>
    </div>
  );
}

function formatBytesLabel(
  p: { bytes_done: number; bytes_total: number | null } | null,
): string | null {
  if (!p) return null;
  const done = formatBytes(p.bytes_done);
  if (p.bytes_total) {
    return `${done} / ${formatBytes(p.bytes_total)}`;
  }
  return done;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}
