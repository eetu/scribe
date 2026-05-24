import { useTheme } from "@emotion/react";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import useSWR, { mutate } from "swr";

import { api } from "../api";
import AccountRow from "../components/AccountRow";

export const Route = createFileRoute("/accounts")({ component: AccountsPage });

const fetcher = () => api.accounts();

const LOCALES = [
  { code: "us", label: "US (.com)" },
  { code: "uk", label: "UK (.co.uk)" },
  { code: "de", label: "DE (.de)" },
  { code: "fr", label: "FR (.fr)" },
  { code: "jp", label: "JP (.co.jp)" },
  { code: "au", label: "AU (.com.au)" },
  { code: "ca", label: "CA (.ca)" },
  { code: "it", label: "IT (.it)" },
  { code: "in", label: "IN (.in)" },
  { code: "es", label: "ES (.es)" },
];

// eslint-disable-next-line react-refresh/only-export-components
function AccountsPage() {
  const theme = useTheme();
  const { data } = useSWR("/api/accounts", fetcher);

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
        accounts
      </h2>
      <div
        css={{
          display: "flex",
          flexDirection: "column",
          gap: 16,
          marginBottom: 32,
        }}
      >
        {(data ?? []).map((a) => (
          <AccountRow key={a.account_id} account={a} />
        ))}
        {(data ?? []).length === 0 && (
          <p css={{ color: theme.colors.text.muted, fontSize: 14 }}>
            no audible accounts linked yet.
          </p>
        )}
      </div>
      <LinkAccount />
    </>
  );
}

type Stage =
  | { kind: "idle" }
  | { kind: "starting" }
  | {
      kind: "open";
      session_id: string;
      open_url: string;
      instructions: string;
      redirect: string;
    }
  | { kind: "finishing" }
  | { kind: "done"; account_id: string }
  | { kind: "error"; message: string };

// eslint-disable-next-line react-refresh/only-export-components
function LinkAccount() {
  const theme = useTheme();
  const [locale, setLocale] = useState("us");
  const [stage, setStage] = useState<Stage>({ kind: "idle" });

  return (
    <div
      css={{
        background: theme.colors.background.main,
        borderRadius: theme.border.radius,
        boxShadow: theme.shadows.main,
        padding: 20,
        display: "flex",
        flexDirection: "column",
        gap: 14,
      }}
    >
      <h3
        css={{
          margin: 0,
          fontFamily: theme.fonts.heading,
          fontSize: 16,
          fontWeight: 500,
        }}
      >
        link an audible account
      </h3>

      {stage.kind === "idle" || stage.kind === "starting" ? (
        <>
          <label css={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <span css={{ fontSize: 12, color: theme.colors.text.muted }}>
              region
            </span>
            <select
              value={locale}
              onChange={(e) => setLocale(e.target.value)}
              css={{
                padding: "8px 10px",
                fontFamily: theme.fonts.body,
                fontSize: 14,
                background: theme.colors.background.light,
                color: theme.colors.text.main,
                border: `1px solid ${theme.colors.border}`,
                borderRadius: 4,
              }}
            >
              {LOCALES.map((l) => (
                <option key={l.code} value={l.code}>
                  {l.label}
                </option>
              ))}
            </select>
          </label>
          <button
            disabled={stage.kind === "starting"}
            onClick={async () => {
              setStage({ kind: "starting" });
              try {
                const r = await api.loginStart({ locale });
                setStage({
                  kind: "open",
                  session_id: r.session_id,
                  open_url: r.open_url,
                  instructions: r.instructions,
                  redirect: "",
                });
              } catch (e) {
                setStage({ kind: "error", message: String(e) });
              }
            }}
            css={{
              padding: "8px 16px",
              background: theme.colors.activity.on,
              color: "white",
              border: "none",
              borderRadius: theme.border.radius,
              fontFamily: theme.fonts.heading,
              fontSize: 14,
              cursor: "pointer",
              alignSelf: "flex-start",
            }}
          >
            {stage.kind === "starting" ? "starting..." : "begin"}
          </button>
        </>
      ) : null}

      {stage.kind === "open" ? (
        <>
          <p
            css={{
              fontSize: 13,
              color: theme.colors.text.muted,
              lineHeight: 1.5,
              margin: 0,
              whiteSpace: "pre-line",
            }}
          >
            {stage.instructions}
          </p>
          <a
            href={stage.open_url}
            target="_blank"
            rel="noreferrer"
            css={{
              color: theme.colors.activity.on,
              fontSize: 13,
              wordBreak: "break-all",
              fontFamily: theme.fonts.body,
            }}
          >
            open amazon sign-in →
          </a>
          <p
            css={{
              fontSize: 12,
              color: theme.colors.text.muted,
              background: theme.colors.activity.onSoft,
              border: `1px solid ${theme.colors.activity.on}`,
              borderRadius: 4,
              padding: "8px 12px",
              margin: 0,
              lineHeight: 1.5,
            }}
          >
            <strong css={{ color: theme.colors.text.main }}>
              looks broken, isn't.
            </strong>{" "}
            amazon often redirects to an error/blank page. that's the right page
            — its URL contains the code we need.
          </p>
          <label css={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <span css={{ fontSize: 12, color: theme.colors.text.muted }}>
              paste the full address-bar URL here
            </span>
            <input
              type="text"
              value={stage.redirect}
              onChange={(e) => setStage({ ...stage, redirect: e.target.value })}
              placeholder="https://www.amazon.com/ap/maplanding?openid.oa2..."
              css={{
                padding: "8px 10px",
                fontFamily: "monospace",
                fontSize: 12,
                background: theme.colors.background.light,
                color: theme.colors.text.main,
                border: `1px solid ${theme.colors.border}`,
                borderRadius: 4,
              }}
            />
          </label>
          <button
            disabled={!stage.redirect.startsWith("http")}
            onClick={async () => {
              setStage({ kind: "finishing" });
              try {
                const r = await api.loginFinish({
                  session_id: stage.session_id,
                  redirect_url: stage.redirect,
                });
                setStage({ kind: "done", account_id: r.account_id });
                mutate("/api/accounts");
                mutate("/api/library");
              } catch (e) {
                setStage({ kind: "error", message: String(e) });
              }
            }}
            css={{
              padding: "8px 16px",
              background: theme.colors.activity.on,
              color: "white",
              border: "none",
              borderRadius: theme.border.radius,
              fontFamily: theme.fonts.heading,
              fontSize: 14,
              cursor: "pointer",
              alignSelf: "flex-start",
              opacity: stage.redirect.startsWith("http") ? 1 : 0.5,
            }}
          >
            finish
          </button>
        </>
      ) : null}

      {stage.kind === "finishing" ? (
        <p css={{ color: theme.colors.text.muted, fontSize: 13 }}>
          finalising registration with amazon...
        </p>
      ) : null}

      {stage.kind === "done" ? (
        <p css={{ color: theme.colors.connected, fontSize: 13 }}>
          linked. account_id {stage.account_id}. library sync started.
        </p>
      ) : null}

      {stage.kind === "error" ? (
        <p css={{ color: theme.colors.error, fontSize: 13 }}>{stage.message}</p>
      ) : null}
    </div>
  );
}
