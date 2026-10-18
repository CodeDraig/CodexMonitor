import {useEffect,useState} from "react";
import {runtimeRequest} from "@services/tauri";
import {runtimeError} from "../types";
/** Binary content stays in the store until explicitly requested by its renderer. */
export function AcpContent({sessionId,blocks}:{sessionId:string;blocks:Record<string,unknown>[]}){
  return <div className="acp-content">{blocks.map((block,index)=><ContentBlock key={index} sessionId={sessionId} block={block}/>)}</div>;
}
function ContentBlock({sessionId,block}:{sessionId:string;block:Record<string,unknown>}){
  const [source,setSource]=useState<string|null>(null),[error,setError]=useState<string|null>(null);
  const attachment=typeof block.attachmentId==="string"?block.attachmentId:null;
  useEffect(()=>{setSource(null);setError(null);},[attachment,sessionId]);
  const load=async()=>{if(!attachment)return;try{const result=await runtimeRequest<{mimeType:string;data:string}>("read_conversation_attachment",{sessionId,attachmentId:attachment});setSource(`data:${result.mimeType};base64,${result.data}`);}catch(e){setError(runtimeError(e));}};
  const kind=String(block.type??"content");
  if(kind==="text")return block.annotations?<details><summary>Text annotations</summary><pre>{JSON.stringify(block.annotations,null,2)}</pre></details>:null;
  if(kind==="image"||kind==="audio")return <figure>{source?(kind==="image"?<img src={source} alt={String(block.uri??"Agent image")} loading="lazy"/>:<audio src={source} controls/>):<button className="ghost" type="button" onClick={()=>void load()}>Load {kind} · {String(block.mimeType??"media")}</button>}{error&&<p role="alert">{error}</p>}<details><summary>Media metadata</summary><pre>{JSON.stringify(block,null,2)}</pre></details></figure>;
  if(kind==="resource")return <section><p>Embedded resource</p><ContentBlock sessionId={sessionId} block={block.resource as Record<string,unknown>??{}}/></section>;
  if(kind==="resource_link")return <p>{/^https?:\/\//.test(String(block.uri))?<a href={String(block.uri)} target="_blank" rel="noreferrer">{String(block.name??block.uri)}</a>:String(block.name??block.uri)}<details><summary>Resource metadata</summary><pre>{JSON.stringify(block,null,2)}</pre></details></p>;
  if(typeof block.text==="string")return <details><summary>{String(block.uri??"Resource text")}</summary><pre>{block.text}</pre><pre>{JSON.stringify(block,null,2)}</pre></details>;
  return <details><summary>{attachment?"Embedded binary resource":"Agent content"} · {kind}</summary><pre>{JSON.stringify(block,null,2)}</pre>{attachment&&<button type="button" className="ghost" onClick={()=>void load()}>Load resource data</button>}{source&&<a href={source} download={String(block.name??"agent-resource")}>Save resource</a>}{error&&<p role="alert">{error}</p>}</details>;
}
