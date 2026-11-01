import { describe, expect, it } from "vitest";
import { keyProblem } from "./keys";

describe("keyProblem", () => {
  it("accepts a plain key and refuses pasted extras", () => {
    expect(keyProblem("abcd1234efgh")).toBeNull();
    expect(keyProblem("short")).not.toBeNull();
    expect(keyProblem("abcd 1234 efgh")).not.toBeNull();
    expect(keyProblem("abcd1234\nefgh")).not.toBeNull();
    expect(keyProblem("x".repeat(513))).not.toBeNull();
  });
});
