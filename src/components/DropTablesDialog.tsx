import { useEffect, useState } from "react";
import { api, errorMessage } from "../api";
import { useJobProgress } from "../jobs";
import type { DropTablesPreview, SyncRunResult } from "../types";
import type { TableRef } from "./Sidebar";
import { ConfirmDialog } from "./ConfirmDialog";
import { RunResultPanel } from "./RunResultPanel";
import { SqlPreviewModal } from "./SqlPreviewModal";
import { IconWarn } from "./Icons";

interface Props {
  refs: TableRef[];
  onClose: () => void;
  onDone: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

/**
 * Batch table removal.
 *
 * The plan is generated up front so the user sees exactly which statements run,
 * including the foreign-key releases the planner had to add for tables that were
 * not selected.
 */
export function DropTablesDialog({ refs, onClose, onDone, onLog }: Props) {
  const job = useJobProgress();
  const [preview, setPreview] = useState<DropTablesPreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showSql, setShowSql] = useState(false);
  const [result, setResult] = useState<SyncRunResult | null>(null);

  const scope = refs[0];
  const single = refs.length === 1;

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const outcome = await api.previewDropTables(
          scope.connectionId,
          scope.namespace,
          refs.map((ref) => ref.table),
        );
        if (!cancelled) setPreview(outcome);
      } catch (err) {
        if (!cancelled) setError(errorMessage(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
    // `refs` is stable for the lifetime of the dialog.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const run = async () => {
    setBusy(true);
    setError(null);
    const jobId = job.begin();
    try {
      const outcome = await api.applyDropTables({
        connectionId: scope.connectionId,
        namespace: scope.namespace,
        tables: refs.map((ref) => ref.table),
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
        onDone();
      }
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `删除失败: ${errorMessage(err)}`);
    } finally {
      job.end();
      setBusy(false);
    }
  };

  const dependentViews = preview?.dependentViews ?? [];
  const warnings = preview?.warnings ?? [];
  const statements = preview?.statements ?? [];
  const fkReleases = statements.filter((s) => s.category === "drop_foreign_key").length;

  return (
    <>
      <ConfirmDialog
        title={single ? `删除表 ${scope.table}` : `删除 ${refs.length} 张表`}
        danger
        busy={busy}
        confirmLabel={result ? "关闭" : "确认删除"}
        cancelLabel={result ? "关闭" : "取消"}
        onClose={onClose}
        onConfirm={() => {
          if (result) {
            onClose();
            return;
          }
          void run();
        }}
        error={error}
        message={
          <div>
            即将从 <strong className="mono">{scope.namespace}</strong> 删除：
            <div
              className="mono"
              style={{
                marginTop: 6,
                maxHeight: 160,
                overflow: "auto",
                background: "var(--bg-subtle)",
                border: "1px solid var(--border)",
                borderRadius: 6,
                padding: "6px 9px",
              }}
            >
              {refs.map((ref) => (
                <div key={ref.table}>{ref.table}</div>
              ))}
            </div>
            <div className="faint" style={{ marginTop: 8 }}>
              {loading
                ? "正在生成删除计划…"
                : `共 ${statements.length} 条语句${
                    fkReleases > 0 ? `（其中 ${fkReleases} 条用于先解除指向这些表的外键）` : ""
                  }`}
            </div>
          </div>
        }
      >
        {dependentViews.length > 0 && (
          <div className="errbox" style={{ marginTop: 12 }}>
            以下视图依赖这些表，删除会失败（或需要级联删除）：{dependentViews.join("、")}
            <div style={{ marginTop: 6 }}>请先删除这些视图，或改用结构同步处理。</div>
          </div>
        )}
        {warnings.length > 0 && (
          <div className="warnbox" style={{ marginTop: 12 }}>
            {warnings.map((warning, index) => (
              <div key={index}>
                <IconWarn size={11} /> {warning}
              </div>
            ))}
          </div>
        )}

        {statements.length > 0 && (
          <div style={{ marginTop: 12 }} className="row">
            <button className="btn small" onClick={() => setShowSql(true)}>
              预览 SQL
            </button>
            <span className="faint">失败会整体回滚（PostgreSQL）</span>
          </div>
        )}

        {result && (
          <div style={{ marginTop: 12 }}>
            <RunResultPanel result={result} title="删除表" />
          </div>
        )}
      </ConfirmDialog>

      {showSql && (
        <SqlPreviewModal
          title="删除表 SQL 预览"
          statements={statements.map((statement) => statement.sql)}
          onClose={() => setShowSql(false)}
          onLog={onLog}
        />
      )}
    </>
  );
}
