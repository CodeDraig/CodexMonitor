import {describe,it,expect} from "vitest";
import {acpRef,conversationKey,isAcpConversation,runtimeBackendKey} from "./conversationIdentity";
import {conversationItems} from "./conversationProjection";
describe("conversation identity and shared ACP projection",()=>{
 it("scopes UI keys by backend and store and keeps native IDs separate",()=>{
  const ref={kind:"acp" as const,backendId:"backend",storeId:"store:a",sessionId:"uuid"};
  expect(acpRef(conversationKey(ref))).toEqual(ref);expect(isAcpConversation(conversationKey(ref))).toBe(true);
  expect(conversationKey({...ref,backendId:"other"})).not.toBe(conversationKey(ref));expect(conversationKey({...ref,storeId:"other"})).not.toBe(conversationKey(ref));
  expect(acpRef("native-codex-id")).toBeNull();expect(acpRef("acp:v1:%:bad:key")).toBeNull();
 });
 it("does not expose backend tokens in UI IDs",()=>{
  const settings={backendMode:"remote" as const,activeRemoteBackendId:"r",remoteBackends:[{id:"r",name:"Remote",host:"127.0.0.1:9999",token:"secret-native-test",provider:"tcp" as const}]};
  const key=runtimeBackendKey(settings);expect(key).not.toContain("secret");expect(key).toBe(runtimeBackendKey(settings));
 });
 it("preserves repeated messages, supplied IDs, attachments, tool patches, and compaction data",()=>{
  const rows=[{id:"u1",kind:"message",role:"user",text:"again"},{id:"a1",kind:"message",role:"assistant",text:"again",content:[{type:"image",attachmentId:"x"}]},{id:"u2",kind:"message",role:"user",text:"again"},{id:"t",kind:"tool",title:"Command",status:"completed",content:[{type:"content",content:{type:"audio",attachmentId:"a"}}]},{id:"c",kind:"protocol",type:"compaction",data:{summary:"Retained summary"}}];
  const items=conversationItems(rows,"session");expect(items.filter(i=>i.kind==="message"&&i.text==="again")).toHaveLength(3);expect(items[1]).toMatchObject({id:"a1",acpContent:{sessionId:"session",blocks:[{attachmentId:"x"}]}});expect(items[3]).toMatchObject({title:"Command",status:"completed",acpContent:{blocks:[{type:"audio"}]}});expect(items[4]).toMatchObject({title:expect.stringContaining("compaction"),output:expect.stringContaining("Retained summary"),acpContent:{blocks:[]}});
 });
});
