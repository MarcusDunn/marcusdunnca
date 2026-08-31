import {
  CONFIDENCE_BANDS,
  CONFIDENCE_BOUNDS,
  maxScoreBits as ceilingFor,
  type Confidence,
  type HistoryAttempt,
  type QuestionFormat,
  type Skill,
  type Topic,
} from "../lib/schemas";

/**
 * Below this many observations we show the count and withhold the percentage.
 *
 * Ten questions per document, a handful of tags each, means a single
 * skill × topic cell collects maybe eight observations in two months. At n=3 a
 * "67%" is one question away from "33%", and the eye reads it as a finding
 * anyway. Withholding the number is the only reliable way to stop that; greying
 * it out is not, because the reader still reads it.
 */
export const MIN_OBSERVATIONS = 5;

/** One graded question, flattened out of its attempt. */
export type Observation = {
  attemptId: string;
  questionId: string;
  documentId: string;
  documentTitle: string;
  submittedAt: string;
  format: QuestionFormat;
  skill: Skill;
  topics: readonly Topic[];
  correct: boolean;
  /** Undefined on answers given before confidence was asked for. */
  confidence?: Confidence;
  /** Undefined on answers given before the slider — a band is not a number. */
  confidencePercent?: number;
  scoreBits: number;
};

export type Rate = {
  n: number;
  correct: number;
  /** null whenever n < MIN_OBSERVATIONS — there is no "raw rate" to reach for. */
  rate: number | null;
  suppressed: boolean;
};

export type HistoryFilters = {
  /**
   * Exactly one format, never a set.
   *
   * MCQ figure-recall and written figure-recall are different scales measuring
   * the same skill; a combined rate is a number with no referent. The type here
   * is the enforcement — there is no shape this module accepts that could
   * produce a cross-format pool, so no future caller can ask for one by accident.
   */
  format: QuestionFormat;
  /** Empty means "all" for both of these. */
  skills: readonly Skill[];
  topics: readonly Topic[];
};

const EMPTY_RATE: Rate = { n: 0, correct: 0, rate: null, suppressed: true };

export function toRate(correct: number, n: number): Rate {
  if (n === 0) return EMPTY_RATE;
  const suppressed = n < MIN_OBSERVATIONS;
  return { n, correct, rate: suppressed ? null : correct / n, suppressed };
}

function summarize(observations: readonly Observation[]): Rate {
  let correct = 0;
  for (const observation of observations) if (observation.correct) correct += 1;
  return toRate(correct, observations.length);
}

/** Flattens attempts to questions, applying the format segment and the filters. */
export function selectObservations(
  attempts: readonly HistoryAttempt[],
  filters: HistoryFilters,
): Observation[] {
  const skillFilter = new Set<Skill>(filters.skills);
  const topicFilter = new Set<Topic>(filters.topics);

  const observations: Observation[] = [];
  for (const attempt of attempts) {
    for (const question of attempt.questions) {
      if (question.format !== filters.format) continue;
      if (skillFilter.size > 0 && !skillFilter.has(question.skill)) continue;
      if (topicFilter.size > 0 && !question.topics.some((t) => topicFilter.has(t))) {
        continue;
      }
      observations.push(flatten(attempt, question));
    }
  }
  return observations;
}

function flatten(
  attempt: HistoryAttempt,
  question: HistoryAttempt["questions"][number],
): Observation {
  return {
    attemptId: attempt.attemptId,
    questionId: question.questionId,
    documentId: attempt.documentId,
    documentTitle: attempt.documentTitle,
    submittedAt: attempt.submittedAt,
    format: question.format,
    skill: question.skill,
    topics: question.topics,
    correct: question.correct,
    ...(question.confidence ? { confidence: question.confidence } : {}),
    ...(question.confidencePercent === undefined
      ? {}
      : { confidencePercent: question.confidencePercent }),
    scoreBits: question.scoreBits,
  };
}

/* ------------------------------------------------------------------ *
 * Calibration
 *
 * A deliberately different pool from the accuracy matrix above, and the
 * difference is the interesting part.
 *
 * `HistoryFilters` refuses to mix formats because a multiple-choice
 * figure-recall rate and a typed one are rates on different tasks — guessing
 * pays 25% on one and nothing on the other, so averaging them describes
 * nothing. Calibration is not subject to that: it asks whether a claim of "80%
 * or better" was right 80% of the time or better, and that question is about
 * the *reader's report*, not about the task the report was made on. Pooling is
 * not just permissible here, it is what makes the n usable at ten questions a
 * document.
 *
 * The one place the format does leak in is the bottom band, whose floor is 25%
 * on four options and roughly zero on a typed figure. It is labelled rather
 * than corrected — see `Confidence::belief_range` in the Rust.
 * ------------------------------------------------------------------ */

export type CalibrationFilters = {
  skills: readonly Skill[];
  topics: readonly Topic[];
};

/** Every answered question, both formats, narrowed by skill and topic only. */
export function selectRatedObservations(
  attempts: readonly HistoryAttempt[],
  filters: CalibrationFilters,
): Observation[] {
  const skillFilter = new Set<Skill>(filters.skills);
  const topicFilter = new Set<Topic>(filters.topics);

  const observations: Observation[] = [];
  for (const attempt of attempts) {
    for (const question of attempt.questions) {
      if (skillFilter.size > 0 && !skillFilter.has(question.skill)) continue;
      if (topicFilter.size > 0 && !question.topics.some((t) => topicFilter.has(t))) {
        continue;
      }
      observations.push(flatten(attempt, question));
    }
  }
  return observations;
}

/** How a band's observed accuracy compares with what the band claimed. */
export type Verdict = "overconfident" | "underconfident" | "calibrated";

export type BandStat = {
  band: Confidence;
  rate: Rate;
  /** The belief range this band asserts, as percentages. */
  claimed: { low: number; high: number };
  /**
   * null when the rate is suppressed. There is no "provisional" verdict: telling
   * someone they are overconfident on the strength of four questions is exactly
   * the over-reading the suppression rule exists to prevent.
   */
  verdict: Verdict | null;
};

export type Calibration = {
  bands: readonly BandStat[];
  /**
   * Answers with no confidence recorded, excluded from every band above.
   *
   * Surfaced rather than silently dropped: if this is large, the bands are
   * describing a small slice of the history and the reader should know which.
   */
  unrated: number;
  scoreBits: number;
  maxScoreBits: number;
  /**
   * Mean Brier score over every answer that stated a probability, and how many
   * did. `null` below the observation floor.
   *
   * **A measurement, not a score to play against.** Lower is better; 0.25 is
   * what saying 50% to everything gets you, so that is the line worth beating.
   * It correctly rewards having said 30% about something you got wrong, which
   * is exactly why it cannot double as `points` — points have to make being
   * sure and wrong hurt.
   *
   * Answers recorded before the slider carry a band and no number, and are
   * excluded rather than assigned a midpoint.
   */
  brier: { score: number | null; n: number };
  /**
   * Wrong answers given as `certain`, newest first.
   *
   * The most useful list on the page. These are the beliefs that would have
   * been stated out loud and been wrong — and they are also the ones that
   * correct best once contradicted, which is the one place where the worst
   * errors and the best learning opportunities are the same items.
   */
  confidentErrors: readonly Observation[];
};

export function buildCalibration(observations: readonly Observation[]): Calibration {
  const tallies = new Map<Confidence, Tally>();
  let unrated = 0;
  let scoreBits = 0;
  let maxScoreBits = 0;
  const confidentErrors: Observation[] = [];

  for (const observation of observations) {
    if (!observation.confidence) {
      unrated += 1;
      continue;
    }
    bump(tallies, observation.confidence, observation.correct);
    scoreBits += observation.scoreBits;
    maxScoreBits += ceilingFor(observation.format);
    if (observation.confidence === "certain" && !observation.correct) {
      confidentErrors.push(observation);
    }
  }

  let brierSum = 0;
  let brierN = 0;
  for (const observation of observations) {
    if (observation.confidencePercent === undefined) continue;
    const p = observation.confidencePercent / 100;
    const outcome = observation.correct ? 1 : 0;
    brierSum += (p - outcome) ** 2;
    brierN += 1;
  }

  const bands = CONFIDENCE_BANDS.map((band) => {
    const tally = tallies.get(band) ?? { n: 0, correct: 0 };
    const rate = toRate(tally.correct, tally.n);
    const claimed = CONFIDENCE_BOUNDS[band];
    return {
      band,
      rate,
      claimed,
      verdict: rate.rate === null ? null : judge(rate.rate, claimed),
    };
  });

  confidentErrors.sort((a, b) => Date.parse(b.submittedAt) - Date.parse(a.submittedAt));

  return {
    bands,
    unrated,
    scoreBits,
    maxScoreBits,
    // Suppressed on the same floor as every other rate here. A Brier score off
    // three answers is a number, not an estimate.
    brier: {
      score: brierN >= MIN_OBSERVATIONS ? brierSum / brierN : null,
      n: brierN,
    },
    confidentErrors,
  };
}

function judge(rate: number, claimed: { low: number; high: number }): Verdict {
  if (rate < claimed.low / 100) return "overconfident";
  if (rate > claimed.high / 100) return "underconfident";
  return "calibrated";
}

export type Breakdown = {
  /** Every rate below is drawn from this format only. */
  format: QuestionFormat;
  overall: Rate;
  skills: readonly Skill[];
  topics: readonly Topic[];
  bySkill: ReadonlyMap<Skill, Rate>;
  byTopic: ReadonlyMap<Topic, Rate>;
  /** Keyed `${skill} ${topic}`; absent key means zero observations. */
  cells: ReadonlyMap<string, Rate>;
};

export const cellKey = (skill: Skill, topic: Topic): string => `${skill} ${topic}`;

type Tally = { n: number; correct: number };

function bump<K>(map: Map<K, Tally>, key: K, correct: boolean): void {
  const entry = map.get(key) ?? { n: 0, correct: 0 };
  entry.n += 1;
  if (correct) entry.correct += 1;
  map.set(key, entry);
}

function finalize<K>(tally: Map<K, Tally>): Map<K, Rate> {
  const out = new Map<K, Rate>();
  for (const [key, value] of tally) out.set(key, toRate(value.correct, value.n));
  return out;
}

/**
 * Builds the skill × topic table for one format.
 *
 * The margins are NOT the sum of the cells. A question carries several topics, so
 * it lands in several topic columns; adding those columns up would count it more
 * than once and inflate `n` past the number of questions actually answered. Row,
 * column and overall margins are therefore each computed from distinct questions.
 */
export function buildBreakdown(
  observations: readonly Observation[],
  skills: readonly Skill[],
  topics: readonly Topic[],
): Breakdown {
  const format: QuestionFormat = observations[0]?.format ?? "multiple_choice";

  const bySkillTally = new Map<Skill, Tally>();
  const byTopicTally = new Map<Topic, Tally>();
  const cellTally = new Map<string, Tally>();

  const topicSet = new Set(topics);
  for (const observation of observations) {
    bump(bySkillTally, observation.skill, observation.correct);
    // A question tagged with the same topic twice would otherwise double-count
    // inside its own row.
    const seen = new Set<Topic>();
    for (const topic of observation.topics) {
      if (!topicSet.has(topic) || seen.has(topic)) continue;
      seen.add(topic);
      bump(byTopicTally, topic, observation.correct);
      bump(cellTally, cellKey(observation.skill, topic), observation.correct);
    }
  }

  return {
    format,
    overall: summarize(observations),
    skills,
    topics,
    bySkill: finalize(bySkillTally),
    byTopic: finalize(byTopicTally),
    cells: finalize(cellTally),
  };
}

/** Per-attempt roll-up for the attempt log, still segmented by format. */
export type AttemptRow = {
  attemptId: string;
  documentId: string;
  documentTitle: string;
  submittedAt: string;
  rate: Rate;
};

export function attemptRows(observations: readonly Observation[]): AttemptRow[] {
  const grouped = new Map<string, Observation[]>();
  for (const observation of observations) {
    const bucket = grouped.get(observation.attemptId);
    if (bucket) bucket.push(observation);
    else grouped.set(observation.attemptId, [observation]);
  }

  const rows: AttemptRow[] = [];
  for (const bucket of grouped.values()) {
    const first = bucket[0];
    if (!first) continue;
    rows.push({
      attemptId: first.attemptId,
      documentId: first.documentId,
      documentTitle: first.documentTitle,
      submittedAt: first.submittedAt,
      // An attempt's own score is exempt from suppression below — see
      // formatAttemptScore. A single attempt is a fact ("7 of 10"), not an
      // estimate of a rate, so it is always shown as a fraction.
      rate: summarize(bucket),
    });
  }
  rows.sort((a, b) => Date.parse(b.submittedAt) - Date.parse(a.submittedAt));
  return rows;
}

/* ------------------------------------------------------------------ *
 * Presentation
 * ------------------------------------------------------------------ */

/** "72%" when there's enough to say so, "—" when there isn't. Never a bare number. */
export function formatRate(rate: Rate): string {
  if (rate.n === 0) return "—";
  if (rate.rate === null) return "—";
  return `${Math.round(rate.rate * 100)}%`;
}

/** The `n` that must appear beside every rate. */
export function formatN(rate: Rate): string {
  return `n=${rate.n}`;
}

export function suppressionReason(rate: Rate): string | null {
  if (rate.n === 0) return "No questions yet";
  if (rate.suppressed) {
    return `${rate.correct}/${rate.n} correct — too few to quote a rate (need ${MIN_OBSERVATIONS})`;
  }
  return null;
}
