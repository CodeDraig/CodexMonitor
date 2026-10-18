import type {ConversationItem} from "@/types";
/** Materialized ACP items use the same renderer input for replay and live content. */
export function conversationItems(rows:Record<string,unknown>[],sessionId:string):ConversationItem[] {
  return rows.map(p=>{
    const id=String(p.id);
    if(p.kind==="message")return {id,kind:"message",role:p.role==="user"?"user":"assistant",text:String(p.text??""),acpContent:{sessionId,blocks:[...(Array.isArray(p.content)?p.content:[]),...(p.native?[{type:"message_metadata",data:p.native}]:[])]}};
    if(p.kind==="reasoning")return {id,kind:"reasoning",summary:"Reasoning",content:String(p.text??""),acpContent:{sessionId,blocks:[...(Array.isArray(p.content)?p.content:[]),...(p.native?[{type:"message_metadata",data:p.native}]:[])]}};
    if(p.kind==="tool")return {id,kind:"tool",toolType:"acp",acpContent:{sessionId,blocks:Array.isArray(p.content)?p.content.filter(c=>c.type==="content").map(c=>c.content):[]},title:String(p.title??"Tool"),detail:p.rawInput===undefined?"":JSON.stringify(p.rawInput,null,2),status:p.status===undefined?undefined:String(p.status),output:p.content===undefined?undefined:JSON.stringify(p.content,null,2),changes:Array.isArray(p.content)?p.content.filter(c=>c.type==="diff").map(c=>({path:String(c.path??""),diff:`${c.oldText??""}\n${c.newText??""}`})):undefined};
    if(p.type==="compaction"){
      const data=p.data as {summary?:Record<string,unknown>[];status?:string};
      return {id,kind:"tool",toolType:"acp-compaction",title:"Context compaction · Original content may be unavailable",status:data.status,detail:"Agent-supplied compacted history",output:JSON.stringify(data,null,2),acpContent:{sessionId,blocks:Array.isArray(data.summary)?data.summary:[]}};
    }
    return {id,kind:"tool",toolType:"acp-protocol",title:String(p.type??"Agent content"),detail:"Supplied by the agent",output:JSON.stringify(p.data??p,null,2)};
  });
}
