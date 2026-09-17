import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, errorMessage } from "../api";
import type { ColumnMeta, TableData } from "../types";

interface Props {
  connectionId: string;
  namespace: string;
  table: string;
  onLog: (level: "info" | "ok" | "warn" | "err" | "sql", message: string) => void;
}

const PAGE_SIZE = 200;

export function TableDataView({ connectionId, namespace, table, onLog }: Props) {
  const [data, setData] = useState<TableData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [offset, setOffset] = useState(0);
  const [filter, setFilter] = useState("");
  const [showSql, setShowSql] = useState(false);
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
    [connectionId, namespace, table, onLog],
  );

  useEffect(() => {
    void load(0);
  }, [load]);

  const columns: ColumnMeta[] = data?.columns ?? [];

  const rows = useMemo(() => {
    if (!data) return [];
    const keyword = filter.trim().toLowerCase();
    if (!keyword) return data.rows;
    return data.rows.filter((row) =>
      row.some((cell) => (cell.text ?? "").toLowerCase().includes(keyword)),
    );
  }, [data, filter]);

  return (
    <div className="panel">
      <div className="section" style={{ padding: "8px 12px" }}>
        <div className="row">
          <span className="badge accent">{namespace}</span>
          <strong className="mono">{table}</strong>
          <div className="grow" />
          <div style={{ position: "relative", display: "inline-flex", alignItems: "center" }}>
            <input
              type="text"
              placeholder="在结果中过滤…"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              style={{ width: 190, paddingLeft: 24 }}
            />
            <span
              style={{
                position: "absolute",
                left: 6,
                color: "var(--text-faint)",
                display: "inline-flex",
              }}
            >
              <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
                <circle cx="7" cy="7" r="4.2" />
                <path d="M10.2 10.2 14 14" strokeLinecap="round" />
              </svg>
            </span>
          </div>
          <button className="btn small" onClick={() => setShowSql((v) => !v)}>
            {showSql ? "隐藏 SQL" : "查看 SQL"}
          </button>
          <button className="btn small" onClick={() => void load(offset)} disabled={loading}>
            刷新
          </button>
        </div>
        {showSql && data && (
          <textarea
            readOnly
            value={data.sql}
            rows={3}
            style={{ marginTop: 8, width: "100%" }}
          />
        )}
      </div>

      {error && (
        <div className="section" style={{ paddingTop: 8, paddingBottom: 8 }}>
          <div className="errbox">{error}</div>
        </div>
      )}

      <div className="grid-wrap">
        <table className="grid">
          <thead>
            <tr>
              <th className="rowno">#</th>
              {columns.map((column) => (
                <th key={column.name} title={`${column.name} : ${column.dataType}`}>
                  <span>
                    {column.primaryKey && (
                      <span className="pk" title="主键">
                        ●
                      </span>
                    )}
                    {column.name}
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
                  return (
                    <td
                      key={cellIndex}
                      className={isNull ? "null" : meta?.kind === "int" || meta?.kind === "uint" || meta?.kind === "float" || meta?.kind === "decimal" ? "num" : ""}
                      title={cell.text ?? "NULL"}
                    >
                      {isNull ? "NULL" : cell.text}
                    </td>
                  );
                })}
              </tr>
            ))}
            {rows.length === 0 && !loading && (
              <tr>
                <td colSpan={columns.length + 1} style={{ textAlign: "center", color: "var(--text-faint)", height: 60 }}>
                  没有数据
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <div className="statusbar" style={{ borderTop: "1px solid var(--border)" }}>
        <span>
          第 {offset + 1} - {offset + rows.length} 行
          {filter && `（已过滤，共 ${data?.rows.length ?? 0} 行）`}
        </span>
        <span>·</span>
        <span>{columns.length} 列</span>
        {loading && <span>加载中…</span>}
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
