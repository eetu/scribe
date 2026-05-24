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
  "writing_nas",
]);

const PHASE_ORDER = [
  "queued",
  "fetching_voucher",
  "downloading",
  "converting",
  "writing_nas",
];

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
  // Byte-precise progress wins over the phase-bucketed default whenever
  // the press worker has told us a total.
  const precise =
    liveProgress && liveProgress.bytes_total
      ? Math.min(
          100,
          Math.round((liveProgress.bytes_done / liveProgress.bytes_total) * 100),
        )
      : null;
  const progress = precise ?? computeProgress(status);
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
        <div
          css={{
            position: "relative",
            height: 4,
            background: theme.colors.activity.offBackground,
            borderRadius: 2,
            overflow: "hidden",
          }}
        >
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

function computeProgress(phase: string): number {
  if (phase === "done") return 100;
  if (phase === "failed") return 100;
  if (phase === "cancelled") return 100;
  const idx = PHASE_ORDER.indexOf(phase);
  if (idx < 0) return 0;
  return Math.round(((idx + 1) / PHASE_ORDER.length) * 100);
}
