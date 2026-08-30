import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { Busy, ErrorNotice } from "../components/ui";
import {
  attemptRows,
  buildBreakdown,
  cellKey,
  formatN,
  formatRate,
  MIN_OBSERVATIONS,
  selectObservations,
  suppressionReason,
  type Breakdown,
  type Rate,
} from "../history/aggregate";
import { historyQuery } from "../lib/queries";
import {
  FORMATS,
  FORMAT_LABELS,
  SKILLS,
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
  const visibleSkills = skills.length > 0 ? skills : SKILLS;
  const visibleTopics = topics.length > 0 ? topics : observedTopics;
  const breakdown = buildBreakdown(observations, visibleSkills, visibleTopics);
  const rows = attemptRows(observations);

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
          won&apos;t be: a multiple-choice figure-recall question and a written
          figure-recall question measure the same skill on different scales, so
          averaging them produces a number that doesn&apos;t describe anything.
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
