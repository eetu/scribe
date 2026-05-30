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
  const [prevJobId, setPrevJobId] = useState(jobId);

  // Reset stale state when jobId changes via the "compare in render"
  if (jobId !== prevJobId) {
    setPrevJobId(jobId);
    setLive({ status: null, progress: null });
  }

  useEffect(() => {
    if (!jobId) return;
    let es: EventSource | null = null;
    let cancelled = false; // unmount / jobId change
    let terminal = false; // job reached done/failed/cancelled
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const connect = () => {
      es = new EventSource(`/api/jobs/${jobId}/sse`);
      es.onmessage = (e) => {
        attempt = 0; // a healthy frame resets the backoff
        try {
          const ev = JSON.parse(e.data) as JobSseEvent;
          if (ev.kind === "progress") {
            setLive((prev) => ({ ...prev, progress: ev }));
            return;
          }
          // Queue Phase events are coarse ("downloading" covers the whole
          // press round-trip including ffmpeg). Press Progress events are
          // fine-grained. Independent dimensions — a Phase event must not
          // clobber the Progress phase. Only terminal events drop progress.
          const isTerminal =
            ev.kind === "done" ||
            ev.kind === "failed" ||
            ev.kind === "cancelled";
          if (isTerminal) {
            terminal = true;
            es?.close();
          }
          setLive((prev) => ({
            status: ev,
            progress: isTerminal ? null : prev.progress,
          }));
        } catch {
          // ignore malformed frames; the next one will arrive shortly.
        }
      };
      es.onerror = () => {
        // A network blip closes the stream; EventSource won't resume after a
        // manual close, so reconnect with capped exponential backoff. Stop
        // once terminal (channel is evicted server-side) or unmounted. The
        // 5s /api/jobs poll resyncs the final state if we gave up mid-flight.
        es?.close();
        if (cancelled || terminal || attempt >= 6) return;
        const delay = Math.min(1000 * 2 ** attempt, 15000);
        attempt += 1;
        timer = setTimeout(connect, delay);
      };
    };
    connect();

    return () => {
      cancelled = true;
      clearTimeout(timer);
      es?.close();
    };
  }, [jobId]);

  return live;
}
