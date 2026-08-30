import type { z } from "zod";
import { clearSession, getToken } from "./auth";
import {
  ApiErrorBody,
  AttemptResult,
  AuthChallenge,
  AuthSession,
  CreateDocumentResult,
  DocumentList,
  DocumentUrl,
  History,
  Quiz,
  type CreateDocumentRequest,
  type DocumentSummary,
  type SubmitQuizRequest,
} from "./schemas";

/**
 * Read the base URL once at module load and fail hard if it's missing. A build
 * shipped without VITE_API_BASE_URL would otherwise issue same-origin requests
 * against the static site bucket and surface as confusing 403s from S3.
 */
const BASE_URL: string = (() => {
  const raw = import.meta.env.VITE_API_BASE_URL;
  if (!raw) {
    throw new Error(
      "VITE_API_BASE_URL is not set. Copy .env.example to .env.local and fill in the Function URL.",
    );
  }
  return raw.replace(/\/+$/, "");
})();

export class ApiError extends Error {
  readonly status: number;
  readonly url: string;

  constructor(message: string, status: number, url: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.url = url;
  }
}

/**
 * A response that parsed as JSON but didn't match the schema. Separate from
 * ApiError so the UI can say "the server sent something I don't understand"
 * instead of pretending it was a network problem — and so a retry button doesn't
 * offer to retry something that will deterministically fail again.
 */
export class SchemaError extends Error {
  readonly issues: string;

  constructor(url: string, error: z.ZodError) {
    const issues = error.issues
      .map((issue) => `${issue.path.join(".") || "<root>"}: ${issue.message}`)
      .join("; ");
    super(`Unexpected response shape from ${url} — ${issues}`);
    this.name = "SchemaError";
    this.issues = issues;
  }
}

type RequestOptions = {
  method?: "GET" | "POST" | "PUT";
  body?: unknown;
  signal?: AbortSignal;
  /** Endpoints reachable before we hold a token. */
  anonymous?: boolean;
};

async function request<T>(
  path: string,
  schema: z.ZodType<T>,
  options: RequestOptions = {},
): Promise<T> {
  const { method = "GET", body, signal, anonymous = false } = options;
  const url = `${BASE_URL}${path}`;

  const headers = new Headers({ accept: "application/json" });
  if (body !== undefined) headers.set("content-type", "application/json");
  if (!anonymous) {
    const token = getToken();
    if (!token) throw new ApiError("Not signed in", 401, url);
    headers.set("authorization", `Bearer ${token}`);
  }

  let response: Response;
  try {
    response = await fetch(url, {
      method,
      headers,
      // The Function URL's CORS config sets allow_credentials = false, so a
      // credentialed request would be rejected outright. The JWT rides in the
      // Authorization header above; nothing here depends on a cookie.
      credentials: "omit",
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      ...(signal ? { signal } : {}),
    });
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === "AbortError") throw cause;
    throw new ApiError("Network request failed", 0, url);
  }

  if (!response.ok) {
    // The token is dead or was revoked. Evict it here — this is the one place
    // that sees every authenticated call, so the router guard can rely on it.
    if (response.status === 401) clearSession();
    throw new ApiError(await errorMessage(response), response.status, url);
  }

  const payload: unknown = await response.json().catch(() => {
    throw new ApiError("Response was not JSON", response.status, url);
  });

  const parsed = schema.safeParse(payload);
  if (!parsed.success) throw new SchemaError(url, parsed.error);
  return parsed.data;
}

async function errorMessage(response: Response): Promise<string> {
  try {
    const parsed = ApiErrorBody.safeParse(await response.json());
    if (parsed.success) {
      const message = parsed.data.message ?? parsed.data.error;
      if (message) return message;
    }
  } catch {
    /* fall through to the status text */
  }
  return response.statusText || `Request failed with status ${response.status}`;
}

/* ------------------------------------------------------------------ *
 * Endpoints
 * ------------------------------------------------------------------ */

export const api = {
  authChallenge: () =>
    request("/auth/challenge", AuthChallenge, {
      method: "POST",
      body: {},
      anonymous: true,
    }),

  authVerify: (assertion: unknown) =>
    request("/auth/verify", AuthSession, {
      method: "POST",
      body: assertion,
      anonymous: true,
    }),

  listDocuments: (signal?: AbortSignal) =>
    request("/docs", DocumentList, signal ? { signal } : {}).then((r) => r.documents),

  createDocument: (body: CreateDocumentRequest) =>
    request("/docs", CreateDocumentResult, { method: "POST", body }),

  documentUrl: (id: string) =>
    request(`/docs/${encodeURIComponent(id)}/url`, DocumentUrl),

  quiz: (id: string) => request(`/docs/${encodeURIComponent(id)}/quiz`, Quiz),

  submitQuiz: (id: string, body: SubmitQuizRequest) =>
    request(`/docs/${encodeURIComponent(id)}/submit`, AttemptResult, {
      method: "POST",
      body,
    }),

  history: () => request("/history", History).then((r) => r.attempts),
};

/**
 * Upload straight to S3. The bytes never touch the API: a Lambda Function URL
 * caps request payloads near 6 MB, and a 100-page PDF blows past that routinely.
 *
 * The content-type must match what the presign was signed with or S3 rejects the
 * PUT with SignatureDoesNotMatch.
 */
export async function uploadToS3(
  uploadUrl: string,
  file: File,
  signal?: AbortSignal,
): Promise<void> {
  const response = await fetch(uploadUrl, {
    method: "PUT",
    headers: { "content-type": "application/pdf" },
    // The presigned URL carries its own authorization in the query string; an
    // Authorization header here would collide with the signature.
    credentials: "omit",
    body: file,
    ...(signal ? { signal } : {}),
  });
  if (!response.ok) {
    throw new ApiError(
      `Upload to storage failed (${response.status}). The presigned URL may have expired — try again.`,
      response.status,
      uploadUrl,
    );
  }
}

export type { DocumentSummary };
