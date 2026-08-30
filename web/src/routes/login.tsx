import { useMutation } from "@tanstack/react-query";
import { useNavigate, useRouter, useSearch } from "@tanstack/react-router";
import { useEffect } from "react";
import { BusyMark } from "../components/ui";
import { useSession } from "../lib/auth";
import { isWebAuthnAvailable, PasskeyCancelled, signInWithPasskey } from "../lib/webauthn";

export function LoginScreen() {
  const navigate = useNavigate();
  const router = useRouter();
  const session = useSession();
  const { redirect } = useSearch({ from: "/login" });
  const supported = isWebAuthnAvailable();

  const signIn = useMutation({
    mutationFn: () => signInWithPasskey(),
    // Navigation lives in the effect below rather than here so an already-valid
    // session (returning visit, or a second tab) lands the same way as a fresh
    // sign-in.
  });

  useEffect(() => {
    if (!session) return;
    if (redirect) {
      // The deep-link target is a runtime string, so it can't go through
      // `navigate({ to })`, which is typed against the literal route paths.
      // Pushing it onto the router's own history keeps this a client-side
      // navigation; router.tsx has already validated it as same-origin.
      router.history.replace(redirect);
      return;
    }
    void navigate({ to: "/docs", replace: true });
  }, [session, navigate, router, redirect]);

  const cancelled = signIn.error instanceof PasskeyCancelled;

  if (!supported) {
    return (
      <section>
        <h1>Reading Trainer</h1>
        <p role="alert">
          This browser can&apos;t use passkeys, or the page isn&apos;t being served
          over HTTPS. Both are required — there is no password fallback.
        </p>
      </section>
    );
  }

  return (
    <section>
      <h1>Reading Trainer</h1>
      <p>
        One account, one passkey. There is no password and no sign-up form — the
        passkey was registered out of band.
      </p>

      <p>
        <button type="button" disabled={signIn.isPending} onClick={() => signIn.mutate()}>
          {signIn.isPending ? "Waiting for passkey…" : "Sign in with passkey"}
        </button>{" "}
        {signIn.isPending ? <BusyMark label="Waiting for passkey" /> : null}
      </p>

      {cancelled ? <p>Passkey prompt dismissed. Tap the button again when ready.</p> : null}

      {signIn.error && !cancelled ? (
        <p role="alert">
          {signIn.error instanceof Error ? signIn.error.message : "Sign-in failed."}
        </p>
      ) : null}
    </section>
  );
}
