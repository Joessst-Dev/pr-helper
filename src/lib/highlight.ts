// Syntax highlighting with shiki's fine-grained core. The JavaScript regex engine needs no
// WASM and no eval, which fits the app's CSP. Grammars and themes are code-split and loaded on
// first use. Callers render the tokens as React elements: the code is PR author-controlled, so
// it must never go through innerHTML.
import type { HighlighterCore, LanguageInput } from "shiki/core";

export type Token = { content: string; style?: Record<string, string> };

/** Above this, files are shown as plain text: tokenizing stays fast and the UI responsive. */
export const MAX_HIGHLIGHT_LINES = 5000;
const MAX_HIGHLIGHT_CHARS = 400_000;

const LANGS: Record<string, () => Promise<{ default: LanguageInput }>> = {
  typescript: () => import("@shikijs/langs/typescript"),
  tsx: () => import("@shikijs/langs/tsx"),
  javascript: () => import("@shikijs/langs/javascript"),
  jsx: () => import("@shikijs/langs/jsx"),
  json: () => import("@shikijs/langs/json"),
  jsonc: () => import("@shikijs/langs/jsonc"),
  rust: () => import("@shikijs/langs/rust"),
  go: () => import("@shikijs/langs/go"),
  python: () => import("@shikijs/langs/python"),
  java: () => import("@shikijs/langs/java"),
  kotlin: () => import("@shikijs/langs/kotlin"),
  scala: () => import("@shikijs/langs/scala"),
  groovy: () => import("@shikijs/langs/groovy"),
  swift: () => import("@shikijs/langs/swift"),
  "objective-c": () => import("@shikijs/langs/objective-c"),
  c: () => import("@shikijs/langs/c"),
  cpp: () => import("@shikijs/langs/cpp"),
  csharp: () => import("@shikijs/langs/csharp"),
  ruby: () => import("@shikijs/langs/ruby"),
  php: () => import("@shikijs/langs/php"),
  perl: () => import("@shikijs/langs/perl"),
  dart: () => import("@shikijs/langs/dart"),
  elixir: () => import("@shikijs/langs/elixir"),
  haskell: () => import("@shikijs/langs/haskell"),
  clojure: () => import("@shikijs/langs/clojure"),
  lua: () => import("@shikijs/langs/lua"),
  r: () => import("@shikijs/langs/r"),
  zig: () => import("@shikijs/langs/zig"),
  nix: () => import("@shikijs/langs/nix"),
  shellscript: () => import("@shikijs/langs/shellscript"),
  yaml: () => import("@shikijs/langs/yaml"),
  toml: () => import("@shikijs/langs/toml"),
  ini: () => import("@shikijs/langs/ini"),
  xml: () => import("@shikijs/langs/xml"),
  html: () => import("@shikijs/langs/html"),
  css: () => import("@shikijs/langs/css"),
  scss: () => import("@shikijs/langs/scss"),
  vue: () => import("@shikijs/langs/vue"),
  svelte: () => import("@shikijs/langs/svelte"),
  markdown: () => import("@shikijs/langs/markdown"),
  sql: () => import("@shikijs/langs/sql"),
  graphql: () => import("@shikijs/langs/graphql"),
  proto: () => import("@shikijs/langs/proto"),
  hcl: () => import("@shikijs/langs/hcl"),
  terraform: () => import("@shikijs/langs/terraform"),
  docker: () => import("@shikijs/langs/docker"),
  make: () => import("@shikijs/langs/make"),
};

const EXTENSIONS: Record<string, string> = {
  ts: "typescript", mts: "typescript", cts: "typescript", tsx: "tsx",
  js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "jsx",
  json: "json", jsonc: "jsonc", json5: "jsonc",
  rs: "rust", go: "go", py: "python", pyi: "python",
  java: "java", kt: "kotlin", kts: "kotlin", scala: "scala", sc: "scala", groovy: "groovy", gradle: "groovy",
  swift: "swift", m: "objective-c", mm: "objective-c",
  c: "c", h: "c", cc: "cpp", cpp: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp", hxx: "cpp",
  cs: "csharp", rb: "ruby", rake: "ruby", php: "php", pl: "perl", pm: "perl",
  dart: "dart", ex: "elixir", exs: "elixir", hs: "haskell",
  clj: "clojure", cljs: "clojure", cljc: "clojure", edn: "clojure",
  lua: "lua", r: "r", zig: "zig", nix: "nix",
  sh: "shellscript", bash: "shellscript", zsh: "shellscript",
  yml: "yaml", yaml: "yaml", toml: "toml", ini: "ini", cfg: "ini", properties: "ini",
  xml: "xml", plist: "xml", svg: "xml", xsd: "xml", csproj: "xml",
  html: "html", htm: "html", css: "css", scss: "scss", vue: "vue", svelte: "svelte",
  md: "markdown", mdx: "markdown", sql: "sql", graphql: "graphql", gql: "graphql",
  proto: "proto", hcl: "hcl", tf: "terraform", tfvars: "terraform",
};

const FILENAMES: Record<string, string> = {
  dockerfile: "docker",
  makefile: "make",
  gnumakefile: "make",
  gemfile: "ruby",
  rakefile: "ruby",
  podfile: "ruby",
  jenkinsfile: "groovy",
  "cargo.lock": "toml",
  ".bashrc": "shellscript",
  ".zshrc": "shellscript",
};

/** The shiki language for a repo path, or `null` for plain text. */
export function languageFor(path: string): string | null {
  const name = path.split("/").pop()?.toLowerCase() ?? "";
  if (FILENAMES[name]) return FILENAMES[name];
  if (name.startsWith("dockerfile") || name.endsWith(".dockerfile")) return "docker";
  const dot = name.lastIndexOf(".");
  if (dot < 0) return null;
  return EXTENSIONS[name.slice(dot + 1)] ?? null;
}

let highlighter: Promise<HighlighterCore> | null = null;

function getHighlighter(): Promise<HighlighterCore> {
  highlighter ??= (async () => {
    const [{ createHighlighterCore }, { createJavaScriptRegexEngine }] = await Promise.all([
      import("shiki/core"),
      import("shiki/engine/javascript"),
    ]);
    return createHighlighterCore({
      themes: [import("@shikijs/themes/github-light"), import("@shikijs/themes/github-dark")],
      langs: [],
      // Skip the few patterns JS regexes can't express instead of failing the whole grammar.
      engine: createJavaScriptRegexEngine({ forgiving: true }),
    });
  })().catch((e) => {
    highlighter = null;
    throw e;
  });
  return highlighter;
}

/**
 * Tokens per line, or `null` when the file stays plain text (unknown language, too large, or
 * the highlighter failed). Colors come as `--shiki-light`/`--shiki-dark` custom properties;
 * the `.shiki-token` class in index.css picks one for the current color scheme.
 */
export async function tokenize(code: string, path: string): Promise<Token[][] | null> {
  const lang = languageFor(path);
  if (!lang || !LANGS[lang]) return null;
  if (code.length > MAX_HIGHLIGHT_CHARS || code.split("\n").length > MAX_HIGHLIGHT_LINES) return null;
  try {
    const hl = await getHighlighter();
    if (!hl.getLoadedLanguages().includes(lang)) {
      await hl.loadLanguage((await LANGS[lang]()).default);
    }
    const { tokens } = hl.codeToTokens(code, {
      lang,
      themes: { light: "github-light", dark: "github-dark" },
      defaultColor: false,
    });
    return tokens.map((line) => line.map((t) => ({ content: t.content, style: t.htmlStyle })));
  } catch {
    return null;
  }
}
