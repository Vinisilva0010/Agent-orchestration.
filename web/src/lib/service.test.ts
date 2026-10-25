import { describe, expect, it } from "vitest";
import { serviceName } from "./service";

describe("serviceName", () => {
  it("reports the service name", () => {
    expect(serviceName()).toBe("web");
  });
});
