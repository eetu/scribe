import { useTheme } from "@emotion/react";
import { createFileRoute } from "@tanstack/react-router";
import useSWR, { mutate } from "swr";

import { api } from "../api";
import JobRow from "../components/JobRow";

export const Route = createFileRoute("/jobs")({ component: JobsPage });

const jobsFetcher = () => api.jobs();
const libraryFetcher = () => api.library();

function JobsPage() {
  const theme = useTheme();
  const { data, isLoading } = useSWR("/api/jobs", jobsFetcher, {
    refreshInterval: 3000,
  });
  const { data: lib } = useSWR("/api/library", libraryFetcher);

  if (isLoading) return null;

  const jobs = data?.items ?? [];

  if (jobs.length === 0) {
    return (
      <div
        css={{
          textAlign: "center",
          marginTop: "12vh",
          color: theme.colors.text.muted,
        }}
      >
        nothing in progress.
      </div>
    );
  }

  const bookByAsin = new Map(
    (lib?.items ?? []).map((b) => [b.asin, b] as const),
  );

  return (
    <>
      <h2
        css={{
          margin: "0 0 16px",
          fontFamily: theme.fonts.heading,
          fontSize: 20,
          fontWeight: 500,
          color: theme.colors.text.main,
        }}
      >
        jobs
        <span
          css={{
            color: theme.colors.text.muted,
            fontWeight: 400,
            marginLeft: 8,
            fontSize: 14,
          }}
        >
          {jobs.length}
        </span>
      </h2>
      <div css={{ display: "flex", flexDirection: "column", gap: 8 }}>
        {jobs.map((j) => (
          <JobRow
            key={j.id}
            job={j}
            book={bookByAsin.get(j.asin)}
            onCancel={async () => {
              await api.cancelJob(j.id);
              mutate("/api/jobs");
            }}
          />
        ))}
      </div>
    </>
  );
}
