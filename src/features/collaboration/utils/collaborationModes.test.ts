import { describe, expect, it } from "vitest";
import { parseCollaborationModes } from "./collaborationModes";

describe("collaboration mode metadata", () => {
  it("reads the current flat preset shape and preserves the original payload", () => {
    const preset = { name: "Plan", mode: "plan", model: "fixture-model", reasoning_effort: "high" };
    expect(parseCollaborationModes({ result: { data: [preset] } })).toEqual([{
      id: "plan", label: "Plan", mode: "plan", model: "fixture-model",
      reasoningEffort: "high", developerInstructions: null, value: preset,
    }]);
  });

  it("uses the required name when optional preset fields are absent", () => {
    const [preset] = parseCollaborationModes({ result: { data: [{ name: "Custom" }] } });
    expect(preset).toMatchObject({ id: "Custom", label: "Custom", model: "", reasoningEffort: null });
  });

  it("ignores malformed entries and handles an empty list", () => {
    expect(parseCollaborationModes({ result: { data: [null, {}, 42] } })).toEqual([]);
    expect(parseCollaborationModes({ result: { data: [] } })).toEqual([]);
  });
});
