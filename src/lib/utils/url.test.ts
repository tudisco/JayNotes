import { describe, expect, it } from "vitest";
import { isRelativeUrl } from "./url";

describe("isRelativeUrl", () => {
  it("treats plain relative paths as relative", () => {
    expect(isRelativeUrl("attachments/pic.png")).toBe(true);
    expect(isRelativeUrl("images/sub/photo.jpg")).toBe(true);
    expect(isRelativeUrl("pic.png")).toBe(true);
    expect(isRelativeUrl("./pic.png")).toBe(true);
    expect(isRelativeUrl("a b/my image.webp")).toBe(true);
  });

  it("treats scheme URLs as non-relative", () => {
    expect(isRelativeUrl("http://example.com/a.png")).toBe(false);
    expect(isRelativeUrl("https://example.com/a.png")).toBe(false);
    expect(isRelativeUrl("data:image/png;base64,AAAA")).toBe(false);
    expect(isRelativeUrl("blob:abcd")).toBe(false);
    expect(isRelativeUrl("file:///Users/x/a.png")).toBe(false);
    expect(isRelativeUrl("asset://localhost/a.png")).toBe(false);
  });

  it("treats absolute and protocol-relative paths as non-relative", () => {
    expect(isRelativeUrl("/Users/x/a.png")).toBe(false);
    expect(isRelativeUrl("//cdn.example.com/a.png")).toBe(false);
  });

  it("returns false for empty or whitespace input", () => {
    expect(isRelativeUrl("")).toBe(false);
    expect(isRelativeUrl("   ")).toBe(false);
  });
});
