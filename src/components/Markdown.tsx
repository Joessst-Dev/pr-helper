import type { ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { openUrl } from "@tauri-apps/plugin-opener";

/** Only web links are opened, and only in the system browser. */
export function safeHref(href: string | undefined): string | null {
  if (!href) return null;
  try {
    const url = new URL(href);
    return url.protocol === "https:" || url.protocol === "http:" ? url.href : null;
  } catch {
    return null;
  }
}

function ExternalLink({ href, children }: { href?: string; children?: ReactNode }) {
  const target = safeHref(href);
  if (!target) return <span className="text-fg">{children}</span>;
  return (
    <a
      href={target}
      title={target}
      className="text-accent hover:underline"
      onClick={(e) => {
        e.preventDefault();
        void openUrl(target);
      }}
    >
      {children}
    </a>
  );
}

const components: Components = {
  a: ({ href, children }) => <ExternalLink href={href}>{children}</ExternalLink>,
  // Remote images are never loaded (tracking pixels, IP leaks). Show a link instead.
  img: ({ src, alt }) => (
    <ExternalLink href={typeof src === "string" ? src : undefined}>[image{alt ? `: ${alt}` : ""}]</ExternalLink>
  ),
  h1: ({ children }) => <h3 className="mt-4 mb-2 text-base font-semibold first:mt-0">{children}</h3>,
  h2: ({ children }) => <h4 className="mt-4 mb-2 text-[15px] font-semibold first:mt-0">{children}</h4>,
  h3: ({ children }) => <h5 className="mt-3 mb-1.5 font-semibold first:mt-0">{children}</h5>,
  h4: ({ children }) => <h6 className="mt-3 mb-1 font-semibold first:mt-0">{children}</h6>,
  h5: ({ children }) => <h6 className="mt-3 mb-1 font-semibold first:mt-0">{children}</h6>,
  h6: ({ children }) => <h6 className="mt-3 mb-1 font-semibold first:mt-0">{children}</h6>,
  p: ({ children }) => <p className="my-2 first:mt-0 last:mb-0">{children}</p>,
  ul: ({ children, className }) => (
    <ul className={className?.includes("contains-task-list") ? "my-2 space-y-0.5" : "my-2 list-disc space-y-0.5 pl-5"}>
      {children}
    </ul>
  ),
  ol: ({ children }) => <ol className="my-2 list-decimal space-y-0.5 pl-5">{children}</ol>,
  blockquote: ({ children }) => <blockquote className="my-2 border-l-2 border-line pl-3 text-muted">{children}</blockquote>,
  code: ({ children, className }) =>
    className ? (
      <code className={className}>{children}</code>
    ) : (
      <code className="rounded bg-panel-2 px-1 py-0.5 font-mono text-[12px]">{children}</code>
    ),
  pre: ({ children }) => (
    <pre className="my-2 overflow-x-auto rounded-md bg-panel-2 p-3 font-mono text-[12px] leading-relaxed">{children}</pre>
  ),
  table: ({ children }) => (
    <div className="my-2 overflow-x-auto">
      <table className="border-collapse text-[13px]">{children}</table>
    </div>
  ),
  th: ({ children }) => <th className="border border-line bg-panel-2 px-2 py-1 text-left font-medium">{children}</th>,
  td: ({ children }) => <td className="border border-line px-2 py-1">{children}</td>,
  hr: () => <hr className="my-3 border-line" />,
};

/**
 * Renders author-controlled markdown (PR descriptions). Raw HTML is dropped, images are not
 * loaded and links open in the system browser, never in the app's webview.
 */
export function Markdown({ children }: { children: string }) {
  return (
    <div className="text-[13px] leading-relaxed break-words">
      <ReactMarkdown remarkPlugins={[remarkGfm]} skipHtml components={components}>
        {children}
      </ReactMarkdown>
    </div>
  );
}
