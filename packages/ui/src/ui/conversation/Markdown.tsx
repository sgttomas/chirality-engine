import { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { useStoreHandle } from "../../state/store";

/** The agent's text as Markdown; links open in the browser. */
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  const store = useStoreHandle();
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ href, children }) => (
            <a
              href={href}
              onClick={(e) => {
                e.preventDefault();
                if (href) void store.attempt(() => store.host.openUrl(href));
              }}
            >
              {children}
            </a>
          ),
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
