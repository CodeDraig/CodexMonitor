import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ParsedFileLocation } from "@utils/fileLinks";
import { remarkFileLinks } from "@/features/messages/utils/messageFileLinks";
import { normalizeListIndentation } from "@/features/messages/utils/normalizeListIndentation";
import { normalizeStructuredReviewTables } from "@/features/messages/utils/structuredReviewTables";
import { MarkdownPreBlock, type PreProps } from "./MarkdownPreBlock";
import { createMarkdownLinks } from "./markdownLinks";

type MarkdownProps = {
  value: string;
  className?: string;
  codeBlock?: boolean;
  codeBlockStyle?: "default" | "message";
  codeBlockCopyUseModifier?: boolean;
  showFilePath?: boolean;
  workspacePath?: string | null;
  onOpenFileLink?: (path: ParsedFileLocation) => void;
  onOpenFileLinkMenu?: (event: React.MouseEvent, path: ParsedFileLocation) => void;
  onOpenThreadLink?: (threadId: string) => void;
};

export function Markdown({
  value,
  className,
  codeBlock,
  codeBlockStyle = "default",
  codeBlockCopyUseModifier = false,
  showFilePath = true,
  workspacePath = null,
  onOpenFileLink,
  onOpenFileLinkMenu,
  onOpenThreadLink,
}: MarkdownProps) {
  const normalizedValue = codeBlock
    ? value
    : normalizeStructuredReviewTables(normalizeListIndentation(value));
  const content = codeBlock
    ? `\`\`\`\n${normalizedValue}\n\`\`\``
    : normalizedValue;
  const { components, urlTransform } = createMarkdownLinks({
    showFilePath, workspacePath, onOpenFileLink, onOpenFileLinkMenu, onOpenThreadLink,
  });

  if (codeBlockStyle === "message") {
    components.pre = ({ node, children }) => (
      <MarkdownPreBlock node={node as PreProps["node"]} copyUseModifier={codeBlockCopyUseModifier}>
        {children}
      </MarkdownPreBlock>
    );
  }

  return (
    <div className={className}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkFileLinks]}
        urlTransform={urlTransform}
        components={components}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
