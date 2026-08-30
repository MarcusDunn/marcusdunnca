import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [
    react({
      // React Compiler runs through oxc's native port (the `oxc-transform-react`
      // peer), not Babel. If that package is ever missing the plugin calls
      // `this.error(...)` and the build dies — there is no silent fallback to an
      // uncompiled bundle, which is exactly the failure mode we want here.
      //
      // `logDiagnostics` surfaces bailouts (components the compiler refused to
      // memoize) as Vite warnings instead of swallowing them.
      compiler: { logDiagnostics: true },
    }),
  ],
  build: {
    // The app is five screens behind a login wall on a personal domain; a single
    // chunk beats waterfalling lazy routes over a cold CDN edge.
    target: "es2022",
    sourcemap: true,
  },
});
