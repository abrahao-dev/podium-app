import type { ReactNode } from "react";

// Minimal markdown renderer producing React elements — safe by construction
// (no HTML injection), so agent-captured output can't XSS the webview.
// Supports: # headings, **bold**, *italic*, `code`, ``` blocks, - lists,
// [text](url), paragraphs.

function renderInline(text: string, keyPrefix: string): ReactNode[] {
  const out: ReactNode[] = [];
  const pattern =
    /(\*\*([^*]+)\*\*)|(\*([^*]+)\*)|(`([^`]+)`)|(\[([^\]]+)\]\((https?:\/\/[^)\s]+)\))/g;
  let last = 0;
  let match: RegExpExecArray | null;
  let i = 0;
  while ((match = pattern.exec(text)) !== null) {
    if (match.index > last) out.push(text.slice(last, match.index));
    const key = `${keyPrefix}-${i++}`;
    if (match[2] !== undefined) out.push(<strong key={key}>{match[2]}</strong>);
    else if (match[4] !== undefined) out.push(<em key={key}>{match[4]}</em>);
    else if (match[6] !== undefined) out.push(<code key={key}>{match[6]}</code>);
    else if (match[8] !== undefined)
      out.push(
        <a key={key} href={match[9]} target="_blank" rel="noreferrer">
          {match[8]}
        </a>,
      );
    last = match.index + match[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function renderMarkdown(source: string): ReactNode[] {
  const lines = source.split("\n");
  const blocks: ReactNode[] = [];
  let i = 0;
  let key = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (line.startsWith("```")) {
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i].startsWith("```")) {
        code.push(lines[i]);
        i += 1;
      }
      i += 1; // closing fence
      blocks.push(
        <pre key={key++}>
          <code>{code.join("\n")}</code>
        </pre>,
      );
      continue;
    }

    const heading = /^(#{1,3})\s+(.*)$/.exec(line);
    if (heading) {
      const level = heading[1].length;
      const content = renderInline(heading[2], `h${key}`);
      blocks.push(
        level === 1 ? (
          <h1 key={key++}>{content}</h1>
        ) : level === 2 ? (
          <h2 key={key++}>{content}</h2>
        ) : (
          <h3 key={key++}>{content}</h3>
        ),
      );
      i += 1;
      continue;
    }

    if (/^\s*-\s+/.test(line)) {
      const items: ReactNode[] = [];
      while (i < lines.length && /^\s*-\s+/.test(lines[i])) {
        items.push(
          <li key={`${key}-${items.length}`}>
            {renderInline(lines[i].replace(/^\s*-\s+/, ""), `li${key}-${items.length}`)}
          </li>,
        );
        i += 1;
      }
      blocks.push(<ul key={key++}>{items}</ul>);
      continue;
    }

    if (line.trim() === "") {
      i += 1;
      continue;
    }

    const para: string[] = [line];
    i += 1;
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !lines[i].startsWith("```") &&
      !/^(#{1,3})\s+/.test(lines[i]) &&
      !/^\s*-\s+/.test(lines[i])
    ) {
      para.push(lines[i]);
      i += 1;
    }
    blocks.push(<p key={key++}>{renderInline(para.join(" "), `p${key}`)}</p>);
  }

  return blocks;
}
