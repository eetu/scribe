import { useEffect, useState } from "react";

import type { JobSseEvent } from "../api";

/**
 * Per-job SSE subscription. Returns the most recent event seen.
 * Returns null while the EventSource is opening or after it errors out.
 */
export function useJobSse(jobId: string | null): JobSseEvent | null {
  const [event, setEvent] = useState<JobSseEvent | null>(null);

  useEffect(() => {
    if (!jobId) return;
    const es = new EventSource(`/api/jobs/${jobId}/sse`);
    es.onmessage = (e) => {
      try {
        setEvent(JSON.parse(e.data) as JobSseEvent);
      } catch {
        // ignore malformed frames; the next one will arrive shortly.
      }
    };
    es.onerror = () => {
      // Let SWR refetch on close — the backend probably finished and the
      // broadcast channel is empty.
      es.close();
    };
    return () => es.close();
  }, [jobId]);

  return event;
}
