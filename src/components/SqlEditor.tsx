import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import type { CompletionCatalog } from "../types";

/**
 * A `<textarea>` with SQL highlighting and completion.
 *
 * Highlighting uses the overlay technique: a `<pre>` renders the coloured text
 * behind a transparent `<textarea>` that owns the caret, selection and input.
 * The two must share every metric that affects layout — font, padding, border,
 * wrapping — or the colours drift away from the characters, which is why the
 * style object below is applied to both.
 */

const KEYWORDS = [
  "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "LIKE",
  "BETWEEN", "EXISTS", "CASE", "WHEN", "THEN", "ELSE", "END", "AS", "ON",
  "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "USING",
  "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET", "UNION", "ALL",
  "DISTINCT", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE",
  "ALTER", "DROP", "TABLE", "VIEW", "INDEX", "PRIMARY", "KEY", "FOREIGN",
  "REFERENCES", "UNIQUE", "DEFAULT", "CONSTRAINT", "CASCADE", "WITH",
  "RECURSIVE", "ASC", "DESC", "COUNT", "SUM", "AVG", "MIN", "MAX", "COALESCE",
  "CAST", "CONVERT", "TRUNCATE", "EXPLAIN", "BEGIN", "COMMIT", "ROLLBACK",
  "GRANT", "REVOKE", "IF", "REPLACE", "TEMPORARY", "RETURNING", "INTERVAL",
  "CURRENT_DATE", "CURRENT_TIMESTAMP", "NOW", "TRUE", "FALSE", "EXCEPT",
  "INTERSECT", "ANY", "SOME", "OVER", "PARTITION", "WINDOW", "FILTER",
];

const DIALECT_KEYWORDS: Record<string, string[]> = {
  postgres: ["ILIKE", "SERIAL", "BIGSERIAL", "JSONB", "ARRAY", "LATERAL", "ON CONFLICT", "DO", "NOTHING"],
  mysql: ["AUTO_INCREMENT", "ENGINE", "CHARSET", "COLLATE", "DUPLICATE", "SHOW", "DESCRIBE", "IGNORE"],
};

type TokenType =
  | "plain"
  | "identifier"
  | "keyword"
  | "string"
  | "comment"
  | "number"
  | "function";

interface Token {
  text: string;
  type: TokenType;
  /** Offset of the token's first character, for mapping the caret to a scope. */
  start: number;
}

interface Scan {
  tokens: Token[];
  /** Problems visible without asking the server. */
  issues: string[];
}

/** Tokenise SQL and flag the structural mistakes a single pass can see. */
export function scan(input: string, keywords: Set<string>): Scan {
  const tokens: Token[] = [];
  const issues: string[] = [];
  let buffer = "";
  let bufferType: TokenType = "plain";
  let depth = 0;

  let bufferStart = 0;
  const flush = () => {
    if (buffer) {
      tokens.push({ text: buffer, type: bufferType, start: bufferStart });
      buffer = "";
    }
  };
  const push = (text: string, type: TokenType) => {
    flush();
    tokens.push({ text, type, start: index });
  };

  let index = 0;
  while (index < input.length) {
    const char = input[index];
    const next = input[index + 1];

    // Line comment: runs to the end of the line, newline included so the
    // highlight layer keeps the same line count as the textarea.
    if (char === "-" && next === "-") {
      const end = input.indexOf("\n", index);
      const stop = end === -1 ? input.length : end;
      push(input.slice(index, stop), "comment");
      index = stop;
      continue;
    }
    if (char === "/" && next === "*") {
      const end = input.indexOf("*/", index + 2);
      if (end === -1) {
        issues.push("块注释 /* 没有闭合");
        push(input.slice(index), "comment");
        index = input.length;
        continue;
      }
      push(input.slice(index, end + 2), "comment");
      index = end + 2;
      continue;
    }
    if (char === "'" || char === '"' || char === "`") {
      // `''` is an escaped quote in every dialect here; a backslash escape is
      // accepted too because MySQL allows it.
      let cursor = index + 1;
      let closed = false;
      while (cursor < input.length) {
        if (input[cursor] === "\\" && char === "'") {
          cursor += 2;
          continue;
        }
        if (input[cursor] === char) {
          if (input[cursor + 1] === char) {
            cursor += 2;
            continue;
          }
          closed = true;
          cursor += 1;
          break;
        }
        cursor += 1;
      }
      if (!closed) {
        issues.push(`字符串 ${char} 没有闭合`);
        push(input.slice(index), "string");
        index = input.length;
        continue;
      }
      push(input.slice(index, cursor), "string");
      index = cursor;
      continue;
    }
    if (/[0-9]/.test(char) && !/[A-Za-z0-9_$]/.test(input[index - 1] ?? "")) {
      let cursor = index;
      while (cursor < input.length && /[0-9.eE+-]/.test(input[cursor])) {
        // `+`/`-` only continue a number right after an exponent.
        if (
          (input[cursor] === "+" || input[cursor] === "-") &&
          !/[eE]/.test(input[cursor - 1] ?? "")
        ) {
          break;
        }
        cursor += 1;
      }
      push(input.slice(index, cursor), "number");
      index = cursor;
      continue;
    }
    if (/[A-Za-z_$\u4e00-\u9fff]/.test(char)) {
      let cursor = index;
      while (cursor < input.length && /[A-Za-z0-9_$.\u4e00-\u9fff]/.test(input[cursor])) {
        cursor += 1;
      }
      const word = input.slice(index, cursor);
      const upper = word.toUpperCase();
      // A word followed by `(` reads as a function call, which colours better
      // than treating every name the same.
      const following = input.slice(cursor).match(/^\s*\(/);
      // An identifier gets its own type: the alias resolver has to tell a name
      // apart from punctuation, and `plain` covers both.
      const type: TokenType = keywords.has(upper)
        ? "keyword"
        : following
          ? "function"
          : "identifier";
      push(word, type);
      index = cursor;
      continue;
    }
    if (char === "(") depth += 1;
    if (char === ")") {
      depth -= 1;
      if (depth < 0) {
        issues.push("多余的右括号 )");
        depth = 0;
      }
    }
    if (buffer === "") bufferStart = index;
    bufferType = "plain";
    buffer += char;
    index += 1;
  }
  flush();
  if (depth > 0) issues.push(`有 ${depth} 个左括号 ( 没有闭合`);

  return { tokens, issues };
}

/**
 * Resolve the table aliases visible at a position.
 *
 * Scopes follow parentheses: a subquery can read the aliases of the query it is
 * nested in (that is how correlated references work), while two sibling
 * subqueries cannot see each other's. A derived table contributes the names from
 * its own select list, which is all a client can know without asking the server.
 */
export function visibleAliases(
  tokens: Token[],
  caret: number,
  columnsByTable: Map<string, { column: string; dataType: string }[]>,
): Map<string, { column: string; dataType: string }[]> {
  // Plain runs are split into single characters so punctuation can be matched
  // without caring how the tokenizer glued it together (`u) a, (` is one run).
  const flat: Token[] = [];
  for (const token of tokens) {
    if (token.type === "plain" && token.text.length > 1) {
      let offset = token.start;
      for (const char of token.text) {
        flat.push({ text: char, type: "plain", start: offset });
        offset += char.length;
      }
    } else {
      flat.push(token);
    }
  }

  // A stack of scopes: `(` pushes, `)` pops. Index juggling would break as soon
  // as scopes nest.
  const scopes: Map<string, { column: string; dataType: string }[]>[] = [new Map()];
  const current = () => scopes[scopes.length - 1];

  const skip = (from: number): number => {
    let index = from;
    while (flat[index] && flat[index].type === "plain" && flat[index].text.trim() === "") {
      index += 1;
    }
    return index;
  };

  /** An identifier, which may be schema-qualified. */
  const identifier = (index: number): { name: string; next: number } | null => {
    const token = flat[index];
    if (!token || (token.type !== "identifier" && token.type !== "keyword")) return null;
    const match = token.text.match(
      /^[A-Za-z_$\u4e00-\u9fff][A-Za-z0-9_$.\u4e00-\u9fff]*/,
    );
    if (!match) return null;
    return { name: match[0].replace(/\.$/, ""), next: index + 1 };
  };

  /** Columns a source exposes, and the alias it is known by. */
  const parseSource = (
    from: number,
  ): {
    names: string[];
    columns: { column: string; dataType: string }[];
    next: number;
  } | null => {
    let cursor = skip(from);
    if (flat[cursor]?.text === "(") {
      // A derived table: read its select list, then its alias.
      let depth = 0;
      let end = cursor;
      for (; end < flat.length; end += 1) {
        if (flat[end].text === "(") depth += 1;
        else if (flat[end].text === ")") {
          depth -= 1;
          if (depth === 0) break;
        }
      }
      const projection: { column: string; dataType: string }[] = [];
      let inList = false;
      for (let index = cursor + 1; index < end; index += 1) {
        const token = flat[index];
        if (token.type === "comment" || token.type === "string") continue;
        const upper = token.text.toUpperCase();
        if (upper === "SELECT") {
          inList = true;
          continue;
        }
        if (inList && (upper === "FROM" || upper === "WHERE")) break;
        if (!inList) continue;
        // Only names become columns; punctuation separates them.
        if (token.type === "identifier" && !KEYWORDS.includes(upper)) {
          // `u.id` in the subquery is the column `id` of the derived table.
          const bare = token.text.split(".").pop() ?? token.text;
          projection.push({ column: bare, dataType: "" });
        }
      }
      cursor = skip(end + 1);
      if (flat[cursor]?.text.toUpperCase() === "AS") cursor = skip(cursor + 1);
      const alias = identifier(cursor);
      if (!alias) return null;
      return { names: [alias.name], columns: projection, next: alias.next };
    }

    const first = identifier(cursor);
    if (!first) return null;
    // A schema-qualified name is tried whole, then by its last segment: the
    // catalog is keyed by table name alone.
    const table = first.name.split(".").pop() ?? first.name;
    const columns =
      columnsByTable.get(first.name.toLowerCase()) ??
      columnsByTable.get(table.toLowerCase()) ??
      [];
    cursor = skip(first.next);
    if (flat[cursor]?.text.toUpperCase() === "AS") cursor = skip(cursor + 1);
    const alias = identifier(cursor);
    // A bare keyword after the table is not an alias.
    if (alias && !KEYWORDS.includes(alias.name.toUpperCase())) {
      // Both work as a qualifier, so both are registered.
      return { names: [alias.name, table], columns, next: alias.next };
    }
    return { names: [first.name, table], columns, next: first.next };
  };

  for (let index = 0; index < flat.length; index += 1) {
    const token = flat[index];
    if (token.start >= caret) break;
    if (token.type === "comment" || token.type === "string") continue;

    if (token.type === "plain") {
      if (token.text === "(") scopes.push(new Map());
      else if (token.text === ")" && scopes.length > 1) scopes.pop();
      continue;
    }

    const upper = token.text.toUpperCase();
    if (upper !== "FROM" && upper !== "JOIN") continue;

    // A FROM clause lists sources separated by commas, each with its own alias.
    let cursor = index + 1;
    while (true) {
      const source = parseSource(cursor);
      if (!source) break;
      // A later source overrides an earlier one with the same name, which is
      // what SQL scoping does too.
      for (const name of new Set(source.names)) {
        if (name) current().set(name.toLowerCase(), source.columns);
      }
      cursor = source.next;
      const next = skip(cursor);
      if (flat[next]?.text !== ",") break;
      cursor = next + 1;
    }
    // Skip what this clause consumed; walking it again would count its
    // parentheses twice.
    index = cursor - 1;
  }

  const merged = new Map<string, { column: string; dataType: string }[]>();
  for (const scope of scopes) {
    for (const [alias, columns] of scope) merged.set(alias, columns);
  }
  return merged;
}

type Kind = "keyword" | "table" | "view" | "column" | "database";

interface Suggestion {
  insert: string;
  label: string;
  kind: Kind;
  detail?: string;
}

const KIND_LABEL: Record<Kind, string> = {
  keyword: "关键字",
  table: "表",
  view: "视图",
  column: "字段",
  database: "库",
};

interface Props {
  value: string;
  onChange: (next: string) => void;
  onRun: () => void;
  onSave: () => void;
  catalog: CompletionCatalog | null;
  dialect: string;
  height?: number;
}

function wordBefore(text: string, caret: number): { word: string; start: number } {
  let start = caret;
  while (start > 0 && /[A-Za-z0-9_$.\u4e00-\u9fff]/.test(text[start - 1])) {
    start -= 1;
  }
  return { word: text.slice(start, caret), start };
}

/** Where the caret sits on screen, measured with a mirror element. */
function caretPosition(area: HTMLTextAreaElement, index: number, style: CSSProperties) {
  const computed = window.getComputedStyle(area);
  const mirror = document.createElement("div");
  for (const property of [
    "fontFamily", "fontSize", "fontWeight", "lineHeight", "letterSpacing",
    "paddingTop", "paddingRight", "paddingBottom", "paddingLeft",
    "borderTopWidth", "borderRightWidth", "borderBottomWidth", "borderLeftWidth",
    "boxSizing", "textTransform", "tabSize",
  ]) {
    const css = property.replace(/[A-Z]/g, (ch) => `-${ch.toLowerCase()}`);
    mirror.style.setProperty(css, computed.getPropertyValue(css));
  }
  mirror.style.position = "absolute";
  mirror.style.top = "0";
  mirror.style.left = "0";
  mirror.style.visibility = "hidden";
  mirror.style.whiteSpace = "pre-wrap";
  mirror.style.wordWrap = "break-word";
  mirror.style.width = `${area.clientWidth}px`;
  mirror.textContent = area.value.slice(0, index);
  const marker = document.createElement("span");
  marker.textContent = area.value.slice(index) || ".";
  mirror.appendChild(marker);
  document.body.appendChild(mirror);

  const rect = area.getBoundingClientRect();
  const lineHeight = Number.parseFloat(computed.lineHeight) || 16;
  const top = rect.top + marker.offsetTop - area.scrollTop + lineHeight;
  const left = rect.left + marker.offsetLeft - area.scrollLeft;
  document.body.removeChild(mirror);
  void style;
  return { top, left };
}

export function SqlEditor({
  value,
  onChange,
  onRun,
  onSave,
  catalog,
  dialect,
  height = 200,
}: Props) {
  const area = useRef<HTMLTextAreaElement | null>(null);
  const highlight = useRef<HTMLPreElement | null>(null);
  const [items, setItems] = useState<Suggestion[]>([]);
  const [active, setActive] = useState(0);
  const [anchor, setAnchor] = useState<{ top: number; left: number } | null>(null);
  const [prefix, setPrefix] = useState<{ word: string; start: number } | null>(null);

  const keywords = useMemo(() => {
    const set = new Set<string>(KEYWORDS);
    for (const extra of DIALECT_KEYWORDS[dialect] ?? []) set.add(extra);
    return set;
  }, [dialect]);

  const { tokens, issues } = useMemo(() => scan(value, keywords), [value, keywords]);

  /** Column lookup by table name, used to resolve an alias to its columns. */
  const columnsByTable = useMemo(() => {
    const map = new Map<string, { column: string; dataType: string }[]>();
    for (const entry of catalog?.columns ?? []) {
      const key = entry.table.toLowerCase();
      const list = map.get(key) ?? [];
      list.push({ column: entry.column, dataType: entry.dataType });
      map.set(key, list);
    }
    return map;
  }, [catalog]);

  const all = useMemo<Suggestion[]>(() => {
    const list: Suggestion[] = [...keywords].map((word) => ({
      insert: word,
      label: word,
      kind: "keyword",
    }));
    for (const database of catalog?.databases ?? []) {
      list.push({ insert: database, label: database, kind: "database" });
    }
    for (const table of catalog?.tables ?? []) {
      list.push({ insert: table, label: table, kind: "table" });
    }
    for (const view of catalog?.views ?? []) {
      list.push({ insert: view, label: view, kind: "view" });
    }
    for (const column of catalog?.columns ?? []) {
      list.push({
        insert: column.column,
        label: column.column,
        kind: "column",
        detail: `${column.table}.${column.column} : ${column.dataType}`,
      });
      list.push({
        insert: `${column.table}.${column.column}`,
        label: `${column.table}.${column.column}`,
        kind: "column",
        detail: column.dataType,
      });
    }
    return list;
  }, [catalog, keywords]);

  const close = useCallback(() => {
    setItems([]);
    setAnchor(null);
    setPrefix(null);
  }, []);

  const suggest = useCallback(
    (force: boolean) => {
      const element = area.current;
      if (!element) return;
      const caret = element.selectionStart ?? 0;
      const { word, start } = wordBefore(element.value, caret);
      const dot = word.lastIndexOf(".");
      const needle = dot >= 0 ? word.slice(dot + 1) : word;
      if (!force && (needle.length === 0 || word.length === 0)) {
        close();
        return;
      }
      const scope = dot >= 0 ? word.slice(0, dot) : null;
      const lowered = needle.toLowerCase();
      let pool = all;
      if (scope) {
        // A qualifier resolves through the aliases in scope at the caret, so
        // `u.` works inside a subquery where `u` is the outer table's alias.
        const aliases = visibleAliases(tokens, caret, columnsByTable);
        const resolved =
          aliases.get(scope.toLowerCase()) ??
          columnsByTable.get(scope.toLowerCase()) ??
          null;
        if (resolved) {
          pool = resolved.map((entry) => ({
            insert: `${scope}.${entry.column}`,
            label: entry.column,
            kind: "column" as Kind,
            detail: entry.dataType,
          }));
        } else {
          pool = all.filter((item) =>
            item.insert.toLowerCase().startsWith(`${scope.toLowerCase()}.`),
          );
        }
      }
      const matches = pool
        .filter((item) => item.label.toLowerCase().startsWith(lowered))
        .sort((left, right) => left.label.length - right.label.length)
        .slice(0, 12);
      if (matches.length === 0) {
        close();
        return;
      }
      setItems(matches);
      setActive(0);
      setPrefix({ word, start });
      setAnchor(caretPosition(element, start, {}));
    },
    [all, close, columnsByTable, tokens],
  );

  const accept = useCallback(
    (suggestion: Suggestion) => {
      const element = area.current;
      if (!element || !prefix) return;
      const caret = element.selectionStart ?? 0;
      const next =
        element.value.slice(0, prefix.start) + suggestion.insert + element.value.slice(caret);
      onChange(next);
      close();
      const position = prefix.start + suggestion.insert.length;
      requestAnimationFrame(() => {
        element.focus();
        element.setSelectionRange(position, position);
      });
    },
    [close, onChange, prefix],
  );

  const onKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (items.length > 0) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActive((index) => (index + 1) % items.length);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        setActive((index) => (index - 1 + items.length) % items.length);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        accept(items[active]);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        close();
        return;
      }
    }
    if ((event.metaKey || event.ctrlKey) && event.key === " ") {
      event.preventDefault();
      suggest(true);
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
      event.preventDefault();
      onRun();
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      onSave();
    }
  };

  useEffect(() => {
    close();
  }, [close]);

  // The highlight layer follows the textarea's scroll, or the colours would sit
  // still while the text moves.
  const syncScroll = () => {
    const element = area.current;
    const layer = highlight.current;
    if (!element || !layer) return;
    layer.scrollTop = element.scrollTop;
    layer.scrollLeft = element.scrollLeft;
  };

  // Both layers must lay out identically, so they share one style object.
  const shared: CSSProperties = {
    margin: 0,
    padding: "6px 8px",
    border: "1px solid var(--border)",
    borderRadius: 6,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    fontSize: 12,
    lineHeight: 1.7,
    letterSpacing: "normal",
    tabSize: 4,
    whiteSpace: "pre-wrap",
    overflowWrap: "break-word",
    wordBreak: "break-word",
    boxSizing: "border-box",
    width: "100%",
    height,
  };

  // A trailing newline needs a character after it, or the layer is one line
  // shorter than the textarea.
  const painted = value.endsWith("\n") ? `${value} ` : value;

  return (
    <div style={{ position: "relative" }}>
      <div style={{ position: "relative", height }}>
        <pre
          ref={highlight}
          aria-hidden
          className="sql-highlight"
          style={{
            ...shared,
            position: "absolute",
            inset: 0,
            overflow: "hidden",
            pointerEvents: "none",
            background: "#fff",
          }}
        >
          {tokens.map((token, index) => (
            <span key={index} className={`tok-${token.type}`}>
              {token.text}
            </span>
          ))}
          {painted.length > value.length ? " " : null}
        </pre>
        <textarea
          ref={area}
          value={value}
          spellCheck={false}
          onChange={(event) => {
            onChange(event.target.value);
            requestAnimationFrame(() => {
              syncScroll();
              suggest(false);
            });
          }}
          onKeyDown={onKeyDown}
          onScroll={syncScroll}
          onClick={close}
          onBlur={() => window.setTimeout(close, 150)}
          placeholder="在这里写 SQL，⌘/Ctrl + Enter 执行，⌘/Ctrl + S 保存，⌘/Ctrl + 空格 唤出补全"
          style={{
            ...shared,
            position: "absolute",
            inset: 0,
            // The text itself is invisible; the layer behind shows the colours.
            color: "transparent",
            caretColor: "var(--text)",
            background: "transparent",
            resize: "none",
            overflow: "auto",
          }}
        />
      </div>

      {issues.length > 0 && (
        <div className="warnbox" style={{ marginTop: 6 }}>
          {issues.join("；")}
        </div>
      )}

      {items.length > 0 && anchor && (
        <div
          className="completion"
          style={{
            top: Math.min(anchor.top, window.innerHeight - 240),
            left: Math.min(anchor.left, window.innerWidth - 340),
          }}
        >
          {items.map((item, index) => (
            <div
              key={`${item.kind}-${item.insert}`}
              className={`completion-item ${index === active ? "active" : ""}`}
              onMouseEnter={() => setActive(index)}
              onMouseDown={(event) => {
                // Keep the focus in the textarea.
                event.preventDefault();
                accept(item);
              }}
            >
              <span className={`badge kind-${item.kind}`}>{KIND_LABEL[item.kind]}</span>
              <span className="mono">{item.label}</span>
              {item.detail && <span className="faint">{item.detail}</span>}
            </div>
          ))}
          <div className="completion-hint faint">
            ↑↓ 选择 · Enter/Tab 接受 · Esc 关闭
          </div>
        </div>
      )}
    </div>
  );
}
