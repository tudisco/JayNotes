import { describe, it, expect } from "vitest";
import {
  languageForFilename,
  decodeTextFile,
  normalizeText,
} from "./textFile";

describe("languageForFilename", () => {
  it("maps known data extensions to a fence language", () => {
    expect(languageForFilename("delegate-mcp-clients.json")).toBe("json");
    expect(languageForFilename("config.YAML")).toBe("yaml");
    expect(languageForFilename("litestream-b2.env")).toBe("ini");
    expect(languageForFilename("notes.md")).toBe("markdown");
  });

  it("treats a leading-dot name as an extension, not a dotfile", () => {
    expect(languageForFilename(".env")).toBe("ini");
  });

  it("leaves unknown types unlabeled (still a code block)", () => {
    for (const name of [
      "delegate-mcp-owner.secret",
      "delegate-mcp.token",
      "tinylord.key",
      "README.txt",
      "output.log",
      "mystery",
    ]) {
      expect(languageForFilename(name)).toBe("");
    }
  });

  it("recognizes extensionless code filenames", () => {
    expect(languageForFilename("Dockerfile")).toBe("dockerfile");
    expect(languageForFilename("Makefile")).toBe("makefile");
  });

  it("ignores directory components", () => {
    expect(languageForFilename("/Users/me/secrets/keys.json")).toBe("json");
    expect(languageForFilename("C:\\data\\notes.yml")).toBe("yaml");
  });

  it("falls back to unlabeled for an empty name", () => {
    expect(languageForFilename("")).toBe("");
    expect(languageForFilename("...")).toBe("");
  });
});

describe("decodeTextFile", () => {
  it("decodes UTF-8 including multi-byte characters", () => {
    const bytes = new TextEncoder().encode('{"café": "☕"}');
    expect(decodeTextFile(bytes)).toBe('{"café": "☕"}');
  });

  it("rejects content holding a NUL byte", () => {
    expect(decodeTextFile(new Uint8Array([0x68, 0x69, 0x00, 0x21]))).toBeNull();
  });

  it("rejects invalid UTF-8", () => {
    expect(decodeTextFile(new Uint8Array([0x41, 0xff, 0xfe, 0x42]))).toBeNull();
  });

  it("accepts an empty file", () => {
    expect(decodeTextFile(new Uint8Array([]))).toBe("");
  });
});

describe("normalizeText", () => {
  it("strips a BOM and normalizes line endings", () => {
    expect(normalizeText("\uFEFFa\r\nb\rc")).toBe("a\nb\nc");
  });

  it("drops trailing newlines", () => {
    expect(normalizeText("a\n\n\n")).toBe("a");
  });

  it("preserves interior blank lines and indentation verbatim", () => {
    expect(normalizeText("{\n\n  'a': 1\n}")).toBe("{\n\n  'a': 1\n}");
  });
});
