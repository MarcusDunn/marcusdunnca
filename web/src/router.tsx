import type { QueryClient } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  redirect,
} from "@tanstack/react-router";
import { z } from "zod";
import { getSession } from "./lib/auth";
import { AttemptScreen, AttemptsScreen } from "./routes/attempts";
import { DocumentsScreen } from "./routes/documents";
import { HistoryScreen } from "./routes/history";
import { LoginScreen } from "./routes/login";
import { ReadScreen } from "./routes/read";
import { ReviewScreen } from "./routes/review";
import { NotFound, RootLayout } from "./routes/root";
import { UploadScreen } from "./routes/upload";

/*
 * Code-based routing, not file-based.
 *
 * A handful of routes and one guard fit in this file, which makes the auth boundary and
 * the redirect rules readable in one screen. File-based routing would buy
 * generated types we already get here, at the cost of a routeTree.gen.ts that has
 * to be regenerated, gitignored-or-committed, and kept in step with the build —
 * overhead that pays off at thirty routes, not at five.
 */

const rootRoute = createRootRouteWithContext<{ queryClient: QueryClient }>()({
  component: RootLayout,
  notFoundComponent: NotFound,
});

/**
 * Only same-origin absolute paths survive validation. Single-user app or not, a
 * `?redirect=` that accepts anything is an open redirect, and the login screen
 * navigates to whatever it's handed.
 */
const loginSearch = z.object({
  redirect: z
    .string()
    .refine((value) => value.startsWith("/") && !value.startsWith("//"))
    .optional()
    .catch(undefined),
});

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/login",
  validateSearch: loginSearch,
  component: LoginScreen,
});

/**
 * Pathless layout route: everything below it requires a session. Guarding here
 * rather than inside each screen means a new route is authenticated by default —
 * you have to opt out by hanging it off the root instead.
 */
const authRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "_auth",
  beforeLoad: ({ location }) => {
    if (!getSession()) {
      throw redirect({
        to: "/login",
        search: { redirect: location.href },
        replace: true,
      });
    }
  },
});

const indexRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/",
  beforeLoad: () => {
    throw redirect({ to: "/docs", replace: true });
  },
});

const documentsRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/docs",
  component: DocumentsScreen,
});

const readRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/docs/$documentId",
  component: ReadScreen,
});

/*
 * Declared after `readRoute` and matched ahead of it regardless: the router
 * ranks by specificity, not by declaration order, so `/docs/$documentId` does
 * not swallow `/docs/x/attempts`. Written out because the opposite is a
 * reasonable thing to assume and the failure — the read screen rendering for an
 * attempts URL — looks like a data bug rather than a routing one.
 */
const attemptsRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/docs/$documentId/attempts",
  component: AttemptsScreen,
});

const attemptRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/docs/$documentId/attempts/$attemptId",
  component: AttemptScreen,
});

const uploadRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/upload",
  component: UploadScreen,
});

const reviewRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/review",
  component: ReviewScreen,
});

const historyRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/history",
  component: HistoryScreen,
});

const routeTree = rootRoute.addChildren([
  loginRoute,
  authRoute.addChildren([
    indexRoute,
    documentsRoute,
    readRoute,
    attemptsRoute,
    attemptRoute,
    uploadRoute,
    reviewRoute,
    historyRoute,
  ]),
]);

export function createAppRouter(queryClient: QueryClient) {
  return createRouter({
    routeTree,
    context: { queryClient },
    defaultPreload: "intent",
    scrollRestoration: true,
  });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof createAppRouter>;
  }
}
