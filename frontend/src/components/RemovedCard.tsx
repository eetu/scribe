import { useTheme } from "@emotion/react";

import type { RemovedBook } from "../api";
import { coverUrl } from "../api";

type Props = {
  book: RemovedBook;
  /** Clear the tombstone and re-sync this one book from Audible so it
   * returns to the library. */
  onRestore: () => void;
  busy?: boolean;
};

/** A tombstoned book in the "removed" view. Same footprint as BookCard so
 * the grid stays even, but desaturated — it's a record of something the
 * user set aside, not a live library entry. */
export default function RemovedCard({ book, onRestore, busy = false }: Props) {
  const theme = useTheme();
  const title = book.title ?? book.asin;

  return (
    <article
      css={{
        background: theme.colors.background.main,
        borderRadius: theme.border.radius,
        boxShadow: theme.shadows.main,
        overflow: "hidden",
        display: "flex",
        flexDirection: "column",
      }}
    >
      <div
        css={{
          position: "relative",
          background: theme.colors.background.light,
          aspectRatio: "1 / 1",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {book.cover_url ? (
          <img
            src={coverUrl(book.asin)}
            alt=""
            css={{
              width: "100%",
              height: "100%",
              objectFit: "cover",
              filter: "grayscale(1)",
              opacity: 0.55,
            }}
            loading="lazy"
          />
        ) : (
          <span css={{ color: theme.colors.text.muted, fontSize: 12 }}>no cover</span>
        )}
        {book.region && (
          <span
            title={`account region: ${book.region.toUpperCase()}`}
            css={{
              position: "absolute",
              top: 6,
              left: 6,
              fontFamily: theme.fonts.heading,
              fontSize: 10,
              fontWeight: 600,
              padding: "1px 6px",
              borderRadius: 3,
              letterSpacing: "0.04em",
              background: "rgba(0, 0, 0, 0.55)",
              color: "#fff",
              textTransform: "uppercase",
              pointerEvents: "none",
            }}
          >
            {book.region}
          </span>
        )}
      </div>
      <div css={{ padding: 12, display: "flex", flexDirection: "column", gap: 6, flex: 1 }}>
        <h3
          title={title}
          css={{
            margin: 0,
            fontFamily: theme.fonts.heading,
            fontSize: 14,
            fontWeight: 500,
            lineHeight: 1.25,
            color: theme.colors.text.muted,
            display: "-webkit-box",
            WebkitLineClamp: 2,
            WebkitBoxOrient: "vertical",
            overflow: "hidden",
            minHeight: "calc(14px * 1.25 * 2)",
          }}
        >
          {title}
        </h3>
        <p
          title={book.authors.join(", ")}
          css={{
            margin: 0,
            fontFamily: theme.fonts.body,
            fontSize: 12,
            color: theme.colors.text.muted,
            lineHeight: 1.3,
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
          }}
        >
          {book.authors.join(", ") || (book.title ? "unknown author" : "removed before snapshots")}
        </p>
        <div
          css={{
            display: "flex",
            alignItems: "center",
            justifyContent: "flex-end",
            marginTop: "auto",
            paddingTop: 4,
            minHeight: 28,
            gap: 8,
          }}
        >
          <button
            onClick={onRestore}
            disabled={busy}
            title="clear the tombstone and re-sync this book from audible"
            css={{
              background: "transparent",
              border: `1px solid ${theme.colors.border}`,
              borderRadius: 4,
              fontFamily: theme.fonts.heading,
              fontSize: 11,
              padding: "3px 8px",
              color: theme.colors.text.main,
              cursor: busy ? "wait" : "pointer",
              whiteSpace: "nowrap",
              flexShrink: 0,
              "&:hover": {
                borderColor: theme.colors.activity.on,
                color: theme.colors.activity.on,
              },
            }}
          >
            {busy ? "restoring..." : "restore"}
          </button>
        </div>
      </div>
    </article>
  );
}
