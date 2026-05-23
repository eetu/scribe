import { useTheme } from "@emotion/react";
import { Link, useLocation } from "@tanstack/react-router";

type NavItem = { to: string; label: string };

const items: NavItem[] = [
  { to: "/", label: "library" },
  { to: "/jobs", label: "jobs" },
  { to: "/accounts", label: "accounts" },
  { to: "/settings", label: "settings" },
];

export default function Nav() {
  const theme = useTheme();
  const location = useLocation();
  return (
    <nav
      css={{
        display: "flex",
        gap: 18,
        alignItems: "center",
      }}
    >
      {items.map((item) => {
        const active =
          item.to === "/"
            ? location.pathname === "/"
            : location.pathname.startsWith(item.to);
        return (
          <Link
            key={item.to}
            to={item.to}
            css={{
              fontFamily: theme.fonts.heading,
              fontSize: 14,
              fontWeight: 500,
              textDecoration: "none",
              color: active ? theme.colors.text.main : theme.colors.text.muted,
              borderBottom: `2px solid ${active ? theme.colors.activity.on : "transparent"}`,
              paddingBottom: 2,
              transition: "color 0.15s, border-color 0.15s",
            }}
          >
            {item.label}
          </Link>
        );
      })}
    </nav>
  );
}
