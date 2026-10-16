import Diff from "lucide-react/dist/esm/icons/diff";
import FileDiffIcon from "lucide-react/dist/esm/icons/file-diff";
import FileText from "lucide-react/dist/esm/icons/file-text";
import Image from "lucide-react/dist/esm/icons/image";
import Search from "lucide-react/dist/esm/icons/search";
import Terminal from "lucide-react/dist/esm/icons/terminal";
import Users from "lucide-react/dist/esm/icons/users";
import Wrench from "lucide-react/dist/esm/icons/wrench";
import type { ConversationItem } from "@/types";
import { toolNameFromTitle, type ToolSummary } from "../utils/messageRenderUtils";

export function toolIconForSummary(
  item: Extract<ConversationItem, { kind: "tool" }>,
  summary: ToolSummary,
) {
  if (item.toolType === "commandExecution") {
    return Terminal;
  }
  if (item.toolType === "fileChange") {
    return FileDiffIcon;
  }
  if (item.toolType === "webSearch") {
    return Search;
  }
  if (item.toolType === "imageView") {
    return Image;
  }
  if (item.toolType === "collabToolCall") {
    return Users;
  }

  const label = summary.label.toLowerCase();
  if (label === "read") {
    return FileText;
  }
  if (label === "searched" || label === "searching") {
    return Search;
  }

  const toolName = toolNameFromTitle(item.title).toLowerCase();
  const title = item.title.toLowerCase();
  if (toolName.includes("diff") || title.includes("diff")) {
    return Diff;
  }

  return Wrench;
}
