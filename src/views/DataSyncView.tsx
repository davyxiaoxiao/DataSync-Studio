import { useEffect, useMemo, useState } from "react";
import { api, errorMessage } from "../api";
import { EndpointPicker, type EndpointValue } from "../components/EndpointPicker";
import { RunResultPanel } from "../components/RunResultPanel";
import { SqlPreviewModal } from "../components/SqlPreviewModal";
import { IconChevronRight, IconCompare } from "../components/Icons";
import { useStore } from "../store";
import type { DataDiffResult, RowChange, SyncRunResult } from "../types";

type ChangeTab = "insert" | "update" | "delete";

export function DataSyncView({ initial }: { initial?: EndpointValue }) {
  const { connections, log } = useStore();
  const [source, setSource] = useState<EndpointValue>(
    initial ?? { connectionId: "", namespace: "", table: "" },
  );
  const [target, setTarget] = useState<EndpointValue>({ connectionId: "", namespace: "", table: "" });
  const [rowLimit, setRowLimit] = useState(200000);
  const [batchSize, setBatchSize] = useState(200);

  // The source engine anchors the pair: only same-engine targets are offered.
  const sourceDialect = useMemo(
    () => connections.find((c) => c.id === source.connectionId)?.dialect,
    [connections, source.connectionId],
  );

  const [comparing, setComparing] = useState(false);
  const [result, setResult] = useState<DataDiffResult | null>(null);
  const [changeTab, setChangeTab] = useState<ChangeTab>("insert");
  const [previewOpen, setPreviewOpen] = useState(false);

  const [doInsert, setDoInsert] = useState(true);
  const [doUpdate, setDoUpdate] = useState(true);
  const [doDelete, setDoDelete] = useState(false);
  const [useTransaction, setUseTransaction] = useState(true);

  const [applying, setApplying] = useState(false);
  const [runResult, setRunResult] = useState<SyncRunResult | null>(null);

  // Any endpoint change invalidates the previous comparison plan.
  useEffect(() => {
    setResult(null);
    setRunResult(null);
  }, [source.connectionId, source.namespace, source.table, target.connectionId, target.namespace, target.table]);

  const ready = Boolean(
    source.connectionId &&
      source.namespace &&
      source.table &&
      target.connectionId &&
      target.namespace &&
      target.table,
  );

  const runCompare = async () => {
    if (!ready) {
      log("warn", "请先选择源表与目标表");
      return;
    }
    setComparing(true);
    setRunResult(null);
    log("info", `开始对比数据：${source.table} → ${target.table} …`);
    const started = Date.now();
    try {
      const outcome = await api.compareData({
        source: {
          connectionId: source.connectionId,
          namespace: source.namespace,
          table: source.table!,
        },
        target: {
          connectionId: target.connectionId,
          namespace: target.namespace,
          table: target.table!,
        },
        keyColumns: [],
        columnMap: [],
        rowLimit,
        batchSize,
      });
      setResult(outcome);
      for (const warning of outcome.warnings) log("warn", warning);
      const s = outcome.summary;
      log(
        "ok",
        `对比完成（${Date.now() - started} ms）：源 ${s.sourceRows} 行 / 目标 ${s.targetRows} 行 · ` +
          `新增 ${s.inserts}，修改 ${s.updates}，删除 ${s.deletes}，未变 ${s.unchanged}`,
      );
      setChangeTab(s.inserts > 0 ? "insert" : s.updates > 0 ? "update" : "delete");
    } catch (error) {
      log("err", `数据对比失败: ${errorMessage(error)}`);
    } finally {
      setComparing(false);
    }
  };

  const doApply = async () => {
    if (!result) return;
    setApplying(true);
    setRunResult(null);
    log("info", "开始执行数据同步…");
    try {
      const outcome = await api.applyDataSync({
        planId: result.planId,
        connectionId: target.connectionId,
        insert: doInsert,
        update: doUpdate,
        delete: doDelete,
        useTransaction,
      });
      setRunResult(outcome);
      if (outcome.failed > 0) {
        log("err", outcome.message);
        for (const failure of outcome.failures.slice(0, 8)) {
          log("err", `${failure.sql} → ${failure.error}`);
        }
      } else {
        log("ok", outcome.message);
      }
      setResult(null);
    } catch (error) {
      log("err", `数据同步失败: ${errorMessage(error)}`);
    } finally {
      setApplying(false);
    }
  };

  const selectedStatements = useMemo(() => {
    if (!result) return 0;
    const s = result.summary;
    return (doInsert ? s.inserts : 0) + (doUpdate ? s.updates : 0) + (doDelete ? s.deletes : 0);
  }, [result, doInsert, doUpdate, doDelete]);

  const previewRows: RowChange[] = useMemo(() => {
    if (!result) return [];
    if (changeTab === "insert") return result.insertPreview;
    if (changeTab === "update") return result.updatePreview;
    return result.deletePreview;
  }, [result, changeTab]);

  return (
    <div className="panel-scroll">
      <div className="section">
        <div className="section-title">
          数据同步{" "}
          <span className="hint">
            按主键匹配，把源表数据合并到目标表；仅支持同类型数据库，目标连接会按源的类型过滤
          </span>
        </div>
        <div className="split">
          <EndpointPicker
            title="源表"
            accent="var(--green)"
            value={source}
            withTable
            onChange={setSource}
            onLog={log}
            disabled={comparing || applying}
          />
          <EndpointPicker
            title="目标表"
            accent="var(--orange)"
            value={target}
            withTable
            onChange={setTarget}
            onLog={log}
            disabled={comparing || applying}
            dialectFilter={sourceDialect}
          />
        </div>

        <div className="row" style={{ marginTop: 12 }}>
          <div className="field">
            <label>单表最大比较行数</label>
            <input
              type="number"
              min={100}
              step={1000}
              value={rowLimit}
              onChange={(event) => setRowLimit(Number(event.target.value) || 200000)}
              style={{ width: 130 }}
            />
          </div>
          <div className="field">
            <label>INSERT 批量行数</label>
            <input
              type="number"
              min={1}
              max={1000}
              value={batchSize}
              onChange={(event) => setBatchSize(Number(event.target.value) || 200)}
              style={{ width: 110 }}
            />
          </div>
          <div className="grow" />
          <button className="btn primary" onClick={() => void runCompare()} disabled={!ready || comparing}>
            {comparing ? "对比中…" : "开始对比"}
          </button>
        </div>
      </div>

      {result && (
        <>
          <div className="section">
            <div className="stat-row">
              <div className="stat new">
                <span className="num">{result.summary.inserts}</span>
                <span className="cap">待插入</span>
              </div>
              <div className="stat changed">
                <span className="num">{result.summary.updates}</span>
                <span className="cap">待更新</span>
              </div>
              <div className="stat missing">
                <span className="num">{result.summary.deletes}</span>
                <span className="cap">待删除</span>
              </div>
              <div className="stat">
                <span className="num">{result.summary.unchanged}</span>
                <span className="cap">无差异</span>
              </div>
              <div className="stat">
                <span className="num">{result.summary.sourceRows}</span>
                <span className="cap">源表行数</span>
              </div>
              <div className="stat">
                <span className="num">{result.summary.targetRows}</span>
                <span className="cap">目标行数</span>
              </div>
            </div>

            <div className="row" style={{ marginTop: 12 }}>
              <span className="faint">匹配主键：</span>
              {result.keyColumns.map((pair) => (
                <span key={pair.target} className="badge purple">
                  {pair.source === pair.target ? pair.source : `${pair.source} → ${pair.target}`}
                </span>
              ))}
              <span className="faint" style={{ marginLeft: 8 }}>
                字段映射 {result.columns.length} 个
              </span>
            </div>

            <div className="row" style={{ marginTop: 10 }}>
              <label className="check">
                <input
                  type="checkbox"
                  checked={doInsert}
                  onChange={(event) => setDoInsert(event.target.checked)}
                />
                执行插入
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={doUpdate}
                  onChange={(event) => setDoUpdate(event.target.checked)}
                />
                执行更新
              </label>
              <label className="check">
                <input
                  type="checkbox"
                  checked={doDelete}
                  onChange={(event) => setDoDelete(event.target.checked)}
                />
                执行删除
              </label>
              <span className="faint">（删除默认关闭）</span>
              <label className="check">
                <input
                  type="checkbox"
                  checked={useTransaction}
                  onChange={(event) => setUseTransaction(event.target.checked)}
                />
                使用事务
              </label>
              <div className="grow" />
              <span className="faint">共 {selectedStatements} 行受影响</span>
              <button className="btn" onClick={() => setPreviewOpen(true)} disabled={selectedStatements === 0}>
                预览 SQL
              </button>
              <button
                className="btn primary"
                onClick={() => void doApply()}
                disabled={applying || selectedStatements === 0}
              >
                {applying ? "执行中…" : "执行同步"}
              </button>
            </div>

            {runResult && <RunResultPanel result={runResult} title="数据同步" />}
          </div>

          <div className="section" style={{ paddingBottom: 0 }}>
            <div className="row">
              <div className="pill-tabs">
                <button
                  className={changeTab === "insert" ? "active" : ""}
                  onClick={() => setChangeTab("insert")}
                >
                  新增 {result.summary.inserts}
                </button>
                <button
                  className={changeTab === "update" ? "active" : ""}
                  onClick={() => setChangeTab("update")}
                >
                  修改 {result.summary.updates}
                </button>
                <button
                  className={changeTab === "delete" ? "active" : ""}
                  onClick={() => setChangeTab("delete")}
                >
                  删除 {result.summary.deletes}
                </button>
              </div>
              <span className="faint">
                {result.previewTruncated
                  ? "明细仅展示前 200 行，执行时仍处理全部差异"
                  : `共 ${result.summary.inserts + result.summary.updates + result.summary.deletes} 行差异`}
              </span>
              <div className="grow" />
              <button className="btn small" onClick={() => void runCompare()} disabled={comparing}>
                重新对比
              </button>
            </div>
          </div>

          <div className="section" style={{ padding: 0, borderBottom: "none" }}>
            <RowChangeTable rows={previewRows} kind={changeTab} />
          </div>
        </>
      )}

      {!result && (
        <div className="empty">
          <IconCompare size={26} />
          <h2>还没有数据对比结果</h2>
          <p>
            选择源表与目标表后点击「开始对比」。工具会按主键（目标表主键，或同名字段）
            找出新增、修改与删除的行，并生成对应的 DML。
          </p>
        </div>
      )}

      {previewOpen && result && (
        <SqlPreviewModal
          title="数据同步 SQL 预览"
          statements={[
            ...(doDelete ? result.sqlPreview.filter((s) => s.startsWith("DELETE")) : []),
            ...(doUpdate ? result.sqlPreview.filter((s) => s.startsWith("UPDATE")) : []),
            ...(doInsert ? result.sqlPreview.filter((s) => s.startsWith("INSERT")) : []),
          ]}
          notice={`实际将执行 ${selectedStatements} 行变更；预览仅展示前 ${result.sqlPreview.length} 条语句`}
          onClose={() => setPreviewOpen(false)}
          onLog={log}
        />
      )}
    </div>
  );
}

function cellText(cell: { kind: string; text?: string | null } | undefined) {
  if (!cell || cell.kind === "null" || cell.text == null) return "NULL";
  return cell.text;
}

function RowChangeTable({ rows, kind }: { rows: RowChange[]; kind: ChangeTab }) {
  if (rows.length === 0) {
    return (
      <div style={{ padding: 22, textAlign: "center", color: "var(--text-faint)" }}>
        没有需要{kind === "insert" ? "新增" : kind === "update" ? "修改" : "删除"}的行
      </div>
    );
  }

  return (
    <div className="grid-wrap" style={{ maxHeight: 420 }}>
      <table className="difftable">
        <thead>
          <tr>
            <th style={{ width: 42 }}>#</th>
            <th style={{ width: 230 }}>主键</th>
            <th>变更内容</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => (
            <tr key={index}>
              <td className="faint">{index + 1}</td>
              <td>
                {row.key.map((key, keyIndex) => (
                  <div key={keyIndex} style={{ whiteSpace: "nowrap" }}>
                    <span className="faint">{key.column}=</span>
                    <span className="mono">{cellText(key.value)}</span>
                  </div>
                ))}
              </td>
              <td>
                {kind === "update"
                  ? row.cells.map((cell, cellIndex) => (
                      <div key={cellIndex} style={{ whiteSpace: "nowrap" }}>
                        <span className="faint">{cell.column}: </span>
                        <span className="before">{cellText(cell.before)}</span>
                        <span className="faint"> → </span>
                        <span className="after">{cellText(cell.after)}</span>
                      </div>
                    ))
                  : row.cells
                      .filter((cell) =>
                        kind === "insert" ? cell.after.kind !== "null" : cell.before.kind !== "null",
                      )
                      .map((cell, cellIndex) => (
                        <div key={cellIndex} style={{ whiteSpace: "nowrap" }}>
                          <span className="faint">{cell.column}: </span>
                          <span className={kind === "insert" ? "after" : "before"}>
                            {cellText(kind === "insert" ? cell.after : cell.before)}
                          </span>
                        </div>
                      ))}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {rows.length >= 200 && (
        <div className="faint" style={{ padding: "6px 10px", display: "flex", alignItems: "center", gap: 4 }}>
          <IconChevronRight size={11} /> 仅展示前 200 行，执行时会处理全部差异行
        </div>
      )}
    </div>
  );
}
