import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import { Loading } from "./Loading";
import { useStore } from "../store";
import type {
  ColumnInfo,
  DdlStatement,
  ForeignKeyInfo,
  IndexInfo,
  SyncRunResult,
} from "../types";
import {
  ForeignKeyList,
  newForeignKey,
  toWireForeignKeys,
  validateForeignKeys,
  fkKey,
  type EditableForeignKey,
} from "./ForeignKeyList";
import { RunResultPanel } from "./RunResultPanel";
import { SqlPreviewModal } from "./SqlPreviewModal";
import { IconArrowDown, IconArrowUp, IconPlus, IconTrash, IconWarn } from "./Icons";

interface Props {
  connectionId: string;
  namespace: string;
  table: string;
  /** `create` starts from an empty design instead of loading an existing table. */
  mode?: "edit" | "create";
  onClose: () => void;
  onSaved: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

/** One index row being edited; columns are picked from the desired column list. */
interface EditableIndex {
  key: string;
  name: string;
  columns: string[];
  isUnique: boolean;
  /** True for indexes that already exist on the server. */
  existing: boolean;
  /**
   * The index only exists because a foreign key needs it. MySQL creates it
   * automatically and refuses to drop it, so it is shown read-only.
   */
  fkBacked: boolean;
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

type Tab = "columns" | "indexes" | "foreignKeys";

/**
 * Table editor.
 *
 * It edits the column list and the secondary indexes, and nothing else: the
 * backend compares what is submitted with the live table and keeps only column
 * and index statements, so this dialog can never create or drop the table itself.
 */
export function TableEditorDialog({
  connectionId,
  namespace,
  table,
  mode = "edit",
  onClose,
  onSaved,
  onLog,
}: Props) {
  const creating = mode === "create";
  const { connections } = useStore();
  const job = useJobProgress();
  const dialect = connections.find((c) => c.id === connectionId)?.dialect ?? "mysql";

  const [tab, setTab] = useState<Tab>("columns");
  const [tableName, setTableName] = useState(table);
  const [columns, setColumns] = useState<EditableColumn[]>([]);
  const [indexes, setIndexes] = useState<EditableIndex[]>([]);
  const [foreignKeys, setForeignKeys] = useState<EditableForeignKey[]>([]);
  const [primaryKey, setPrimaryKey] = useState<string[]>([]);
  const [indexType, setIndexType] = useState<Map<string, string | null>>(new Map());
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
        if (creating) {
          // A new table starts with one column so there is something to fill in.
          setColumns([
            { key: nextKey(), column: blankColumn(1), originalName: null },
          ]);
          setPrimaryKey([]);
          setLoading(false);
          return;
        }
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
        setIndexes(
          structure.indexes.map((entry) => ({
            key: nextKey(),
            name: entry.name,
            columns: [...entry.columns],
            isUnique: entry.isUnique,
            existing: true,
            fkBacked: entry.requiredByForeignKey === true,
          })),
        );
        setIndexType(
          new Map(structure.indexes.map((entry) => [entry.name, entry.indexType ?? null])),
        );
        setForeignKeys(
          (structure.foreignKeys ?? []).map((entry: ForeignKeyInfo) => {
            const pairs = entry.columns.map((column, index) => ({
              key: fkKey(),
              column,
              refColumn: entry.refColumns[index] ?? "",
            }));
            return {
              key: fkKey(),
              name: entry.name,
              refSchema: entry.refSchema ?? namespace,
              refTable: entry.refTable,
              pairs: pairs.length > 0 ? pairs : [{ key: fkKey(), column: "", refColumn: "" }],
              onDelete: entry.onDelete || "NO ACTION",
              onUpdate: entry.onUpdate || "NO ACTION",
              existing: true,
            };
          }),
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
  }, [connectionId, namespace, table, creating]);

  const patch = useCallback((key: string, changes: Partial<ColumnInfo>) => {
    setColumns((prev) => {
      const target = prev.find((entry) => entry.key === key);
      const previousName = target?.column.name;
      const nextName = changes.name;
      // Renaming a column has to carry through to the indexes using it,
      // otherwise the plan would reference a column that no longer exists.
      if (previousName !== undefined && nextName !== undefined && previousName !== nextName) {
        setIndexes((entries) =>
          entries.map((entry) => ({
            ...entry,
            columns: entry.columns.map((column) =>
              column === previousName ? nextName : column,
            ),
          })),
        );
      }
      return prev.map((entry) =>
        entry.key === key ? { ...entry, column: { ...entry.column, ...changes } } : entry,
      );
    });
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

  const indexPayload: IndexInfo[] = useMemo(() => {
    const types = indexType;
    return indexes.map((entry) => ({
      name: entry.name.trim(),
      columns: entry.columns.filter((column) => column !== ""),
      isUnique: entry.isUnique,
      indexType: types.get(entry.name) ?? null,
    }));
  }, [indexes, indexType]);

  const columnNames = useMemo(
    () => columns.map((entry) => entry.column.name.trim()).filter((name) => name !== ""),
    [columns],
  );

  const foreignKeyPayload = useMemo(
    () => toWireForeignKeys(foreignKeys),
    [foreignKeys],
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
    if (creating && tableName.trim() === "") return "请填写表名";
    const names = payload.map((column) => column.name.trim().toLowerCase());
    if (payload.length === 0) return "表至少需要一个字段";
    if (names.some((name) => !name)) return "有字段没有填写名称";
    if (new Set(names).size !== names.length) return "字段名重复";
    if (payload.some((column) => !column.fullType.trim())) return "有字段没有填写类型";

    // Index problems are caught here too, so the user sees them while editing
    // rather than as a server error after pressing 应用.
    const indexNames = indexPayload.map((entry) => entry.name.toLowerCase());
    if (indexPayload.some((entry) => entry.name === "")) return "有索引没有填写名称";
    if (new Set(indexNames).size !== indexNames.length) return "索引名重复";
    if (indexPayload.some((entry) => entry.columns.length === 0))
      return "有索引没有选择任何字段";
    for (const entry of indexPayload) {
      for (const column of entry.columns) {
        if (!names.includes(column.trim().toLowerCase()) && !column.includes("(")) {
          return `索引 ${entry.name} 引用了不存在的字段 ${column}`;
        }
      }
    }
    if (indexPayload.some((entry) => primaryKey.includes(entry.name)))
      return "索引名不能与主键同名";
    return validateForeignKeys(foreignKeys, names.filter((name) => name !== ""));
  }, [payload, indexPayload, primaryKey, foreignKeys]);

  const doPreview = async () => {
    setPreviewing(true);
    try {
      const outcome = await api.previewAlterTable(
        connectionId,
        namespace,
        table,
        payload,
        renames,
        indexPayload,
        foreignKeyPayload,
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
        indexes: indexPayload,
        foreignKeys: foreignKeyPayload,
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
            编辑表 · <span className="mono">{table}</span>{" "}
            <span className="badge accent">{namespace}</span>
          </span>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <button className="icon-btn" onClick={onClose} title="关闭">
              <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
                <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
              </svg>
            </button>
          </div>
        </div>

        {creating && (
          <div className="section" style={{ padding: "8px 12px", borderBottom: "1px solid var(--border)" }}>
            <div className="row" style={{ gap: 8 }}>
              <span className="faint">表名</span>
              <input
                type="text"
                value={tableName}
                autoFocus
                placeholder="例如：orders"
                style={{ width: 240 }}
                onChange={(event) => setTableName(event.target.value)}
              />
              <span className="faint">模式 {namespace}</span>
            </div>
          </div>
        )}

        <div className="modal-tabs">
          {(
            [
              ["columns", `表字段 (${columns.length})`],
              ["indexes", `表索引 (${indexes.length})`],
              ["foreignKeys", `外键 (${foreignKeys.length})`],
            ] as [Tab, string][]
          ).map(([value, label]) => (
            <button
              key={value}
              className={`modal-tab ${tab === value ? "active" : ""}`}
              onClick={() => setTab(value)}
            >
              {label}
            </button>
          ))}
          <div className="grow" />
          <span className="faint" style={{ alignSelf: "center", paddingRight: 10 }}>
            {tab === "columns"
              ? "行顺序即字段顺序（MySQL 会据此使用 AFTER 定位）"
              : tab === "indexes"
                ? "索引列的顺序会影响查询能否命中"
                : "外键的两端字段数量必须一致"}
          </span>
        </div>

        <div className="modal-body" style={{ padding: 0, display: "flex", flexDirection: "column" }}>
          {loading && (
            <Loading
              label="正在读取表结构…"
              detail={`${namespace}.${table}`}
            />
          )}
          {loadError && (
            <div style={{ padding: 16 }}>
              <div className="errbox">{loadError}</div>
            </div>
          )}

          {!loading && !loadError && (
            <>
              <div style={{ padding: "8px 12px", borderBottom: "1px solid var(--border)" }} className="row">
                <button
                  className="btn small"
                  disabled={tab === "foreignKeys"}
                  onClick={
                    tab === "indexes"
                      ? () =>
                          setIndexes((prev) => [
                            ...prev,
                            {
                              key: `idx-${++keySeq}`,
                              name: `idx_${table}_${prev.length + 1}`,
                              columns: columnNames.slice(0, 1),
                              isUnique: false,
                              existing: false,
                              fkBacked: false,
                            },
                          ])
                      : add
                  }
                >
                  <IconPlus size={11} />{" "}
                  {creating || tab === "columns"
                    ? "添加字段"
                    : tab === "indexes"
                      ? "添加索引"
                      : "外键"}
                </button>
                {tab === "foreignKeys" && (
                  <button
                    className="btn small"
                    disabled={columnNames.length === 0}
                    onClick={() =>
                      setForeignKeys((prev) => [
                        ...prev,
                        newForeignKey(columnNames, namespace),
                      ])
                    }
                  >
                    <IconPlus size={11} /> 添加外键
                  </button>
                )}
                <span className="faint">
                  {tab === "columns"
                    ? `${columns.length} 个字段`
                    : tab === "indexes"
                      ? `${indexes.length} 个索引`
                      : `${foreignKeys.length} 个外键`}
                </span>
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

              {tab === "columns" && (
              <div style={{ overflow: "auto", flex: 1 }}>
                <table className="grid designer">
                  <thead>
                    <tr>
                      <th className="rowno">#</th>
                      <th style={{ width: 180 }}>字段名</th>
                      <th style={{ width: 190 }}>类型</th>
                      <th style={{ width: 46 }}>主键</th>
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
                              checked={primaryKey.includes(column.name)}
                              disabled={
                                !creating ||
                                column.isGenerated ||
                                column.name.trim() === ""
                              }
                              title={
                                creating
                                  ? "勾选作为主键"
                                  : "主键在建表时设定，之后可用结构同步调整"
                              }
                              onChange={(event) =>
                                setPrimaryKey((prev) =>
                                  event.target.checked
                                    ? [...prev, column.name]
                                    : prev.filter((name) => name !== column.name),
                                )
                              }
                            />
                          </td>
                          <td style={{ textAlign: "center" }}>
                            <input
                              type="checkbox"
                              checked={column.nullable}
                              disabled={
                                column.isGenerated ||
                                isPrimary ||
                                primaryKey.includes(column.name)
                              }
                              title={
                                isPrimary || primaryKey.includes(column.name)
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
              )}

              {tab === "indexes" && (
                <div style={{ overflow: "auto", flex: 1 }}>
                  {indexes.length === 0 && (
                    <div style={{ padding: 20 }} className="faint">
                      这张表还没有二级索引。索引能加速查询，但会拖慢写入。
                    </div>
                  )}
                  <table className="grid designer">
                    <thead>
                      <tr>
                        <th style={{ width: 220 }}>索引名</th>
                        <th>列（按顺序）</th>
                        <th style={{ width: 70 }}>唯一</th>
                        <th style={{ width: 70 }}>类型</th>
                        <th style={{ width: 60 }}>操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {primaryKey.length > 0 && (
                        <tr key="__pk">
                          <td className="mono">
                            {dialect === "mysql" ? "PRIMARY" : `${table}_pkey`}
                          </td>
                          <td className="faint mono">{primaryKey.join(", ")}</td>
                          <td style={{ textAlign: "center" }} className="faint">
                            是
                          </td>
                          <td className="faint">btree</td>
                          <td className="faint" style={{ textAlign: "center" }} title="主键请在上方「表字段」里调整">
                            主键
                          </td>
                        </tr>
                      )}
                      {indexes.map((entry) => (
                        <tr key={entry.key} className={entry.fkBacked ? "fk-index" : ""}>
                          <td>
                            <input
                              type="text"
                              value={entry.name}
                              placeholder="索引名"
                              title={
                                entry.fkBacked ? "该索引由外键自动创建，不能改名或删除" : undefined
                              }
                              disabled={entry.fkBacked}
                              style={{ width: "100%" }}
                              onChange={(event) =>
                                setIndexes((prev) =>
                                  prev.map((item) =>
                                    item.key === entry.key
                                      ? { ...item, name: event.target.value }
                                      : item,
                                  ),
                                )
                              }
                            />
                          </td>
                          <td>
                            <div className="row" style={{ flexWrap: "wrap", gap: 4 }}>
                              {entry.columns.map((column, columnIndex) => (
                                <span className="chip" key={columnIndex}>
                                  <select
                                    value={column}
                                    onChange={(event) =>
                                      setIndexes((prev) =>
                                        prev.map((item) =>
                                          item.key === entry.key
                                            ? {
                                                ...item,
                                                columns: item.columns.map((c, i) =>
                                                  i === columnIndex ? event.target.value : c,
                                                ),
                                              }
                                            : item,
                                        ),
                                      )
                                    }
                                  >
                                    {columnNames.map((name) => (
                                      <option key={name} value={name}>
                                        {name}
                                      </option>
                                    ))}
                                    {!columnNames.includes(column) && (
                                      <option value={column}>{column}</option>
                                    )}
                                  </select>
                                  <button
                                    className="icon-btn"
                                    title="移出该列"
                                    disabled={entry.fkBacked}
                                    onClick={() =>
                                      setIndexes((prev) =>
                                        prev.map((item) =>
                                          item.key === entry.key
                                            ? {
                                                ...item,
                                                columns: item.columns.filter(
                                                  (_, i) => i !== columnIndex,
                                                ),
                                              }
                                            : item,
                                        ),
                                      )
                                    }
                                  >
                                    <svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8">
                                      <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
                                    </svg>
                                  </button>
                                </span>
                              ))}
                              <button
                                className="btn small"
                                disabled={columnNames.length === 0 || entry.fkBacked}
                                title={entry.fkBacked ? "该索引由外键自动创建" : undefined}
                                onClick={() =>
                                  setIndexes((prev) =>
                                    prev.map((item) =>
                                      item.key === entry.key
                                        ? {
                                            ...item,
                                            columns: [
                                              ...item.columns,
                                              columnNames.find(
                                                (name) => !item.columns.includes(name),
                                              ) ?? columnNames[0],
                                            ],
                                          }
                                        : item,
                                    ),
                                  )
                                }
                              >
                                + 列
                              </button>
                            </div>
                          </td>
                          <td style={{ textAlign: "center" }}>
                            <input
                              type="checkbox"
                              checked={entry.isUnique}
                              disabled={entry.fkBacked}
                              onChange={(event) =>
                                setIndexes((prev) =>
                                  prev.map((item) =>
                                    item.key === entry.key
                                      ? { ...item, isUnique: event.target.checked }
                                      : item,
                                  ),
                                )
                              }
                            />
                          </td>
                          <td className="faint mono" style={{ fontSize: 11 }}>
                            {entry.existing ? (indexType.get(entry.name) ?? "btree") : "btree"}
                          </td>
                          <td style={{ textAlign: "center" }}>
                            {entry.fkBacked ? (
                              <span
                                className="badge"
                                title="外键需要这个索引，删除会失败；请先删除对应的外键"
                              >
                                外键
                              </span>
                            ) : (
                              <button
                                className="icon-btn"
                                title="删除该索引"
                                onClick={() =>
                                  setIndexes((prev) =>
                                    prev.filter((item) => item.key !== entry.key),
                                  )
                                }
                              >
                                <IconTrash size={11} />
                              </button>
                            )}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
              {tab === "foreignKeys" && (
                <div style={{ overflow: "auto", flex: 1 }}>
                  <ForeignKeyList
                    value={foreignKeys}
                    onChange={setForeignKeys}
                    columnNames={columnNames}
                    schema={namespace}
                    disabled={busy}
                  />
                </div>
              )}
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
