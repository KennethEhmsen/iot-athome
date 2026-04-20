import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // Panel palette — neutral slate for ambient mode.
        surface: {
          50: "#f8fafc",
          900: "#0f172a",
          950: "#020617",
        },
      },
    },
  },
  plugins: [],
} satisfies Config;
