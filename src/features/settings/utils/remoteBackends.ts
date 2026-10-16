import type { AppSettings, RemoteBackendTarget } from "@/types";

type RemoteSettings = Pick<AppSettings, "remoteBackends" | "activeRemoteBackendId">;

export function defaultRemoteBackend(): RemoteBackendTarget {
  return {
    id: "remote-default",
    name: "Primary remote",
    provider: "tcp",
    host: "127.0.0.1:4732",
    token: null,
    lastConnectedAtMs: null,
  };
}

export function getConfiguredRemoteBackends(settings: RemoteSettings): RemoteBackendTarget[] {
  return settings.remoteBackends.length > 0 ? settings.remoteBackends : [defaultRemoteBackend()];
}

export function getActiveRemoteBackend(settings: RemoteSettings): RemoteBackendTarget {
  const configured = getConfiguredRemoteBackends(settings);
  return configured.find((entry) => entry.id === settings.activeRemoteBackendId) ?? configured[0];
}

export function normalizeRemoteBackends(settings: RemoteSettings): RemoteSettings {
  const usedIds = new Set<string>();
  const remoteBackends = getConfiguredRemoteBackends(settings).map((entry, index) => {
    const baseId = entry.id?.trim() || `remote-${index + 1}`;
    let id = baseId;
    let suffix = 2;
    while (usedIds.has(id)) {
      id = `${baseId}-${suffix}`;
      suffix += 1;
    }
    usedIds.add(id);
    return {
      id,
      name: entry.name?.trim() || `Remote ${index + 1}`,
      provider: entry.provider,
      host: entry.host?.trim() || "127.0.0.1:4732",
      token: entry.token?.trim() || null,
      lastConnectedAtMs:
        typeof entry.lastConnectedAtMs === "number" && Number.isFinite(entry.lastConnectedAtMs)
          ? entry.lastConnectedAtMs
          : null,
    };
  });
  const active = getActiveRemoteBackend({ ...settings, remoteBackends });
  return { remoteBackends, activeRemoteBackendId: active.id };
}

export function patchActiveRemoteBackend(
  settings: AppSettings,
  patch: Partial<RemoteBackendTarget>,
): AppSettings {
  const active = getActiveRemoteBackend(settings);
  return {
    ...settings,
    activeRemoteBackendId: active.id,
    remoteBackends: getConfiguredRemoteBackends(settings).map((entry) =>
      entry.id === active.id ? { ...entry, ...patch } : entry,
    ),
  };
}
