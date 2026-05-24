import { useTheme } from "@emotion/react";
import { createRootRoute, Outlet } from "@tanstack/react-router";

import HealthBanner from "../components/HealthBanner";
import LoginGate from "../components/LoginGate";
import Nav from "../components/Nav";
import Wordmark from "../components/Wordmark";

export const Route = createRootRoute({ component: RootLayout });

// eslint-disable-next-line react-refresh/only-export-components
function RootLayout() {
  const theme = useTheme();

  return (
    <div
      css={{
        minHeight: "100vh",
        display: "flex",
        flexDirection: "column",
        background: theme.colors.body,
      }}
    >
      <header
        css={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "14px 24px",
          borderBottom: `1px solid ${theme.colors.border}`,
          background: theme.colors.background.main,
        }}
      >
        <Wordmark size={22} />
        <Nav />
      </header>
      <HealthBanner />
      <main
        css={{
          flex: 1,
          padding: "24px",
          maxWidth: 1280,
          margin: "0 auto",
          width: "100%",
          boxSizing: "border-box",
        }}
      >
        <LoginGate>
          <Outlet />
        </LoginGate>
      </main>
    </div>
  );
}
