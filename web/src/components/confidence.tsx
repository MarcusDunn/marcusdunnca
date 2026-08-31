import {
  CHANCE_FLOOR_PERCENT,
  MAX_PERCENT,
  scoreBits,
  type QuestionFormat,
} from "../lib/schemas";

/**
 * How sure you are, as a number.
 *
 * # Why a slider
 *
 * The score is continuous in this number — every step of the track changes what
 * the answer is worth, in both directions. The three bands survive only as the
 * review scheduler's input, and nothing the reader sees depends on them.
 *
 * Asking for a band was a poor thing to ask for.
 * "Fairly sure" invites a feeling; "68%" is a claim you can be wrong about, and
 * being asked to produce one is most of what calibration training consists of.
 *
 * It also makes the record far more useful. 51% and 79% used to score
 * identically; now they are different claims priced differently, and a
 * reliability curve built from real probabilities says something after one
 * attempt rather than after months.
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
  const ifRight = scoreBits(value, true, format);
  const ifWrong = scoreBits(value, false, format);
  const inputId = `${idPrefix}-confidence`;

  return (
    <div>
      <label htmlFor={inputId}>How sure are you?</label>{" "}
      <input
        type="range"
        id={inputId}
        name={inputId}
        min={floor}
        max={MAX_PERCENT}
        step={1}
        value={value}
        disabled={disabled}
        // `aria-valuetext` because a screen reader announcing "68" is much less
        // use than "68 percent, fairly sure, plus 2 or minus 1" — the price is
        // the part being decided, and it is not in the number.
        aria-valuetext={`${value} percent: ${signed(ifRight)} bits if right, ${signed(ifWrong)} if wrong`}
        onChange={(event) => onChange(Number(event.target.value))}
      />{" "}
      <output htmlFor={inputId}>
        {percent === null ? (
          // Not "25%". An untouched slider has said nothing, and showing a
          // number would make it look like it had.
          <>not set — drag to state a probability</>
        ) : (
          <>
            {value}% · {signed(ifRight)} if right, {signed(ifWrong)} if wrong
          </>
        )}
      </output>
    </div>
  );
}

/** Two decimals with an explicit sign, so a gain and a cost read differently. */
export function signed(bits: number): string {
  return `${bits >= 0 ? "+" : "\u2212"}${Math.abs(bits).toFixed(2)}`;
}
