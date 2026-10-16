import { getActiveRemoteBackend, patchActiveRemoteBackend } from "@settings/utils/remoteBackends";
import { useCallback, useEffect, useMemo, useState } from "react";
import { listWorkspaces } from "../../../services/tauri";
import type { AppSettings } from "../../../types";
import { isMobilePlatform } from "../../../utils/platformPaths";
import type { MobileServerSetupWizardProps } from "../components/MobileServerSetupWizard";

type UseMobileServerSetupParams = {
  appSettings: AppSettings;
  appSettingsLoading: boolean;
  queueSaveSettings: (next: AppSettings) => Promise<AppSettings>;
  refreshWorkspaces: () => Promise<unknown>;
};

type UseMobileServerSetupResult = {
  isMobileRuntime: boolean;
  showMobileSetupWizard: boolean;
  mobileSetupWizardProps: MobileServerSetupWizardProps;
  handleMobileConnectSuccess: () => Promise<void>;
};

function isRemoteServerConfigured(settings: AppSettings): boolean {
  const remote = getActiveRemoteBackend(settings);
  return Boolean(remote.token?.trim()) && Boolean(remote.host.trim());
}

function defaultMobileSetupMessage(): string {
  return "Enter your desktop Tailscale host and token, then run Connect & test.";
}

export function useMobileServerSetup({
  appSettings,
  appSettingsLoading,
  queueSaveSettings,
  refreshWorkspaces,
}: UseMobileServerSetupParams): UseMobileServerSetupResult {
  const activeRemote = getActiveRemoteBackend(appSettings);
  const isMobileRuntime = useMemo(() => isMobilePlatform(), []);

  const [remoteHostDraft, setRemoteHostDraft] = useState(activeRemote.host);
  const [remoteTokenDraft, setRemoteTokenDraft] = useState(activeRemote.token ?? "");
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);
  const [statusError, setStatusError] = useState(false);
  const [mobileServerReady, setMobileServerReady] = useState(!isMobileRuntime);
  const [setupWizardDismissed, setSetupWizardDismissed] = useState(false);

  useEffect(() => {
    if (!isMobileRuntime) {
      return;
    }
    setRemoteHostDraft(activeRemote.host);
    setRemoteTokenDraft(activeRemote.token ?? "");
  }, [
    activeRemote.host,
    activeRemote.token,
    isMobileRuntime,
  ]);

  const runConnectivityCheck = useCallback(
    async (options?: { announceSuccess?: boolean }) => {
      if (!isMobileRuntime) {
        return true;
      }
      try {
        const entries = await listWorkspaces();
        try {
          await refreshWorkspaces();
        } catch {
          // Connectivity is confirmed by listWorkspaces; refresh is best-effort.
        }
        setMobileServerReady(true);
        setStatusError(false);
        if (options?.announceSuccess) {
          const count = entries.length;
          const workspaceWord = count === 1 ? "workspace" : "workspaces";
          setStatusMessage(`Connected. ${count} ${workspaceWord} available from your desktop backend.`);
        } else {
          setStatusMessage(null);
        }
        return true;
      } catch (error) {
        const message =
          error instanceof Error ? error.message : "Unable to reach remote backend.";
        setMobileServerReady(false);
        setStatusError(true);
        setStatusMessage(message);
        return false;
      }
    },
    [isMobileRuntime, refreshWorkspaces],
  );

  const onConnectTest = useCallback(() => {
    void (async () => {
      if (!isMobileRuntime || busy) {
        return;
      }

      const nextHost = remoteHostDraft.trim();
      const nextToken = remoteTokenDraft.trim() ? remoteTokenDraft.trim() : null;

      if (!nextHost || !nextToken) {
        setMobileServerReady(false);
        setStatusError(true);
        setStatusMessage(defaultMobileSetupMessage());
        return;
      }

      setBusy(true);
      setSetupWizardDismissed(false);
      setStatusError(false);
      setStatusMessage(null);
      try {
        const saved = await queueSaveSettings({
          ...patchActiveRemoteBackend(appSettings, { host: nextHost, token: nextToken }),
          backendMode: "remote",
        });
        const connected = await runConnectivityCheck({ announceSuccess: true });
        if (connected) {
          await queueSaveSettings(patchActiveRemoteBackend(saved, { lastConnectedAtMs: Date.now() }));
        }
      } catch (error) {
        const message =
          error instanceof Error ? error.message : "Unable to save remote backend settings.";
        setMobileServerReady(false);
        setStatusError(true);
        setStatusMessage(message);
      } finally {
        setBusy(false);
      }
    })();
  }, [
    appSettings,
    busy,
    isMobileRuntime,
    queueSaveSettings,
    remoteHostDraft,
    remoteTokenDraft,
    runConnectivityCheck,
  ]);

  useEffect(() => {
    if (!isMobileRuntime || appSettingsLoading || busy) {
      return;
    }
    if (!isRemoteServerConfigured(appSettings)) {
      setMobileServerReady(false);
      setChecking(false);
      setStatusError(true);
      setStatusMessage(defaultMobileSetupMessage());
      return;
    }

    let active = true;
    setChecking(true);

    void (async () => {
      const ok = await runConnectivityCheck();
      if (active && !ok) {
        setStatusMessage((previous) => previous ?? "Unable to connect to remote backend.");
      }
      if (active) {
        setChecking(false);
      }
    })();

    return () => {
      active = false;
    };
  }, [
    appSettings,
    appSettingsLoading,
    busy,
    isMobileRuntime,
    runConnectivityCheck,
  ]);

  const handleMobileConnectSuccess = useCallback(async () => {
    if (!isMobileRuntime) {
      return;
    }
    setStatusError(false);
    setStatusMessage(null);
    setMobileServerReady(true);
    setSetupWizardDismissed(false);
    try {
      await refreshWorkspaces();
    } catch {
      // Keep successful connectivity result even if local refresh fails.
    }
  }, [isMobileRuntime, refreshWorkspaces]);

  return {
    isMobileRuntime,
    showMobileSetupWizard:
      isMobileRuntime && !appSettingsLoading && !mobileServerReady && !setupWizardDismissed,
    mobileSetupWizardProps: {
      remoteHostDraft,
      remoteTokenDraft,
      busy,
      checking,
      statusMessage,
      statusError,
      onClose: () => {
        setSetupWizardDismissed(true);
      },
      onRemoteHostChange: setRemoteHostDraft,
      onRemoteTokenChange: setRemoteTokenDraft,
      onConnectTest,
    },
    handleMobileConnectSuccess,
  };
}
