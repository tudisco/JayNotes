// Friendly relative-time formatting for note timestamps.
//
// Input is a Unix time in *seconds* (as `list_notes` / `NoteRef.mtime` return
// them). Output is a short, human label suited to a dense list: "2m", "3h",
// "yesterday", "Jul 2" (same calendar year), or "2025-11-03" (older).

const MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];

function pad2(n: number): string {
  return n < 10 ? `0${n}` : `${n}`;
}

/**
 * Formats `mtimeSecs` (Unix seconds) relative to `now` as a compact label.
 * `now` is injectable for deterministic tests.
 */
export function relativeTime(mtimeSecs: number, now: Date = new Date()): string {
  const nowSecs = Math.floor(now.getTime() / 1000);
  const diff = nowSecs - mtimeSecs;

  // Just-written or clock skew (mtime slightly ahead of us).
  if (diff < 60) return "now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  if (diff < 172800) return "yesterday";

  const d = new Date(mtimeSecs * 1000);
  if (d.getFullYear() === now.getFullYear()) {
    return `${MONTHS[d.getMonth()]} ${d.getDate()}`;
  }
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}
