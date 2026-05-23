import { Global, ThemeProvider } from "@emotion/react";
import { useEffect, useState } from "react";
import { useMediaQuery } from "usehooks-ts";

import {
  readThemeOverride,
  type ThemeOverride,
  ThemeOverrideContext,
  writeThemeOverride,
} from "./theme";
import { darkTheme, lightTheme } from "./themes";

export function Root({ children }: { children: React.ReactNode }) {
  const systemDark = useMediaQuery("(prefers-color-scheme: dark)");
  const [override, setOverride] = useState<ThemeOverride>(() =>
    readThemeOverride(),
  );

  useEffect(() => {
    writeThemeOverride(override);
  }, [override]);

  const useDark = override === "dark" || (override === "system" && systemDark);
  const theme = useDark ? darkTheme : lightTheme;

  return (
    <ThemeOverrideContext value={{ override, setOverride }}>
      <ThemeProvider theme={theme}>
        <Global
          styles={{
            "html, body, #root": {
              margin: 0,
              padding: 0,
              height: "100%",
              background: theme.colors.body,
              color: theme.colors.text.main,
              fontFamily: theme.fonts.body,
              fontSize: theme.typography.body1.fontSize,
            },
            "*": { boxSizing: "border-box" },
          }}
        />
        {children}
      </ThemeProvider>
    </ThemeOverrideContext>
  );
}
