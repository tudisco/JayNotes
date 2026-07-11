import { describe, expect, it } from "vitest";
import { relativeTime } from "./time";

// A fixed reference "now": 2026-07-11 12:00:00 local time.
const now = new Date(2026, 6, 11, 12, 0, 0);
const nowSecs = Math.floor(now.getTime() / 1000);

function ago(seconds: number): number {
  return nowSecs - seconds;
}

describe("relativeTime", () => {
  it("shows 'now' for very recent times", () => {
    expect(relativeTime(nowSecs, now)).toBe("now");
    expect(relativeTime(ago(30), now)).toBe("now");
    expect(relativeTime(ago(59), now)).toBe("now");
  });

  it("treats future/clock-skewed times as 'now'", () => {
    expect(relativeTime(ago(-120), now)).toBe("now");
  });

  it("shows minutes under an hour", () => {
    expect(relativeTime(ago(60), now)).toBe("1m");
    expect(relativeTime(ago(120), now)).toBe("2m");
    expect(relativeTime(ago(59 * 60), now)).toBe("59m");
  });

  it("shows hours under a day", () => {
    expect(relativeTime(ago(3600), now)).toBe("1h");
    expect(relativeTime(ago(3 * 3600), now)).toBe("3h");
    expect(relativeTime(ago(23 * 3600), now)).toBe("23h");
  });

  it("shows 'yesterday' between 24 and 48 hours", () => {
    expect(relativeTime(ago(25 * 3600), now)).toBe("yesterday");
    expect(relativeTime(ago(47 * 3600), now)).toBe("yesterday");
  });

  it("shows month + day for older times in the same year", () => {
    // 2026-07-02
    const d = new Date(2026, 6, 2, 9, 0, 0);
    expect(relativeTime(Math.floor(d.getTime() / 1000), now)).toBe("Jul 2");
    // 2026-01-15
    const jan = new Date(2026, 0, 15, 9, 0, 0);
    expect(relativeTime(Math.floor(jan.getTime() / 1000), now)).toBe("Jan 15");
  });

  it("shows an ISO date for times in a previous year", () => {
    const d = new Date(2025, 10, 3, 9, 0, 0); // 2025-11-03
    expect(relativeTime(Math.floor(d.getTime() / 1000), now)).toBe("2025-11-03");
    const d2 = new Date(2024, 0, 5, 9, 0, 0); // 2024-01-05
    expect(relativeTime(Math.floor(d2.getTime() / 1000), now)).toBe("2024-01-05");
  });
});
