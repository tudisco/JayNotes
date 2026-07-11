// A tiny hand-rolled fuzzy subsequence matcher for the quick switcher.
//
// `fuzzyScore` answers "do the characters of `query` appear in order within
// `target`?" and, if so, how good the match is. It is deliberately dependency
// free and cheap enough to run over every note on each keystroke.
//
// Scoring rewards the kinds of matches humans mean:
//   - word-start hits (after `/ - _ . space`, or at position 0) score highest,
//   - runs of consecutive characters score next,
//   - earlier and shorter matches edge ahead on ties.
// Matched character indices are returned so the UI can highlight them.

export interface FuzzyResult {
  /** Higher is better. Only meaningful relative to other scores. */
  score: number;
  /** Indices into `target` that were matched, ascending. */
  positions: number[];
}

const WORD_START = 12;
const CONSECUTIVE = 6;
const MATCH = 1;

function isBoundary(prev: string): boolean {
  return prev === "/" || prev === " " || prev === "-" || prev === "_" || prev === ".";
}

/**
 * Scores `query` against `target`. Returns `null` when `query` is not a
 * subsequence of `target`. An empty query trivially matches with score 0.
 * Case-insensitive.
 */
export function fuzzyScore(query: string, target: string): FuzzyResult | null {
  if (query === "") return { score: 0, positions: [] };

  const q = query.toLowerCase();
  const t = target.toLowerCase();
  const positions: number[] = [];
  let score = 0;
  let qi = 0;
  let prevMatch = -2; // so the first match is never "consecutive"

  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] !== q[qi]) continue;

    let bonus = MATCH;
    if (ti === 0 || isBoundary(target[ti - 1])) bonus += WORD_START;
    if (ti === prevMatch + 1) bonus += CONSECUTIVE;

    score += bonus;
    positions.push(ti);
    prevMatch = ti;
    qi += 1;
  }

  if (qi < q.length) return null; // ran out of target before matching all of query

  // Tie-breakers: prefer matches that start early and live in shorter strings.
  score -= positions[0] * 0.1;
  score -= t.length * 0.01;
  return { score, positions };
}
