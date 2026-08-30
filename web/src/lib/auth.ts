import { useSyncExternalStore } from "react";
import { AuthSession } from "./schemas";

const STORAGE_KEY = "reading-trainer.session";

/**
 * The JWT lives in localStorage rather than an httpOnly cookie because the API
 * is a Lambda Function URL on a different origin than the site: a cookie would
 * have to be SameSite=None and the Function URL would have to echo credentialed
 * CORS, which is a worse trade than accepting the XSS exposure on a single-user
 * app that renders no third-party content. sessionStorage was rejected because a
 * 30-day token that dies with the tab defeats the point of a 30-day token.
 */
let cached: AuthSession | null | undefined;
const listeners = new Set<() => void>();

function emit(): void {
  for (const listener of listeners) listener();
}

function read(): AuthSession | null {
  if (cached !== undefined) return cached;
  cached = null;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = AuthSession.safeParse(JSON.parse(raw));
      // A session we can't parse is a session from an older shape. Drop it
      // silently — the only cost is one extra passkey tap.
      if (parsed.success) cached = parsed.data;
      else localStorage.removeItem(STORAGE_KEY);
    }
  } catch {
    cached = null;
  }
  return cached;
}

/** Treats an expired token as absent so we route to login instead of eating a 401. */
export function getSession(): AuthSession | null {
  const session = read();
  if (!session) return null;
  if (Date.parse(session.expiresAt) <= Date.now()) {
    clearSession();
    return null;
  }
  return session;
}

export function getToken(): string | null {
  return getSession()?.token ?? null;
}

export function setSession(session: AuthSession): void {
  cached = session;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(session));
  } catch {
    // Private-mode Safari can refuse writes; keep the in-memory session so the
    // current visit still works.
  }
  emit();
}

export function clearSession(): void {
  cached = null;
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* ignore */
  }
  emit();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  // Another tab logging out should log this one out too.
  const onStorage = (event: StorageEvent) => {
    if (event.key === STORAGE_KEY || event.key === null) {
      cached = undefined;
      listener();
    }
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("storage", onStorage);
  };
}

/**
 * Snapshot must be side-effect free and referentially stable — `getSession`
 * clears expired tokens, which would emit mid-render and spin. So the hook reads
 * raw and lets the api layer's 401 handling do the eviction.
 */
function getSnapshot(): AuthSession | null {
  const session = read();
  if (session && Date.parse(session.expiresAt) <= Date.now()) return null;
  return session;
}

export function useSession(): AuthSession | null {
  return useSyncExternalStore(subscribe, getSnapshot, () => null);
}
