import { useTheme } from "@emotion/react";
import { Link } from "@tanstack/react-router";

import { mq } from "../mq";

type WordmarkProps = {
  size?: number;
  short?: boolean;
};

/**
 * Brand mark for scribe. Closed-book outline with a warm accent dot
 * standing in for a speaker cone, then two audio ripples opening to the
 * right — readable as "audiobook" at favicon size.
 *
 * Same accent + tracking as the halo and chat wordmarks. Only the glyph
 * differs from the sibling apps.
 */
export default function Wordmark({ size = 22, short = false }: WordmarkProps) {
  const theme = useTheme();
  const accent = theme.colors.activity.on;

  return (
    <Link
      to="/"
      css={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        color: theme.colors.text.main,
        textDecoration: "none",
      }}
    >
      <svg
        width={size}
        height={size}
        viewBox="0 0 64 64"
        fill="none"
        aria-hidden="true"
      >
        {/* Closed book outline. */}
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
        {/* Spine groove. */}
        <line
          x1="18"
          y1="12"
          x2="18"
          y2="52"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
        />
        {/* Warm dot — speaker cone, replaced by the family accent. */}
        <circle cx="27" cy="32" r="3.5" fill={accent} />
        {/* Audio ripples opening right from the dot. */}
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
      <span
        css={{
          fontFamily: theme.fonts.body,
          fontWeight: 600,
          letterSpacing: "-0.04em",
          fontSize: size,
          lineHeight: 1,
          whiteSpace: "nowrap",
        }}
      >
        {short ? null : (
          <span css={{ [mq[0]]: { display: "none" } }}>
            the path of the righteous{" "}
          </span>
        )}
        scribe<span css={{ color: accent }}>.</span>
      </span>
    </Link>
  );
}
