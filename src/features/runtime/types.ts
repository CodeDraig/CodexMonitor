import type { ExecutionOutcome, RunContext, SubmissionOutcome } from "@/features/runs/types";
export type RuntimeKind = "codex" | "open_code_acp" | "acp";
export type RuntimeRef = {profileId: string; profileRevision: number; kind: RuntimeKind};
export type RuntimeProfile = {id: string; revision: number; name: string; kind: RuntimeKind; executable: string; args: string[]; workingDirectory?: string | null};
export type RuntimeCapabilities = {loadSession: boolean; sessionDiscovery: boolean; images: boolean; audio?: boolean; embeddedContext?: boolean; configuration: boolean; cancellation: boolean; steering: boolean; permissions: boolean};
export type RuntimeConfigOption = {id: string; name: string; type: string; currentValue: string; options: ({value: string; name: string} | {name: string; options: {value: string; name: string}[]})[]};
export type RuntimeSession = {storeId: string; runtime: RuntimeRef; sessionId: string; nativeSessionId: string; workspaceId: string; cwd: string; profile: RuntimeProfile; capabilities: RuntimeCapabilities; configuration: {configOptions?: RuntimeConfigOption[]; modes?: {currentModeId: string; availableModes: {id: string; name: string}[]}; models?: {currentModelId: string; availableModels: {modelId: string; name: string}[]}} | null; agentInfo: Record<string, unknown>};
export type RuntimeSubmit = {requestId: string; sessionId: string; prompt: Record<string, unknown>[]; options?: Record<string, unknown>; runContext?: RunContext};
export type ExecutionTicket = {executionId: string; requestId: string; sessionId: string; submission: SubmissionOutcome; execution: ExecutionOutcome; nativeTurnId: string | null; input: RuntimeSubmit; error: string | null};
export type RuntimeEvent = {workspaceId?:string;schemaVersion: number; storeId: string; runtime: RuntimeRef; sessionId: string; executionId: string | null; sequence: number; historyLoadId?: string; source: "live" | "history"; kind: string; payload: Record<string, unknown>; raw: Record<string, unknown>};
export function runtimeError(error: unknown) {return typeof error === "object" && error !== null && "message" in error ? String(error.message) : String(error);}

export type ConversationRef = {kind:"codex";threadId:string} | {kind:"acp";backendId:string;storeId:string;sessionId:string};
export type CatalogSession = {workspaceId?:string|null;catalogId:string;storeId:string;connectionScope:string;nativeSessionId:string;sessionId:string|null;profile:RuntimeProfile;cwd:string;title:string|null;updatedAt:string|null;metadata:Record<string,unknown>;revision:number;presentation:{titleOverride?:string|null;pinned?:boolean;hidden?:boolean}};
export type ConversationPage = {connectionReady?:boolean;storeId:string;sessionId:string;generation:string;items:Record<string,unknown>[];nextCursor:number|null;load:{id:string;state:"loading"|"failed"|"complete";error:string|null}|null;coverage:"agent_replay"|"monitor_observed_only";freshness:{loadedAt:number|null;stale:boolean};readOnly:boolean;activeExecutionId:string|null};
export type DiscoveryPage = {sessions:CatalogSession[];capabilities:RuntimeCapabilities;sourceStatus:"ready"|"unsupported";nextCursor:string|null};
