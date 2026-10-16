import type { Components } from "react-markdown";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  describeFileTarget,
  formatParsedFileLocation,
  isFileLinkUrl,
  parseFileLinkUrl,
  parseInlineFileTarget,
  resolveMessageFileHref,
  toFileLink,
} from "@/features/messages/utils/messageFileLinks";
import type { ParsedFileLocation } from "@utils/fileLinks";

type MarkdownLinkOptions = {
  showFilePath?: boolean;
  workspacePath?: string | null;
  onOpenFileLink?: (path: ParsedFileLocation) => void;
  onOpenFileLinkMenu?: (event: React.MouseEvent, path: ParsedFileLocation) => void;
  onOpenThreadLink?: (threadId: string) => void;
};

function FileReferenceLink({
  href,
  rawPath,
  showFilePath,
  workspacePath,
  onClick,
  onContextMenu,
}: {
  href: string;
  rawPath: ParsedFileLocation;
  showFilePath: boolean;
  workspacePath?: string | null;
  onClick: (event: React.MouseEvent, path: ParsedFileLocation) => void;
  onContextMenu: (event: React.MouseEvent, path: ParsedFileLocation) => void;
}) {
  const { fullPath, fileName, lineLabel, parentPath } = describeFileTarget(rawPath, workspacePath);
  return (
    <a
      href={href}
      className="message-file-link"
      title={fullPath}
      onClick={(event) => onClick(event, rawPath)}
      onContextMenu={(event) => onContextMenu(event, rawPath)}
    >
      <span className="message-file-link-name">{fileName}</span>
      {lineLabel ? <span className="message-file-link-line">L{lineLabel}</span> : null}
      {showFilePath && parentPath ? (
        <span className="message-file-link-path">{parentPath}</span>
      ) : null}
    </a>
  );
}

export function createMarkdownLinks({ showFilePath = true, workspacePath = null, onOpenFileLink, onOpenFileLinkMenu, onOpenThreadLink }: MarkdownLinkOptions) {
  const handleFileLinkClick = (event: React.MouseEvent, path: ParsedFileLocation) => {
    event.preventDefault();
    event.stopPropagation();
    onOpenFileLink?.(path);
  };
  const handleLocalLinkClick = (event: React.MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
  };
  const handleFileLinkContextMenu = (
    event: React.MouseEvent,
    path: ParsedFileLocation,
  ) => {
    event.preventDefault();
    event.stopPropagation();
    onOpenFileLinkMenu?.(event, path);
  };
  const resolvedHrefFilePathCache = new Map<string, ParsedFileLocation | null>();
  const resolveHrefFilePath = (url: string) => {
    if (resolvedHrefFilePathCache.has(url)) {
      return resolvedHrefFilePathCache.get(url) ?? null;
    }
    const resolvedPath = resolveMessageFileHref(url, workspacePath);
    if (!resolvedPath) {
      resolvedHrefFilePathCache.set(url, null);
      return null;
    }
    resolvedHrefFilePathCache.set(url, resolvedPath);
    return resolvedPath;
  };
  const components: Components = {
    table: ({ children }) => (
      <div className="markdown-table-wrap">
        <table className="markdown-table">{children}</table>
      </div>
    ),
    a: ({ href, children }) => {
      const url = (href ?? "").trim();
      const threadId = url.startsWith("thread://")
        ? url.slice("thread://".length).trim()
        : url.startsWith("/thread/")
          ? url.slice("/thread/".length).trim()
          : "";
      if (threadId) {
        return (
          <a
            href={href}
            onClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              onOpenThreadLink?.(threadId);
            }}
          >
            {children}
          </a>
        );
      }
      if (isFileLinkUrl(url)) {
        const path = parseFileLinkUrl(url);
        if (!path) {
          return (
            <a
              href={href}
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
              }}
            >
              {children}
            </a>
          );
        }
        return (
          <FileReferenceLink
            href={href ?? toFileLink(path)}
            rawPath={path}
            showFilePath={showFilePath}
            workspacePath={workspacePath}
            onClick={handleFileLinkClick}
            onContextMenu={handleFileLinkContextMenu}
          />
        );
      }
      const hrefFilePath = resolveHrefFilePath(url);
      if (hrefFilePath) {
        const formattedHrefFilePath = formatParsedFileLocation(hrefFilePath);
        const clickHandler = (event: React.MouseEvent) =>
          handleFileLinkClick(event, hrefFilePath);
        const contextMenuHandler = onOpenFileLinkMenu
          ? (event: React.MouseEvent) => handleFileLinkContextMenu(event, hrefFilePath)
          : undefined;
        return (
          <a
            href={href ?? toFileLink(hrefFilePath)}
            title={formattedHrefFilePath}
            onClick={clickHandler}
            onContextMenu={contextMenuHandler}
          >
            {children}
          </a>
        );
      }
      const isExternal =
        url.startsWith("http://") ||
        url.startsWith("https://") ||
        url.startsWith("mailto:");

      if (!isExternal) {
        if (url.startsWith("#")) {
          return <a href={href}>{children}</a>;
        }
        return (
          <a href={href} onClick={handleLocalLinkClick}>
            {children}
          </a>
        );
      }

      return (
        <a
          href={href}
          onClick={(event) => {
            event.preventDefault();
            event.stopPropagation();
            void openUrl(url);
          }}
        >
          {children}
        </a>
      );
    },
    code: ({ className: codeClassName, children }) => {
      if (codeClassName) {
        return <code className={codeClassName}>{children}</code>;
      }
      const text = String(children ?? "").trim();
      const fileTarget = parseInlineFileTarget(text);
      if (!fileTarget) {
        return <code>{children}</code>;
      }
      const href = toFileLink(fileTarget);
      return (
        <FileReferenceLink
          href={href}
          rawPath={fileTarget}
          showFilePath={showFilePath}
          workspacePath={workspacePath}
          onClick={handleFileLinkClick}
          onContextMenu={handleFileLinkContextMenu}
        />
      );
    },
  };

  const urlTransform = (url: string) => {
  const hasScheme = /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(url);
  // Keep file-like hrefs intact before scheme sanitization runs, otherwise
  // Windows absolute paths such as C:/repo/file.ts look like unknown schemes.
  if (resolveHrefFilePath(url)) {
    return url;
  }
  if (
    isFileLinkUrl(url) ||
    url.startsWith("http://") ||
    url.startsWith("https://") ||
    url.startsWith("mailto:") ||
    url.startsWith("#") ||
    url.startsWith("/") ||
    url.startsWith("./") ||
    url.startsWith("../")
  ) {
    return url;
  }
  if (!hasScheme) {
    return url;
  }
  return "";
  };
  return { components, urlTransform };
}
