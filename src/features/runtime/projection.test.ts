import {describe,it,expect} from "vitest";
import {projectRuntimeEvents} from "./projection";
import type {RuntimeEvent} from "./types";
const event = (sequence:number,kind:string,payload:Record<string,unknown>):RuntimeEvent=>({schemaVersion:1,storeId:"store",runtime:{kind:"open_code_acp",profileId:"opencode",profileRevision:1},sessionId:"session",executionId:"execution",sequence,source:"live",kind,payload,raw:{}});
describe("runtime event projection",()=>{
 it("deduplicates replay and merges partial tool updates without clearing omitted output",()=>{
  const events=[event(1,"message_delta",{role:"assistant",text:"first "}),event(2,"message_delta",{role:"assistant",text:"result"}),event(3,"tool_patch",{toolCallId:"tool",title:"Command",content:["output"]}),event(4,"tool_patch",{toolCallId:"tool",status:"completed"})];
  const {items}=projectRuntimeEvents([...events,...events]);
  expect(items).toHaveLength(2);expect(items[0]).toMatchObject({kind:"message",text:"first result"});expect(items[1]).toMatchObject({kind:"tool",title:"Command",status:"completed",output:'[\n  "output"\n]'});
 });
 it("settles permissions and keeps distinct executions apart",()=>{
  const first=event(1,"permission",{requestId:"generation:1",options:[]});
  const second=event(2,"permission_settled",{requestId:"generation:1"});
  expect(projectRuntimeEvents([first,second]).permissions).toEqual({});
  const a=event(3,"message_delta",{text:"A"});const b={...event(4,"message_delta",{text:"B"}),executionId:"followup"};
  expect(projectRuntimeEvents([a,b]).items).toHaveLength(2);
 });
});
it("shows the submitted user input from durable domain events and existing stored raw envelopes",()=>{
 const submitted=event(1,"execution_submitted",{input:{prompt:[{type:"text",text:"original input"}]}});
 expect(projectRuntimeEvents([submitted]).items).toMatchObject([{kind:"message",role:"user",text:"original input"}]);
 const stored={...event(2,"protocol",{}),raw:{method:"runtime/begin",params:{input:{prompt:[{type:"text",text:"stored input"}]}}}};
 expect(projectRuntimeEvents([stored]).items).toMatchObject([{kind:"message",role:"user",text:"stored input"}]);
});
it("does not append agent history replay to the recorded conversation",()=>{
 const live=event(1,"message_delta",{text:"saved result"});
 const replay={...event(2,"message_delta",{text:"saved result"}),source:"history" as const,executionId:null};
 expect(projectRuntimeEvents([live,replay]).items).toMatchObject([{text:"saved result"}]);
});
it("does not settle a new execution's permission when an old connection closes",()=>{
 const pending={...event(1,"permission",{requestId:"new-generation:1",options:[]}),executionId:"new-execution"};
 const closed={...event(2,"disconnected",{}),executionId:null};
 expect(projectRuntimeEvents([pending,closed]).permissions).toHaveProperty("new-generation:1");
});

it("projects Codex user content without duplicating Monitor's submitted input",()=>{
 const submitted=event(1,"execution_submitted",{input:{prompt:[{type:"text",text:"input"}]}});
 const user=event(2,"item",{id:"native-user",type:"userMessage",content:[{type:"text",text:"input"}]});
 expect(projectRuntimeEvents([submitted,user]).items).toMatchObject([{role:"user",text:"input"}]);
 expect(projectRuntimeEvents([user]).items).toMatchObject([{role:"user",text:"input"}]);
});
