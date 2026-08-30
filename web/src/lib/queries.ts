import { queryOptions } from "@tanstack/react-query";
import { api } from "./api";
import { isUnsettled, type DocumentSummary } from "./schemas";

export const queryKeys = {
  documents: ["documents"] as const,
  documentUrl: (id: string) => ["documents", id, "url"] as const,
  quiz: (id: string) => ["documents", id, "quiz"] as const,
  history: ["history"] as const,
};

/**
 * Generation takes 30–90 seconds, so 5s is the sweet spot: under twenty requests
 * across the worst case, and the user never waits long enough on a finished
 * document to wonder whether the page is stuck.
 *
 * The interval matters more than it looks. The api and generate Lambdas share an
 * account concurrency limit of 10, so every poll is competing with the very job
 * it's waiting on. One serial poll at 5s is a rounding error against that budget;
 * a 500ms poll, or a poll per document, would not be.
 */
const POLL_INTERVAL_MS = 5000;

/**
 * One list call covers every document's status. There is deliberately no
 * per-document status query: ten documents in flight would mean ten concurrent
 * calls against a shared limit of 10, starving the generate function that the
 * poll is waiting on.
 */
export const documentsQuery = () =>
  queryOptions({
    queryKey: queryKeys.documents,
    queryFn: ({ signal }) => api.listDocuments(signal),
    // Poll only while something is actually in flight. Returning false collapses
    // the interval entirely, so a settled list costs nothing — which matters when
    // the tab is left open for days on a phone.
    refetchInterval: (query) => {
      const documents = query.state.data as DocumentSummary[] | undefined;
      if (!documents) return false;
      return documents.some((document) => isUnsettled(document.status))
        ? POLL_INTERVAL_MS
        : false;
    },
    // Keep polling when the tab is backgrounded: the whole point is to come back
    // to a finished document rather than to a spinner that only starts on focus.
    refetchIntervalInBackground: true,
  });

/**
 * The presigned GET is short-lived, but we deliberately do *not* refetch it on an
 * interval: swapping `<embed src>` remounts the plugin and throws away the
 * reader's scroll position mid-quiz. The read screen offers a manual reload
 * instead, which is the rare case and a deliberate action.
 */
export const documentUrlQuery = (id: string) =>
  queryOptions({
    queryKey: queryKeys.documentUrl(id),
    queryFn: () => api.documentUrl(id),
    staleTime: Infinity,
    gcTime: 5 * 60_000,
    retry: 1,
  });

export const quizQuery = (id: string) =>
  queryOptions({
    queryKey: queryKeys.quiz(id),
    queryFn: () => api.quiz(id),
    // Questions are fixed once generated; refetching mid-quiz would be a way to
    // lose answers, not a way to get fresher data.
    staleTime: Infinity,
  });

export const historyQuery = () =>
  queryOptions({
    queryKey: queryKeys.history,
    queryFn: () => api.history(),
    // Filtering happens in memory rather than as query params. Single user, low
    // volume, and it keeps the `n` counts stable while filters change instead of
    // flickering through loading states on every checkbox.
    staleTime: 30_000,
  });
