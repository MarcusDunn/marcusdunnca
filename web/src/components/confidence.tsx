import {
  bandForPercent,
  CHANCE_FLOOR_PERCENT,
  CONFIDENCE_LABELS,
  CONFIDENCE_POINTS,
  type QuestionFormat,
} from "../lib/schemas";

/**
 * How sure you are, as a number.
 *
 * # Why a slider rather than three buttons
 *
 * The three bands are still what gets *scored* — the points table and the
 * review scheduler both want buckets — but they are a poor thing to ask for.
 * "Fairly sure" invites a feeling; "68%" is a claim you can be wrong about, and
 * being asked to produce one is most of what calibration training consists of.
 *
 * It also makes the record far more useful. Two answers in the same band can be
 * 51% and 79%, which are very different statements, and a reliability curve
 * built from three buckets needs a great deal of data before it says anything.
 * A stated probability gives a Brier score from the first attempt.
 *
 * # Why it starts at chance and cannot go below it
 *
 * A slider with a default anchors you, so the default is the one position that
 * asserts nothing: pure chance. On four options that is 25% — you will answer
 * *something*, so a lower report is not modesty, it is an error, and the track
 * simply does not go there. A typed figure has no options and so effectively no
 * floor.
 *
 * Moving it is still required before the form will submit. Leaving it at the
 * floor is a legitimate claim, but it has to be a claim you made rather than
 * one you defaulted into.
 */
export function ConfidenceSlider({
  idPrefix,
  format,
  percent,
  onChange,
  disabled,
}: {
  idPrefix: string;
  format: QuestionFormat;
  /** `null` until the reader has touched it. */
  percent: number | null;
  onChange: (next: number) => void;
  disabled: boolean;
}) {
  const floor = CHANCE_FLOOR_PERCENT[format];
  const value = percent ?? floor;
  const band = bandForPercent(value);
  const points = CONFIDENCE_POINTS[band];
  const inputId = `${idPrefix}-confidence`;

  return (
    <div>
      <label htmlFor={inputId}>How sure are you?</label>{" "}
      <input
        type="range"
        id={inputId}
        name={inputId}
        min={floor}
        max={100}
        step={1}
        value={value}
        disabled={disabled}
        // `aria-valuetext` because a screen reader announcing "68" is much less
        // use than "68 percent, fairly sure, plus 2 or minus 1" — the price is
        // the part being decided, and it is not in the number.
        aria-valuetext={`${value}%, ${CONFIDENCE_LABELS[band].toLowerCase()}, +${points.correct} if right, ${points.wrong} if wrong`}
        onChange={(event) => onChange(Number(event.target.value))}
      />{" "}
      <output htmlFor={inputId}>
        {percent === null ? (
          // Not "25%". An untouched slider has said nothing, and showing a
          // number would make it look like it had.
          <>not set — drag to state a probability</>
        ) : (
          <>
            {value}% · {CONFIDENCE_LABELS[band]} (+{points.correct} / {points.wrong})
          </>
        )}
      </output>
    </div>
  );
}
