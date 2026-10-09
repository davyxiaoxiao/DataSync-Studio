import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import type { SqlRunResult, SqlStatementResult } from "../types";

interface Props {
  connectionId: string;
  namespace: string;
  /** Absolute path of the `.sql` file this tab edits. */
  path: string;
  onLog: (level: "info" | "ok" | "warn" | "err" | "sql", message: string) => void;
  onTitleChange: (title: string, dirty: boolean) => void;
}

const DEFAULT_SQL = "-- 在这里写 SQL，⌘/Ctrl + Enter 执行，⌘/Ctrl + S 保存\nSELECT 1;\n";

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
}: Props) {
  const job = useJobProgress();
  const [sql, setSql] = useState("");
  const [saved, setSaved] = useState("");
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<SqlRunResult | null>(null);
  const textarea = useRef<HTMLTextAreaElement | null>(null);

  const dirty = sql !== saved;

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const query = await api.readQuery({ path });
        if (cancelled) return;
        // An empty file would look broken, so it starts with a hint.
        setSql(query.sql.trim() === "" ? DEFAULT_SQL : query.sql);
        setSaved(query.sql.trim() === "" ? DEFAULT_SQL : query.sql);
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
    setSaving(true);
    setError(null);
    try {
      const query = await api.saveQuery({ path, sql });
      setSaved(sql);
      onTitleChange(query.name, false);
      onLog("ok", `已保存 ${query.name}`);
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `保存失败: ${errorMessage(err)}`);
    } finally {
      setSaving(false);
    }
  }, [path, sql, onLog, onTitleChange]);

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

  // Editor shortcuts: run and save are the two things done constantly.
  const onKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
      event.preventDefault();
      void run();
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void save();
    }
  };

  useEffect(() => {
    onTitleChange(
      path.split("/").pop()?.replace(/\.sql$/, "") ?? "查询",
      dirty,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty]);

  return (
    <div className="panel">
      <div className="section" style={{ padding: "8px 12px" }}>
        <div className="row">
          <span className="badge accent">{namespace}</span>
          <span className="mono faint" style={{ fontSize: 11 }}>
            {path}
          </span>
          {dirty && <span className="badge purple">未保存</span>}
          <div className="grow" />
          <button className="btn small" onClick={() => void save()} disabled={saving || !dirty}>
            {saving ? "保存中…" : "保存"}
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
        {loading && <div className="faint">正在读取 {path} …</div>}
        <textarea
          ref={textarea}
          value={sql}
          onChange={(event) => setSql(event.target.value)}
          onKeyDown={onKeyDown}
          spellCheck={false}
          placeholder={DEFAULT_SQL}
          style={{
            width: "100%",
            height: 180,
            fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
            fontSize: 12,
            lineHeight: 1.7,
            resize: "vertical",
          }}
        />
      </div>

      <div style={{ overflow: "auto", flex: 1, padding: "0 12px 12px" }}>
        {running && <div className="faint">正在执行…</div>}
        {!running && !result && (
          <div className="faint">
            还没有执行结果。写好后按 ⌘/Ctrl + Enter 运行。
          </div>
        )}
        {result?.cancelled && (
          <div className="warnbox" style={{ marginBottom: 8 }}>
            执行已取消，结果可能不完整。
          </div>
        )}
        {result?.results.map((entry, index) => (
          <StatementResult key={index} index={index} entry={entry} />
        ))}
      </div>
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
