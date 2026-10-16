import { describe, expect, it } from "vitest";
import type { AppSettings } from "@/types";
import {
  defaultRemoteBackend,
  getActiveRemoteBackend,
  normalizeRemoteBackends,
  patchActiveRemoteBackend,
} from "./remoteBackends";

describe("remote backend selection", () => {
  const first = { ...defaultRemoteBackend(), id: "first", host: "first:4732", token: "first-token" };
  const second = { ...defaultRemoteBackend(), id: "second", host: "second:5732", token: "second-token" };

  it("uses the selected entry without overwriting its connection details", () => {
    const settings = normalizeRemoteBackends({ remoteBackends: [first, second], activeRemoteBackendId: "second" });
    expect(getActiveRemoteBackend(settings)).toEqual(second);
    expect(settings.remoteBackends[0]).toEqual(first);
  });

  it("selects the first available entry when the selected ID is missing", () => {
    const settings = normalizeRemoteBackends({ remoteBackends: [first, second], activeRemoteBackendId: "missing" });
    expect(settings.activeRemoteBackendId).toBe("first");
    expect(getActiveRemoteBackend(settings)).toEqual(first);
  });

  it("creates the current default for an empty collection and disambiguates repeated IDs", () => {
    expect(normalizeRemoteBackends({ remoteBackends: [], activeRemoteBackendId: null })).toEqual({
      remoteBackends: [defaultRemoteBackend()], activeRemoteBackendId: "remote-default",
    });
    const settings = normalizeRemoteBackends({ remoteBackends: [first, first, first], activeRemoteBackendId: "first" });
    expect(settings.remoteBackends.map((entry) => entry.id)).toEqual(["first", "first-2", "first-3"]);
  });

  it("updates only the selected connection and preserves saved connections", () => {
    const settings = { remoteBackends: [first, second], activeRemoteBackendId: "second" } as AppSettings;
    const patched = patchActiveRemoteBackend(settings, { host: "new:5732", token: "new-token", lastConnectedAtMs: 123 });
    expect(getActiveRemoteBackend(patched)).toEqual({ ...second, host: "new:5732", token: "new-token", lastConnectedAtMs: 123 });
    expect(patched.remoteBackends[0]).toEqual(first);
    expect(settings.remoteBackends).toEqual([first, second]);
  });
});
