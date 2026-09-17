import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import { useStore } from "../store";
import type { ColumnInfo, DdlStatement, SyncRunResult } from "../types";
import { RunResultPanel } from "./RunResultPanel";
import { SqlPreviewModal } from "./SqlPreviewModal";
import { IconArrowDown, IconArrowUp, IconPlus, IconTrash, IconWarn } from "./Icons";

interface Props {
  connectionId: string;
  namespace: string;
  table: string;
  onClose: () => void;
  onSaved: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

interface EditableColumn {
  /** Stable React key; the column name may be edited or duplicated mid-edit. */
  key: string;
  column: ColumnInfo;
  /** Original name for existing columns, so renames can be reported as such. */
  originalName: string | null;
}

const TYPE_SUGGESTIONS: Record<string, string[]> = {
  mysql: [
    "int",
    "bigint",
    "tinyint(1)",
    "smallint",
    "decimal(10,2)",
    "varchar(64)",
    "varchar(255)",
    "text",
    "longtext",
    "datetime",
    "timestamp",
    "date",
    "time",
    "json",
    "longblob",
  ],
  postgres: [
    "integer",
    "bigint",
    "smallint",
    "numeric(10,2)",
    "character varying(64)",
    "character varying(255)",
    "text",
    "boolean",
    "timestamp",
    "timestamptz",
    "date",
    "time",
    "jsonb",
    "bytea",
    "uuid",
  ],
};

let keySeq = 0;
const nextKey = () => `col-${++keySeq}`;

function blankColumn(ordinal: number): ColumnInfo {
  return {
    name: "",
    ordinal,
    dataType: "",
    fullType: "",
    nullable: true,
    default: null,
    isAutoIncrement: false,
    autoKind: null,
    isGenerated: false,
    generationExpression: null,
    generationKind: null,
    isUnsigned: false,
    comment: null,
    charset: null,
    collation: null,
    udtName: null,
  };
}

/**
 * Table designer.
 *
 * It edits the column list and nothing else: the backend compares the desired
 * columns with the live table and emits only column statements, so this dialog
 * can never create or drop the table itself.
 */
export function TableDesignerDialog({
  connectionId,
  namespace,
  table,
  onClose,
  onSaved,
  onLog,
}: Props) {
  const { connections } = useStore();
  const job = useJobProgress();
  const dialect = connections.find((c) => c.id === connectionId)?.dialect ?? "mysql";

  const [columns, setColumns] = useState<EditableColumn[]>([]);
  const [primaryKey, setPrimaryKey] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [preview, setPreview] = useState<DdlStatement[] | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [applying, setApplying] = useState(false);
  const [result, setResult] = useState<SyncRunResult | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const structure = await api.getTableStructure(connectionId, namespace, table);
        if (cancelled) return;
        setPrimaryKey(structure.primaryKey);
        setColumns(
          structure.columns.map((column) => ({
            key: nextKey(),
            column,
            originalName: column.name,
          })),
        );
      } catch (error) {
        if (!cancelled) setLoadError(errorMessage(error));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [connectionId, namespace, table]);

  const patch = useCallback((key: string, changes: Partial<ColumnInfo>) => {
    setColumns((prev) =>
      prev.map((entry) =>
        entry.key === key ? { ...entry, column: { ...entry.column, ...changes } } : entry,
      ),
    );
    setResult(null);
  }, []);

  const move = useCallback((index: number, delta: number) => {
    setColumns((prev) => {
      const next = [...prev];
      const target = index + delta;
      if (target < 0 || target >= next.length) return prev;
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
    setResult(null);
  }, []);

  const remove = useCallback((key: string) => {
    setColumns((prev) => prev.filter((entry) => entry.key !== key));
    setResult(null);
  }, []);

  const add = useCallback(() => {
    setColumns((prev) => [
      ...prev,
      { key: nextKey(), column: blankColumn(prev.length + 1), originalName: null },
    ]);
    setResult(null);
  }, []);

  const payload = useMemo(
    () => columns.map((entry, index) => ({ ...entry.column, ordinal: index + 1 })),
    [columns],
  );

  /// Renaming is reported explicitly: without it the backend would see a drop
  /// plus an add and the column's data would be lost.
  const renames = useMemo(
    () =>
      columns
        .filter(
          (entry) =>
            entry.originalName !== null &&
            entry.originalName !== entry.column.name.trim() &&
            entry.column.name.trim() !== "",
        )
        .map((entry) => ({
          from: entry.originalName as string,
          to: entry.column.name.trim(),
        })),
    [columns],
  );

  const localError = useMemo(() => {
    const names = payload.map((column) => column.name.trim().toLowerCase());
    if (payload.length === 0) return "表至少需要一个字段";
    if (names.some((name) => !name)) return "有字段没有填写名称";
    if (new Set(names).size !== names.length) return "字段名重复";
    if (payload.some((column) => !column.fullType.trim())) return "有字段没有填写类型";
    return null;
  }, [payload]);

  const doPreview = async () => {
    setPreviewing(true);
    try {
      const outcome = await api.previewAlterTable(
        connectionId,
        namespace,
        table,
        payload,
        renames,
      );
      setPreview(outcome.statements);
      setWarnings(outcome.warnings);
      for (const warning of outcome.warnings) onLog("warn", warning);
      onLog("info", `已生成 ${outcome.statements.length} 条字段变更语句`);
    } catch (error) {
      onLog("err", `生成 SQL 失败: ${errorMessage(error)}`);
    } finally {
      setPreviewing(false);
    }
  };

  const doApply = async () => {
    setApplying(true);
    setResult(null);
    const jobId = job.begin();
    try {
      const outcome = await api.applyAlterTable({
        connectionId,
        namespace,
        table,
        columns: payload,
        renames,
        jobId,
      });
      setResult(outcome);
      for (const warning of outcome.warnings ?? []) onLog("warn", warning);
      if (outcome.failed > 0) {
        onLog("err", outcome.message);
        for (const failure of outcome.failures.slice(0, 5)) {
          onLog("err", `${failure.sql} → ${failure.error}`);
        }
      } else {
        onLog("ok", outcome.message);
        onSaved();
      }
    } catch (error) {
      onLog("err", `字段修改失败: ${errorMessage(error)}`);
    } finally {
      job.end();
      setApplying(false);
    }
  };

  const suggestions = TYPE_SUGGESTIONS[dialect] ?? TYPE_SUGGESTIONS.mysql;
  const busy = applying || loading;

  return (
    <div className="overlay" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="modal" style={{ width: 1040, height: "82vh" }}>
        <div className="modal-head">
          <span>
            编辑字段 · <span className="mono">{table}</span>{" "}
            <span className="badge accent">{namespace}</span>
          </span>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span className="faint" style={{ fontWeight: 400 }}>
              行顺序即字段顺序（MySQL 会据此使用 AFTER 定位）
            </span>
            <button className="icon-btn" onClick={onClose} title="关闭">
              <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
                <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
              </svg>
            </button>
          </div>
        </div>

        <div className="modal-body" style={{ padding: 0, display: "flex", flexDirection: "column" }}>
          {loading && <div style={{ padding: 20 }}>正在读取表结构…</div>}
          {loadError && (
            <div style={{ padding: 16 }}>
              <div className="errbox">{loadError}</div>
            </div>
          )}

          {!loading && !loadError && (
            <>
              <div style={{ padding: "8px 12px", borderBottom: "1px solid var(--border)" }} className="row">
                <button className="btn small" onClick={add}>
                  <IconPlus size={11} /> 添加字段
                </button>
                <span className="faint">{columns.length} 个字段</span>
                {renames.length > 0 && (
                  <span className="badge purple">
                    重命名 {renames.map((rename) => `${rename.from}→${rename.to}`).join("、")}
                  </span>
                )}
                <div className="grow" />
                <button
                  className="btn small"
                  onClick={() => void doPreview()}
                  disabled={previewing || busy || localError !== null}
                >
                  {previewing ? "生成中…" : "预览 SQL"}
                </button>
                <button
                  className="btn primary small"
                  onClick={() => void doApply()}
                  disabled={applying || localError !== null}
                >
                  {applying ? "应用中…" : "应用"}
                </button>
              </div>

              {localError && (
                <div style={{ padding: "8px 12px" }}>
                  <div className="warnbox">{localError}</div>
                </div>
              )}

              <div style={{ overflow: "auto", flex: 1 }}>
                <table className="grid designer">
                  <thead>
                    <tr>
                      <th className="rowno">#</th>
                      <th style={{ width: 180 }}>字段名</th>
                      <th style={{ width: 190 }}>类型</th>
                      <th style={{ width: 56 }}>可空</th>
                      <th style={{ width: 150 }}>默认值</th>
                      <th style={{ width: 150 }}>注释</th>
                      <th style={{ width: 52 }}>自增</th>
                      <th style={{ width: 96 }}>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {columns.map((entry, index) => {
                      const column = entry.column;
                      const isPrimary = primaryKey.includes(column.name);
                      return (
                        <tr key={entry.key}>
                          <td className="rowno">{index + 1}</td>
                          <td>
                            <input
                              type="text"
                              value={column.name}
                              onChange={(event) => patch(entry.key, { name: event.target.value })}
                              placeholder="字段名"
                              style={{ width: "100%" }}
                            />
                          </td>
                          <td>
                            <input
                              type="text"
                              list="datasync-column-types"
                              value={column.fullType}
                              disabled={column.isGenerated}
                              title={column.isGenerated ? "生成列的类型由表达式决定" : undefined}
                              onChange={(event) =>
                                patch(entry.key, { fullType: event.target.value })
                              }
                              placeholder="类型"
                              style={{ width: "100%" }}
                            />
                          </td>
                          <td style={{ textAlign: "center" }}>
                            <input
                              type="checkbox"
                              checked={column.nullable}
                              disabled={column.isGenerated || isPrimary}
                              title={
                                isPrimary
                                  ? "主键字段必须非空"
                                  : column.isGenerated
                                    ? "生成列由表达式决定"
                                    : undefined
                              }
                              onChange={(event) =>
                                patch(entry.key, { nullable: event.target.checked })
                              }
                            />
                          </td>
                          <td>
                            <input
                              type="text"
                              value={column.default ?? ""}
                              disabled={column.isGenerated}
                              onChange={(event) =>
                                patch(entry.key, { default: event.target.value || null })
                              }
                              placeholder="默认值"
                              style={{ width: "100%" }}
                            />
                          </td>
                          <td>
                            <input
                              type="text"
                              value={column.comment ?? ""}
                              onChange={(event) =>
                                patch(entry.key, { comment: event.target.value || null })
                              }
                              placeholder="注释"
                              style={{ width: "100%" }}
                            />
                          </td>
                          <td style={{ textAlign: "center" }}>
                            <input
                              type="checkbox"
                              checked={column.isAutoIncrement}
                              disabled={column.isGenerated || !isPrimary}
                              title={isPrimary ? undefined : "自增需要该字段是主键"}
                              onChange={(event) =>
                                patch(entry.key, {
                                  isAutoIncrement: event.target.checked,
                                  autoKind: event.target.checked ? "auto_increment" : null,
                                })
                              }
                            />
                          </td>
                          <td>
                            <div style={{ display: "flex", gap: 2, alignItems: "center" }}>
                              {isPrimary && <span className="badge">PK</span>}
                              {column.isGenerated && (
                                <span className="badge purple" title={column.generationExpression ?? ""}>
                                  生成
                                </span>
                              )}
                              <button
                                className="icon-btn"
                                title="上移"
                                disabled={index === 0}
                                onClick={() => move(index, -1)}
                              >
                                <IconArrowUp size={11} />
                              </button>
                              <button
                                className="icon-btn"
                                title="下移"
                                disabled={index === columns.length - 1}
                                onClick={() => move(index, 1)}
                              >
                                <IconArrowDown size={11} />
                              </button>
                              <button
                                className="icon-btn"
                                title="删除该字段"
                                disabled={columns.length <= 1}
                                onClick={() => remove(entry.key)}
                              >
                                <IconTrash size={11} />
                              </button>
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
                <datalist id="datasync-column-types">
                  {suggestions.map((type) => (
                    <option key={type} value={type} />
                  ))}
                </datalist>
              </div>
            </>
          )}

          {(warnings.length > 0 || result) && (
            <div style={{ padding: "8px 12px", borderTop: "1px solid var(--border)" }}>
              {warnings.map((warning, index) => (
                <div key={index} className="warnbox" style={{ marginBottom: 6 }}>
                  <IconWarn size={11} /> {warning}
                </div>
              ))}
              {result && <RunResultPanel result={result} title="字段修改" />}
            </div>
          )}
        </div>

        <div className="modal-foot">
          <span className="faint">
            只影响字段定义；不会创建或删除表，也不会写入任何行数据
          </span>
          <div className="spacer" />
          <button className="btn" onClick={onClose} disabled={applying}>
            关闭
          </button>
        </div>

        {preview && (
          <SqlPreviewModal
            title="字段变更 SQL 预览"
            statements={preview.map((statement) => statement.sql)}
            onClose={() => setPreview(null)}
            onLog={onLog}
          />
        )}
      </div>
    </div>
  );
}
