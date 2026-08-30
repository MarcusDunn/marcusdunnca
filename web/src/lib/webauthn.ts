import { api } from "./api";
import { setSession } from "./auth";
import type { AuthChallenge, AuthSession } from "./schemas";

/* WebAuthn speaks ArrayBuffers; JSON speaks base64url. These two functions are
 * the whole impedance mismatch. */

// Returns Uint8Array<ArrayBuffer> specifically: BufferSource excludes
// SharedArrayBuffer-backed views, and the default Uint8Array type is generic
// over ArrayBufferLike.
function base64UrlToBytes(value: string): Uint8Array<ArrayBuffer> {
  const padded = value.replaceAll("-", "+").replaceAll("_", "/");
  const binary = atob(padded.padEnd(Math.ceil(padded.length / 4) * 4, "="));
  const bytes = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function bytesToBase64Url(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export function isWebAuthnAvailable(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.PublicKeyCredential === "function" &&
    // Passkeys require a secure context; localhost counts.
    window.isSecureContext
  );
}

function toRequestOptions(challenge: AuthChallenge): PublicKeyCredentialRequestOptions {
  return {
    challenge: base64UrlToBytes(challenge.challenge),
    rpId: challenge.rpId,
    userVerification: challenge.userVerification,
    ...(challenge.timeoutMs === undefined ? {} : { timeout: challenge.timeoutMs }),
    // An empty allowCredentials is not a mistake: it lets the authenticator offer
    // whichever discoverable passkey it holds for this rpId, which is what a
    // single-user app with no username field wants.
    allowCredentials: challenge.allowCredentials.map((credential) => ({
      type: "public-key" as const,
      id: base64UrlToBytes(credential.id),
      ...(credential.transports === undefined
        ? {}
        : { transports: credential.transports as AuthenticatorTransport[] }),
    })),
  };
}

/** Thrown when the user dismisses the passkey sheet — not worth a red error box. */
export class PasskeyCancelled extends Error {
  constructor() {
    super("Passkey prompt dismissed");
    this.name = "PasskeyCancelled";
  }
}

export async function signInWithPasskey(signal?: AbortSignal): Promise<AuthSession> {
  if (!isWebAuthnAvailable()) {
    throw new Error("This browser can't use passkeys, or the page isn't on HTTPS.");
  }

  const challenge = await api.authChallenge();

  let credential: Credential | null;
  try {
    credential = await navigator.credentials.get({
      publicKey: toRequestOptions(challenge),
      ...(signal ? { signal } : {}),
    });
  } catch (error) {
    // NotAllowedError covers both "user cancelled" and "timed out"; neither is a
    // failure the user needs an error report about.
    if (error instanceof DOMException && error.name === "NotAllowedError") {
      throw new PasskeyCancelled();
    }
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new PasskeyCancelled();
    }
    throw error;
  }

  if (!credential || !("response" in credential)) {
    throw new PasskeyCancelled();
  }

  const assertion = credential as PublicKeyCredential;
  const response = assertion.response as AuthenticatorAssertionResponse;

  const session = await api.authVerify({
    id: assertion.id,
    rawId: bytesToBase64Url(assertion.rawId),
    type: assertion.type,
    clientExtensionResults: assertion.getClientExtensionResults(),
    response: {
      clientDataJSON: bytesToBase64Url(response.clientDataJSON),
      authenticatorData: bytesToBase64Url(response.authenticatorData),
      signature: bytesToBase64Url(response.signature),
      userHandle: response.userHandle ? bytesToBase64Url(response.userHandle) : null,
    },
  });

  setSession(session);
  return session;
}
