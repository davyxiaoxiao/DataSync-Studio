import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorMessage } from "../api";
import type {
  Cell,
  ColumnMeta,
  FilterGroup,
  RowKeyValue,
  SortSpec,
  TableData,
} from "../types";
import {
  FilterBuilder,
  countConditions,
  newDraftCondition,
  newDraftGroup,
  toWireGroup,
  type DraftGroup,
} from "../components/FilterBuilder";

interface Props {
  connectionId: string;
  namespace: string;
  table: string;
  onLog: (level: "info" | "ok" | "warn" | "err" | "sql", message: string) => void;
}

const PAGE_SIZE = 200;

const cellText = (cell: Cell | undefined): string => cell?.text ?? "";

export function TableDataView({ connectionId, namespace, table, onLog }: Props) {
  const [data, setData] = useState<TableData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [offset, setOffset] = useState(0);
  const [showSql, setShowSql] = useState(false);
  const [showFilters, setShowFilters] = useState(false);
  // Draft rows are only turned into a query when 查询 is pressed, so typing a
  // value does not fire a request per keystroke.
  const [draft, setDraft] = useState<DraftGroup>(() => newDraftGroup());
  const [filters, setFilters] = useState<FilterGroup | null>(null);
  const [sort, setSort] = useState<SortSpec[]>([]);
  const [editing, setEditing] = useState<{ row: number; column: number } | null>(null);
  const [editValue, setEditValue] = useState("");
  const [saving, setSaving] = useState(false);
  const [cellError, setCellError] = useState<string | null>(null);
  const requestSeq = useRef(0);

  const load = useCallback(
    async (nextOffset: number) => {
      const seq = ++requestSeq.current;
      setLoading(true);
      setError(null);
      try {
        const result = await api.previewTableData(
          connectionId,
          namespace,
          table,
          PAGE_SIZE,
          nextOffset,
          filters,
          sort,
        );
        if (seq !== requestSeq.current) return;
        setData(result);
        setOffset(nextOffset);
        onLog(
          "ok",
          `已读取 ${namespace}.${table} 共 ${result.rows.length} 行（${result.elapsedMs} ms）`,
        );
      } catch (err) {
        if (seq !== requestSeq.current) return;
        setError(errorMessage(err));
        onLog("err", `读取 ${namespace}.${table} 失败: ${errorMessage(err)}`);
      } finally {
        if (seq === requestSeq.current) setLoading(false);
      }
    },
    [connectionId, namespace, table, filters, sort, onLog],
  );

  useEffect(() => {
    void load(0);
  }, [load]);

  // An import or edit elsewhere invalidates what is on screen.
  useEffect(() => {
    const onRefresh = () => void load(offset);
    window.addEventListener("datasync:refresh-data", onRefresh);
    return () => window.removeEventListener("datasync:refresh-data", onRefresh);
  }, [load, offset]);

  const columns: ColumnMeta[] = data?.columns ?? [];
  const rows = data?.rows ?? [];
  const hasKey = (data?.primaryKey.length ?? 0) > 0;

  const openFilters = () => {
    setShowFilters(true);
    // A fresh tree starts with one empty row so there is something to type into.
    if (countConditions(draft) === 0 && draft.children.length === 0) {
      setDraft((prev) => ({
        ...prev,
        children: [newDraftCondition(columns[0]?.name ?? "")],
      }));
    }
  };

  const applyFilters = () => {
    setFilters(toWireGroup(draft));
    setEditing(null);
  };

  const clearFilters = () => {
    setDraft(newDraftGroup());
    setFilters(null);
    setEditing(null);
  };

  const toggleSort = (column: string) => {
    setEditing(null);
    setSort((prev) => {
      const current = prev.find((entry) => entry.column === column);
      if (!current) return [{ column, descending: false }];
      if (!current.descending) return [{ column, descending: true }];
      return [];
    });
  };

  const sortMark = (column: string) => {
    const current = sort.find((entry) => entry.column === column);
    if (!current) return "";
    return current.descending ? "▼" : "▲";
  };

  const startEdit = (rowIndex: number, cellIndex: number) => {
    if (!hasKey || saving || loading) return;
    const meta = columns[cellIndex];
    if (!meta || meta.generated) return;
    setCellError(null);
    setEditing({ row: rowIndex, column: cellIndex });
    setEditValue(cellText(rows[rowIndex]?.[cellIndex]));
  };

  const commitEdit = async () => {
    if (!editing || !data) return;
    const { row, column } = editing;
    const meta = columns[column];
    const original = rows[row]?.[column];
    if (!meta || !original) {
      setEditing(null);
      return;
    }
    const previous = original.text ?? null;
    const next = editValue;
    if ((previous ?? "") === next) {
      setEditing(null);
      return;
    }

    const key: RowKeyValue[] = data.primaryKey.map((name) => {
      const index = columns.findIndex((entry) => entry.name === name);
      return { column: name, value: rows[row]?.[index]?.text ?? null };
    });

    setSaving(true);
    try {
      const result = await api.updateTableCell({
        connectionId,
        namespace,
        table,
        key,
        column: meta.name,
        value: next === "" ? null : next,
        previous,
      });
      setEditing(null);
      if (!result.updated) {
        // The statement matched nothing, which means the row moved on.
        setCellError(result.message);
        onLog("warn", `${namespace}.${table}: ${result.message}`);
      } else {
        onLog("ok", `已修改 ${namespace}.${table}.${meta.name}`);
      }
      // Re-read rather than patching locally: the server's stored form (a
      // DECIMAL, a date, a cast) is the truth.
      await load(offset);
    } catch (err) {
      setCellError(errorMessage(err));
      onLog("err", `修改失败: ${errorMessage(err)}`);
      setEditing(null);
    } finally {
      setSaving(false);
    }
  };

  const filtered = filters !== null;
  const pageStart = rows.length === 0 ? 0 : offset + 1;
  const pageEnd = offset + rows.length;

  return (
    <div className="panel">
      <div className="section" style={{ padding: "8px 12px" }}>
        <div className="row">
          <span className="badge accent">{namespace}</span>
          <strong className="mono">{table}</strong>
          {hasKey ? (
            <span className="faint" title={data?.primaryKey.join("、")}>
              可编辑
            </span>
          ) : (
            <span className="faint" title="没有主键就无法定位要修改的行">
              只读（该表没有主键）
            </span>
          )}
          <div className="grow" />
          <button
            className="btn small"
            onClick={() => (showFilters ? setShowFilters(false) : openFilters())}
          >
            搜索条件{filters ? ` (${countConditions(draft)})` : ""}
          </button>
          <button className="btn small" onClick={() => setShowSql((value) => !value)}>
            {showSql ? "隐藏 SQL" : "查看 SQL"}
          </button>
          <button className="btn small" onClick={() => void load(offset)} disabled={loading}>
            刷新
          </button>
        </div>

        {showFilters && (
          <FilterBuilder
            columns={columns}
            value={draft}
            onChange={setDraft}
            onApply={applyFilters}
            onClear={clearFilters}
            busy={loading}
          />
        )}

        {showSql && data && (
          <textarea
            readOnly
            value={data.sql}
            rows={3}
            style={{ marginTop: 8, width: "100%" }}
          />
        )}
      </div>

      {(error || cellError) && (
        <div className="section" style={{ paddingTop: 8, paddingBottom: 8 }}>
          {error && <div className="errbox">{error}</div>}
          {cellError && (
            <div className="warnbox" style={{ marginTop: error ? 6 : 0 }}>
              {cellError}
            </div>
          )}
        </div>
      )}

      <div className="grid-wrap">
        <table className="grid">
          <thead>
            <tr>
              <th className="rowno">#</th>
              {columns.map((column) => (
                <th
                  key={column.name}
                  className="sortable"
                  title={`${column.name} : ${column.dataType}\n单击排序`}
                  onClick={() => toggleSort(column.name)}
                >
                  <span>
                    {column.primaryKey && (
                      <span className="pk" title="主键">
                        ●
                      </span>
                    )}
                    {column.name}
                    <span className="sortmark">{sortMark(column.name)}</span>
                  </span>
                  <span className="coltype">{column.dataType}</span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, rowIndex) => (
              <tr key={rowIndex}>
                <td className="rowno">{offset + rowIndex + 1}</td>
                {row.map((cell, cellIndex) => {
                  const meta = columns[cellIndex];
                  const isNull = cell.kind === "null" || cell.text == null;
                  const isEditing =
                    editing?.row === rowIndex && editing?.column === cellIndex;
                  const numeric =
                    meta?.kind === "int" ||
                    meta?.kind === "uint" ||
                    meta?.kind === "float" ||
                    meta?.kind === "decimal";
                  if (isEditing) {
                    return (
                      <td key={cellIndex} className="editing">
                        <input
                          type="text"
                          autoFocus
                          value={editValue}
                          disabled={saving}
                          onChange={(event) => setEditValue(event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") void commitEdit();
                            if (event.key === "Escape") {
                              setEditing(null);
                              setCellError(null);
                            }
                          }}
                          onBlur={() => void commitEdit()}
                        />
                      </td>
                    );
                  }
                  const editable = hasKey && !meta?.generated;
                  return (
                    <td
                      key={cellIndex}
                      className={`${isNull ? "null" : ""} ${numeric ? "num" : ""} ${
                        editable ? "editable" : ""
                      }`}
                      title={
                        meta?.generated
                          ? "生成列不可编辑"
                          : editable
                            ? `单击编辑 · ${cell.text ?? "NULL"}`
                            : "该表没有主键，无法编辑"
                      }
                      onClick={() => startEdit(rowIndex, cellIndex)}
                    >
                      {isNull ? "NULL" : cell.text}
                    </td>
                  );
                })}
              </tr>
            ))}
            {rows.length === 0 && !loading && (
              <tr>
                <td
                  colSpan={columns.length + 1}
                  style={{ textAlign: "center", color: "var(--text-faint)", height: 60 }}
                >
                  {filtered ? "没有符合条件的数据" : "没有数据"}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <div className="statusbar" style={{ borderTop: "1px solid var(--border)" }}>
        <span>
          {rows.length === 0 ? "0 行" : `第 ${pageStart} - ${pageEnd} 行`}
        </span>
        <span>·</span>
        <span>
          {data?.total != null ? `符合条件 ${data.total} 行` : `${rows.length} 行`}
        </span>
        <span>·</span>
        <span>{columns.length} 列</span>
        {sort.length > 0 && (
          <>
            <span>·</span>
            <span>
              排序 {sort[0].column} {sort[0].descending ? "降序" : "升序"}
            </span>
          </>
        )}
        {loading && <span>加载中…</span>}
        {saving && <span>保存中…</span>}
        <div className="spacer" />
        <button
          className="btn small"
          disabled={offset === 0 || loading}
          onClick={() => void load(Math.max(0, offset - PAGE_SIZE))}
        >
          上一页
        </button>
        <button
          className="btn small"
          disabled={!data?.hasMore || loading}
          onClick={() => void load(offset + PAGE_SIZE)}
        >
          下一页
        </button>
      </div>
    </div>
  );
}
