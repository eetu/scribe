import { useTheme } from "@emotion/react";
import { createFileRoute } from "@tanstack/react-router";
import { Fragment } from "react";
import useSWR from "swr";

import { api } from "../api";
import { type ThemeOverride, useThemeOverride } from "../theme";

export const Route = createFileRoute("/settings")({ component: SettingsPage });

const fetcher = () => api.status();

const OVERRIDES: Array<{ value: ThemeOverride; label: string }> = [
  { value: "system", label: "system" },
  { value: "light", label: "light" },
  { value: "dark", label: "dark" },
];

function SettingsPage() {
  const theme = useTheme();
  const { data } = useSWR("/status", fetcher);
  const { override, setOverride } = useThemeOverride();

  if (!data) return null;

  const rows: Array<[string, React.ReactNode]> = [
    ["version", data.version],
    ["shim", data.shim_url],
    ["shim healthy", data.shim_healthy ? "yes" : "no"],
    ["press", data.press_url ?? "(not configured)"],
    ["press healthy", data.press_healthy ? "yes" : "no"],
    ["dev_auth", data.dev_auth ? "on" : "off"],
    [
      "auto enqueue",
      data.auto_enqueue
        ? "on (poller queues new books)"
        : "off (manual downloads only)",
    ],
    ["library dir", data.library_dir],
    ["original dir", data.original_dir],
    ["poll interval (min)", data.poll_interval_min],
  ];

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
        settings
      </h2>
      <div
        css={{
          background: theme.colors.background.main,
          borderRadius: theme.border.radius,
          boxShadow: theme.shadows.main,
          padding: 16,
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
      <h3
        css={{
          margin: "24px 0 12px",
          fontFamily: theme.fonts.heading,
          fontSize: 16,
          fontWeight: 500,
          color: theme.colors.text.main,
        }}
      >
        appearance
      </h3>
      <div
        css={{
          background: theme.colors.background.main,
          borderRadius: theme.border.radius,
          boxShadow: theme.shadows.main,
          padding: 16,
          display: "flex",
          gap: 8,
        }}
      >
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
                border: `1px solid ${active ? theme.colors.activity.on : theme.colors.border}`,
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
      <p
        css={{
          marginTop: 18,
          fontSize: 12,
          color: theme.colors.text.muted,
          lineHeight: 1.6,
        }}
      >
        everything else is in env vars. see CLAUDE.md.
      </p>
    </>
  );
}
