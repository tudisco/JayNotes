import { describe, expect, it } from "vitest";
import { basename, shortenPath } from "./path";

describe("basename", () => {
  it("returns the final path segment", () => {
    expect(basename("/Users/jay/Notes")).toBe("Notes");
    expect(basename("/Volumes/WorkDrive/My Vault")).toBe("My Vault");
    expect(basename("Notes")).toBe("Notes");
  });

  it("ignores a trailing slash", () => {
    expect(basename("/Users/jay/Notes/")).toBe("Notes");
    expect(basename("/Users/jay/Notes///")).toBe("Notes");
  });

  it("handles backslash separators", () => {
    expect(basename("C:\\Users\\jay\\Notes")).toBe("Notes");
  });
});

describe("shortenPath", () => {
  it("collapses the home directory to ~", () => {
    expect(shortenPath("/Users/jay/Notes")).toBe("~/Notes");
    expect(shortenPath("/home/jay/Notes")).toBe("~/Notes");
  });

  it("leaves short paths untouched", () => {
    expect(shortenPath("/Volumes/WD/Notes")).toBe("/Volumes/WD/Notes");
  });

  it("elides the middle but keeps the final segment", () => {
    const p = "/Volumes/WorkDrive/Hot/Projects/DeepFolder/MyVault";
    const short = shortenPath(p, 30);
    expect(short.length).toBeLessThanOrEqual(30);
    expect(short.endsWith("MyVault")).toBe(true);
    expect(short).toContain("…");
  });

  it("hard-truncates a single over-long segment", () => {
    const long = "/" + "x".repeat(80);
    const short = shortenPath(long, 20);
    expect(short.length).toBeLessThanOrEqual(20);
    expect(short.startsWith("…")).toBe(true);
  });
});
