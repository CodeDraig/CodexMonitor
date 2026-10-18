import type {AppSettings} from "@/types";
import {getActiveRemoteBackend} from "@settings/utils/remoteBackends";
import type {ConversationRef} from "./types";
export function runtimeBackendKey(settings: Pick<AppSettings,"backendMode"|"remoteBackends"|"activeRemoteBackendId">) {
  const backend=getActiveRemoteBackend(settings);
  const text=JSON.stringify([settings.backendMode,settings.activeRemoteBackendId,backend.host,backend.token]);
  // No credentials appear in UI IDs, drafts, or DOM attributes.
  let hash=0xcbf29ce484222325n;
  for(const c of text) hash=BigInt.asUintN(64,(hash^BigInt(c.charCodeAt(0)))*0x100000001b3n);
  return hash.toString(16);
}
export function conversationKey(ref:ConversationRef):string {
  return ref.kind==="codex"?ref.threadId:`acp:v1:${encodeURIComponent(ref.backendId)}:${encodeURIComponent(ref.storeId)}:${encodeURIComponent(ref.sessionId)}`;
}
export function acpRef(key:string|null|undefined):Extract<ConversationRef,{kind:"acp"}>|null {
  if(!key?.startsWith("acp:v1:"))return null;
  const parts=key.split(":");if(parts.length!==5)return null;
  try{return {kind:"acp",backendId:decodeURIComponent(parts[2]),storeId:decodeURIComponent(parts[3]),sessionId:decodeURIComponent(parts[4])};}catch{return null;}
}
export const isAcpConversation=(key:string|null|undefined)=>Boolean(key?.startsWith("acp:"));
