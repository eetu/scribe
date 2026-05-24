import { useTheme } from "@emotion/react";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import useSWR, { mutate } from "swr";

import { api, type Book, type Job } from "../api";
import BookCard from "../components/BookCard";

export const Route = createFileRoute("/")({ component: LibraryPage });

const fetcher = () => api.library();
const accountsFetcher = () => api.accounts();
const jobsFetcher = () => api.jobs();

// eslint-disable-next-line react-refresh/only-export-components
function LibraryPage() {
  const theme = useTheme();
  const { data, isLoading } = useSWR("/api/library", fetcher);
  const { data: accounts } = useSWR("/api/accounts", accountsFetcher);
  const { data: jobs } = useSWR("/api/jobs", jobsFetcher, {
    refreshInterval: 5000,
  });
  const [syncing, setSyncing] = useState(false);

  if (isLoading) return null;

  const items: Book[] = data?.items ?? [];

  if (items.length === 0) {
    return (
      <div
        css={{
          textAlign: "center",
          marginTop: "12vh",
          color: theme.colors.text.muted,
          fontSize: 16,
        }}
      >
        {(accounts?.length ?? 0) === 0
          ? "no audible accounts linked yet. visit accounts."
          : "no books yet. they show up here after the next sync."}
      </div>
    );
  }

  const jobByAsin = new Map<string, Job>();
  for (const j of jobs?.items ?? []) {
    const prior = jobByAsin.get(j.asin);
    if (!prior || j.updated_at > prior.updated_at) jobByAsin.set(j.asin, j);
  }

  // Heuristic dupe detection — same title + primary author across distinct
  // ASINs. Catches the common "same book on US and UK accounts" case
  // without making any claim about content equality (Audible doesn't
  // expose that). The badge is a warning, not an auto-skip.
  const sig = (b: Book) =>
    `${b.title.toLowerCase().trim()}|${(b.authors[0] ?? "").toLowerCase().trim()}`;
  const sigToAsins = new Map<string, string[]>();
  for (const b of items) {
    const k = sig(b);
    const arr = sigToAsins.get(k) ?? [];
    if (!arr.includes(b.asin)) arr.push(b.asin);
    sigToAsins.set(k, arr);
  }
  const dupesByAsin = new Map<string, string[]>();
  for (const b of items) {
    const peers = (sigToAsins.get(sig(b)) ?? []).filter((a) => a !== b.asin);
    if (peers.length > 0) dupesByAsin.set(b.asin, peers);
  }

  return (
    <>
      <div
        css={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          marginBottom: 16,
        }}
      >
        <h2
          css={{
            margin: 0,
            fontFamily: theme.fonts.heading,
            fontSize: 20,
            fontWeight: 500,
            color: theme.colors.text.main,
          }}
        >
          library
          <span
            css={{
              color: theme.colors.text.muted,
              fontWeight: 400,
              marginLeft: 8,
              fontSize: 14,
            }}
          >
            {items.length}
          </span>
        </h2>
        <div css={{ display: "flex", gap: 8 }}>
          <button
            onClick={async () => {
              if (
                !confirm(
                  "Queue downloads for every Active book that isn't already in jobs?",
                )
              )
                return;
              const r = await api.enqueueAll({});
              mutate("/api/jobs");
              alert(
                `queued ${r.queued} new job(s) across ${r.accounts} account(s)`,
              );
            }}
            css={chipButton(theme)}
          >
            download all
          </button>
          <button
            onClick={async () => {
              setSyncing(true);
              try {
                await api.syncLibrary({ full: true });
                mutate("/api/library");
              } finally {
                setSyncing(false);
              }
            }}
            disabled={syncing}
            css={{
              ...chipButton(theme),
              cursor: syncing ? "wait" : "pointer",
            }}
          >
            {syncing ? "refreshing..." : "refresh"}
          </button>
        </div>
      </div>
      <div
        css={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
          gap: 16,
        }}
      >
        {items.map((b) => (
          <BookCard
            key={`${b.account_id}:${b.asin}`}
            book={b}
            job={jobByAsin.get(b.asin) ?? null}
            duplicateOf={dupesByAsin.get(b.asin)}
            onDownload={async () => {
              await api.enqueueJob({ account_id: b.account_id, asin: b.asin });
              mutate("/api/jobs");
            }}
          />
        ))}
      </div>
    </>
  );
}

function chipButton(theme: ReturnType<typeof useTheme>) {
  return {
    background: "transparent",
    border: `1px solid ${theme.colors.border}`,
    borderRadius: theme.border.radius,
    padding: "6px 12px",
    cursor: "pointer",
    color: theme.colors.text.muted,
    fontFamily: theme.fonts.heading,
    fontSize: 12,
    "&:hover": {
      color: theme.colors.activity.on,
      borderColor: theme.colors.activity.on,
    },
  } as const;
}
