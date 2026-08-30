/**
 * Reading a typed figure, client-side.
 *
 * # This is a mirror, not the grader
 *
 * `parse_reader_value` in `app/core/src/numeric.rs` is authoritative — it is
 * what actually decides whether an answer counts, and nothing here is consulted
 * at grading time. This exists for one job: stopping a submission that contains
 * an entry the server would not be able to read.
 *
 * That job matters because of the confidence bands. An unparseable entry grades
 * as wrong, and a wrong answer marked `certain` costs five points. Letting a
 * stray keystroke do that would price a typo as a false belief, which is the
 * one thing the scoring rule must never do.
 *
 * # Which way it may drift
 *
 * If the two disagree, the safe direction is for this one to be **stricter**:
 * the reader is asked to retype something the server would have accepted, which
 * is a small annoyance. The unsafe direction is this one being more permissive,
 * because then an entry sails past the check and is graded wrong. So the rules
 * below are kept deliberately identical rather than "close enough", and any
 * loosening belongs in the Rust first.
 */

/**
 * Characters stripped wherever they appear.
 *
 * Written as escapes rather than as literals because two of them are
 * non-breaking spaces — one from iOS keyboards, one from pasting a figure out
 * of a PDF — and a maintainer cannot see those in a string literal. This set
 * must stay identical to the one in `numeric::parse_reader_value`.
 */
const IGNORED = new Set([",", "_", " ", "\u00a0", "\u202f", "$", "%"]);

/**
 * Turn what the reader typed into a number, or `null`.
 *
 * Forgiving about the shapes a financial document uses — thousands separators,
 * a leading currency symbol, a trailing percent sign, accounting parentheses
 * for a negative, and the Unicode minus iOS substitutes. Unforgiving about
 * everything else: `"about 4"` and `"4 or 5"` are hedges, not answers, and a
 * question that already states its own tolerance has no use for a range.
 */
export function parseFigure(raw: string): number | null {
  const trimmed = raw.trim();
  if (trimmed === "") return null;

  let body = trimmed;
  let parenthesised = false;
  if (body.startsWith("(") && body.endsWith(")")) {
    body = body.slice(1, -1).trim();
    parenthesised = true;
  }

  let cleaned = "";
  let seenDigit = false;
  let seenDot = false;

  for (let i = 0; i < body.length; i += 1) {
    const ch = body[i] as string;

    // A sign is only a sign in first position. `4-5` is a range, not a number,
    // and must not collapse to `45`.
    if (i === 0 && (ch === "-" || ch === "−")) {
      cleaned += "-";
      continue;
    }
    if (i === 0 && ch === "+") continue;

    // Separators and unit noise the reader echoed back.
    if (IGNORED.has(ch)) continue;

    if (ch === ".") {
      // A second dot means a date or a version, not a figure.
      if (seenDot) return null;
      seenDot = true;
      cleaned += ".";
      continue;
    }

    if (ch >= "0" && ch <= "9") {
      seenDigit = true;
      cleaned += ch;
      continue;
    }

    // A letter, a slash, a second sign: a phrase is not a number.
    return null;
  }

  if (!seenDigit) return null;

  const parsed = Number(cleaned);
  if (!Number.isFinite(parsed)) return null;

  return parenthesised && parsed > 0 ? -parsed : parsed;
}

/**
 * Render a figure without the trailing zeros a float carries when it happens to
 * be integral. Display only, and only here — the server sends figures as
 * numbers and never formats one.
 */
export function formatFigure(value: number): string {
  if (!Number.isFinite(value)) return "—";
  if (Number.isInteger(value)) return String(value);
  return String(Number(value.toFixed(4)));
}

/** "±1 %", or "±1" when the figure is a bare count. */
export function formatTolerance(tolerance: number, unit: string): string {
  const suffix = unit.trim() === "" ? "" : ` ${unit.trim()}`;
  return `±${formatFigure(tolerance)}${suffix}`;
}
