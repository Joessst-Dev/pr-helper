/**
 * Converts Jira wiki markup (what the Jira REST v2 API returns for descriptions and comments)
 * into GitHub flavoured markdown for the `Markdown` component. Best effort: unknown macros are
 * dropped, their text is kept.
 *
 * Embedded images `!file.png!` become `![file.png](<file.png>)`, so the caller can resolve the
 * file name to a ticket attachment.
 */
export function jiraToMarkdown(input: string): string {
  const lines = input.replace(/\r\n?/g, "\n").split("\n");
  const out: Line[] = [];
  let code: { kind: string } | null = null;
  let quote = false;

  const emit: Emit = (text, kind, row) => out.push({ text, kind, cells: row?.cells, header: row?.header });
  const blankBefore = () => {
    if (out.length > 0 && out[out.length - 1].kind !== "blank") emit("", "blank");
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    if (code) {
      const close = line.search(new RegExp(`\\{${code.kind}\\}`, "i"));
      if (close === -1) {
        emit(line, "code");
        continue;
      }
      if (line.slice(0, close).trim()) emit(line.slice(0, close), "code");
      emit(FENCE, "code");
      const rest = line.slice(close + code.kind.length + 2);
      code = null;
      if (rest.trim()) lines.splice(i + 1, 0, rest);
      continue;
    }

    const open = /^\s*\{(code|noformat)(?::([^}]*))?\}(.*)$/i.exec(line);
    if (open) {
      const kind = open[1].toLowerCase();
      const lang = kind === "code" ? codeLanguage(open[2]) : "";
      blankBefore();
      emit(FENCE + lang, "code");
      code = { kind };
      if (open[3]) lines.splice(i + 1, 0, open[3]);
      continue;
    }

    if (/\{quote\}/i.test(line)) {
      const parts = line.split(/\{quote\}/i);
      parts.forEach((part, idx) => {
        if (idx > 0) {
          quote = !quote;
          if (!quote) blankBefore();
        }
        if (part.trim()) emitText(part, quote, emit, blankBefore);
      });
      continue;
    }

    emitText(line, quote, emit, blankBefore);
  }
  if (code) emit(FENCE, "code");

  // Jira keeps single line breaks; markdown needs a hard break for that.
  return fixTables(out)
    .map((l, idx, all) => {
      const next = all[idx + 1];
      return l.kind === "text" && next?.kind === "text" ? `${l.text}  ` : l.text;
    })
    .join("\n");
}

type LineKind = "text" | "blank" | "block" | "code" | "table";
type Emit = (text: string, kind: LineKind, row?: TableRow) => void;
type Line = { text: string; kind: LineKind; cells?: number; header?: boolean };

const FENCE = "~~~~";

/** `{code:java}` or `{code:title=x|language=java}` → `java`. */
function codeLanguage(params: string | undefined): string {
  if (!params) return "";
  for (const p of params.split("|")) {
    const [k, v] = p.split("=");
    if (v === undefined && /^[\w+#-]+$/.test(k.trim())) return k.trim().toLowerCase();
    if (k.trim().toLowerCase() === "language" && v && /^[\w+#-]+$/.test(v.trim())) return v.trim().toLowerCase();
  }
  return "";
}

function emitText(
  raw: string,
  quote: boolean,
  emit: Emit,
  blankBefore: () => void,
) {
  const prefix = quote ? "> " : "";
  const line = raw.replace(/\{(panel|color|section|column|expand)(?::[^}]*)?\}/gi, "");
  const trimmed = line.trim();

  if (!trimmed) {
    if (quote) emit(">", "block");
    else emit("", "blank");
    return;
  }

  if (/^-{4,}$/.test(trimmed)) {
    blankBefore();
    emit("---", "block");
    return;
  }

  const heading = /^h([1-6])\.\s*(.*)$/.exec(trimmed);
  if (heading) {
    emit(`${prefix}${"#".repeat(Number(heading[1]))} ${inline(heading[2])}`, "block");
    return;
  }

  const bq = /^bq\.\s*(.*)$/.exec(trimmed);
  if (bq) {
    emit(`> ${inline(bq[1])}`, "block");
    return;
  }

  const list = /^([*#]+|-)\s+(.*)$/.exec(trimmed);
  if (list) {
    const markers = list[1];
    let indent = "";
    for (const m of markers.slice(0, -1)) indent += m === "#" ? "   " : "  ";
    const bullet = markers.endsWith("#") ? "1." : "-";
    emit(`${prefix}${indent}${bullet} ${inline(list[2])}`, "block");
    return;
  }

  if (trimmed.startsWith("|")) {
    const row = tableRow(trimmed);
    // Tables inside quotes are rare; they are kept as plain rows there.
    if (quote) emit(`${prefix}${row.text}`, "block");
    else emit(row.text, "table", row);
    return;
  }

  // A literal ">" at the start would turn into a quote in markdown.
  const text = inline(trimmed).replace(/^>/, "\\>");
  emit(`${prefix}${text}`, quote ? "block" : "text");
}

type TableRow = { text: string; cells: number; header: boolean };

/**
 * GFM tables need a header row followed by a separator, and blank lines around them.
 * Jira tables may have no header row (`|a|b|` only), so an empty one is added.
 */
function fixTables(lines: Line[]): Line[] {
  const result: Line[] = [];
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    if (l.kind !== "table") {
      result.push(l);
      continue;
    }
    const run: Line[] = [];
    while (i < lines.length && lines[i].kind === "table") run.push(lines[i++]);
    i--;
    const width = Math.max(...run.map((r) => r.cells ?? 1));
    if (result.length > 0 && result[result.length - 1].kind !== "blank") result.push({ text: "", kind: "blank" });
    const sep = `|${" --- |".repeat(width)}`;
    if (run[0].header) {
      result.push({ text: run[0].text, kind: "block" }, { text: sep, kind: "block" });
      run.shift();
    } else {
      result.push({ text: `|${"  |".repeat(width)}`, kind: "block" }, { text: sep, kind: "block" });
    }
    for (const r of run) result.push({ text: r.text, kind: "block" });
    result.push({ text: "", kind: "blank" });
  }
  return result;
}

/** `||a||b||` (header) or `|a|b|` → `| a | b |`. */
function tableRow(line: string): TableRow {
  const header = line.startsWith("||");
  const tokens: string[] = [];
  const protectedLine = protect(line, tokens);
  const cells = protectedLine
    .replace(header ? /^\|\|/ : /^\|/, "")
    .replace(header ? /\|\|\s*$/ : /\|\s*$/, "")
    .split(header ? "||" : "|")
    .map((c) => restore(transform(c.trim()), tokens).replace(/\|/g, "\\|"));
  return { text: `| ${cells.join(" | ")} |`, cells: cells.length, header };
}

/** Inline formatting of one line of text. */
function inline(text: string): string {
  const tokens: string[] = [];
  return restore(transform(protect(text, tokens)), tokens);
}

const TOKEN = "\u0000";

/** Replaces code, images, links and URLs with placeholders so later rules leave them alone. */
function protect(text: string, tokens: string[]): string {
  const keep = (md: string) => `${TOKEN}${tokens.push(md) - 1}${TOKEN}`;
  return (
    text
      // {{monospace}}
      .replace(/\{\{(.+?)\}\}/g, (_, c: string) => keep(inlineCode(c)))
      // !image.png! or !image.png|thumbnail! or !https://…!
      .replace(
        /!([^!\n|]+?\.(?:png|jpe?g|gif|webp|bmp|svg)|https?:\/\/[^!\s|]+)(?:\|[^!\n]*)?!/gi,
        (_, src: string) => keep(`![${escapeText(basename(src))}](<${src.replace(/[<>]/g, "")}>)`),
      )
      // [text|url], [url], [~user], [^attachment]
      .replace(/\[([^[\]\n]+)\]/g, (whole, body: string) => {
        const md = link(body);
        return md === null ? whole : keep(md);
      })
      // Bare URLs stay as they are (GFM autolinks them).
      .replace(/https?:\/\/[^\s<>\]]+/g, (url) => keep(url))
  );
}

function restore(text: string, tokens: string[]): string {
  return text.replace(new RegExp(`${TOKEN}(\\d+)${TOKEN}`, "g"), (_, i: string) => tokens[Number(i)]);
}

function transform(text: string): string {
  return (
    text
      // Raw HTML is dropped by the renderer, so keep "<" literal. "~" is GFM strikethrough.
      .replace(/[<~]/g, (c) => `\\${c}`)
      // *bold*
      .replace(/(^|[^\w*\\])\*(?=\S)([^*\n]*?\S)\*(?=$|[^\w*])/g, "$1**$2**")
      // -strikethrough-
      .replace(/(^|[\s(])-(?=[^\s-])([^\n]*?[^\s-])-(?=$|[\s.,;:!?)])/g, "$1~~$2~~")
      // +underline+ (no markdown equivalent)
      .replace(/(^|[\s(])\+(?=\S)([^+\n]*?\S)\+(?=$|[\s.,;:!?)])/g, "$1$2")
      // ??citation??
      .replace(/\?\?(?=\S)(.+?\S)\?\?/g, "*$1*")
      // ^superscript^ and ~subscript~ have no markdown equivalent; \\ is a line break.
      .replace(/\\\\/g, " ")
  );
  // _italic_ is the same in markdown.
}

function link(body: string): string | null {
  if (body.startsWith("~")) {
    const user = body.slice(1).replace(/^accountid:/i, "");
    return `@${escapeText(user.length > 24 ? "user" : user)}`;
  }
  if (body.startsWith("^")) return escapeText(body.slice(1));
  const sep = body.lastIndexOf("|");
  const label = sep === -1 ? null : body.slice(0, sep).trim();
  const target = (sep === -1 ? body : body.slice(sep + 1)).trim();
  if (/^(https?:\/\/|mailto:)/i.test(target)) {
    const url = target.replace(/[<>\s]/g, "");
    return label ? `[${escapeText(label)}](<${url}>)` : `<${url}>`;
  }
  // [ABC-1], [#anchor] and other targets: keep the label or the text.
  return label ? escapeText(label) : null;
}

function inlineCode(content: string): string {
  const longest = Math.max(0, ...(content.match(/`+/g) ?? []).map((m) => m.length));
  const ticks = "`".repeat(longest + 1);
  const pad = content.startsWith("`") || content.endsWith("`") ? " " : "";
  return `${ticks}${pad}${content}${pad}${ticks}`;
}

function escapeText(text: string): string {
  return text.replace(/([\\`*_[\]<>~|])/g, "\\$1");
}

function basename(src: string): string {
  return src.split(/[/\\]/).pop() ?? src;
}
