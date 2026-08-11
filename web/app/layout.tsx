import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "LCode · Session Dashboard",
  description: "Real-time agent session monitor for LCode — event stream, tool calls, task status.",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en" className="h-full">
      <body className="flex h-full min-h-full flex-col bg-[#0b0f14] text-zinc-200 antialiased">
        {children}
      </body>
    </html>
  );
}
