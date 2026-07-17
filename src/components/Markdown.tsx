import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Render a note's markdown as formatted, read-only HTML — the "reading" half of the
 * SOAP editor's Edit ⇄ Preview toggle (§8.5). `react-markdown` escapes HTML by
 * default (it never injects raw markup), so a clinician-typed or model-emitted
 * `<script>` renders as literal text, not an element. GitHub-flavored markdown
 * (tables, task lists) is enabled via `remark-gfm`. Tailwind v4 resets element
 * styles, so headings/lists/emphasis are re-styled here for the dark note surface.
 */
export default function Markdown({ children }: { children: string }) {
  return (
    <div className="flex flex-col gap-3 text-sm leading-relaxed text-neutral-100">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => (
            <h1 className="text-base font-semibold text-neutral-100">{children}</h1>
          ),
          h2: ({ children }) => (
            <h2 className="mt-1 text-sm font-semibold uppercase tracking-wide text-teal-300">
              {children}
            </h2>
          ),
          h3: ({ children }) => (
            <h3 className="font-semibold text-neutral-200">{children}</h3>
          ),
          p: ({ children }) => <p className="text-neutral-200">{children}</p>,
          ul: ({ children }) => (
            <ul className="list-disc space-y-1 pl-5 text-neutral-200">{children}</ul>
          ),
          ol: ({ children }) => (
            <ol className="list-decimal space-y-1 pl-5 text-neutral-200">{children}</ol>
          ),
          li: ({ children }) => <li className="marker:text-neutral-500">{children}</li>,
          strong: ({ children }) => (
            <strong className="font-semibold text-neutral-100">{children}</strong>
          ),
          em: ({ children }) => <em className="italic">{children}</em>,
          a: ({ children }) => <span className="text-neutral-200">{children}</span>,
          hr: () => <hr className="border-neutral-800" />,
          table: ({ children }) => (
            <table className="w-full border-collapse text-left">{children}</table>
          ),
          th: ({ children }) => (
            <th className="border border-neutral-800 px-2 py-1 font-semibold">{children}</th>
          ),
          td: ({ children }) => (
            <td className="border border-neutral-800 px-2 py-1">{children}</td>
          ),
          code: ({ children }) => (
            <code className="rounded bg-neutral-800 px-1 py-0.5 text-xs">{children}</code>
          ),
        }}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}
