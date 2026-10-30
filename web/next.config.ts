import type { NextConfig } from "next";

// Where the Rust server listens. In production a reverse proxy sends /api/*
// to the server directly; in development Next.js forwards it, so pages and
// API share one origin and the session cookie works.
const serverApi = process.env.SERVER_API_URL ?? "http://127.0.0.1:8090";

const nextConfig: NextConfig = {
  poweredByHeader: false,
  // Cache Components prerenders a static shell, which cannot carry a
  // per-request CSP nonce. Every page here is behind sign-in and fetches its
  // data from the browser, so nothing is lost by rendering per request.
  turbopack: {
    rules: {
      "*.css": {
        loaders: ["@tailwindcss/turbopack"],
        as: "*.css",
      },
    },
  },
  async rewrites() {
    return [{ source: "/api/:path*", destination: `${serverApi}/api/:path*` }];
  },
  async headers() {
    return [
      {
        source: "/(.*)",
        headers: [
          { key: "X-Content-Type-Options", value: "nosniff" },
          { key: "Referrer-Policy", value: "no-referrer" },
          { key: "X-Frame-Options", value: "DENY" },
          { key: "Permissions-Policy", value: "camera=(), microphone=(), geolocation=()" },
        ],
      },
    ];
  },
};

export default nextConfig;
