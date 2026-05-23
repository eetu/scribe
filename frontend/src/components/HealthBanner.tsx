import { useTheme } from "@emotion/react";
import useSWR from "swr";

import { api } from "../api";

/**
 * Thin top-of-main banner shown when an upstream is missing. Press is the
 * only critical dependency for downloading new books — the rest of the
 * UI (library browsing, account list) keeps working with just the shim.
 */
const HealthBanner = () => {
  const theme = useTheme();
  const { data } = useSWR("/status", api.status, {
    refreshInterval: 20_000,
    revalidateOnFocus: true,
    shouldRetryOnError: false,
  });
  if (!data) return null;
  const issues: string[] = [];
  if (!data.shim_healthy) issues.push("shim sidecar unreachable");
  if (!data.press_url) issues.push("press worker not configured");
  else if (!data.press_healthy) issues.push("press worker unreachable");
  if (issues.length === 0) return null;
  return (
    <div
      role="status"
      css={{
        ...theme.typography.caption,
        fontFamily: theme.fonts.heading,
        borderBottom: `1px solid ${theme.colors.error}`,
        background: theme.colors.background.light,
        color: theme.colors.error,
        padding: "8px 56px",
        textAlign: "center",
      }}
    >
      {issues.join(" · ")} — new downloads will fail until it&apos;s back
    </div>
  );
};

export default HealthBanner;
