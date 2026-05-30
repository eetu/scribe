export type Status = {
  service: string;
  version: string;
  shim_url: string;
  shim_healthy: boolean;
  press_url: string | null;
  press_healthy: boolean;
  shelf_url: string | null;
  shelf_healthy: boolean;
  dev_auth: boolean;
  auto_enqueue_default: boolean;
  library_dir: string;
  original_dir: string;
  poll_interval_min_default: number;
};

export type Me = {
  sub: string;
  profile_id: number;
  email: string;
  shelf_url: string | null;
  shelf_api_key: string | null;
};

export type SettingEntry = {
  value: string;
  env_default: string;
  overridden: boolean;
};

export type Settings = Record<string, SettingEntry>;

export type Account = {
  account_id: string;
  locale: string | null;
  email_masked: string;
  customer_name: string | null;
  expires_at: number | null;
  needs_refresh: boolean;
  needs_relogin: boolean;
  /** ISO 8601 UTC, e.g. "2024-04-01T12:30:00Z". null if never synced. */
  last_synced_at: string | null;
  book_count: number;
  active_jobs: number;
};

export type Book = {
  asin: string;
  account_id: string;
  title: string;
  authors: string[];
  cover_url: string | null;
  status: string;
  purchase_date: string | null;
  /** Marketplace locale of the owning account ("us", "uk", …). null
   * when scribe predates the join — UI renders no badge in that case. */
  region: string | null;
  /** Audible's total runtime in ms; the preview player's seek
   * denominator (a streamed <audio>.duration can be Infinity). */
  runtime_length_ms: number | null;
  /** Audio quality probed from the converted m4b (no transcode, so it's
   * the delivered tier). null until converted + probed. */
  codec: string | null;
  bitrate_kbps: number | null;
  sample_rate: number | null;
  channels: number | null;
};

export type Job = {
  id: string;
  asin: string;
  account_id: string;
  status: string;
  /** ISO 8601 UTC, e.g. "2024-04-01T12:30:00Z". */
  created_at: string;
  updated_at: string;
  error: string | null;
  m4b_present: boolean;
  aaxc_present: boolean;
};

export type LoginStartResp = {
  session_id: string;
  open_url: string;
  instructions: string;
};

export type LoginFinishResp = {
  account_id: string;
  customer_name: string | null;
  locale: string | null;
};

export type JobSseEvent =
  | { kind: "phase"; phase: string; retry_count: number }
  | {
      kind: "progress";
      phase: string;
      bytes_done: number;
      bytes_total: number | null;
    }
  | { kind: "done"; m4b_path: string; aaxc_path: string }
  | { kind: "failed"; message: string }
  | { kind: "cancelled" };

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(path, {
    ...init,
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
  });
  if (!r.ok) {
    throw new ApiError(r.status, await r.text().catch(() => ""));
  }
  if (r.status === 204) return undefined as T;
  return r.json() as Promise<T>;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    body: string,
  ) {
    super(`HTTP ${status}${body ? `: ${body.slice(0, 200)}` : ""}`);
  }
}

/** Same-origin cover endpoint — serves the disk-cached copy (lazily
 * mirrored from Amazon), so art survives Amazon pulling a title. */
export const coverUrl = (asin: string) =>
  `/api/books/${encodeURIComponent(asin)}/cover`;

/** Cross-origin shelf stream for the in-UI preview player. shelf's
 * /file/{ino} ignores the ino, resolving the m4b from {account}:{asin}
 * alone, so a literal 0 works. Token rides as a query param (no header
 * injection on a media element); it's the same key shown in settings. */
export const audioUrl = (
  shelfUrl: string,
  token: string,
  accountId: string,
  asin: string,
) =>
  `${shelfUrl.replace(/\/+$/, "")}/api/items/${encodeURIComponent(
    `${accountId}:${asin}`,
  )}/file/0?token=${encodeURIComponent(token)}`;

export const api = {
  status: () => req<Status>("/status"),
  me: () => req<Me>("/api/me"),
  accounts: () => req<Account[]>("/api/accounts"),
  loginStart: (body: { locale: string; with_username?: boolean }) =>
    req<LoginStartResp>("/api/accounts/login/start", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  loginFinish: (body: { session_id: string; redirect_url: string }) =>
    req<LoginFinishResp>("/api/accounts/login/finish", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  library: () => req<{ items: Book[] }>("/api/library"),
  syncLibrary: (body: { account_id?: string; full?: boolean }) =>
    req<{ syncs: unknown[] }>("/api/library/sync", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  refreshAccount: (account_id: string) =>
    req<{ expires_at: number }>(`/api/accounts/${account_id}/refresh`, {
      method: "POST",
    }),
  deregisterAccount: (account_id: string) =>
    req<{ deregistered: boolean }>(`/api/accounts/${account_id}/deregister`, {
      method: "POST",
    }),
  jobs: () => req<{ items: Job[] }>("/api/jobs"),
  enqueueJob: (body: { account_id: string; asin: string }) =>
    req<{ job_id: string }>("/api/jobs", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  enqueueAll: (body: { account_id?: string } = {}) =>
    req<{ queued: number; accounts: number }>("/api/jobs/enqueue_all", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  cancelJob: (id: string) =>
    req<{ cancelled: boolean }>(`/api/jobs/${id}/cancel`, { method: "POST" }),
  reconvertJob: (id: string) =>
    req<{ ok: boolean }>(`/api/jobs/${id}/reconvert`, { method: "POST" }),
  removeBook: (asin: string) =>
    req<{ removed: boolean; books_deleted: number; jobs_deleted: number }>(
      `/api/books/${encodeURIComponent(asin)}`,
      { method: "DELETE" },
    ),
  refreshBook: (asin: string) =>
    req<{ refreshed: number }>(
      `/api/books/${encodeURIComponent(asin)}/refresh`,
      { method: "POST" },
    ),
  refreshLibrary: () =>
    req<{ started: boolean }>("/api/library/refresh", { method: "POST" }),
  logout: () => req<void>("/auth/logout", { method: "POST" }),
  settings: () => req<Settings>("/api/settings"),
  patchSettings: (body: Record<string, string>) =>
    req<{ ok: boolean }>("/api/settings", {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  resetSetting: (key: string) =>
    req<{ ok: boolean }>(`/api/settings/${encodeURIComponent(key)}`, {
      method: "DELETE",
    }),
};
