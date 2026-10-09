import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import type {
  ColumnMapping,
  ExcelImportPlan,
  ImportOutcome,
  ImportStrategy,
} from "../types";
import type { TableRef } from "./Sidebar";
import { IconFile, IconWarn } from "./Icons";
import { JobProgress } from "./JobProgress";

interface Props {
  target: TableRef;
  onClose: () => void;
  onImported: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

/** Sentinel used by the mapping selects for "do not import this column". */
const UNMAPPED = -1;

const EXCEL_EXTENSIONS = ["xlsx", "xlsm", "xlsb", "xls", "ods"];

/**
 * Excel import: choose a workbook, map its columns onto the table's, import,
 * then read what happened.
 *
 * The whole plan is validated on the Rust side before anything is written, so
 * the dialog only has to present choices.
 */
export function ExcelImportDialog({ target, onClose, onImported, onLog }: Props) {
  const job = useJobProgress();
  const [path, setPath] = useState<string | null>(null);
  const [sheet, setSheet] = useState<string | null>(null);
  const [headerRow, setHeaderRow] = useState<number | null>(0);
  const [plan, setPlan] = useState<ExcelImportPlan | null>(null);
  const [mapping, setMapping] = useState<Map<string, number>>(new Map());
  const [loading, setLoading] = useState(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [skipInvalid, setSkipInvalid] = useState(true);
  const [useTransaction, setUseTransaction] = useState(true);
  const [strategy, setStrategy] = useState<ImportStrategy>("append");
  const [outcome, setOutcome] = useState<ImportOutcome | null>(null);

  const loadPlan = useCallback(
    async (file: string, nextSheet: string | null, nextHeader: number | null) => {
      setLoading(true);
      setError(null);
      setOutcome(null);
      try {
        const result = await api.planExcelImport(
          target.connectionId,
          target.namespace,
          target.table,
          file,
          nextSheet,
          nextHeader,
        );
        setPlan(result);
        setSheet(result.preview.sheet);
        // A new sheet or header changes the column set, so the mapping is
        // re-derived rather than carried over from a different shape.
        setMapping(
          new Map(result.suggested.map((entry) => [entry.column, entry.excelIndex])),
        );
      } catch (err) {
        setPlan(null);
        setError(errorMessage(err));
      } finally {
        setLoading(false);
      }
    },
    [target.connectionId, target.namespace, target.table],
  );

  const pickFile = async () => {
    try {
      const selected = await openFileDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "Excel 工作簿", extensions: EXCEL_EXTENSIONS }],
      });
      if (typeof selected !== "string") return;
      setPath(selected);
      setHeaderRow(0);
      await loadPlan(selected, null, 0);
      onLog("info", `已选择 ${selected}`);
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  // Selecting the file is the first thing the user has to do.
  useEffect(() => {
    if (!path) void pickFile();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const mapped = useMemo(() => {
    const entries: ColumnMapping[] = [];
    for (const column of plan?.columns ?? []) {
      const index = mapping.get(column.name);
      if (index !== undefined && index !== UNMAPPED) {
        entries.push({ column: column.name, excelIndex: index });
      }
    }
    return entries;
  }, [mapping, plan]);

  const unmappedRequired = useMemo(
    () =>
      (plan?.columns ?? []).filter(
        (column) =>
          !column.nullable &&
          (column.default === null || column.default === undefined) &&
          (mapping.get(column.name) ?? UNMAPPED) === UNMAPPED,
      ),
    [mapping, plan],
  );

  const duplicates = useMemo(() => {
    const seen = new Map<number, number>();
    const clashes = new Set<string>();
    for (const column of plan?.columns ?? []) {
      const index = mapping.get(column.name);
      if (index === undefined || index === UNMAPPED) continue;
      if (seen.has(index)) {
        clashes.add(column.name);
        clashes.add(
          (plan?.columns ?? []).find((c) => mapping.get(c.name) === index)?.name ?? "",
        );
      }
      seen.set(index, 1);
    }
    return clashes;
  }, [mapping, plan]);

  const blocker =
    !plan
      ? "请先选择 Excel 文件"
      : mapped.length === 0
        ? "请至少映射一个字段"
        : unmappedRequired.length > 0
          ? `字段 ${unmappedRequired.map((c) => c.name).join("、")} 非空且无默认值，必须映射`
          : duplicates.size > 0
            ? `Excel 列被重复映射到 ${[...duplicates].join("、")}`
            : null;

  const run = async () => {
    if (blocker || !path) return;
    setRunning(true);
    setError(null);
    const jobId = job.begin();
    try {
      const result = await api.importExcel({
        connectionId: target.connectionId,
        namespace: target.namespace,
        table: target.table,
        path,
        sheet,
        headerRow,
        mapping: mapped,
        options: { skipInvalid, useTransaction, strategy },
        jobId,
      });
      setOutcome(result);
      for (const warning of result.warnings ?? []) onLog("warn", warning);
      if (result.failed > 0) {
        onLog(
          "err",
          `导入完成但有 ${result.failed} 行未写入（成功 ${result.inserted} 行）`,
        );
      } else {
        onLog(
          "ok",
          `导入完成：写入 ${result.inserted} 行，跳过 ${result.skipped} 行`,
        );
      }
      if (result.inserted > 0) onImported();
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `导入失败: ${errorMessage(err)}`);
    } finally {
      job.end();
      setRunning(false);
    }
  };

  const preview = plan?.preview;
  const headerChoices = preview?.head.length ?? 0;

  return (
    <div
      className="overlay"
      onMouseDown={(event) => event.target === event.currentTarget && onClose()}
    >
      <div className="modal" style={{ width: 1080, height: "84vh" }}>
        <div className="modal-head">
          <span>
            从 Excel 导入 · <span className="mono">{target.table}</span>{" "}
            <span className="badge accent">{target.namespace}</span>
          </span>
          <button className="icon-btn" onClick={onClose} title="关闭">
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
              <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        <div className="modal-body" style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          {/* ---- step 1: the file ------------------------------------- */}
          <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
            <button className="btn small" onClick={() => void pickFile()} disabled={running}>
              <IconFile size={11} /> {path ? "更换文件" : "选择 Excel 文件"}
            </button>
            <span className="mono faint" style={{ maxWidth: 460, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {path ?? "尚未选择文件"}
            </span>
            {loading && <span className="faint">正在解析…</span>}
          </div>

          {preview && (
            <div className="row" style={{ gap: 14, flexWrap: "wrap" }}>
              <label className="row" style={{ gap: 6 }}>
                <span className="faint">工作表</span>
                <select
                  value={sheet ?? ""}
                  disabled={running}
                  onChange={(event) => {
                    const next = event.target.value;
                    setSheet(next);
                    if (path) void loadPlan(path, next, headerRow);
                  }}
                >
                  {preview.sheets.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="row" style={{ gap: 6 }}>
                <span className="faint">表头行</span>
                <select
                  value={headerRow === null ? "none" : String(headerRow)}
                  disabled={running}
                  onChange={(event) => {
                    const next =
                      event.target.value === "none" ? null : Number(event.target.value);
                    setHeaderRow(next);
                    if (path) void loadPlan(path, sheet, next);
                  }}
                >
                  <option value="none">无表头</option>
                  {Array.from({ length: headerChoices }, (_, index) => (
                    <option key={index} value={index}>
                      第 {index + 1} 行
                      {preview.head[index]?.length
                        ? `：${preview.head[index].slice(0, 3).join(" | ")}`
                        : ""}
                    </option>
                  ))}
                </select>
              </label>
              <span className="faint">
                共 {preview.totalRows} 行数据 · {preview.headers.length} 列
              </span>
            </div>
          )}

          {error && <div className="errbox">{error}</div>}

          {/* ---- step 2: the mapping ---------------------------------- */}
          {plan && (
            <div style={{ border: "1px solid var(--border)", borderRadius: 7, overflow: "hidden" }}>
              <div
                className="row"
                style={{ padding: "6px 10px", background: "var(--bg-subtle)", borderBottom: "1px solid var(--border)" }}
              >
                <span>列映射</span>
                <span className="faint">
                  已映射 {mapped.length} / {plan.columns.length}
                </span>
                <div className="grow" />
                <button
                  className="btn small"
                  disabled={running}
                  onClick={() =>
                    setMapping(
                      new Map(plan.suggested.map((entry) => [entry.column, entry.excelIndex])),
                    )
                  }
                >
                  按名称重新匹配
                </button>
                <button
                  className="btn small"
                  disabled={running}
                  onClick={() => setMapping(new Map())}
                >
                  全部清空
                </button>
              </div>
              <div style={{ maxHeight: 240, overflow: "auto" }}>
                <table className="grid designer">
                  <thead>
                    <tr>
                      <th style={{ width: 200 }}>目标字段</th>
                      <th style={{ width: 150 }}>类型</th>
                      <th style={{ width: 70 }}>约束</th>
                      <th style={{ width: 240 }}>Excel 列</th>
                    </tr>
                  </thead>
                  <tbody>
                    {plan.columns.map((column) => {
                      const index = mapping.get(column.name) ?? UNMAPPED;
                      const isDuplicate = duplicates.has(column.name);
                      return (
                        <tr key={column.name}>
                          <td className="mono">{column.name}</td>
                          <td className="faint mono">{column.fullType}</td>
                          <td>
                            {plan.primaryKey.includes(column.name) && (
                              <span className="badge">PK</span>
                            )}
                            {!column.nullable && <span className="badge">非空</span>}
                            {column.isAutoIncrement && <span className="badge purple">自增</span>}
                          </td>
                          <td>
                            <select
                              value={String(index)}
                              disabled={running}
                              style={{
                                width: "100%",
                                borderColor: isDuplicate ? "var(--red)" : undefined,
                              }}
                              onChange={(event) => {
                                const next = Number(event.target.value);
                                setMapping((prev) => {
                                  const updated = new Map(prev);
                                  if (next === UNMAPPED) updated.delete(column.name);
                                  else updated.set(column.name, next);
                                  return updated;
                                });
                              }}
                            >
                              <option value={String(UNMAPPED)}>（不导入）</option>
                              {preview?.headers.map((header, headerIndex) => (
                                <option key={headerIndex} value={String(headerIndex)}>
                                  {headerIndex + 1}. {header}
                                </option>
                              ))}
                            </select>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* ---- step 3: strategy ------------------------------------- */}
          {plan && (
            <div
              style={{
                border: "1px solid var(--border)",
                borderRadius: 7,
                padding: "10px 12px",
              }}
            >
              <div className="row" style={{ gap: 18, flexWrap: "wrap" }}>
                <span>导入策略</span>
                <label className="check" title="已存在的行会导致主键冲突并报错">
                  <input
                    type="radio"
                    name="import-strategy"
                    checked={strategy === "append"}
                    disabled={running}
                    onChange={() => setStrategy("append")}
                  />
                  新增（插入为新行）
                </label>
                <label
                  className="check"
                  title="按主键匹配，已存在的行会被这份数据覆盖"
                >
                  <input
                    type="radio"
                    name="import-strategy"
                    checked={strategy === "upsert"}
                    disabled={running || (plan.primaryKey.length === 0)}
                    onChange={() => setStrategy("upsert")}
                  />
                  覆盖（按主键更新已有行）
                </label>
              </div>
              <div className="faint" style={{ marginTop: 6 }}>
                {plan.primaryKey.length === 0 ? (
                  "该表没有主键，只能使用新增模式"
                ) : strategy === "upsert" ? (
                  <>
                    匹配主键 <span className="mono">{plan.primaryKey.join("、")}</span>
                    ，命中的行会被 Excel 里的值覆盖；未命中的行正常插入。
                    {!plan.primaryKey.every((key) =>
                      mapping.has(key),
                    ) && (
                      <span style={{ color: "var(--amber)" }}>
                        {" "}
                        主键未映射时会全部按新增处理。
                      </span>
                    )}
                  </>
                ) : (
                  "已存在的行会因主键冲突而报错，不会覆盖"
                )}
              </div>
            </div>
          )}

          {/* ---- step 4: options -------------------------------------- */}
          {plan && (
            <div className="row" style={{ gap: 16 }}>
              <label className="check">
                <input
                  type="checkbox"
                  checked={skipInvalid}
                  disabled={running}
                  onChange={(event) => setSkipInvalid(event.target.checked)}
                />
                跳过无法转换的行
              </label>
              <label className="check" title="PostgreSQL 可整体回滚；MySQL 会从失败处停止且无法回滚">
                <input
                  type="checkbox"
                  checked={useTransaction}
                  disabled={running}
                  onChange={(event) => setUseTransaction(event.target.checked)}
                />
                整体事务（失败即回滚）
              </label>
              {!skipInvalid && (
                <span className="faint">遇到第一个无法转换的值就停止，不写入任何行</span>
              )}
            </div>
          )}

          {/* ---- sheet preview ---------------------------------------- */}
          {preview && preview.rows.length > 0 && (
            <div style={{ border: "1px solid var(--border)", borderRadius: 7, overflow: "hidden" }}>
              <div
                style={{ padding: "6px 10px", background: "var(--bg-subtle)", borderBottom: "1px solid var(--border)" }}
              >
                数据预览（前 {preview.rows.length} 行）
              </div>
              <div style={{ maxHeight: 190, overflow: "auto" }}>
                <table className="grid">
                  <thead>
                    <tr>
                      <th className="rowno">#</th>
                      {preview.headers.map((header, index) => (
                        <th key={index}>{header}</th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {preview.rows.map((row, rowIndex) => (
                      <tr key={rowIndex}>
                        <td className="rowno">{rowIndex + 1}</td>
                        {preview.headers.map((_, cellIndex) => (
                          <td key={cellIndex}>
                            {row[cellIndex] ?? ""}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* ---- step 5: the outcome ---------------------------------- */}
          {outcome && (
            <div
              style={{
                border: `1px solid ${outcome.failed > 0 ? "var(--red)" : "var(--border)"}`,
                borderRadius: 7,
                padding: 12,
              }}
            >
              <div className="row" style={{ gap: 18, flexWrap: "wrap" }}>
                <Stat label="读取" value={outcome.read} />
                <Stat
                  label={outcome.strategy === "upsert" ? "受影响行数" : "写入"}
                  value={outcome.inserted}
                  tone="var(--green)"
                />
                {outcome.existingKeys != null && (
                  <>
                    <Stat
                      label="其中覆盖"
                      value={outcome.existingKeys}
                      tone="var(--amber)"
                    />
                    <Stat
                      label="新增"
                      value={Math.max(0, outcome.prepared - outcome.existingKeys)}
                      tone="var(--green)"
                    />
                  </>
                )}
                <Stat label="跳过" value={outcome.skipped} tone={outcome.skipped ? "var(--amber)" : undefined} />
                <Stat label="未写入" value={outcome.failed} tone={outcome.failed ? "var(--red)" : undefined} />
                <Stat label="语句" value={outcome.statements} />
                <span className="faint">{outcome.elapsedMs} ms</span>
              </div>

              {outcome.rolledBack && (
                <div className="warnbox" style={{ marginTop: 10 }}>
                  事务已回滚：目标表未写入任何数据。
                </div>
              )}
              {outcome.warnings.map((warning, index) => (
                <div key={index} className="warnbox" style={{ marginTop: 8 }}>
                  <IconWarn size={11} /> {warning}
                </div>
              ))}

              {outcome.errors.length > 0 && (
                <div style={{ marginTop: 10 }}>
                  <div className="faint" style={{ marginBottom: 4 }}>
                    问题明细（前 {outcome.errors.length} 条）
                  </div>
                  <div
                    className="mono"
                    style={{
                      maxHeight: 150,
                      overflow: "auto",
                      background: "var(--bg-subtle)",
                      border: "1px solid var(--border)",
                      borderRadius: 6,
                      padding: "6px 9px",
                      fontSize: 11,
                      lineHeight: 1.7,
                    }}
                  >
                    {outcome.errors.map((entry, index) => (
                      <div key={index}>
                        第 {entry.row} 行
                        {entry.column ? ` · ${entry.column}` : ""} — {entry.message}
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>

        {(running || job.update) && (
          <div style={{ padding: "0 12px" }}>
            <JobProgress
              update={job.update}
              running={running}
              onDismiss={job.dismiss}
              onLog={onLog}
            />
          </div>
        )}

        <div className="modal-foot">
          <span className="faint">
            {blocker ?? `将写入 ${plan?.columns.length ?? 0} 个字段`}
          </span>
          <div className="spacer" />
          <button className="btn" onClick={onClose} disabled={running}>
            关闭
          </button>
          <button
            className="btn primary"
            onClick={() => void run()}
            disabled={running || blocker !== null || !path}
          >
            {running ? "导入中…" : "开始导入"}
          </button>
        </div>
      </div>
    </div>
  );
}

function Stat({ label, value, tone }: { label: string; value: number; tone?: string }) {
  return (
    <span className="row" style={{ gap: 5 }}>
      <span className="faint">{label}</span>
      <strong style={{ color: tone, fontSize: 15 }}>
        {value.toLocaleString()}
      </strong>
    </span>
  );
}
