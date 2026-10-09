import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorMessage } from "../api";
import { InlineLoading, Loading, LoadingOverlay } from "../components/Loading";
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

/** Page sizes the grid offers; the server refuses anything above 1000. */
const PAGE_SIZES = [100, 200, 500, 1000];
const DEFAULT_PAGE_SIZE = 1000;

/** The row-number gutter is not resizable. */
const ROWNO_WIDTH = 52;
const MIN_WIDTH = 56;
const MAX_WIDTH = 720;

/**
 * Column widths are remembered per table for the session, so switching tabs and
 * coming back does not throw away an arrangement the user just made.
 */
interface RememberedWidths {
  /** The column set the widths belong to; a changed schema invalidates them. */
  columns: string;
  widths: Record<string, number>;
}

const widthCache = new Map<string, RememberedWidths>();

/** Roughly how wide a string renders, counting CJK as double. */
function displayWidth(text: string): number {
  let width = 0;
  for (const char of text) {
    width += /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe30-\ufe6f\uff00-\uff60\uffe0-\uffe6]/.test(
      char,
    )
      ? 2
      : 1;
  }
  return width;
}

/** An initial width guessed from the header and the first page of values. */
function measureColumn(column: ColumnMeta, index: number, rows: Cell[][]): number {
  let widest = displayWidth(column.name) + (column.primaryKey ? 2 : 0);
  for (const row of rows.slice(0, 60)) {
    const text = row[index]?.text;
    if (text) widest = Math.max(widest, Math.min(displayWidth(text), 60));
  }
  return Math.min(Math.max(Math.round(widest * 7.4 + 30), MIN_WIDTH), MAX_WIDTH);
}

const cellText = (cell: Cell | undefined): string => cell?.text ?? "";

export function TableDataView({ connectionId, namespace, table, onLog }: Props) {
  const [data, setData] = useState<TableData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [offset, setOffset] = useState(0);
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_SIZE);
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
  const [widths, setWidths] = useState<Record<string, number>>({});
  const [resizing, setResizing] = useState<{
    column: string;
    startX: number;
    startWidth: number;
  } | null>(null);
  const [tip, setTip] = useState<{
    column: ColumnMeta;
    /** The hovered cell's full value; absent when hovering the header. */
    value?: string;
    /**
     * True only for a real NULL. An empty string is a value too, and showing it
     * as NULL would be a lie.
     */
    isNull?: boolean;
    x: number;
    y: number;
  } | null>(null);
  const tipTimer = useRef<number | null>(null);
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
          pageSize,
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
    [connectionId, namespace, table, pageSize, filters, sort, onLog],
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
  const cacheKey = `${connectionId}::${namespace}::${table}`;

  // Widths are seeded from a measurement of the first page, unless the user has
  // already arranged this table.
  useEffect(() => {
    if (columns.length === 0) return;
    setWidths((previous) => {
      const remembered = widthCache.get(cacheKey);
      const names = columns.map((column) => column.name).join("|");
      if (remembered && remembered.columns === names) return remembered.widths;
      const measured: Record<string, number> = {};
      columns.forEach((column, index) => {
        measured[column.name] = previous[column.name] ?? measureColumn(column, index, rows);
      });
      return measured;
    });
    // Only when the column set changes; re-measuring on every page would fight
    // whatever the user has dragged.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cacheKey, columns.map((column) => column.name).join("|")]);

  // Persist on change so a tab switch keeps the arrangement.
  useEffect(() => {
    if (Object.keys(widths).length === 0) return;
    widthCache.set(cacheKey, {
      columns: columns.map((column) => column.name).join("|"),
      widths,
    });
  }, [cacheKey, widths]);

  useEffect(() => {
    if (!resizing) return;
    const onMove = (event: MouseEvent) => {
      const next = Math.min(
        Math.max(resizing.startWidth + (event.clientX - resizing.startX), MIN_WIDTH),
        MAX_WIDTH,
      );
      setWidths((previous) => ({ ...previous, [resizing.column]: next }));
    };
    const onUp = () => setResizing(null);
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [resizing]);

  // A tooltip pinned to a scrolled-away header would be misleading.
  useEffect(() => {
    if (!tip) return;
    const hide = () => setTip(null);
    window.addEventListener("scroll", hide, true);
    return () => window.removeEventListener("scroll", hide, true);
  }, [tip]);

  const columnWidth = (name: string) => widths[name] ?? 180;
  const tableWidth = ROWNO_WIDTH + columns.reduce((sum, column) => sum + columnWidth(column.name), 0);

  /// Show the column box near `target`.
  ///
  /// Hovering the header answers "what is this column"; hovering a cell answers
  /// the same thing plus "what does this cell actually contain", which matters
  /// because long values are ellipsised in the grid.
  const showTip = (
    column: ColumnMeta,
    target: HTMLElement,
    value?: string,
    isNull = false,
  ) => {
    if (tipTimer.current !== null) window.clearTimeout(tipTimer.current);
    // A short delay keeps the box from flashing while the pointer moves.
    tipTimer.current = window.setTimeout(() => {
      const rect = target.getBoundingClientRect();
      // Roughly the box's height, so it flips above when there is no room below.
      const estimated = 96 + (column.comment ? 30 : 0) + (value !== undefined ? 46 : 0);
      const below = rect.bottom + 6;
      const y =
        below + estimated > window.innerHeight - 8
          ? Math.max(rect.top - estimated - 6, 8)
          : below;
      setTip({
        column,
        value,
        isNull,
        x: Math.min(rect.left, Math.max(window.innerWidth - 330, 8)),
        y,
      });
    }, 220);
  };

  const hideTip = () => {
    if (tipTimer.current !== null) window.clearTimeout(tipTimer.current);
    tipTimer.current = null;
    setTip(null);
  };

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
    hideTip();
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
          <label className="row" style={{ gap: 4 }} title="每页最多 1000 行">
            <span className="faint">每页</span>
            <select
              value={String(pageSize)}
              disabled={loading}
              onChange={(event) => setPageSize(Number(event.target.value))}
            >
              {PAGE_SIZES.map((size) => (
                <option key={size} value={String(size)}>
                  {size}
                </option>
              ))}
            </select>
          </label>
          <button
            className="btn small"
            title="把列宽恢复为按内容自动测量"
            onClick={() => {
              const measured: Record<string, number> = {};
              columns.forEach((column, index) => {
                measured[column.name] = measureColumn(column, index, rows);
              });
              setWidths(measured);
            }}
          >
            重置列宽
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
        {/* Rows already on screen stay visible but dimmed, so a re-query does
            not blank out what the user was reading. */}
        {loading && rows.length > 0 && (
          <LoadingOverlay
            label={offset === 0 ? "正在重新读取…" : "正在翻页…"}
            detail={`${namespace}.${table}`}
          />
        )}
        <table
          className={`grid resizable${resizing ? " resizing" : ""}`}
          style={{ width: tableWidth, tableLayout: "fixed" }}
        >
          <colgroup>
            <col style={{ width: ROWNO_WIDTH }} />
            {columns.map((column) => (
              <col key={column.name} style={{ width: columnWidth(column.name) }} />
            ))}
          </colgroup>
          <thead>
            <tr>
              <th className="rowno">#</th>
              {columns.map((column) => (
                <th
                  key={column.name}
                  className="sortable"
                  onClick={() => toggleSort(column.name)}
                  onMouseEnter={(event) => showTip(column, event.currentTarget)}
                  onMouseLeave={hideTip}
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
                  <span
                    className="col-resizer"
                    title="拖动调整列宽，双击恢复自动宽度"
                    onClick={(event) => event.stopPropagation()}
                    onMouseDown={(event) => {
                      event.preventDefault();
                      event.stopPropagation();
                      hideTip();
                      setResizing({
                        column: column.name,
                        startX: event.clientX,
                        startWidth: columnWidth(column.name),
                      });
                    }}
                    onDoubleClick={(event) => {
                      event.stopPropagation();
                      const index = columns.findIndex((entry) => entry.name === column.name);
                      setWidths((previous) => ({
                        ...previous,
                        [column.name]: measureColumn(column, index, rows),
                      }));
                    }}
                  />
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
                      // The browser tooltip is gone: the column box carries the
                      // name, the type, the comment and the full value.
                      onMouseEnter={(event) => {
                        if (meta) {
                          showTip(meta, event.currentTarget, cellText(cell), isNull);
                        }
                      }}
                      onMouseLeave={hideTip}
                      onClick={() => startEdit(rowIndex, cellIndex)}
                    >
                      {isNull ? "NULL" : cell.text}
                    </td>
                  );
                })}
              </tr>
            ))}
            {loading && rows.length === 0 && (
              <Loading
                label="正在读取表数据…"
                detail={`${namespace}.${table}`}
              />
            )}
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

      {tip && (
        <div className="col-tip" style={{ left: tip.x, top: tip.y }} onMouseEnter={hideTip}>
          <div className="row" style={{ gap: 6 }}>
            <strong className="mono">{tip.column.name}</strong>
            {tip.column.primaryKey && <span className="badge">主键</span>}
            {!tip.column.nullable && <span className="badge">非空</span>}
            {tip.column.generated && (
              <span className="badge purple" title="由表达式计算得出">
                生成列
              </span>
            )}
          </div>
          <div className="faint mono" style={{ marginTop: 3, fontSize: 11 }}>
            {tip.column.dataType}
          </div>
          <div style={{ marginTop: 6 }}>
            {tip.column.comment ? (
              tip.column.comment
            ) : (
              <span className="faint">（该列没有备注）</span>
            )}
          </div>
          {tip.value !== undefined && (
            <>
              <div
                className="faint"
                style={{
                  marginTop: 8,
                  paddingTop: 6,
                  borderTop: "1px solid var(--border)",
                  fontSize: 11,
                }}
              >
                本格的值
              </div>
              <div className="mono col-tip-value">
                {tip.isNull ? (
                  <span className="faint">NULL</span>
                ) : tip.value === "" ? (
                  <span className="faint">（空字符串）</span>
                ) : (
                  tip.value
                )}
              </div>
            </>
          )}
        </div>
      )}

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
        {loading && <InlineLoading label="读取中…" />}
        {saving && <span>保存中…</span>}
        <div className="spacer" />
        <button
          className="btn small"
          disabled={offset === 0 || loading}
          onClick={() => void load(Math.max(0, offset - pageSize))}
        >
          上一页
        </button>
        <button
          className="btn small"
          disabled={!data?.hasMore || loading}
          onClick={() => void load(offset + pageSize)}
        >
          下一页
        </button>
      </div>
    </div>
  );
}
