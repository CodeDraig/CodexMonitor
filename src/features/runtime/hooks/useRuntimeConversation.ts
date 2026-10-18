import {useCallback,useEffect,useRef,useState} from "react";
import {runtimeRequest} from "@services/tauri";
import {subscribeRuntimeEvents} from "@services/events";
import type {ExecutionTicket,RuntimeEvent,RuntimeSession} from "../types";
import {runtimeError} from "../types";
export function useRuntimeConversation(session: RuntimeSession | null, backendKey: string) {
  const [events,setEvents] = useState<RuntimeEvent[]>([]);
  const [executions,setExecutions] = useState<ExecutionTicket[]>([]);
  const [error,setError] = useState<string|null>(null);
  const generation = useRef(0);
  const key = `${backendKey}:${session?.storeId}:${session?.sessionId}:${session?.runtime.profileRevision}`;
  const refresh = useRef<() => void>(() => {});
  useEffect(() => {
    const version = ++generation.current;
    let cursor = 0; let loading = false;
    setEvents([]); setExecutions([]); setError(null);
    const load = async () => {
      if (!session || loading) return;
      loading = true;
      try {
        while (version === generation.current) {
          const page = await runtimeRequest<RuntimeEvent[]>("events",{sessionId:session.sessionId,after:cursor});
          if (version !== generation.current) return;
          if (page.some(e => e.storeId !== session.storeId || e.sessionId !== session.sessionId)) throw new Error("Runtime store changed; reopen the conversation");
          if (!page.length) break;
          cursor = page[page.length-1].sequence;
          setEvents(previous => [...new Map([...previous,...page].map(e=>[e.sequence,e])).values()]);
          if (page.length < 1000) break;
        }
        const history = await runtimeRequest<{executions:ExecutionTicket[]}>("history",{sessionId:session.sessionId});
        if (version === generation.current) {setExecutions(history.executions);setError(null);}
      } catch(e) {if(version === generation.current)setError(runtimeError(e));}
      finally {loading = false;}
    };
    refresh.current = () => {void load();};
    // Events wake the durable cursor reader; never advance a cursor from a possibly gapped notification.
    const unsubscribe = subscribeRuntimeEvents(event => {if(event.storeId === session?.storeId && event.sessionId === session.sessionId) void load();});
    const timer = window.setInterval(() => {void load();},1500);
    const focus = () => {void load();};window.addEventListener("focus",focus);window.addEventListener("online",focus);
    void load();
    return () => {++generation.current;unsubscribe();window.clearInterval(timer);window.removeEventListener("focus",focus);window.removeEventListener("online",focus);};
  },[key,session]);
  const reload = useCallback(() => refresh.current(),[]);
  return {events,executions,error,reload};
}
