import { useTheme } from "@emotion/react";
import useSWR from "swr";

import { api, ApiError } from "../api";

const fetcher = () => api.me();

export default function LoginGate({ children }: { children: React.ReactNode }) {
  const { data, error, isLoading } = useSWR("/api/me", fetcher, {
    shouldRetryOnError: false,
  });
  const theme = useTheme();

  if (isLoading) return null;

  const unauthorized = error instanceof ApiError && error.status === 401;

  if (unauthorized) {
    return (
      <div
        css={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 16,
          marginTop: "20vh",
          color: theme.colors.text.muted,
        }}
      >
        <p>no books yet. sign in to begin.</p>
        <a
          href="/auth/login"
          css={{
            padding: "10px 20px",
            background: theme.colors.activity.on,
            color: "white",
            borderRadius: theme.border.radius,
            textDecoration: "none",
            fontFamily: theme.fonts.heading,
            fontWeight: 500,
            letterSpacing: "-0.02em",
          }}
        >
          sign in
        </a>
      </div>
    );
  }

  if (error || !data) {
    return (
      <div
        css={{
          textAlign: "center",
          marginTop: "20vh",
          color: theme.colors.error,
        }}
      >
        backend unreachable.
      </div>
    );
  }

  return <>{children}</>;
}
