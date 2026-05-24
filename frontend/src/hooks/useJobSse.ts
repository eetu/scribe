import { useEffect, useState } from "react";

import type { JobSseEvent } from "../api";

export type JobLive = {
  /** Most recent terminal/phase event. */
  status: JobSseEvent | null;
  /** Latest byte counter for the active phase. Cleared on phase change. */
  progress: {
    phase: string;
    bytes_done: number;
    bytes_total: number | null;
  } | null;
};

/**
 * Per-job SSE subscription. Tracks the latest phase/done/failed event and
 * the most recent byte-progress sample separately so the UI can render
 * both a status chip and a precise progress bar from one EventSource.
 */
export function useJobSse(jobId: string | null): JobLive {
  const [live, setLive] = useState<JobLive>({ status: null, progress: null });

  useEffect(() => {
    if (!jobId) return;
    setLive({ status: null, progress: null });
    const es = new EventSource(`/api/jobs/${jobId}/sse`);
    es.onmessage = (e) => {
      try {
        const ev = JSON.parse(e.data) as JobSseEvent;
        setLive((prev) => {
          if (ev.kind === "progress") {
            return { ...prev, progress: ev };
          }
          // Queue Phase events are coarse ("downloading" covers the whole
          // press round-trip including ffmpeg). Press Progress events are
          // fine-grained ("downloading" → "converting"). They live on
          // independent dimensions — don't let a Phase event clobber the
          // Progress phase. Only terminal events drop progress so a
          // completed job's stale counter doesn't linger.
          const isTerminal =
            ev.kind === "done" ||
            ev.kind === "failed" ||
            ev.kind === "cancelled";
          return {
            status: ev,
            progress: isTerminal ? null : prev.progress,
          };
        });
      } catch {
        // ignore malformed frames; the next one will arrive shortly.
      }
    };
    es.onerror = () => {
      es.close();
    };
    return () => es.close();
  }, [jobId]);

  return live;
}
