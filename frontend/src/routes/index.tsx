import { useTheme } from "@emotion/react";
import { createFileRoute } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import useSWR, { mutate } from "swr";

import { api, type Book, type Job } from "../api";
import BookCard from "../components/BookCard";

const ACTIVE_JOB_PHASES = new Set([
  "queued",
  "fetching_voucher",
  "downloading",
  "converting",
  "streaming",
]);

type FilterKey =
  | "all"
  | "done"
  | "failed"
  | "unavailable"
  | "missing"
  | "in_progress"
  | "new";
type SortKey = "title" | "author" | "added" | "status";

function bucket(job: Job | null): Exclude<FilterKey, "all"> {
  if (!job) return "new";
  if (job.status === "done") return job.m4b_present ? "done" : "missing";
  if (job.status === "failed") {
    return job.error?.toLowerCase().startsWith("license denied")
      ? "unavailable"
      : "failed";
  }
  if (job.status === "cancelled") return "new";
  if (ACTIVE_JOB_PHASES.has(job.status)) return "in_progress";
  return "new";
}

// Status sort needs a deterministic order — group failures, missing,
// and unavailables near the top so the user lands on the actionable
// rows first.
const STATUS_RANK: Record<Exclude<FilterKey, "all">, number> = {
  missing: 0,
  failed: 1,
  unavailable: 2,
  in_progress: 3,
  new: 4,
  done: 5,
};

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
  const [filter, setFilter] = useState<FilterKey>("all");
  const [sort, setSort] = useState<SortKey>("title");

  const items: Book[] = useMemo(() => data?.items ?? [], [data]);

  if (isLoading) return null;

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

  const localeByAccount = new Map<string, string | null>();
  for (const a of accounts ?? []) {
    localeByAccount.set(a.account_id, a.locale);
  }

  const buckets = new Map<string, Exclude<FilterKey, "all">>();
  for (const b of items) {
    buckets.set(b.asin, bucket(jobByAsin.get(b.asin) ?? null));
  }
  const counts: Record<FilterKey, number> = {
    all: items.length,
    done: 0,
    failed: 0,
    unavailable: 0,
    missing: 0,
    in_progress: 0,
    new: 0,
  };
  for (const v of buckets.values()) counts[v]++;

  const visible = items
    .filter((b) => filter === "all" || buckets.get(b.asin) === filter)
    .sort((a, b) => {
      if (sort === "author") {
        return (a.authors[0] ?? "").localeCompare(b.authors[0] ?? "");
      }
      if (sort === "added") {
        return (b.purchase_date ?? "").localeCompare(a.purchase_date ?? "");
      }
      if (sort === "status") {
        const ra = STATUS_RANK[buckets.get(a.asin) ?? "new"];
        const rb = STATUS_RANK[buckets.get(b.asin) ?? "new"];
        if (ra !== rb) return ra - rb;
        return a.title.localeCompare(b.title);
      }
      return a.title.localeCompare(b.title);
    });

  // Backlog hint: books exist but nothing is queued (e.g., first deploy
  // after linking an account). Auto-enqueue only fires when a *new*
  // purchase shows up, so the existing library sits until the user
  // clicks "download all" or buys something new on Audible.
  const undownloaded = items.filter((b) => !jobByAsin.has(b.asin)).length;
  const showBacklogHint = undownloaded > 0 && (jobs?.items ?? []).length === 0;

  return (
    <>
      {showBacklogHint && (
        <div
          css={{
            background: theme.colors.background.main,
            border: `1px solid ${theme.colors.border}`,
            borderRadius: theme.border.radius,
            padding: "10px 14px",
            marginBottom: 16,
            fontSize: 13,
            color: theme.colors.text.muted,
            lineHeight: 1.5,
          }}
        >
          {undownloaded} book{undownloaded === 1 ? "" : "s"} synced, nothing
          downloaded yet. auto-sync only kicks in on the next audible purchase —
          for the backlog, hit{" "}
          <strong css={{ color: theme.colors.text.main }}>download all</strong>{" "}
          or pick books individually.
        </div>
      )}
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
                // Also re-stat job rows so missing-file detection
                // surfaces immediately instead of waiting for the
                // next 5s SWR tick.
                mutate("/api/jobs");
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
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          marginBottom: 16,
          flexWrap: "wrap",
        }}
      >
        <div css={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          {(
            [
              ["all", "all"],
              ["done", "done"],
              ["in_progress", "in progress"],
              ["missing", "missing"],
              ["failed", "failed"],
              ["unavailable", "unavailable"],
              ["new", "new"],
            ] as [FilterKey, string][]
          ).map(([key, label]) => (
            <button
              key={key}
              onClick={() => setFilter(key)}
              css={filterChip(theme, filter === key)}
            >
              {label} {counts[key]}
            </button>
          ))}
        </div>
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value as SortKey)}
          css={{
            background: "transparent",
            border: `1px solid ${theme.colors.border}`,
            borderRadius: theme.border.radius,
            padding: "6px 10px",
            color: theme.colors.text.muted,
            fontFamily: theme.fonts.heading,
            fontSize: 12,
            cursor: "pointer",
          }}
        >
          <option value="title">sort: title</option>
          <option value="author">sort: author</option>
          <option value="added">sort: added</option>
          <option value="status">sort: status</option>
        </select>
      </div>
      <div
        css={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))",
          gap: 16,
        }}
      >
        {visible.map((b) => (
          <BookCard
            key={`${b.account_id}:${b.asin}`}
            book={b}
            job={jobByAsin.get(b.asin) ?? null}
            duplicateOf={dupesByAsin.get(b.asin)}
            region={localeByAccount.get(b.account_id) ?? null}
            onDownload={async () => {
              await api.enqueueJob({ account_id: b.account_id, asin: b.asin });
              mutate("/api/jobs");
            }}
            onReconvert={async () => {
              const j = jobByAsin.get(b.asin);
              if (!j) return;
              await api.reconvertJob(j.id);
              mutate("/api/jobs");
            }}
          />
        ))}
      </div>
    </>
  );
}

function filterChip(theme: ReturnType<typeof useTheme>, active: boolean) {
  return {
    background: active ? theme.colors.activity.offBackground : "transparent",
    border: `1px solid ${active ? theme.colors.activity.on : theme.colors.border}`,
    borderRadius: theme.border.radius,
    padding: "5px 10px",
    cursor: "pointer",
    color: active ? theme.colors.activity.on : theme.colors.text.muted,
    fontFamily: theme.fonts.heading,
    fontSize: 12,
    "&:hover": {
      color: theme.colors.activity.on,
      borderColor: theme.colors.activity.on,
    },
  } as const;
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
