import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { Busy, ErrorNotice } from "../components/ui";
import {
  attemptRows,
  buildBreakdown,
  buildCalibration,
  cellKey,
  formatN,
  formatRate,
  MIN_OBSERVATIONS,
  selectObservations,
  selectRatedObservations,
  suppressionReason,
  type Breakdown,
  type Calibration,
  type Rate,
} from "../history/aggregate";
import { historyQuery } from "../lib/queries";
import {
  CONFIDENCE_LABELS,
  FORMATS,
  FORMAT_LABELS,
  SKILLS,
  SKILLS_BY_FORMAT,
  SKILL_LABELS,
  topicLabel,
  type QuestionFormat,
  type Skill,
  type Topic,
} from "../lib/schemas";

export function HistoryScreen() {
  const history = useQuery(historyQuery());

  // Format is single-select with no "all" option, by design — see the note the UI
  // renders below the picker, and HistoryFilters in history/aggregate.ts.
  const [format, setFormat] = useState<QuestionFormat>("multiple_choice");
  const [skills, setSkills] = useState<Skill[]>([]);
  const [topics, setTopics] = useState<Topic[]>([]);

  if (history.isPending) return <Busy label="Loading history" />;
  if (history.isError) {
    return <ErrorNotice error={history.error} onRetry={() => void history.refetch()} />;
  }

  // Topics are open, so the axis of the matrix is whatever has actually been
  // read — there is no fixed list to lay out. Unlike skills, an unused topic is
  // not a gap worth showing: a column of empty cells for a subject you have
  // never touched is noise, and with the model coining tags there could be any
  // number of them.
  const observedTopics: Topic[] = [
    ...new Set(
      history.data.flatMap((attempt) =>
        attempt.questions.flatMap((question) => question.topics),
      ),
    ),
  ].toSorted();

  const observations = selectObservations(history.data, { format, skills, topics });

  // The rows this format can produce, plus any it *has* produced. The second
  // half is what keeps attempts from before the formats split visible: they
  // contain multiple-choice figure-recall questions, which nothing generates
  // any more, and dropping their row would quietly delete the history rather
  // than mark it superseded.
  const answeredSkills = new Set<Skill>(observations.map((o) => o.skill));
  const axisSkills = SKILLS.filter(
    (skill) => SKILLS_BY_FORMAT[format].includes(skill) || answeredSkills.has(skill),
  );

  const visibleSkills = skills.length > 0 ? skills : axisSkills;
  const visibleTopics = topics.length > 0 ? topics : observedTopics;
  const breakdown = buildBreakdown(observations, visibleSkills, visibleTopics);
  const rows = attemptRows(observations);

  // Deliberately NOT narrowed by `format`. Calibration is a property of the
  // reader's reports rather than of the task they were made on, so pooling the
  // two formats is both legitimate and the only way the n reaches a usable size
  // — see the note in history/aggregate.ts.
  const calibration = buildCalibration(
    selectRatedObservations(history.data, { skills, topics }),
  );

  return (
    <section>
      <h1>History</h1>

      <fieldset>
        <legend>Question format</legend>
        {FORMATS.map((option) => (
          <div key={option}>
            <input
              type="radio"
              id={`format-${option}`}
              name="format"
              value={option}
              checked={format === option}
              onChange={() => setFormat(option)}
            />
            <label htmlFor={`format-${option}`}>{FORMAT_LABELS[option]}</label>
          </div>
        ))}
        <p>
          One format at a time, always. There is no combined view and there
          won&apos;t be: guessing pays 25% on a multiple-choice question and
          nothing at all on a typed figure, so a pooled correct-rate would move
          with the mix of formats rather than with what you know. The
          calibration table above is the one place both are pooled, and it can
          be because it measures your reports rather than the questions.
        </p>
      </fieldset>

      <details>
        <summary>Filters</summary>
        <CheckboxFilter
          idPrefix="skill"
          legend="Skills"
          options={SKILLS}
          labels={SKILL_LABELS}
          selected={skills}
          onChange={setSkills}
        />
        <CheckboxFilter
          idPrefix="topic"
          legend="Topics"
          options={observedTopics}
          labels={Object.fromEntries(observedTopics.map((t) => [t, topicLabel(t)]))}
          selected={topics}
          onChange={setTopics}
        />
      </details>

      <h2>Calibration</h2>
      <CalibrationSection calibration={calibration} />

      <h2>{FORMAT_LABELS[format]} overall</h2>
      <p>
        <RateText rate={breakdown.overall} /> across {rows.length} attempt
        {rows.length === 1 ? "" : "s"}.
        {breakdown.overall.suppressed && breakdown.overall.n > 0
          ? ` Not enough answered questions to quote a rate yet — ${MIN_OBSERVATIONS} is the floor.`
          : ""}
      </p>

      <h2>Skill by topic</h2>
      <p>
        Every cell carries its n. Cells under n={MIN_OBSERVATIONS} show the count and
        no percentage — with ten questions a document and a few tags each, a cell can
        sit at three observations for weeks, and &ldquo;67%&rdquo; from three
        questions reads as a finding when it is noise.
      </p>
      <BreakdownTable breakdown={breakdown} />
      <p>
        Row and column totals are counted from distinct questions, not by adding the
        cells up: a question carries several topics, so it appears in several columns.
      </p>

      <h2>Attempts</h2>
      {rows.length === 0 ? (
        <p>No {FORMAT_LABELS[format].toLowerCase()} answers match these filters yet.</p>
      ) : (
        <ul>
          {rows.map((row) => (
            <li key={row.attemptId}>
              <Link to="/docs/$documentId" params={{ documentId: row.documentId }}>
                {row.documentTitle || "Untitled"}
              </Link>
              {" — "}
              {/* A single attempt is reported as the raw fraction. It's an event,
                  not an estimate of a rate, so suppression doesn't apply — but it
                  isn't shown as a percentage either, because "70%" off ten
                  questions invites exactly the same over-reading. */}
              {row.rate.correct} of {row.rate.n} correct,{" "}
              {new Date(row.submittedAt).toLocaleString()}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * Whether each confidence band was worth what it claimed.
 *
 * The suppression rule from the accuracy tables applies here too and matters
 * more: "you are overconfident" is a claim about someone's judgement, and
 * making it off four questions is precisely the over-reading the floor exists
 * to prevent. A band under the floor shows its count and no verdict.
 */
function CalibrationSection({ calibration }: { calibration: Calibration }) {
  const rated = calibration.bands.reduce((sum, band) => sum + band.rate.n, 0);

  if (rated === 0) {
    return (
      <p>
        Nothing to show yet — calibration is measured from the confidence you
        record with each answer, and none of the attempts so far carry one.
        {calibration.unrated > 0
          ? ` (${calibration.unrated} earlier answers predate the confidence bands and are excluded rather than counted as guesses.)`
          : ""}
      </p>
    );
  }

  return (
    <>
      <p>
        {calibration.points} of {calibration.maxPoints} points across {rated}{" "}
        answered question{rated === 1 ? "" : "s"}. Both formats are pooled here:
        this table is about your reports, not about the questions.
        {calibration.unrated > 0
          ? ` ${calibration.unrated} earlier answer${
              calibration.unrated === 1 ? " is" : "s are"
            } excluded — they were given before a confidence was asked for, which is not the same as guessing.`
          : ""}
      </p>

      <table>
        <caption>
          What each band claimed, against how often it was right. A band is
          working when the observed rate lands inside the range it asserts.
        </caption>
        <thead>
          <tr>
            <th scope="col">Band</th>
            <th scope="col">Claimed</th>
            <th scope="col">Observed</th>
            <th scope="col">Reading</th>
          </tr>
        </thead>
        <tbody>
          {calibration.bands.map((band) => (
            <tr key={band.band}>
              <th scope="row">{CONFIDENCE_LABELS[band.band]}</th>
              <td>
                {band.claimed.low}–{band.claimed.high}%
              </td>
              <td title={suppressionReason(band.rate) ?? undefined}>
                {formatRate(band.rate)} ({formatN(band.rate)})
              </td>
              <td>{VERDICT_TEXT[band.verdict ?? "unknown"]}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p>
        The bottom band&apos;s floor is 25% on a multiple-choice question and
        near zero on a typed figure, so a low &ldquo;guessing&rdquo; rate is
        less informative than it looks. The two bands above it are the ones
        worth reading.
      </p>

      {calibration.confidentErrors.length > 0 ? (
        <>
          <h3>Sure, and wrong</h3>
          <p>
            Every answer given as certain that turned out not to be. This is the
            shortlist worth rereading: they are the beliefs that would have been
            stated out loud, and the ones that correct most durably once
            contradicted.
          </p>
          <ul>
            {calibration.confidentErrors.map((observation) => (
              <li key={`${observation.attemptId}-${observation.questionId}`}>
                <Link
                  to="/docs/$documentId"
                  params={{ documentId: observation.documentId }}
                >
                  {observation.documentTitle || "Untitled"}
                </Link>
                {" — "}
                {SKILL_LABELS[observation.skill]},{" "}
                {new Date(observation.submittedAt).toLocaleDateString()}
              </li>
            ))}
          </ul>
        </>
      ) : null}
    </>
  );
}

const VERDICT_TEXT: Record<"overconfident" | "underconfident" | "calibrated" | "unknown", string> =
  {
    overconfident: "Overconfident — right less often than claimed",
    underconfident: "Understated — right more often than claimed",
    calibrated: "About right",
    unknown: `Too few to say (need ${MIN_OBSERVATIONS})`,
  };

function CheckboxFilter<T extends string>({
  idPrefix,
  legend,
  options,
  labels,
  selected,
  onChange,
}: {
  idPrefix: string;
  legend: string;
  options: readonly T[];
  labels: Record<T, string>;
  selected: T[];
  onChange: (next: T[]) => void;
}) {
  return (
    <fieldset>
      <legend>
        {legend} {selected.length === 0 ? "(all)" : `(${selected.length} selected)`}
      </legend>
      {options.map((option) => {
        const inputId = `${idPrefix}-${option}`;
        return (
          <div key={option}>
            <input
              type="checkbox"
              id={inputId}
              name={idPrefix}
              value={option}
              checked={selected.includes(option)}
              onChange={(event) =>
                onChange(
                  event.target.checked
                    ? [...selected, option]
                    : selected.filter((o) => o !== option),
                )
              }
            />
            <label htmlFor={inputId}>{labels[option]}</label>
          </div>
        );
      })}
      <button type="button" onClick={() => onChange([])} disabled={selected.length === 0}>
        Clear {legend.toLowerCase()}
      </button>
    </fieldset>
  );
}

/** The rate and its n are one unit. No code path renders one without the other. */
function RateText({ rate }: { rate: Rate }) {
  return (
    <>
      {formatRate(rate)} ({formatN(rate)})
    </>
  );
}

const ZERO: Rate = { n: 0, correct: 0, rate: null, suppressed: true };

function Cell({ rate }: { rate: Rate }) {
  const reason = suppressionReason(rate);
  // The title carries the raw correct/total for a suppressed cell, so the number
  // is available on demand without being on the page inviting a false reading.
  return (
    <td title={reason ?? undefined}>
      {formatRate(rate)} ({formatN(rate)})
    </td>
  );
}

function BreakdownTable({ breakdown }: { breakdown: Breakdown }) {
  return (
    <table>
      <caption>
        Correct rate by skill and topic — {FORMAT_LABELS[breakdown.format]} only. Each
        cell shows the rate and the number of questions behind it; cells with fewer
        than {MIN_OBSERVATIONS} questions show no rate.
      </caption>
      <thead>
        <tr>
          <th scope="col">Skill</th>
          {breakdown.topics.map((topic) => (
            <th key={topic} scope="col">
              {topicLabel(topic)}
            </th>
          ))}
          <th scope="col">All topics</th>
        </tr>
      </thead>
      <tbody>
        {breakdown.skills.map((skill) => (
          <tr key={skill}>
            <th scope="row">{SKILL_LABELS[skill]}</th>
            {breakdown.topics.map((topic) => (
              <Cell key={topic} rate={breakdown.cells.get(cellKey(skill, topic)) ?? ZERO} />
            ))}
            <Cell rate={breakdown.bySkill.get(skill) ?? ZERO} />
          </tr>
        ))}
      </tbody>
      <tfoot>
        <tr>
          <th scope="row">All skills</th>
          {breakdown.topics.map((topic) => (
            <Cell key={topic} rate={breakdown.byTopic.get(topic) ?? ZERO} />
          ))}
          <Cell rate={breakdown.overall} />
        </tr>
      </tfoot>
    </table>
  );
}
