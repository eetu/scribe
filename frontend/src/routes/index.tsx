import { useTheme } from "@emotion/react";
import { createFileRoute } from "@tanstack/react-router";
import { useMemo, useRef, useState } from "react";
import useSWR, { mutate } from "swr";

import { api, audioUrl, type Book, type Job } from "../api";
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
const meFetcher = () => api.me();

// eslint-disable-next-line react-refresh/only-export-components
function LibraryPage() {
  const theme = useTheme();
  const { data, isLoading } = useSWR("/api/library", fetcher);
  const { data: accounts } = useSWR("/api/accounts", accountsFetcher);
  const { data: jobs } = useSWR("/api/jobs", jobsFetcher, {
    refreshInterval: 5000,
  });
  const { data: me } = useSWR("/api/me", meFetcher);
  const [syncing, setSyncing] = useState(false);
  const [filter, setFilter] = useState<FilterKey>("all");
  const [sort, setSort] = useState<SortKey>("title");
  // Per-asin cache-buster so a refreshed cover reloads past the browser's
  // long-lived image cache (the cover endpoint sets max-age).
  const [coverBust, setCoverBust] = useState<Record<string, number>>({});

  // Single shared <audio> for the preview player so only one book ever
  // plays at a time. Sourced from the shelf sidecar; the feature hides
  // entirely when shelf isn't configured. Per-book positions live in a
  // ref (ephemeral — a page refresh forgets them and replays from 0),
  // so pause/resume and switching between books keep their place within
  // the session without persisting anything to the library.
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const positionsRef = useRef<Map<string, number>>(new Map());
  const loadedAsinRef = useRef<string | null>(null);
  const [playingAsin, setPlayingAsin] = useState<string | null>(null);
  // 0..1 fraction through the playing book, for the cover progress arc.
  // Rounded to 0.5% steps so the ~4Hz timeupdate doesn't re-render the
  // grid on every tick (React skips when the value is unchanged).
  const [progress, setProgress] = useState(0);
  const shelfReady = Boolean(me?.shelf_url && me?.shelf_api_key);

  const stashPosition = (a: HTMLAudioElement) => {
    if (loadedAsinRef.current) {
      positionsRef.current.set(loadedAsinRef.current, a.currentTime);
    }
  };

  const togglePlay = (book: Book) => {
    const a = audioRef.current;
    if (!a || !me?.shelf_url || !me?.shelf_api_key) return;
    // Pause the active book — keep its spot for an in-place resume.
    if (playingAsin === book.asin) {
      stashPosition(a);
      a.pause();
      setPlayingAsin(null);
      return;
    }
    // Switch streams: save the loaded book's spot, point the element at
    // the new one, and seek to its remembered offset once metadata is in
    // (currentTime can't be set before then).
    if (loadedAsinRef.current !== book.asin) {
      stashPosition(a);
      a.src = audioUrl(
        me.shelf_url,
        me.shelf_api_key,
        book.account_id,
        book.asin,
      );
      loadedAsinRef.current = book.asin;
      setProgress(0);
      const target = positionsRef.current.get(book.asin) ?? 0;
      if (target > 0) {
        const seek = () => {
          a.currentTime = target;
          a.removeEventListener("loadedmetadata", seek);
        };
        a.addEventListener("loadedmetadata", seek);
      }
    }
    void a.play();
    setPlayingAsin(book.asin);
  };

  // Mouse-only scrub from the cover ring. Seeks the currently-loaded
  // stream to `fraction` of the book; uses Audible's runtime as the
  // denominator since a streamed <audio>.duration can be Infinity.
  const scrubTo = (book: Book, fraction: number) => {
    const a = audioRef.current;
    if (!a || loadedAsinRef.current !== book.asin) return;
    const totalSec =
      Number.isFinite(a.duration) && a.duration > 0
        ? a.duration
        : (book.runtime_length_ms ?? 0) / 1000;
    if (totalSec <= 0) return;
    const f = Math.min(1, Math.max(0, fraction));
    a.currentTime = f * totalSec;
    setProgress(f);
  };

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

  // Among dupes, flag the copies that have a higher-bitrate sibling so the
  // user can prefer the better edition. Only meaningful once both probed.
  const byAsin = new Map(items.map((b) => [b.asin, b]));
  const betterDupeKbps = new Map<string, number>();
  for (const b of items) {
    const peers = dupesByAsin.get(b.asin);
    if (!peers || b.bitrate_kbps == null) continue;
    let best = b.bitrate_kbps;
    for (const peerAsin of peers) {
      const k = byAsin.get(peerAsin)?.bitrate_kbps;
      if (k != null && k > best) best = k;
    }
    if (best > b.bitrate_kbps) betterDupeKbps.set(b.asin, best);
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
      <audio
        ref={audioRef}
        onTimeUpdate={(e) => {
          const a = e.currentTarget;
          if (a.duration > 0) {
            setProgress(
              Math.min(1, Math.round((a.currentTime / a.duration) * 200) / 200),
            );
          }
        }}
        onEnded={() => {
          if (loadedAsinRef.current) {
            positionsRef.current.delete(loadedAsinRef.current);
          }
          setProgress(0);
          setPlayingAsin(null);
        }}
        hidden
      />
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
                // Full refresh: re-syncs metadata, then re-caches covers
                // and re-probes quality in the background on the Pi.
                await api.refreshLibrary();
              } finally {
                setSyncing(false);
              }
              // Bust every cover so rotated art reloads, and refetch as
              // the background pass lands (metadata first, then derived).
              setCoverBust(
                Object.fromEntries(items.map((b) => [b.asin, Date.now()])),
              );
              mutate("/api/library");
              mutate("/api/jobs");
              setTimeout(() => {
                mutate("/api/library");
                mutate("/api/jobs");
              }, 4000);
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
          )
            // Hide empty buckets so the row stays compact; keep "all"
            // always, and keep the active filter visible even at zero so
            // a user-driven empty bucket isn't yanked from under them.
            .filter(
              ([key]) => key === "all" || counts[key] > 0 || filter === key,
            )
            .map(([key, label]) => (
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
            dupeBetterKbps={betterDupeKbps.get(b.asin)}
            region={b.region}
            showStatusBand={filter === "all"}
            canPlay={shelfReady && buckets.get(b.asin) === "done"}
            isPlaying={playingAsin === b.asin}
            progress={playingAsin === b.asin ? progress : 0}
            onTogglePlay={() => togglePlay(b)}
            onScrub={(f) => scrubTo(b, f)}
            coverBust={coverBust[b.asin]}
            onRefresh={
              buckets.get(b.asin) === "done"
                ? async () => {
                    await api.refreshBook(b.asin);
                    setCoverBust((prev) => ({ ...prev, [b.asin]: Date.now() }));
                    mutate("/api/library");
                    mutate("/api/jobs");
                  }
                : undefined
            }
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
            onRemove={async () => {
              if (
                !confirm(
                  `remove "${b.title}" from scribe?\n\nthe library and source files stay on disk — only scribe's record is removed. it won't come back unless you re-buy it on audible.`,
                )
              )
                return;
              await api.removeBook(b.asin);
              mutate("/api/library");
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
