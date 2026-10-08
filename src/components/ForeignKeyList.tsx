import type { ForeignKeyInfo } from "../types";

/**
 * Foreign key editor.
 *
 * A key is a list of column pairs, so the two sides are edited together: the
 * engines require the same number of columns on each side, and letting them
 * drift apart only produces a server error later.
 */

export interface FkPair {
  key: string;
  column: string;
  refColumn: string;
}

export interface EditableForeignKey {
  key: string;
  name: string;
  refSchema: string;
  refTable: string;
  pairs: FkPair[];
  onDelete: string;
  onUpdate: string;
  /** True for keys that already exist on the server. */
  existing: boolean;
}

const ACTIONS = ["NO ACTION", "CASCADE", "SET NULL", "RESTRICT", "SET DEFAULT"];

let seq = 0;
export const fkKey = () => `fk-${++seq}`;

export function newForeignKey(columnNames: string[], schema: string): EditableForeignKey {
  return {
    key: fkKey(),
    name: `fk_${columnNames[0] ?? "column"}`,
    refSchema: schema,
    refTable: "",
    pairs: [
      {
        key: fkKey(),
        column: columnNames[0] ?? "",
        refColumn: "id",
      },
    ],
    onDelete: "NO ACTION",
    onUpdate: "NO ACTION",
    existing: false,
  };
}

export function toWireForeignKeys(keys: EditableForeignKey[]): ForeignKeyInfo[] {
  return keys.map((entry) => ({
    name: entry.name.trim(),
    columns: entry.pairs.map((pair) => pair.column).filter((column) => column !== ""),
    refSchema: entry.refSchema.trim() === "" ? null : entry.refSchema.trim(),
    refTable: entry.refTable.trim(),
    refColumns: entry.pairs.map((pair) => pair.refColumn.trim()),
    onUpdate: entry.onUpdate,
    onDelete: entry.onDelete,
  }));
}

/** Problems the user can fix while still in the dialog. */
export function validateForeignKeys(
  keys: EditableForeignKey[],
  columnNames: string[],
): string | null {
  const names = keys.map((entry) => entry.name.trim().toLowerCase());
  if (names.some((name) => name === "")) return "有外键没有填写名称";
  if (new Set(names).size !== names.length) return "外键名重复";
  for (const entry of keys) {
    if (entry.refTable.trim() === "") return `外键 ${entry.name} 没有填写引用表`;
    if (entry.pairs.length === 0) return `外键 ${entry.name} 没有字段对`;
    for (const pair of entry.pairs) {
      if (pair.column === "") return `外键 ${entry.name} 有未选择的本表字段`;
      if (!columnNames.includes(pair.column)) {
        return `外键 ${entry.name} 引用了本表不存在的字段 ${pair.column}`;
      }
      if (pair.refColumn.trim() === "") {
        return `外键 ${entry.name} 有未填写的引用字段`;
      }
    }
  }
  return null;
}

interface Props {
  value: EditableForeignKey[];
  onChange: (next: EditableForeignKey[]) => void;
  columnNames: string[];
  schema: string;
  disabled?: boolean;
}

export function ForeignKeyList({ value, onChange, columnNames, schema, disabled }: Props) {
  if (value.length === 0) {
    return (
      <div style={{ padding: 20 }} className="faint">
        这张表还没有外键。外键由数据库保证引用完整性，写入不合法的引用会被拒绝。
      </div>
    );
  }

  const patch = (key: string, changes: Partial<EditableForeignKey>) =>
    onChange(value.map((entry) => (entry.key === key ? { ...entry, ...changes } : entry)));

  return (
    <div style={{ padding: 10, display: "flex", flexDirection: "column", gap: 10 }}>
      {value.map((entry) => (
        <div
          key={entry.key}
          style={{
            border: "1px solid var(--border)",
            borderRadius: 7,
            padding: 10,
            background: entry.existing ? "var(--bg-subtle)" : "#fff",
          }}
        >
          <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
            <label className="row" style={{ gap: 5 }}>
              <span className="faint">名称</span>
              <input
                type="text"
                value={entry.name}
                disabled={disabled}
                style={{ width: 190 }}
                onChange={(event) => patch(entry.key, { name: event.target.value })}
              />
            </label>
            <label className="row" style={{ gap: 5 }}>
              <span className="faint">引用</span>
              <input
                type="text"
                value={entry.refSchema}
                disabled={disabled}
                placeholder="模式"
                title="留空表示与当前表同一个模式"
                style={{ width: 110 }}
                onChange={(event) => patch(entry.key, { refSchema: event.target.value })}
              />
              <span className="faint">.</span>
              <input
                type="text"
                value={entry.refTable}
                disabled={disabled}
                placeholder="引用表"
                style={{ width: 150 }}
                onChange={(event) => patch(entry.key, { refTable: event.target.value })}
              />
            </label>
            <div className="grow" />
            {entry.existing && <span className="badge">已有</span>}
            <button
              className="icon-btn"
              title="删除该外键"
              disabled={disabled}
              onClick={() => onChange(value.filter((item) => item.key !== entry.key))}
            >
              <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
                <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
              </svg>
            </button>
          </div>

          <div style={{ marginTop: 8 }}>
            {entry.pairs.map((pair, index) => (
              <div className="row" key={pair.key} style={{ gap: 6, marginBottom: 4 }}>
                <span className="faint" style={{ width: 52 }}>
                  {index === 0 ? "字段" : ""}
                </span>
                <select
                  value={pair.column}
                  disabled={disabled}
                  style={{ width: 170 }}
                  onChange={(event) =>
                    patch(entry.key, {
                      pairs: entry.pairs.map((item) =>
                        item.key === pair.key ? { ...item, column: event.target.value } : item,
                      ),
                    })
                  }
                >
                  {columnNames.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
                <span className="faint">→</span>
                <input
                  type="text"
                  value={pair.refColumn}
                  disabled={disabled}
                  placeholder="引用字段"
                  style={{ width: 170 }}
                  onChange={(event) =>
                    patch(entry.key, {
                      pairs: entry.pairs.map((item) =>
                        item.key === pair.key ? { ...item, refColumn: event.target.value } : item,
                      ),
                    })
                  }
                />
                <button
                  className="icon-btn"
                  title="删除该字段对"
                  disabled={disabled || entry.pairs.length <= 1}
                  onClick={() =>
                    patch(entry.key, {
                      pairs: entry.pairs.filter((item) => item.key !== pair.key),
                    })
                  }
                >
                  <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
                    <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
                  </svg>
                </button>
              </div>
            ))}
            <button
              className="btn small"
              disabled={disabled}
              onClick={() =>
                patch(entry.key, {
                  pairs: [
                    ...entry.pairs,
                    {
                      key: fkKey(),
                      column: columnNames.find(
                        (name) => !entry.pairs.some((pair) => pair.column === name),
                      ) ?? columnNames[0] ?? "",
                      refColumn: "",
                    },
                  ],
                })
              }
            >
              + 字段对
            </button>
          </div>

          <div className="row" style={{ gap: 14, marginTop: 8 }}>
            <label className="row" style={{ gap: 5 }}>
              <span className="faint">删除时</span>
              <select
                value={entry.onDelete}
                disabled={disabled}
                style={{ width: 120 }}
                onChange={(event) => patch(entry.key, { onDelete: event.target.value })}
              >
                {ACTIONS.map((action) => (
                  <option key={action} value={action}>
                    {action}
                  </option>
                ))}
              </select>
            </label>
            <label className="row" style={{ gap: 5 }}>
              <span className="faint">更新时</span>
              <select
                value={entry.onUpdate}
                disabled={disabled}
                style={{ width: 120 }}
                onChange={(event) => patch(entry.key, { onUpdate: event.target.value })}
              >
                {ACTIONS.map((action) => (
                  <option key={action} value={action}>
                    {action}
                  </option>
                ))}
              </select>
            </label>
            <span className="faint">
              引用 {entry.refSchema || schema}.{entry.refTable || "?"}
            </span>
          </div>
        </div>
      ))}
    </div>
  );
}
