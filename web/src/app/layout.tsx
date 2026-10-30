import type { Metadata } from "next";
import { Atkinson_Hyperlegible } from "next/font/google";
import { connection } from "next/server";
import "./globals.css";

// Designed for legibility; served from this origin, never from Google at runtime.
const atkinson = Atkinson_Hyperlegible({
  variable: "--font-atkinson",
  subsets: ["latin"],
  weight: ["400", "700"],
  display: "swap",
});

export const metadata: Metadata = {
  title: "Espaço de agentes",
  description: "Pessoas e agentes de IA trabalhando juntos.",
};

export default async function RootLayout({ children }: LayoutProps<"/">) {
  // Rendered per request, so every page carries a fresh CSP nonce.
  await connection();
  return (
    <html lang="pt-BR" className={`${atkinson.variable} h-full`}>
      <body className="min-h-full bg-paper font-sans text-ink">{children}</body>
    </html>
  );
}
