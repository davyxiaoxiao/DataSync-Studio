import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import type {
  CompletionCatalog,
  SqlCheckResult,
  SqlExplainEntry,
  SqlExplainResult,
  SqlRunResult,
  SqlStatementResult,
} from "../types";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { SqlEditor } from "../components/SqlEditor";
import { InlineLoading, Loading } from "../components/Loading";
import {
  ExcelExportDialog,
  type ExportSource,
} from "../components/ExcelExportDialog";

interface Props {
  connectionId: string;
  namespace: string;
  /**
   * Absolute path of the `.sql` file this tab edits. Absent for a temporary
   * query, which lives only in the editor until it is saved.
   */
  path?: string;
  onLog: (level: "info" | "ok" | "warn" | "err" | "sql", message: string) => void;
  onTitleChange: (title: string, dirty: boolean) => void;
  /** Save a temporary query under a name; the shell turns it into a file tab. */
  onSaveAs: (name: string, sql: string) => Promise<void>;
  /** The connection's dialect, which decides the extra keywords. */
  dialect: string;
}

/**
 * A SQL console backed by a `.sql` file on disk.
 *
 * The file is the source of truth: the editor loads it, saves to it, and the
 * sidebar lists whatever is in the folder, so a query written in another editor
 * shows up here too.
 */
export function QueryView({
  connectionId,
  namespace,
  path,
  onLog,
  onTitleChange,
  onSaveAs,
  dialect,
}: Props) {
  const scratch = path === undefined;
  const job = useJobProgress();
  const [sql, setSql] = useState("");
  const [saved, setSaved] = useState("");
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [catalog, setCatalog] = useState<CompletionCatalog | null>(null);
  const [askingName, setAskingName] = useState(false);
  const [newName, setNewName] = useState("");
  const [result, setResult] = useState<SqlRunResult | null>(null);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState<SqlCheckResult | null>(null);
  const [explaining, setExplaining] = useState(false);
  const [explain, setExplain] = useState<SqlExplainResult | null>(null);
  const [exportSource, setExportSource] = useState<ExportSource | null>(null);
  const [checkFailed, setCheckFailed] = useState(false);
  // Guards against an older check landing after a newer one.
  const checkSeq = useRef(0);

  const dirty = sql !== saved;

  // Names for the editor's completion; a failure here only means fewer
  // suggestions, so it is not surfaced as an error.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const names = await api.completionCatalog(connectionId, namespace);
        if (!cancelled) setCatalog(names);
      } catch {
        if (!cancelled) setCatalog(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [connectionId, namespace]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        if (!path) {
          // A temporary query starts empty rather than reading anything: it is
          // not backed by a file until the user saves it.
          setSql("");
          setSaved("");
          return;
        }
        const query = await api.readQuery({ path });
        if (cancelled) return;
        setSql(query.sql);
        setSaved(query.sql);
        onTitleChange(query.name, false);
      } catch (err) {
        if (!cancelled) setError(errorMessage(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
    // The tab is keyed by path, so it never changes under us.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path]);

  const save = useCallback(async () => {
    if (scratch) {
      // Saving a temporary query is what makes it appear in the tree, so the
      // name has to be chosen here.
      setAskingName(true);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const query = await api.saveQuery({ path: path as string, sql });
      setSaved(sql);
      onTitleChange(query.name, false);
      onLog("ok", `已保存 ${query.name}`);
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `保存失败: ${errorMessage(err)}`);
    } finally {
      setSaving(false);
    }
  }, [path, scratch, sql, onLog, onTitleChange]);

  const run = useCallback(async () => {
    setRunning(true);
    setError(null);
    setResult(null);
    const jobId = job.begin();
    try {
      setExplain(null);
      const outcome = await api.runSql({ connectionId, namespace, sql, jobId });
      setResult(outcome);
      const failed = outcome.results.filter((entry) => !entry.ok).length;
      const rowCount = outcome.results.reduce(
        (total, entry) => total + entry.rows.length,
        0,
      );
      if (failed > 0) {
        onLog("err", `SQL 执行出错（${failed} 条语句失败）`);
      } else {
        onLog(
          "ok",
          `SQL 执行完成：${outcome.results.length} 条语句，返回 ${rowCount} 行（${outcome.elapsedMs} ms）`,
        );
      }
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `SQL 执行失败: ${errorMessage(err)}`);
    } finally {
      job.end();
      setRunning(false);
    }
  }, [connectionId, namespace, sql, job, onLog]);

  /// Validate the script automatically, a moment after typing stops.
  ///
  /// Checking on every keystroke would put a round trip behind each character,
  /// and checking instantly would report errors for half-typed words, so the
  /// request is debounced and stale replies are dropped.
  useEffect(() => {
    if (sql.trim() === "" || running) {
      setCheck(null);
      setChecking(false);
      return;
    }
    const timer = window.setTimeout(() => {
      const seq = ++checkSeq.current;
      setChecking(true);
      void (async () => {
        try {
          const outcome = await api.checkSql({ connectionId, namespace, sql });
          if (seq !== checkSeq.current) return;
          setCheck(outcome);
          setCheckFailed(false);
        } catch {
          if (seq !== checkSeq.current) return;
          // A failed check is not a syntax error; say nothing rather than
          // blaming the user's SQL.
          setCheck(null);
          setCheckFailed(true);
        } finally {
          if (seq === checkSeq.current) setChecking(false);
        }
      })();
    }, 700);
    return () => window.clearTimeout(timer);
  }, [sql, connectionId, namespace, running]);

  /// Ask the server for the execution plan. Plain `EXPLAIN`, never `ANALYZE`:
  /// the latter really runs the statement, which would change data.
  const analyse = useCallback(async () => {
    setExplaining(true);
    setError(null);
    setExplain(null);
    try {
      const outcome = await api.explainSql({ connectionId, namespace, sql });
      setExplain(outcome);
      const failed = outcome.entries.filter((entry) => !entry.ok).length;
      onLog(
        failed > 0 ? "err" : "ok",
        failed > 0
          ? `执行计划分析有 ${failed} 条语句失败`
          : `已生成 ${outcome.entries.length} 条语句的执行计划（${outcome.elapsedMs} ms）`,
      );
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `分析失败: ${errorMessage(err)}`);
    } finally {
      setExplaining(false);
    }
  }, [connectionId, namespace, sql, onLog]);

  useEffect(() => {
    const name = path
      ? (path.split("/").pop()?.replace(/\.sql$/, "") ?? "查询")
      : "临时查询";
    onTitleChange(name, dirty || scratch);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty, scratch]);

  return (
    <div className="panel">
      <div className="section" style={{ padding: "8px 12px" }}>
        <div className="row">
          <span className="badge accent">{namespace}</span>
          {scratch ? (
            <span className="faint" title="保存后才会出现在左侧的「查询」里">
              临时查询（未保存到文件）
            </span>
          ) : (
            <span className="mono faint" style={{ fontSize: 11 }}>
              {path}
            </span>
          )}
          {dirty && <span className="badge purple">未保存</span>}
          {checking && <span className="faint">检查中…</span>}
          {!checking && check && check.entries.some((entry) => !entry.ok) && (
            <span className="badge" style={{ color: "var(--red)" }}>
              {check.entries.filter((entry) => !entry.ok).length} 处语法问题
            </span>
          )}
          {!checking &&
            check &&
            check.entries.every((entry) => entry.ok) &&
            check.entries.every((entry) => entry.checkable) && (
              <span className="badge" style={{ color: "var(--green)" }}>
                语法正确
              </span>
            )}
          {!checking && checkFailed && (
            <span className="faint" title="无法连接服务端进行校验">
              未能检查
            </span>
          )}
          <div className="grow" />
          <button
            className="btn small"
            onClick={() => void save()}
            disabled={saving || (!dirty && !scratch)}
          >
            {saving ? "保存中…" : scratch ? "保存到文件…" : "保存"}
          </button>
          <button
            className="btn small"
            onClick={() => void analyse()}
            disabled={explaining || running}
            title="生成执行计划。使用 EXPLAIN 而非 EXPLAIN ANALYZE，因此不会真正执行语句"
          >
            {explaining ? "分析中…" : "分析执行计划"}
          </button>
          <button className="btn primary small" onClick={() => void run()} disabled={running}>
            {running ? "执行中…" : "执行 (⌘↵)"}
          </button>
        </div>
      </div>

      {error && (
        <div className="section" style={{ paddingTop: 8, paddingBottom: 8 }}>
          <div className="errbox">{error}</div>
        </div>
      )}

      <div className="section" style={{ padding: "0 12px 10px" }}>
        {loading && <div className="faint">正在读取…</div>}
        <SqlEditor
          value={sql}
          onChange={setSql}
          onRun={() => void run()}
          onSave={() => void save()}
          catalog={catalog}
          dialect={dialect}
        />
      </div>

      {askingName && (
        <ConfirmDialog
          title="保存查询"
          confirmLabel="保存"
          message={
            <div className="field">
              <label>查询名称（会写成 .sql 文件）</label>
              <input
                type="text"
                value={newName}
                autoFocus
                placeholder="例如：导出数据"
                onChange={(event) => setNewName(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && newName.trim()) {
                    void (async () => {
                      setAskingName(false);
                      await onSaveAs(newName.trim(), sql);
                    })();
                  }
                }}
              />
              <div className="faint" style={{ marginTop: 6 }}>
                保存后才会出现在左侧「查询」分组里
              </div>
            </div>
          }
          onClose={() => setAskingName(false)}
          onConfirm={() => {
            if (!newName.trim()) return;
            setAskingName(false);
            void onSaveAs(newName.trim(), sql);
          }}
        />
      )}

      {/* Only problems are shown; a correct query stays quiet. */}
      {exportSource && (
        <ExcelExportDialog
          source={exportSource}
          onClose={() => setExportSource(null)}
          onLog={onLog}
        />
      )}

      {check && check.entries.some((entry) => !entry.ok) && (
        <div className="section" style={{ paddingTop: 0 }}>
          <div className="errbox">
            {check.entries
              .filter((entry) => !entry.ok)
              .map((entry, index) => (
                <div key={index} style={{ marginBottom: 6 }}>
                  <span className="mono">
                    {entry.sql.replace(/\s+/g, " ").trim().slice(0, 90)}
                  </span>
                  <div style={{ marginTop: 2 }}>{entry.error}</div>
                </div>
              ))}
          </div>
          {check.entries.some((entry) => !entry.checkable) && (
            <div className="faint" style={{ marginTop: 6 }}>
              有语句无法预检（PostgreSQL 的 PREPARE 不支持该类型），只能直接执行确认。
            </div>
          )}
        </div>
      )}

      {explain && (
        <div style={{ overflow: "auto", flex: 1, padding: "0 12px 12px" }}>
          <div className="faint" style={{ marginBottom: 6 }}>
            执行计划（EXPLAIN，未真正执行语句）
          </div>
          {explain.entries.map((entry, index) => (
            <ExplainPlan key={index} index={index} entry={entry} />
          ))}
        </div>
      )}

      {/* Nothing is shown until something has actually been executed. */}
      {(running || result) && (
        <div style={{ overflow: "auto", flex: 1, padding: "0 12px 12px" }}>
        {running && (
          <Loading
            label="正在执行 SQL…"
            detail={
              // A rough count is enough to say whether this will take a moment.
              sql.split(";").filter((part) => part.trim() !== "").length > 1
                ? "多语句脚本，逐条执行"
                : "已发送到服务端，等待结果"
            }
          />
        )}
        {result?.cancelled && (
          <div className="warnbox" style={{ marginBottom: 8 }}>
            执行已取消，结果可能不完整。
          </div>
        )}
        {result?.results.map((entry, index) => (
          <StatementResult
            key={index}
            index={index}
            entry={entry}
            connectionId={connectionId}
            namespace={namespace}
            onLog={onLog}
            onExport={(columns) =>
              setExportSource(
                // A query is re-run for the export, so the whole result is
                // written rather than the page that happens to be on screen.
                isQueryText(entry.sql)
                  ? {
                      kind: "query",
                      title: `查询结果 ${index + 1}`,
                      connectionId,
                      namespace,
                      sql: entry.sql,
                      columns,
                    }
                  : {
                      kind: "rows",
                      title: `查询结果 ${index + 1}`,
                      namespace,
                      columns,
                      rows: entry.rows,
                    },
              )
            }
          />
        ))}
        </div>
      )}
    </div>
  );
}

/** Only these can be re-run for a complete export. */
function isQueryText(sql: string): boolean {
  const first = sql
    .trimStart()
    .split(/\s+/)[0]
    ?.toUpperCase();
  return first === "SELECT" || first === "WITH" || first === "TABLE" || first === "VALUES";
}

function ExplainPlan({ index, entry }: { index: number; entry: SqlExplainEntry }) {
  return (
    <div
      style={{
        border: `1px solid ${entry.ok ? "var(--border)" : "var(--red)"}`,
        borderRadius: 7,
        marginBottom: 10,
        overflow: "hidden",
      }}
    >
      <div
        className="row"
        style={{
          padding: "6px 10px",
          background: "var(--bg-subtle)",
          borderBottom: "1px solid var(--border)",
        }}
      >
        <span className="faint">#{index + 1}</span>
        <span
          className="mono"
          style={{
            fontSize: 11,
            maxWidth: 560,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {entry.sql.replace(/\s+/g, " ").trim()}
        </span>
      </div>

      {!entry.supported && (
        <div className="faint" style={{ padding: 10 }}>
          该语句类型不支持生成执行计划。
        </div>
      )}
      {entry.supported && !entry.ok && (
        <div className="errbox" style={{ margin: 10 }}>
          {entry.error}
        </div>
      )}
      {entry.supported && entry.ok && (
        <div style={{ maxHeight: 300, overflow: "auto" }}>
          <table className="grid">
            <thead>
              <tr>
                <th className="rowno">#</th>
                {entry.columns.map((column) => (
                  <th key={column.name} title={column.name}>
                    <span>{column.name}</span>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {entry.rows.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  <td className="rowno">{rowIndex + 1}</td>
                  {row.map((cell, cellIndex) => (
                    <td key={cellIndex} className={cell.text == null ? "null" : ""}>
                      {cell.text ?? "NULL"}
                    </td>
                  ))}
                </tr>
              ))}
              {entry.rows.length === 0 && (
                <tr>
                  <td
                    colSpan={entry.columns.length + 1}
                    style={{ textAlign: "center", color: "var(--text-faint)", height: 44 }}
                  >
                    没有计划内容
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function StatementResult({
  index,
  entry,
  connectionId,
  namespace,
  onLog,
  onExport,
}: {
  index: number;
  entry: SqlStatementResult;
  connectionId: string;
  namespace: string;
  onLog: (level: "info" | "ok" | "warn" | "err" | "sql", message: string) => void;
  onExport: (columns: { name: string; dataType: string }[]) => void;
}) {
  const pageSize = 1000;
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<{
    columns: { name: string; dataType: string }[];
    rows: SqlStatementResult["rows"];
    hasMore: boolean;
  }>(() => ({
    columns: entry.columns.map((column) => ({
      name: column.name,
      dataType: column.dataType,
    })),
    rows: entry.rows,
    hasMore: entry.hasMore,
  }));
  const [paging, setPaging] = useState(false);

  // Later pages are read from the server on demand; only one page is ever held.
  const load = async (next: number) => {
    setPaging(true);
    try {
      const result = await api.runSqlPage({
        connectionId,
        namespace,
        sql: entry.sql,
        offset: next,
        limit: pageSize,
      });
      setOffset(next);
      setPage({
        columns: result.columns.map((column) => ({
          name: column.name,
          dataType: column.dataType,
        })),
        rows: result.rows,
        hasMore: result.hasMore,
      });
    } catch (error) {
      onLog("err", `读取结果集失败: ${errorMessage(error)}`);
    } finally {
      setPaging(false);
    }
  };

  const paged = entry.columns.length > 0;
  const from = page.rows.length === 0 ? 0 : offset + 1;
  const to = offset + page.rows.length;

  return (
    <div>
      {paged && (
        <div className="row" style={{ marginBottom: 6, gap: 8 }}>
          <span className="faint">
            第 {from}–{to} 行{page.hasMore ? "（还有更多）" : ""}
          </span>
          {paging && <InlineLoading />}
          <button
            className="btn small"
            disabled={offset === 0 || paging}
            onClick={() => void load(Math.max(0, offset - pageSize))}
          >
            上一页
          </button>
          <button
            className="btn small"
            disabled={!page.hasMore || paging}
            onClick={() => void load(offset + pageSize)}
          >
            {paging ? "读取中…" : "下一页"}
          </button>
          <button
            className="btn small"
            onClick={() => onExport(page.columns)}
            title="导出全部行，不只是当前页"
          >
            导出 Excel
          </button>
        </div>
      )}
      <ResultTable index={index} entry={{ ...entry, rows: page.rows }} />
    </div>
  );
}

function ResultTable({ index, entry }: { index: number; entry: SqlStatementResult }) {
  const [open, setOpen] = useState(entry.columns.length > 0);

  return (
    <div
      style={{
        border: `1px solid ${entry.ok ? "var(--border)" : "var(--red)"}`,
        borderRadius: 7,
        marginBottom: 10,
        overflow: "hidden",
      }}
    >
      <div
        className="row"
        style={{
          padding: "6px 10px",
          background: "var(--bg-subtle)",
          borderBottom: "1px solid var(--border)",
        }}
      >
        <span className="faint">#{index + 1}</span>
        <span className="mono" style={{ fontSize: 11, maxWidth: 520, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {entry.sql.replace(/\s+/g, " ").trim()}
        </span>
        <div className="grow" />
        {entry.ok ? (
          entry.columns.length > 0 ? (
            <>
              <span className="faint">{entry.rows.length} 行</span>
              <button className="btn small" onClick={() => setOpen((value) => !value)}>
                {open ? "收起" : "展开"}
              </button>
            </>
          ) : (
            <span className="faint">影响 {entry.rowsAffected} 行</span>
          )
        ) : (
          <span className="badge" style={{ color: "var(--red)" }}>
            失败
          </span>
        )}
        <span className="faint">{entry.elapsedMs} ms</span>
      </div>

      {!entry.ok && entry.error && (
        <div className="errbox" style={{ margin: 10 }}>
          {entry.error}
        </div>
      )}

      {entry.truncated && (
        <div className="warnbox" style={{ margin: 10 }}>
          结果过多，只显示了前 {entry.rows.length} 行
        </div>
      )}

      {entry.ok && open && entry.columns.length > 0 && (
        <div style={{ maxHeight: 320, overflow: "auto" }}>
          <table className="grid">
            <thead>
              <tr>
                <th className="rowno">#</th>
                {entry.columns.map((column) => (
                  <th key={column.name} title={`${column.name} : ${column.dataType}`}>
                    <span>{column.name}</span>
                    <span className="coltype">{column.dataType}</span>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {entry.rows.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  <td className="rowno">{rowIndex + 1}</td>
                  {row.map((cell, cellIndex) => {
                    const isNull = cell.kind === "null" || cell.text == null;
                    return (
                      <td
                        key={cellIndex}
                        className={isNull ? "null" : ""}
                        title={cell.text ?? "NULL"}
                      >
                        {isNull ? "NULL" : cell.text}
                      </td>
                    );
                  })}
                </tr>
              ))}
              {entry.rows.length === 0 && (
                <tr>
                  <td
                    colSpan={entry.columns.length + 1}
                    style={{ textAlign: "center", color: "var(--text-faint)", height: 48 }}
                  >
                    没有数据
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
