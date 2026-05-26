/* eslint-disable react-refresh/only-export-components */
import { useTheme } from "@emotion/react";
import { createFileRoute } from "@tanstack/react-router";
import { Fragment, useState } from "react";
import useSWR, { mutate } from "swr";

import { api, type Me, type SettingEntry } from "../api";
import { type ThemeOverride, useThemeOverride } from "../theme";

export const Route = createFileRoute("/settings")({ component: SettingsPage });

const statusFetcher = () => api.status();
const meFetcher = () => api.me();
const settingsFetcher = () => api.settings();

const OVERRIDES: Array<{ value: ThemeOverride; label: string }> = [
  { value: "system", label: "system" },
  { value: "light", label: "light" },
  { value: "dark", label: "dark" },
];

function SettingsPage() {
  const theme = useTheme();
  const { data: status } = useSWR("/status", statusFetcher);
  const { data: me } = useSWR("/api/me", meFetcher);
  const { data: settings } = useSWR("/api/settings", settingsFetcher);
  const { override, setOverride } = useThemeOverride();

  if (!status || !me || !settings) return null;

  const envRows: Array<[string, React.ReactNode]> = [
    ["version", status.version],
    ["shim", status.shim_url],
    ["shim healthy", status.shim_healthy ? "yes" : "no"],
    ["press", status.press_url ?? "(not configured)"],
    ["press healthy", status.press_healthy ? "yes" : "no"],
    ["shelf", status.shelf_url ?? "(not configured)"],
    ["shelf healthy", status.shelf_healthy ? "yes" : "no"],
    ["dev_auth", status.dev_auth ? "on" : "off"],
    ["library dir", status.library_dir],
    ["original dir", status.original_dir],
  ];

  return (
    <>
      <Header me={me} />

      <SectionTitle theme={theme}>your overrides</SectionTitle>
      <Card theme={theme}>
        <SettingToggle
          theme={theme}
          name="auto_enqueue"
          label="auto-enqueue new books"
          help="poller queues each new purchase as soon as it appears."
          entry={settings.auto_enqueue}
        />
        <Divider theme={theme} />
        <SettingNumber
          theme={theme}
          name="poll_interval_min"
          label="poll interval (minutes)"
          help="how often to check audible for new purchases."
          entry={settings.poll_interval_min}
        />
      </Card>

      {me.shelf_url && me.shelf_api_key && (
        <>
          <SectionTitle theme={theme}>shelf access</SectionTitle>
          <Card theme={theme}>
            <ShelfAccess
              theme={theme}
              url={me.shelf_url}
              apiKey={me.shelf_api_key}
            />
          </Card>
        </>
      )}

      <SectionTitle theme={theme}>environment</SectionTitle>
      <KvCard theme={theme} rows={envRows} />

      <SectionTitle theme={theme}>appearance</SectionTitle>
      <Card theme={theme}>
        <div css={{ display: "flex", gap: 8 }}>
          {OVERRIDES.map((o) => {
            const active = o.value === override;
            return (
              <button
                key={o.value}
                onClick={() => setOverride(o.value)}
                css={{
                  padding: "6px 14px",
                  background: active ? theme.colors.activity.on : "transparent",
                  color: active ? "white" : theme.colors.text.muted,
                  border: `1px solid ${
                    active ? theme.colors.activity.on : theme.colors.border
                  }`,
                  borderRadius: 4,
                  fontFamily: theme.fonts.heading,
                  fontSize: 13,
                  cursor: "pointer",
                }}
              >
                {o.label}
              </button>
            );
          })}
        </div>
      </Card>

      <p
        css={{
          marginTop: 18,
          fontSize: 12,
          color: theme.colors.text.muted,
          lineHeight: 1.6,
        }}
      >
        everything not listed here is in env vars. see backend/CLAUDE.md.
      </p>
    </>
  );
}

function Header({ me }: { me: Me }) {
  const theme = useTheme();
  return (
    <div
      css={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "baseline",
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
        settings
      </h2>
      <span css={{ fontSize: 12, color: theme.colors.text.muted }}>
        {me.email}
      </span>
    </div>
  );
}

function SectionTitle({
  children,
  theme,
}: {
  children: React.ReactNode;
  theme: ReturnType<typeof useTheme>;
}) {
  return (
    <h3
      css={{
        margin: "24px 0 8px",
        fontFamily: theme.fonts.heading,
        fontSize: 14,
        fontWeight: 500,
        color: theme.colors.text.muted,
        textTransform: "uppercase",
        letterSpacing: "0.05em",
      }}
    >
      {children}
    </h3>
  );
}

function ShelfAccess({
  theme,
  url,
  apiKey,
}: {
  theme: ReturnType<typeof useTheme>;
  url: string;
  apiKey: string;
}) {
  return (
    <div css={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <div
        css={{
          fontSize: 12,
          color: theme.colors.text.muted,
          lineHeight: 1.5,
        }}
      >
        scribe-shelf exposes the library to audiobookshelf-compatible clients
        like listen-this. paste the URL + api key into the client's settings.
      </div>
      <Field theme={theme} label="server URL" value={url} />
      <Field theme={theme} label="api key" value={apiKey} secret />
    </div>
  );
}

function Field({
  theme,
  label,
  value,
  secret,
}: {
  theme: ReturnType<typeof useTheme>;
  label: string;
  value: string;
  secret?: boolean;
}) {
  const [revealed, setRevealed] = useState(false);
  const [copied, setCopied] = useState(false);
  const display = secret && !revealed ? value.replace(/./g, "•") : value;
  return (
    <div css={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <span
        css={{
          fontFamily: theme.fonts.heading,
          fontSize: 11,
          color: theme.colors.text.muted,
        }}
      >
        {label}
      </span>
      <div
        css={{
          display: "flex",
          gap: 8,
          alignItems: "center",
          // Narrow viewports (mobile) can't fit the value + both
          // buttons on one row — wrap rather than overflow off-screen.
          flexWrap: "wrap",
        }}
      >
        <code
          css={{
            flex: "1 1 200px",
            minWidth: 0,
            fontFamily: "monospace",
            fontSize: 12,
            color: theme.colors.text.main,
            background: theme.colors.background.light,
            padding: "6px 10px",
            borderRadius: 4,
            border: `1px solid ${theme.colors.border}`,
            wordBreak: "break-all",
          }}
        >
          {display}
        </code>
        {secret && (
          <button onClick={() => setRevealed((v) => !v)} css={fieldBtn(theme)}>
            {revealed ? "hide" : "show"}
          </button>
        )}
        <button
          onClick={async () => {
            await navigator.clipboard.writeText(value);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
          css={fieldBtn(theme)}
        >
          {copied ? "copied" : "copy"}
        </button>
      </div>
    </div>
  );
}

function fieldBtn(theme: ReturnType<typeof useTheme>) {
  return {
    background: "transparent",
    border: `1px solid ${theme.colors.border}`,
    borderRadius: 4,
    fontFamily: theme.fonts.heading,
    fontSize: 11,
    padding: "4px 10px",
    color: theme.colors.text.muted,
    cursor: "pointer",
    "&:hover": {
      color: theme.colors.activity.on,
      borderColor: theme.colors.activity.on,
    },
  } as const;
}

function Card({
  children,
  theme,
}: {
  children: React.ReactNode;
  theme: ReturnType<typeof useTheme>;
}) {
  return (
    <div
      css={{
        background: theme.colors.background.main,
        borderRadius: theme.border.radius,
        boxShadow: theme.shadows.main,
        padding: 16,
      }}
    >
      {children}
    </div>
  );
}

function Divider({ theme }: { theme: ReturnType<typeof useTheme> }) {
  return (
    <hr
      css={{
        border: "none",
        borderTop: `1px solid ${theme.colors.border}`,
        margin: "14px 0",
      }}
    />
  );
}

function KvCard({
  rows,
  theme,
}: {
  rows: Array<[string, React.ReactNode]>;
  theme: ReturnType<typeof useTheme>;
}) {
  return (
    <Card theme={theme}>
      <div
        css={{
          display: "grid",
          gridTemplateColumns: "max-content 1fr",
          rowGap: 8,
          columnGap: 20,
          fontFamily: theme.fonts.body,
          fontSize: 13,
        }}
      >
        {rows.map(([k, v]) => (
          <Fragment key={String(k)}>
            <span css={{ color: theme.colors.text.muted }}>{k}</span>
            <span
              css={{
                fontFamily: "monospace",
                color: theme.colors.text.main,
                wordBreak: "break-all",
              }}
            >
              {v}
            </span>
          </Fragment>
        ))}
      </div>
    </Card>
  );
}

function SettingToggle({
  name,
  label,
  help,
  entry,
  theme,
}: {
  name: string;
  label: string;
  help: string;
  entry: SettingEntry;
  theme: ReturnType<typeof useTheme>;
}) {
  const on = entry.value === "true" || entry.value === "1";
  const toggle = async () => {
    await api.patchSettings({ [name]: on ? "false" : "true" });
    mutate("/api/settings");
  };
  const reset = async () => {
    await api.resetSetting(name);
    mutate("/api/settings");
  };
  return (
    <Row label={label} help={help} entry={entry} onReset={reset} theme={theme}>
      <button
        onClick={toggle}
        css={{
          width: 48,
          height: 24,
          borderRadius: 999,
          border: `1px solid ${
            on ? theme.colors.activity.on : theme.colors.border
          }`,
          background: on ? theme.colors.activity.on : "transparent",
          position: "relative",
          cursor: "pointer",
          padding: 0,
        }}
        aria-pressed={on}
      >
        <span
          css={{
            position: "absolute",
            top: 2,
            left: on ? 26 : 2,
            width: 18,
            height: 18,
            borderRadius: "50%",
            background: on ? "white" : theme.colors.text.muted,
            transition: "left 0.15s",
          }}
        />
      </button>
    </Row>
  );
}

function SettingNumber({
  name,
  label,
  help,
  entry,
  theme,
}: {
  name: string;
  label: string;
  help: string;
  entry: SettingEntry;
  theme: ReturnType<typeof useTheme>;
}) {
  const save = async (next: string) => {
    if (!/^\d+$/.test(next)) return;
    await api.patchSettings({ [name]: next });
    mutate("/api/settings");
  };
  const reset = async () => {
    await api.resetSetting(name);
    mutate("/api/settings");
  };
  return (
    <Row label={label} help={help} entry={entry} onReset={reset} theme={theme}>
      <input
        type="number"
        defaultValue={entry.value}
        onBlur={(e) => {
          if (e.target.value !== entry.value) void save(e.target.value);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        css={{
          width: 64,
          padding: "4px 8px",
          fontFamily: "monospace",
          fontSize: 13,
          background: theme.colors.background.light,
          color: theme.colors.text.main,
          border: `1px solid ${theme.colors.border}`,
          borderRadius: 4,
          textAlign: "right",
        }}
      />
    </Row>
  );
}

function Row({
  label,
  help,
  entry,
  onReset,
  theme,
  children,
}: {
  label: string;
  help: string;
  entry: SettingEntry;
  onReset: () => void;
  theme: ReturnType<typeof useTheme>;
  children: React.ReactNode;
}) {
  return (
    <div
      css={{
        display: "grid",
        gridTemplateColumns: "1fr auto auto",
        alignItems: "center",
        gap: 12,
      }}
    >
      <div>
        <div
          css={{
            fontFamily: theme.fonts.heading,
            fontSize: 14,
            color: theme.colors.text.main,
          }}
        >
          {label}
        </div>
        <div
          css={{
            fontSize: 12,
            color: theme.colors.text.muted,
            marginTop: 2,
          }}
        >
          {help}
          {entry.overridden && (
            <span css={{ marginLeft: 8, color: theme.colors.activity.on }}>
              (override, env: {entry.env_default})
            </span>
          )}
        </div>
      </div>
      {entry.overridden ? (
        <button
          onClick={onReset}
          css={{
            background: "transparent",
            border: "none",
            color: theme.colors.text.muted,
            fontSize: 11,
            fontFamily: theme.fonts.heading,
            cursor: "pointer",
            "&:hover": { color: theme.colors.error },
          }}
        >
          reset
        </button>
      ) : (
        <span />
      )}
      {children}
    </div>
  );
}
