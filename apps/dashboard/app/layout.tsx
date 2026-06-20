import type { Metadata } from "next";

import "./globals.css";
import { readDashboardConfig } from "@/src/config";
import { LiveRefresh } from "@/src/live-refresh";

export const metadata: Metadata = {
  title: "Symphony Dashboard",
  description: "Foundation dashboard shell for Symphony runtime state.",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  const config = readDashboardConfig();
  return (
    <html lang="en">
      <body>
        <LiveRefresh refreshMs={config.refreshMs} />
        {children}
      </body>
    </html>
  );
}
