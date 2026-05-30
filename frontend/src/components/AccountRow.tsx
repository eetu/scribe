import { useTheme } from "@emotion/react";
import { useState } from "react";
import { mutate } from "swr";

import { type Account, api } from "../api";

const LOCALE_LABEL: Record<string, string> = {
  us: "United States",
  uk: "United Kingdom",
  de: "Deutschland",
  fr: "France",
  jp: "Japan",
  au: "Australia",
  ca: "Canada",
  it: "Italia",
  in: "India",
  es: "España",
};

type Props = {
  account: Account;
};

export default function AccountRow({ account }: Props) {
  const theme = useTheme();
  const [busy, setBusy] = useState<"refresh" | "unlink" | null>(null);
  const accent = theme.colors.activity.on;
  const locale = account.locale ?? "?";
  const localeLabel = LOCALE_LABEL[locale] ?? locale.toUpperCase();

  const onRefresh = async () => {
    setBusy("refresh");
    try {
      await api.refreshAccount(account.account_id);
      mutate("/api/accounts");
    } finally {
      setBusy(null);
    }
  };
  const onUnlink = async () => {
    if (
      !confirm(
        `Unlink ${account.customer_name ?? account.email_masked}? Books from this account will be removed from the library (files on disk stay).`,
      )
    ) {
      return;
    }
    setBusy("unlink");
    try {
      await api.deregisterAccount(account.account_id);
      mutate("/api/accounts");
      mutate("/api/library");
    } finally {
      setBusy(null);
    }
  };

  return (
    <article
      css={{
        background: theme.colors.background.main,
        borderRadius: theme.border.radius,
        boxShadow: theme.shadows.main,
        padding: "16px 18px",
        display: "grid",
        gridTemplateColumns: "auto 1fr auto",
        gap: 16,
        alignItems: "center",
      }}
    >
      <div
        css={{
          width: 44,
          height: 44,
          borderRadius: 6,
          background: theme.colors.background.light,
          color: theme.colors.text.muted,
          display: "grid",
          placeItems: "center",
        }}
        aria-hidden="true"
      >
        <svg width="26" height="26" viewBox="0 0 64 64" fill="none">
          <rect
            x="12"
            y="10"
            width="40"
            height="44"
            rx="2"
            stroke="currentColor"
            strokeWidth="3"
            fill="none"
            strokeLinejoin="round"
          />
          <line
            x1="18"
            y1="12"
            x2="18"
            y2="52"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
          />
          <circle cx="27" cy="32" r="3.5" fill={accent} />
          <path
            d="M34 27 A 6 6 0 0 1 34 37"
            stroke="currentColor"
            strokeWidth="2.5"
            fill="none"
            strokeLinecap="round"
          />
          <path
            d="M40 23 A 11 11 0 0 1 40 41"
            stroke="currentColor"
            strokeWidth="2.5"
            fill="none"
            strokeLinecap="round"
          />
        </svg>
      </div>

      <div
        css={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0 }}
      >
        <div
          css={{
            display: "flex",
            alignItems: "baseline",
            gap: 10,
            flexWrap: "wrap",
          }}
        >
          <h3
            css={{
              margin: 0,
              fontFamily: theme.fonts.heading,
              fontSize: 16,
              fontWeight: 500,
              color: theme.colors.text.main,
            }}
          >
            {account.customer_name ?? account.email_masked}
          </h3>
          <span
            css={{
              fontFamily: theme.fonts.heading,
              fontSize: 11,
              padding: "2px 7px",
              border: `1px solid ${theme.colors.border}`,
              borderRadius: 999,
              color: theme.colors.text.muted,
            }}
          >
            {localeLabel}
          </span>
          {account.needs_relogin && (
            <span
              css={{
                fontFamily: theme.fonts.heading,
                fontSize: 11,
                padding: "2px 7px",
                borderRadius: 999,
                background: theme.colors.error,
                color: "white",
              }}
            >
              session expired
            </span>
          )}
        </div>
        <span css={{ fontSize: 12, color: theme.colors.text.muted }}>
          {account.email_masked}
        </span>
        <div
          css={{
            display: "flex",
            gap: 14,
            marginTop: 4,
            fontSize: 12,
            color: theme.colors.text.muted,
          }}
        >
          <Stat label="books" value={account.book_count} accent={accent} />
          <Stat
            label="active jobs"
            value={account.active_jobs}
            accent={accent}
            muted={account.active_jobs === 0}
          />
          <Stat
            label="synced"
            value={relativeTime(account.last_synced_at)}
            accent={accent}
            muted
          />
        </div>
      </div>

      <div css={{ display: "flex", flexDirection: "column", gap: 6 }}>
        <button
          onClick={onRefresh}
          disabled={busy !== null}
          css={chipButton(theme)}
        >
          {busy === "refresh" ? "refreshing…" : "refresh"}
        </button>
        <button
          onClick={onUnlink}
          disabled={busy !== null}
          css={{
            ...chipButton(theme),
            "&:hover": {
              color: theme.colors.error,
              borderColor: theme.colors.error,
            },
          }}
        >
          {busy === "unlink" ? "unlinking…" : "unlink"}
        </button>
      </div>
    </article>
  );
}

function Stat({
  label,
  value,
  accent,
  muted,
}: {
  label: string;
  value: number | string;
  accent: string;
  muted?: boolean;
}) {
  return (
    <span css={{ display: "inline-flex", gap: 4, alignItems: "baseline" }}>
      <strong
        css={{
          fontFamily: "monospace",
          fontSize: 13,
          color: muted ? "inherit" : accent,
        }}
      >
        {value}
      </strong>
      <span>{label}</span>
    </span>
  );
}

function chipButton(theme: ReturnType<typeof useTheme>) {
  return {
    background: "transparent",
    border: `1px solid ${theme.colors.border}`,
    borderRadius: 4,
    fontFamily: theme.fonts.heading,
    fontSize: 11,
    padding: "4px 10px",
    color: theme.colors.text.muted,
    cursor: "pointer",
    whiteSpace: "nowrap" as const,
    "&:disabled": { opacity: 0.5, cursor: "wait" },
    "&:hover": {
      color: theme.colors.activity.on,
      borderColor: theme.colors.activity.on,
    },
  };
}

function relativeTime(iso: string | null): string {
  if (!iso) return "never";
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return "never";
  const diff = Math.floor((Date.now() - ms) / 1000);
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}
