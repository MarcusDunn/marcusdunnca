import { Link, Outlet, useRouterState } from "@tanstack/react-router";
import { clearSession, useSession } from "../lib/auth";

export function RootLayout() {
  const session = useSession();
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const onLogin = pathname === "/login";

  return (
    <>
      {session && !onLogin ? (
        // A <nav> of list items is what makes this navigable without CSS: screen
        // readers announce the landmark and the item count, and the default
        // bullets are a perfectly good visual separator.
        <nav aria-label="Main">
          <ul>
            <li>
              <Link to="/docs">Documents</Link>
            </li>
            <li>
              <Link to="/upload">Upload</Link>
            </li>
            <li>
              <Link to="/history">History</Link>
            </li>
            <li>
              <button type="button" onClick={() => clearSession()}>
                Sign out
              </button>
            </li>
          </ul>
        </nav>
      ) : null}
      <main>
        <Outlet />
      </main>
    </>
  );
}

export function NotFound() {
  return (
    <section>
      <h1>Not found</h1>
      <p>
        <Link to="/docs">Back to documents</Link>
      </p>
    </section>
  );
}
