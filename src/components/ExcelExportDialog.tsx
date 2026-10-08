import { useEffect, useMemo, useState } from "react";
import { save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import type { ColumnInfo, ExportOutcome } from "../types";
import type { TableRef } from "./Sidebar";
import { IconWarn } from "./Icons";

interface Props {
  target: TableRef;
  onClose: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

interface Row {
  key: string;
  column: string;
  dataType: string;
  selected: boolean;
  header: string;
}

let seq = 0;

/** Excel's own limits, mirrored so the form can refuse impossible input. */
const MAX_ROWS = 1_048_576;

/**
 * Excel export: choose the columns, rename their headers, write the file.
 *
 * The order of the rows in this dialog is the order of the columns in the sheet,
 * which is why they can be moved rather than only ticked.
 */
export function ExcelExportDialog({ target, onClose, onLog }: Props) {
  const job = useJobProgress();
  const [rows, setRows] = useState<Row[]>([]);
  const [loading, setLoading] = useState(true);
  const [total, setTotal] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sheetName, setSheetName] = useState(target.table);
  const [maxRows, setMaxRows] = useState("");
  const [includeHeader, setIncludeHeader] = useState(true);
  const [path, setPath] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<ExportOutcome | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const structure = await api.getTableStructure(
          target.connectionId,
          target.namespace,
          target.table,
        );
        if (cancelled) return;
        setRows(
          structure.columns.map((column: ColumnInfo) => ({
            key: `col-${++seq}`,
            column: column.name,
            dataType: column.dataType,
            selected: true,
            header: column.name,
          })),
        );
        // The count is advisory: knowing there are two million rows before
        // starting is the difference between patience and a surprise.
        const count = await api.countTableRows(
          target.connectionId,
          target.namespace,
          target.table,
        );
        if (!cancelled) setTotal(count);
      } catch (err) {
        if (!cancelled) setError(errorMessage(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [target.connectionId, target.namespace, target.table]);

  const selected = useMemo(() => rows.filter((row) => row.selected), [rows]);

  const patch = (key: string, changes: Partial<Row>) =>
    setRows((prev) => prev.map((row) => (row.key === key ? { ...row, ...changes } : row)));

  const move = (index: number, delta: number) =>
    setRows((prev) => {
      const next = [...prev];
      const to = index + delta;
      if (to < 0 || to >= next.length) return prev;
      [next[index], next[to]] = [next[to], next[index]];
      return next;
    });

  const parsedMax = maxRows.trim() === "" ? null : Number(maxRows);
  const maxInvalid =
    parsedMax !== null && (!Number.isFinite(parsedMax) || parsedMax < 0 || parsedMax > MAX_ROWS);

  const blocker =
    selected.length === 0
      ? "请至少选择一列"
      : maxInvalid
        ? `行数上限需在 0 到 ${MAX_ROWS.toLocaleString()} 之间`
        : null;

  const willTruncate =
    parsedMax !== null && parsedMax > 0 && total !== null && total > parsedMax;

  const pickPath = async () => {
    try {
      const chosen = await saveFileDialog({
        defaultPath: `${target.table}.xlsx`,
        filters: [{ name: "Excel 工作簿", extensions: ["xlsx"] }],
      });
      if (typeof chosen === "string") setPath(chosen);
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const run = async () => {
    if (blocker) return;
    let destination = path;
    if (!destination) {
      await pickPath();
      return;
    }
    setRunning(true);
    setError(null);
    setOutcome(null);
    const jobId = job.begin();
    try {
      const result = await api.exportExcel({
        connectionId: target.connectionId,
        namespace: target.namespace,
        table: target.table,
        path: destination,
        columns: selected.map((row) => ({
          column: row.column,
          header: row.header.trim() === "" ? null : row.header.trim(),
        })),
        options: {
          sheetName: sheetName.trim() === "" ? null : sheetName.trim(),
          maxRows: parsedMax && parsedMax > 0 ? parsedMax : null,
          includeHeader,
        },
        jobId,
      });
      setOutcome(result);
      for (const warning of result.warnings ?? []) onLog("warn", warning);
      onLog(
        "ok",
        `已导出 ${target.namespace}.${target.table} 共 ${result.rows} 行到 ${result.path}`,
      );
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `导出失败: ${errorMessage(err)}`);
    } finally {
      job.end();
      setRunning(false);
    }
  };

  return (
    <div
      className="overlay"
      onMouseDown={(event) => event.target === event.currentTarget && onClose()}
    >
      <div className="modal" style={{ width: 880, height: "80vh" }}>
        <div className="modal-head">
          <span>
            导出 Excel · <span className="mono">{target.table}</span>{" "}
            <span className="badge accent">{target.namespace}</span>
          </span>
          <button className="icon-btn" onClick={onClose} title="关闭">
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
              <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        <div className="modal-body" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
          {loading && <div>正在读取表结构…</div>}
          {error && <div className="errbox">{error}</div>}

          {/* ---- columns ------------------------------------------------ */}
          {!loading && (
            <div style={{ border: "1px solid var(--border)", borderRadius: 7, overflow: "hidden" }}>
              <div
                className="row"
                style={{
                  padding: "6px 10px",
                  background: "var(--bg-subtle)",
                  borderBottom: "1px solid var(--border)",
                }}
              >
                <span>要导出的列</span>
                <span className="faint">
                  已选 {selected.length} / {rows.length}
                </span>
                <div className="grow" />
                <button
                  className="btn small"
                  disabled={running}
                  onClick={() =>
                    setRows((prev) => prev.map((row) => ({ ...row, selected: true })))
                  }
                >
                  全选
                </button>
                <button
                  className="btn small"
                  disabled={running}
                  onClick={() =>
                    setRows((prev) => prev.map((row) => ({ ...row, selected: false })))
                  }
                >
                  全不选
                </button>
                <button
                  className="btn small"
                  disabled={running}
                  title="把表头恢复成原始列名"
                  onClick={() =>
                    setRows((prev) => prev.map((row) => ({ ...row, header: row.column })))
                  }
                >
                  表头恢复默认
                </button>
              </div>
              <div style={{ maxHeight: 260, overflow: "auto" }}>
                <table className="grid designer">
                  <thead>
                    <tr>
                      <th style={{ width: 46 }}>导出</th>
                      <th style={{ width: 200 }}>列</th>
                      <th style={{ width: 170 }}>类型</th>
                      <th>表头名称</th>
                      <th style={{ width: 70 }}>顺序</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((row, index) => (
                      <tr key={row.key}>
                        <td style={{ textAlign: "center" }}>
                          <input
                            type="checkbox"
                            checked={row.selected}
                            disabled={running}
                            onChange={(event) =>
                              patch(row.key, { selected: event.target.checked })
                            }
                          />
                        </td>
                        <td className="mono">{row.column}</td>
                        <td className="faint mono">{row.dataType}</td>
                        <td>
                          <input
                            type="text"
                            value={row.header}
                            disabled={running}
                            placeholder={row.column}
                            style={{ width: "100%" }}
                            onChange={(event) =>
                              patch(row.key, { header: event.target.value })
                            }
                          />
                        </td>
                        <td style={{ textAlign: "center" }}>
                          <button
                            className="icon-btn"
                            title="上移"
                            disabled={running || index === 0}
                            onClick={() => move(index, -1)}
                          >
                            <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7">
                              <path d="M8 13V3.4M3.8 7.6 8 3.4l4.2 4.2" strokeLinecap="round" />
                            </svg>
                          </button>
                          <button
                            className="icon-btn"
                            title="下移"
                            disabled={running || index === rows.length - 1}
                            onClick={() => move(index, 1)}
                          >
                            <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7">
                              <path d="M8 3v9.6M3.8 8.4 8 12.6l4.2-4.2" strokeLinecap="round" />
                            </svg>
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* ---- options ------------------------------------------------ */}
          {!loading && (
            <div className="row" style={{ gap: 16, flexWrap: "wrap" }}>
              <label className="row" style={{ gap: 5 }}>
                <span className="faint">工作表名</span>
                <input
                  type="text"
                  value={sheetName}
                  disabled={running}
                  style={{ width: 160 }}
                  onChange={(event) => setSheetName(event.target.value)}
                />
              </label>
              <label className="row" style={{ gap: 5 }}>
                <span className="faint">行数上限</span>
                <input
                  type="text"
                  value={maxRows}
                  disabled={running}
                  placeholder="留空 = 全部"
                  style={{
                    width: 130,
                    borderColor: maxInvalid ? "var(--red)" : undefined,
                  }}
                  onChange={(event) => setMaxRows(event.target.value)}
                />
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={includeHeader}
                  disabled={running}
                  onChange={(event) => setIncludeHeader(event.target.checked)}
                />
                包含表头行
              </label>
              <span className="faint">
                {total === null
                  ? "行数未知"
                  : `该表共 ${total.toLocaleString()} 行，Excel 单表上限 ${MAX_ROWS.toLocaleString()} 行`}
              </span>
            </div>
          )}

          {willTruncate && (
            <div className="warnbox">
              <IconWarn size={11} /> 行数上限小于表内行数，只会导出前 {parsedMax?.toLocaleString()} 行
            </div>
          )}

          <div className="row" style={{ gap: 8 }}>
            <button className="btn small" onClick={() => void pickPath()} disabled={running}>
              选择保存位置…
            </button>
            <span className="mono faint" style={{ maxWidth: 560, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {path ?? "尚未选择保存位置"}
            </span>
          </div>

          {/* ---- result -------------------------------------------------- */}
          {outcome && (
            <div
              style={{
                border: "1px solid var(--border)",
                borderRadius: 7,
                padding: 12,
              }}
            >
              <div className="row" style={{ gap: 18, flexWrap: "wrap" }}>
                <span className="row" style={{ gap: 5 }}>
                  <span className="faint">导出</span>
                  <strong style={{ color: "var(--green)", fontSize: 15 }}>
                    {outcome.rows.toLocaleString()}
                  </strong>
                  <span className="faint">行</span>
                </span>
                <span className="row" style={{ gap: 5 }}>
                  <span className="faint">列</span>
                  <strong style={{ fontSize: 15 }}>{outcome.columns}</strong>
                </span>
                <span className="row" style={{ gap: 5 }}>
                  <span className="faint">工作表</span>
                  <strong className="mono">{outcome.sheet}</strong>
                </span>
                <span className="faint">{outcome.elapsedMs} ms</span>
              </div>
              <div className="mono faint" style={{ marginTop: 8, fontSize: 11, wordBreak: "break-all" }}>
                {outcome.path}
              </div>
              {outcome.warnings.map((warning, index) => (
                <div key={index} className="warnbox" style={{ marginTop: 8 }}>
                  <IconWarn size={11} /> {warning}
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="modal-foot">
          <span className="faint">{blocker ?? `将导出 ${selected.length} 列`}</span>
          <div className="spacer" />
          <button className="btn" onClick={onClose} disabled={running}>
            关闭
          </button>
          <button
            className="btn primary"
            onClick={() => void run()}
            disabled={running || blocker !== null}
          >
            {running ? "导出中…" : path ? "开始导出" : "选择位置并导出"}
          </button>
        </div>
      </div>
    </div>
  );
}
