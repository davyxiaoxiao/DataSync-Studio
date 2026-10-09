import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CompletionCatalog } from "../types";

/**
 * A `<textarea>` with SQL completion.
 *
 * The completion is deliberately conservative: it offers names that actually
 * exist (tables, views, columns, databases) plus keywords, and it never rewrites
 * anything the user did not accept.
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
  "CURRENT_DATE", "CURRENT_TIMESTAMP", "NOW", "TRUE", "FALSE",
];

/** Extra words worth having in each dialect. */
const DIALECT_KEYWORDS: Record<string, string[]> = {
  postgres: ["ILIKE", "SERIAL", "BIGSERIAL", "JSONB", "ARRAY", "RETURNING", "LATERAL", "ON CONFLICT"],
  mysql: ["AUTO_INCREMENT", "ENGINE", "CHARSET", "COLLATE", "DUPLICATE", "SHOW", "DESCRIBE"],
};

type Kind = "keyword" | "table" | "view" | "column" | "database";

interface Suggestion {
  /** Text inserted when accepted. */
  insert: string;
  /** Text shown in the list. */
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
  readOnly?: boolean;
}

/** The word being typed immediately before the caret. */
function wordBefore(text: string, caret: number): { word: string; start: number } {
  let start = caret;
  while (start > 0 && /[A-Za-z0-9_$.\u4e00-\u9fff]/.test(text[start - 1])) {
    start -= 1;
  }
  return { word: text.slice(start, caret), start };
}

/** Where the caret sits on screen, measured with a mirror element. */
function caretPosition(textarea: HTMLTextAreaElement, index: number) {
  const style = window.getComputedStyle(textarea);
  const mirror = document.createElement("div");
  const copied = [
    "boxSizing", "width", "borderTopWidth", "borderRightWidth",
    "borderBottomWidth", "borderLeftWidth", "paddingTop", "paddingRight",
    "paddingBottom", "paddingLeft", "fontStyle", "fontVariant", "fontWeight",
    "fontStretch", "fontSize", "fontFamily", "lineHeight", "letterSpacing",
    "textTransform", "wordSpacing", "tabSize", "whiteSpace", "wordWrap",
  ];
  for (const property of copied) {
    const css = property.replace(/[A-Z]/g, (ch) => `-${ch.toLowerCase()}`);
    mirror.style.setProperty(css, style.getPropertyValue(css));
  }
  mirror.style.position = "absolute";
  mirror.style.top = "0";
  mirror.style.left = "0";
  mirror.style.visibility = "hidden";
  mirror.style.whiteSpace = "pre-wrap";
  mirror.style.wordWrap = "break-word";
  mirror.textContent = textarea.value.slice(0, index);
  const marker = document.createElement("span");
  // A character is needed for the marker to have a position at the very end.
  marker.textContent = textarea.value.slice(index) || ".";
  mirror.appendChild(marker);
  document.body.appendChild(mirror);

  const rect = textarea.getBoundingClientRect();
  const lineHeight = Number.parseFloat(style.lineHeight) || 16;
  const top = rect.top + marker.offsetTop - textarea.scrollTop + lineHeight;
  const left = rect.left + marker.offsetLeft - textarea.scrollLeft;
  document.body.removeChild(mirror);
  return { top, left };
}

export function SqlEditor({
  value,
  onChange,
  onRun,
  onSave,
  catalog,
  dialect,
  readOnly,
}: Props) {
  const area = useRef<HTMLTextAreaElement | null>(null);
  const [items, setItems] = useState<Suggestion[]>([]);
  const [active, setActive] = useState(0);
  const [anchor, setAnchor] = useState<{ top: number; left: number } | null>(null);
  const [prefix, setPrefix] = useState<{ word: string; start: number } | null>(null);

  const all = useMemo<Suggestion[]>(() => {
    const keywords = [...KEYWORDS, ...(DIALECT_KEYWORDS[dialect] ?? [])];
    const list: Suggestion[] = keywords.map((word) => ({
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
      // Offered both bare and qualified, which is how people write joins.
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
  }, [catalog, dialect]);

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
      // A `table.` prefix narrows to that table's columns.
      const dot = word.lastIndexOf(".");
      const needle = dot >= 0 ? word.slice(dot + 1) : word;
      if (!force && (needle.length === 0 || word.length === 0)) {
        close();
        return;
      }
      const scope = dot >= 0 ? word.slice(0, dot) : null;
      const lowered = needle.toLowerCase();
      const pool = scope
        ? all.filter((item) => item.insert.toLowerCase().startsWith(`${scope.toLowerCase()}.`))
        : all;
      const matches = pool
        .filter((item) => item.label.toLowerCase().startsWith(lowered))
        // Exact-prefix matches come first, then shorter names.
        .sort((left, right) => left.label.length - right.label.length)
        .slice(0, 12);
      if (matches.length === 0) {
        close();
        return;
      }
      setItems(matches);
      setActive(0);
      setPrefix({ word, start });
      setAnchor(caretPosition(element, start));
    },
    [all, close],
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
      // Put the caret right after the inserted text.
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

  // A popup left over from another tab would point at the wrong place.
  useEffect(() => {
    close();
  }, [close]);

  return (
    <div style={{ position: "relative" }}>
      <textarea
        ref={area}
        value={value}
        readOnly={readOnly}
        spellCheck={false}
        onChange={(event) => {
          onChange(event.target.value);
          // The caret is already at the new position by the time this runs.
          requestAnimationFrame(() => suggest(false));
        }}
        onKeyDown={onKeyDown}
        onClick={close}
        onBlur={() => {
          // A click on the list must land before the popup disappears.
          window.setTimeout(close, 150);
        }}
        placeholder="在这里写 SQL，⌘/Ctrl + Enter 执行，⌘/Ctrl + S 保存，⌘/Ctrl + 空格 唤出补全"
        style={{
          width: "100%",
          height: 200,
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
          fontSize: 12,
          lineHeight: 1.7,
          resize: "vertical",
        }}
      />

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
                // Keep focus in the textarea.
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
