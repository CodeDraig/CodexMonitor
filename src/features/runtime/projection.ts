import type {ConversationItem} from "@/types";
import type {RuntimeEvent} from "./types";
export type RuntimeProjection = {items: ConversationItem[]; permissions: Record<string, Record<string, unknown>>};
export function projectRuntimeEvents(events: RuntimeEvent[]): RuntimeProjection {
  const items: ConversationItem[] = [];
  const permissions: RuntimeProjection["permissions"] = {};
  const permissionExecutions:Record<string,string|null> = {};
  let lastExecution: string | null | undefined;
  for (const event of [...new Map(events.map(e => [e.sequence, e])).values()].sort((a,b) => a.sequence-b.sequence)) {
    if (event.source === "history") continue;
    const submitted = event.kind === "execution_submitted" || event.raw.method === "runtime/begin";
    const p = submitted && event.raw.method === "runtime/begin" ? (event.raw.params as Record<string, unknown>) : event.payload;
    if (event.kind === "permission") {permissions[String(p.requestId)] = p;permissionExecutions[String(p.requestId)] = event.executionId;}
    if (event.kind === "permission_settled") {delete permissions[String(p.requestId)];delete permissionExecutions[String(p.requestId)];}
    if (event.kind === "execution_completed" || event.kind === "disconnected") {for (const key of Object.keys(permissions)) {if(permissionExecutions[key] === event.executionId){delete permissions[key];delete permissionExecutions[key];}}}
    if (submitted) {
      const input = p.input as {prompt?: {type: string; text?: string}[]} | undefined;
      const text = input?.prompt?.filter(c => c.type === "text").map(c => c.text ?? "").join("\n") ?? "";
      if (text) items.push({id: `user:${event.executionId}`,kind: "message",role: "user",text});
    }
    if (event.kind === "message_delta") {
      const role = p.role === "user" ? "user" : "assistant";
      const text = String(p.text ?? "");
      const previous = items[items.length - 1];
      if (p.role === "reasoning") {
        if (previous?.kind === "reasoning" && lastExecution === event.executionId) previous.content += text;
        else items.push({id: `reasoning:${event.sequence}`,kind: "reasoning",summary: "Reasoning",content: text});
      } else if (previous?.kind === "message" && previous.role === role && lastExecution === event.executionId) previous.text += text;
      else items.push({id: String(p.itemId ?? `message:${event.sequence}`),kind: "message",role,text});
    }
    if (event.kind === "tool_patch") {
      const id = String(p.toolCallId);
      const previous = items.find(i => i.id === id && i.kind === "tool");
      const item = previous?.kind === "tool" ? previous : {id,kind: "tool" as const,toolType: "acp",title: "Tool",detail: ""};
      if (p.title !== undefined) item.title = String(p.title);
      if (p.status !== undefined) item.status = String(p.status);
      if (p.content !== undefined) item.output = JSON.stringify(p.content,null,2);
      if (p.rawInput !== undefined) item.detail = JSON.stringify(p.rawInput,null,2);
      if (!previous) items.push(item);
    }
    if (event.kind === "item") {
      const id = String(p.id); const index = items.findIndex(i => i.id === id);
      let item: ConversationItem;
      if (p.type === "agentMessage" || p.type === "userMessage") {
        const content = p.content as {text?:string}[] | undefined;
        const text = String(p.text ?? content?.map(c=>c.text ?? "").join("\n") ?? "");
        if (p.type === "userMessage" && items.some(i=>i.id === `user:${event.executionId}` && i.kind === "message" && i.text === text)) continue;
        item = {id,kind:"message",role:p.type === "userMessage" ? "user" : "assistant",text};
      }
      else item = {id,kind:"tool",toolType:String(p.type),title:String(p.command ?? p.type),detail:"",status:String(p.status ?? ""),output:String(p.aggregatedOutput ?? "")};
      if (index >= 0) items[index] = item; else items.push(item);
    }
    lastExecution = event.executionId;
  }
  return {items,permissions};
}
