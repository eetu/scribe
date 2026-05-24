import { useTheme } from "@emotion/react";
import { useEffect } from "react";
import useSWR from "swr";

import { api, ApiError } from "../api";

const fetcher = () => api.me();

export default function LoginGate({ children }: { children: React.ReactNode }) {
  const { data, error, isLoading } = useSWR("/api/me", fetcher, {
    shouldRetryOnError: false,
  });
  const theme = useTheme();

  const unauthorized = error instanceof ApiError && error.status === 401;

  // Match chat: bounce straight to /auth/login on unauth landings so the
  // user lands on the SSO page (or DEV_AUTH cookie set) without a
  // mid-step "sign in" click. `next` preserves the deep link so OIDC
  // returns the user to where they were heading.
  useEffect(() => {
    if (unauthorized) {
      const next =
        window.location.pathname +
        window.location.search +
        window.location.hash;
      window.location.replace(`/auth/login?next=${encodeURIComponent(next)}`);
    }
  }, [unauthorized]);

  if (isLoading || unauthorized) return null;

  const isBackendDown = error && !unauthorized;

  if (isBackendDown || !data) {
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
