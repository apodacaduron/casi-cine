import { describe, expect, it } from "vitest";
import { formatBytes, formatDuration } from "./lib";

describe("display formatting", () => {
  it("formats duration without rolling over incorrectly", () => {
    expect(formatDuration(65.4)).toBe("1:05");
    expect(formatDuration(-2)).toBe("0:00");
  });
  it("formats binary file sizes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1024)).toBe("1.0 KB");
  });
});
