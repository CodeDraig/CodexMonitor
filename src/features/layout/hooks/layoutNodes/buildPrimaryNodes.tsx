import { RuntimeSidebar } from "@/features/runtime/components/RuntimeSidebar";
import { AcpConversationControls } from "@/features/runtime/components/AcpConversationControls";
import ArrowLeft from "lucide-react/dist/esm/icons/arrow-left";
import { Sidebar } from "../../../app/components/Sidebar";
import { Home } from "../../../home/components/Home";
import { MainHeader } from "../../../app/components/MainHeader";
import { Messages } from "../../../messages/components/Messages";
import { ApprovalToasts } from "../../../app/components/ApprovalToasts";
import { UpdateToast } from "../../../update/components/UpdateToast";
import { ErrorToasts } from "../../../notifications/components/ErrorToasts";
import { Composer } from "../../../composer/components/Composer";
import { TabBar } from "../../../app/components/TabBar";
import { TabletNav } from "../../../app/components/TabletNav";
import type {
  LayoutNodesResult,
  LayoutPrimarySurface,
} from "./types";

export type PrimaryLayoutNodesOptions = LayoutPrimarySurface;

type PrimaryLayoutNodes = Pick<
  LayoutNodesResult,
  | "sidebarNode"
  | "messagesNode"
  | "composerNode"
  | "approvalToastsNode"
  | "updateToastNode"
  | "errorToastsNode"
  | "homeNode"
  | "mainHeaderNode"
  | "desktopTopbarLeftNode"
  | "tabletNavNode"
  | "tabBarNode"
>;

export function buildPrimaryNodes(options: PrimaryLayoutNodesOptions): PrimaryLayoutNodes {
  const runtime=options.runtime;
  const sidebarNode = runtime
    ? <RuntimeSidebar key={runtime.backendKey} sidebarProps={options.sidebarProps} runtime={runtime} />
    : <Sidebar {...options.sidebarProps} />;

  const messagesNode = <Messages {...options.messagesProps} {...(runtime?.selected?{onQuoteMessage:(text:string)=>runtime.setDraft(`${runtime.draft}${runtime.draft?"\n\n":""}${text}`),isThinking:!!runtime.active||!!runtime.pending}: {})} />;

  const composerNode = options.composerProps ? <>{runtime?.selected&&<AcpConversationControls runtime={runtime}/>}<Composer {...options.composerProps} {...(runtime?.selected?{
    runtimeMode:true,runtimeLabel:runtime.session?.profile.name,runtimeCommands:runtime.commands,onSend:runtime.send,onStop:()=>void runtime.stop(),canStop:!!runtime.active,isProcessing:!!runtime.active||!!runtime.pending||runtime.sending,
    disabled:!runtime.session||runtime.page?.connectionReady===false||runtime.page?.readOnly||!!runtime.uncertain,
    appsEnabled:false,steerAvailable:false,followUpMessageBehavior:"queue" as const,
    models:[],collaborationModes:[],reasoningOptions:[],reasoningSupported:false,codexArgsOptions:[],skills:[],apps:[],prompts:[],
    draftText:runtime.draft,onDraftChange:runtime.setDraft,historyKey:options.messagesProps.threadId,
    attachedImages:runtime.images,onAttachImages:(images:string[])=>runtime.setImages([...runtime.images,...images]),onRemoveImage:(image:string)=>runtime.setImages(runtime.images.filter(i=>i!==image)),onPickImages:runtime.session?.capabilities.images?()=>document.getElementById("acp-attachments")?.click():undefined,
    queuedMessages:runtime.queue.map(e=>({id:e.requestId,text:e.text,images:e.images,createdAt:0})),onDeleteQueued:runtime.removeQueued,onEditQueued:undefined,
    prefillDraft:null,insertText:null,contextActions:[],reviewPrompt:null,
    onQuoteMessage:undefined,
  }: {})}/></> : null;

  const approvalToastsNode = <ApprovalToasts {...options.approvalToastsProps} />;

  const updateToastNode = <UpdateToast {...options.updateToastProps} />;

  const errorToastsNode = <ErrorToasts {...options.errorToastsProps} />;

  const homeNode = <Home {...options.homeProps} />;

  const mainHeaderNode = options.mainHeaderProps ? (
    <MainHeader {...options.mainHeaderProps} conversationAgentLabel={runtime?.selected?runtime.session?.profile.name:undefined}/>
  ) : null;

  const desktopTopbarLeftNode = (
    <>
      {options.desktopTopbarProps.showBackToChat && (
        <button
          className="icon-button back-button"
          onClick={options.desktopTopbarProps.onExitDiff}
          aria-label="Back to chat"
        >
          <ArrowLeft aria-hidden />
        </button>
      )}
      {mainHeaderNode}
    </>
  );

  const tabletNavNode = (
    <TabletNav {...options.tabletNavProps} />
  );

  const tabBarNode = <TabBar {...options.tabBarProps} />;

  return {
    sidebarNode,
    messagesNode,
    composerNode,
    approvalToastsNode,
    updateToastNode,
    errorToastsNode,
    homeNode,
    mainHeaderNode,
    desktopTopbarLeftNode,
    tabletNavNode,
    tabBarNode,
  };
}
