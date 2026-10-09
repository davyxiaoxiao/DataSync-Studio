import { useCallback, useEffect, useState } from "react";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import type {
  CompletionCatalog,
  SqlCheckResult,
  SqlRunResult,
  SqlStatementResult,
} from "../types";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { SqlEditor } from "../components/SqlEditor";

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

  /// Ask the server to validate the script without running it.
  const validate = useCallback(async () => {
    setChecking(true);
    setError(null);
    setCheck(null);
    try {
      const outcome = await api.checkSql({ connectionId, namespace, sql });
      setCheck(outcome);
      const failed = outcome.entries.filter((entry) => !entry.ok).length;
      if (failed > 0) {
        onLog("err", `语法检查发现 ${failed} 条语句有问题`);
      } else {
        onLog("ok", `语法检查通过（${outcome.entries.length} 条语句，${outcome.elapsedMs} ms）`);
      }
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `语法检查失败: ${errorMessage(err)}`);
    } finally {
      setChecking(false);
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
            onClick={() => void validate()}
            disabled={checking || running}
            title="让服务端解析并校验，但不执行"
          >
            {checking ? "检查中…" : "检查语法"}
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

      {check && (
        <div className="section" style={{ paddingTop: 0 }}>
          {check.entries.every((entry) => entry.ok) ? (
            <div className="warnbox" style={{ borderColor: "var(--green)", color: "var(--green)" }}>
              语法检查通过：{check.entries.length} 条语句（{check.elapsedMs} ms）
            </div>
          ) : (
            <div className="errbox">
              {check.entries
                .filter((entry) => !entry.ok)
                .map((entry, index) => (
                  <div key={index} style={{ marginBottom: 4 }}>
                    <span className="mono">
                      {entry.sql.replace(/\s+/g, " ").trim().slice(0, 80)}
                    </span>
                    <div style={{ marginTop: 2 }}>{entry.error}</div>
                  </div>
                ))}
            </div>
          )}
          {check.entries.some((entry) => !entry.checkable) && (
            <div className="faint" style={{ marginTop: 6 }}>
              有语句无法预检（PostgreSQL 的 PREPARE 不支持该类型），只能直接执行确认。
            </div>
          )}
        </div>
      )}

      {/* Nothing is shown until something has actually been executed. */}
      {(running || result) && (
        <div style={{ overflow: "auto", flex: 1, padding: "0 12px 12px" }}>
        {running && <div className="faint">正在执行…</div>}
        {result?.cancelled && (
          <div className="warnbox" style={{ marginBottom: 8 }}>
            执行已取消，结果可能不完整。
          </div>
        )}
        {result?.results.map((entry, index) => (
          <StatementResult key={index} index={index} entry={entry} />
        ))}
        </div>
      )}
    </div>
  );
}

function StatementResult({ index, entry }: { index: number; entry: SqlStatementResult }) {
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
