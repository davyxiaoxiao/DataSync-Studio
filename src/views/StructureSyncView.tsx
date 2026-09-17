import { useCallback, useMemo, useState } from "react";
import { api, errorMessage } from "../api";
import { EndpointPicker, type EndpointValue } from "../components/EndpointPicker";
import { JobProgress } from "../components/JobProgress";
import { RunResultPanel } from "../components/RunResultPanel";
import { SqlPreviewModal } from "../components/SqlPreviewModal";
import { IconChevronDown, IconChevronRight, IconEqual } from "../components/Icons";
import { useJobProgress } from "../jobs";
import { useStore } from "../store";
import type {
  ChangeKind,
  TypeDiff,
  ViewDiff,
  ColumnDiff,
  CompareSchemaResult,
  DdlStatement,
  ForeignKeyDiff,
  IndexDiff,
  SyncRunResult,
  SyncSelection,
  TableDiff,
} from "../types";

interface TableSel {
  enabled: boolean;
  wholeTable: boolean;
  columns: Record<string, boolean>;
  primaryKey: boolean;
  indexes: Record<string, boolean>;
  foreignKeys: Record<string, boolean>;
}

const KIND_LABEL: Record<ChangeKind, string> = {
  new: "新增",
  missing: "删除",
  changed: "修改",
  identical: "相同",
};

function changeTone(kind: ChangeKind): string | undefined {
  return kind === "identical" ? undefined : kind;
}

function diffDetail(differences: { field: string; source?: string | null; target?: string | null }[]) {
  return differences.map((d) => `${d.field}: ${d.target ?? "(无)"} → ${d.source ?? "(无)"}`).join("；");
}

export function StructureSyncView() {
  const { connections, log } = useStore();
  const job = useJobProgress();
  const [source, setSource] = useState<EndpointValue>({ connectionId: "", namespace: "" });
  const [target, setTarget] = useState<EndpointValue>({ connectionId: "", namespace: "" });
  const [ignoreComments, setIgnoreComments] = useState(true);
  const [ignoreCase, setIgnoreCase] = useState(false);

  // The source engine anchors the pair: only same-engine targets are offered.
  const sourceDialect = useMemo(
    () => connections.find((c) => c.id === source.connectionId)?.dialect,
    [connections, source.connectionId],
  );

  const [comparing, setComparing] = useState(false);
  const [comparison, setComparison] = useState<CompareSchemaResult | null>(null);
  const [selection, setSelection] = useState<Record<string, TableSel>>({});
  const [viewSelection, setViewSelection] = useState<Record<string, boolean>>({});
  const [typeSelection, setTypeSelection] = useState<Record<string, boolean>>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [filter, setFilter] = useState("");
  const [onlyChanges, setOnlyChanges] = useState(true);

  const [preview, setPreview] = useState<DdlStatement[] | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [applying, setApplying] = useState(false);
  const [runResult, setRunResult] = useState<SyncRunResult | null>(null);
  const [useTransaction, setUseTransaction] = useState(true);

  const diff = comparison?.diff ?? null;

  const buildViewSelection = useCallback(
    (result: CompareSchemaResult, mode: "safe" | "all" | "none") =>
      Object.fromEntries(
        result.diff.views
          .filter((view) => view.kind !== "identical")
          .map((view) => [
            view.name,
            mode === "all" ? true : mode === "none" ? false : view.kind !== "missing",
          ]),
      ),
    [],
  );

  const buildTypeSelection = useCallback(
    (result: CompareSchemaResult, mode: "safe" | "all" | "none") =>
      Object.fromEntries(
        result.diff.types
          .filter((entry) => entry.kind !== "identical")
          .map((entry) => [
            entry.name,
            mode === "all" ? true : mode === "none" ? false : entry.kind !== "missing",
          ]),
      ),
    [],
  );

  const buildSelection = useCallback(
    (result: CompareSchemaResult, mode: "safe" | "all" | "none"): Record<string, TableSel> => {
      const next: Record<string, TableSel> = {};
      for (const table of result.diff.tables) {
        if (table.kind === "identical") continue;
        const destructive = table.kind === "missing";
        const wholeTable = mode === "none" ? false : mode === "all" ? true : !destructive;
        const enabled = mode === "none" ? false : mode === "all" ? true : !destructive;
        const columns: Record<string, boolean> = {};
        for (const column of table.columns) {
          if (column.kind === "identical") continue;
          columns[column.name] = mode === "all" ? true : mode !== "none" && column.kind !== "missing";
        }
        const indexes: Record<string, boolean> = {};
        for (const index of table.indexes) {
          if (index.kind === "identical") continue;
          indexes[index.name] = mode === "all" ? true : mode !== "none" && index.kind !== "missing";
        }
        const foreignKeys: Record<string, boolean> = {};
        for (const fk of table.foreignKeys) {
          if (fk.kind === "identical") continue;
          foreignKeys[fk.name] = mode === "all" ? true : mode !== "none" && fk.kind !== "missing";
        }
        next[table.name] = {
          enabled,
          wholeTable,
          columns,
          primaryKey: mode === "all" ? true : mode !== "none",
          indexes,
          foreignKeys,
        };
      }
      return next;
    },
    [],
  );

  const runCompare = async () => {
    if (!source.connectionId || !source.namespace || !target.connectionId || !target.namespace) {
      log("warn", "请先选择源和目标连接、数据库");
      return;
    }
    setComparing(true);
    setRunResult(null);
    setComparison(null);
    setPreview(null);
    log("info", `开始对比结构：${source.namespace} → ${target.namespace} …`);
    const started = Date.now();
    const jobId = job.begin();
    try {
      const result = await api.compareSchema({
        sourceConnectionId: source.connectionId,
        sourceNamespace: source.namespace,
        targetConnectionId: target.connectionId,
        targetNamespace: target.namespace,
        ignoreComments,
        ignoreCase,
        compareCharset: false,
        jobId,
      });
      setComparison(result);
      setSelection(buildSelection(result, "safe"));
      setViewSelection(buildViewSelection(result, "safe"));
      setTypeSelection(buildTypeSelection(result, "safe"));
      for (const warning of result.warnings) log("warn", warning);
      const summary = result.diff.summary;
      log(
        "ok",
        `对比完成（${Date.now() - started} ms）：新建表 ${summary.newTables}，修改表 ${summary.changedTables}，仅目标存在 ${summary.missingTables}，` +
          `列变更 ${summary.newColumns + summary.changedColumns + summary.missingColumns}，索引变更 ${summary.indexChanges}，外键变更 ${summary.foreignKeyChanges}，` +
          `视图变更 ${summary.newViews + summary.changedViews + summary.missingViews}`,
      );
      const changed = result.diff.tables.filter((t) => t.kind !== "identical");
      setExpanded(Object.fromEntries(changed.slice(0, 8).map((t) => [t.name, true])));
    } catch (error) {
      const message = errorMessage(error);
      log(message.includes("已取消") ? "warn" : "err", `结构对比失败: ${message}`);
    } finally {
      job.end();
      setComparing(false);
    }
  };

  const selectionPayload = useMemo<SyncSelection | null>(() => {
    const tables = Object.entries(selection)
      .filter(([, sel]) => sel.enabled)
      .map(([name, sel]) => ({
        name,
        wholeTable: sel.wholeTable && sel.enabled,
        columns: Object.entries(sel.columns)
          .filter(([, v]) => v)
          .map(([k]) => k),
        primaryKey: sel.primaryKey,
        indexes: Object.entries(sel.indexes)
          .filter(([, v]) => v)
          .map(([k]) => k),
        foreignKeys: Object.entries(sel.foreignKeys)
          .filter(([, v]) => v)
          .map(([k]) => k),
      }));
    return {
      tables,
      views: Object.entries(viewSelection)
        .filter(([, enabled]) => enabled)
        .map(([name]) => name),
      types: Object.entries(typeSelection)
        .filter(([, enabled]) => enabled)
        .map(([name]) => name),
    };
  }, [selection, viewSelection, typeSelection]);

  const selectedCount = useMemo(
    () =>
      Object.values(viewSelection).filter(Boolean).length +
      Object.values(typeSelection).filter(Boolean).length +
      Object.values(selection)
        .filter((s) => s.enabled)
        .reduce((total, s) => {
          const base =
            Object.values(s.columns).filter(Boolean).length +
            Object.values(s.indexes).filter(Boolean).length +
            Object.values(s.foreignKeys).filter(Boolean).length +
            (s.primaryKey ? 1 : 0);
          return total + (s.wholeTable ? 1 : 0) + base;
        }, 0),
    [selection],
  );

  const doPreview = async () => {
    if (!comparison) return;
    setPreviewing(true);
    try {
      const outcome = await api.previewSchemaSql(comparison.compareId, selectionPayload);
      setPreview(outcome.statements);
      for (const warning of outcome.warnings) log("warn", warning);
      log("info", `已生成 ${outcome.statements.length} 条 DDL 语句`);
    } catch (error) {
      log("err", `生成 SQL 失败: ${errorMessage(error)}`);
    } finally {
      setPreviewing(false);
    }
  };

  const doApply = async () => {
    if (!comparison) return;
    setApplying(true);
    setRunResult(null);
    log("info", "开始执行结构同步…");
    const jobId = job.begin();
    try {
      const result = await api.applySchemaSync({
        compareId: comparison.compareId,
        selection: selectionPayload,
        useTransaction,
        jobId,
      });
      setRunResult(result);
      for (const warning of result.warnings ?? []) log("warn", warning);
      if (result.failed > 0) {
        log("err", result.message);
        for (const failure of result.failures.slice(0, 8)) {
          log("err", `${failure.sql} → ${failure.error}`);
        }
      } else {
        log("ok", result.message);
      }
    } catch (error) {
      log("err", `结构同步失败: ${errorMessage(error)}`);
    } finally {
      job.end();
      setApplying(false);
    }
  };

  const visibleTables = useMemo(() => {
    if (!diff) return [];
    const keyword = filter.trim().toLowerCase();
    return diff.tables
      .filter((table) => (onlyChanges ? table.kind !== "identical" : true))
      .filter(
        (table) =>
          !keyword ||
          table.name.toLowerCase().includes(keyword) ||
          table.columns.some((c) => c.name.toLowerCase().includes(keyword)),
      );
  }, [diff, filter, onlyChanges]);

  const visibleTypes = useMemo(() => {
    if (!diff) return [];
    const keyword = filter.trim().toLowerCase();
    return diff.types
      .filter((entry) => (onlyChanges ? entry.kind !== "identical" : true))
      .filter((entry) => !keyword || entry.name.toLowerCase().includes(keyword));
  }, [diff, filter, onlyChanges]);

  const visibleViews = useMemo(() => {
    if (!diff) return [];
    const keyword = filter.trim().toLowerCase();
    return diff.views
      .filter((view) => (onlyChanges ? view.kind !== "identical" : true))
      .filter((view) => !keyword || view.name.toLowerCase().includes(keyword));
  }, [diff, filter, onlyChanges]);

  const ready = Boolean(
    source.connectionId && source.namespace && target.connectionId && target.namespace,
  );

  const toggleTable = (name: string, enabled: boolean) =>
    setSelection((prev) => ({
      ...prev,
      [name]: { ...prev[name], enabled, wholeTable: prev[name].wholeTable && enabled },
    }));

  const togglePrimary = (name: string, value: boolean) =>
    setSelection((prev) => ({ ...prev, [name]: { ...prev[name], primaryKey: value } }));

  const toggleObject = (
    table: string,
    group: "columns" | "indexes" | "foreignKeys",
    key: string,
    value: boolean,
  ) =>
    setSelection((prev) => ({
      ...prev,
      [table]: { ...prev[table], [group]: { ...prev[table][group], [key]: value } },
    }));

  return (
    <div className="panel-scroll">
      <div className="section">
        <div className="section-title">
          同步方向{" "}
          <span className="hint">
            把「源」的表结构应用到「目标」；仅支持同类型数据库（MySQL ↔ MySQL、PostgreSQL ↔
            PostgreSQL），目标连接会按源的类型过滤
          </span>
        </div>
        <div className="split">
          <EndpointPicker
            title="源（结构基准）"
            accent="var(--green)"
            value={source}
            onChange={setSource}
            onLog={log}
            disabled={comparing || applying}
          />
          <EndpointPicker
            title="目标（将被修改）"
            accent="var(--orange)"
            value={target}
            onChange={setTarget}
            onLog={log}
            disabled={comparing || applying}
            dialectFilter={sourceDialect}
          />
        </div>

        <div className="row" style={{ marginTop: 12 }}>
          <label className="check">
            <input
              type="checkbox"
              checked={ignoreComments}
              onChange={(event) => setIgnoreComments(event.target.checked)}
            />
            忽略注释差异
          </label>
          <label className="check">
            <input
              type="checkbox"
              checked={ignoreCase}
              onChange={(event) => setIgnoreCase(event.target.checked)}
            />
            表名忽略大小写
          </label>
          <div className="grow" />
          <button className="btn primary" onClick={() => void runCompare()} disabled={!ready || comparing}>
            {comparing ? "对比中…" : "开始对比"}
          </button>
        </div>
      </div>

      {diff && (
        <>
          <div className="section">
            <div className="row">
              <div className="stat-row grow">
                <div className="stat new">
                  <span className="num">{diff.summary.newTables}</span>
                  <span className="cap">待新建表</span>
                </div>
                <div className="stat changed">
                  <span className="num">{diff.summary.changedTables}</span>
                  <span className="cap">待修改表</span>
                </div>
                <div className="stat missing">
                  <span className="num">{diff.summary.missingTables}</span>
                  <span className="cap">仅目标存在</span>
                </div>
                <div className="stat">
                  <span className="num">{diff.summary.identicalTables}</span>
                  <span className="cap">结构一致</span>
                </div>
                <div className="stat">
                  <span className="num">
                    {diff.summary.newColumns + diff.summary.changedColumns}
                  </span>
                  <span className="cap">列新增/修改</span>
                </div>
                <div className="stat">
                  <span className="num">
                    {diff.summary.indexChanges + diff.summary.foreignKeyChanges}
                  </span>
                  <span className="cap">索引/外键变更</span>
                </div>
                <div className="stat">
                  <span className="num">
                    {diff.summary.newViews +
                      diff.summary.changedViews +
                      diff.summary.missingViews}
                  </span>
                  <span className="cap">视图变更</span>
                </div>
                <div className="stat">
                  <span className="num">
                    {diff.summary.newTypes +
                      diff.summary.changedTypes +
                      diff.summary.missingTypes}
                  </span>
                  <span className="cap">自定义类型</span>
                </div>
              </div>
            </div>

            <div className="row" style={{ marginTop: 12 }}>
              <button
                className="btn small"
                onClick={() => {
                  setSelection(buildSelection(comparison!, "safe"));
                  setViewSelection(buildViewSelection(comparison!, "safe"));
                  setTypeSelection(buildTypeSelection(comparison!, "safe"));
                }}
              >
                默认选择（不含删除）
              </button>
              <button
                className="btn small"
                onClick={() => {
                  setSelection(buildSelection(comparison!, "all"));
                  setViewSelection(buildViewSelection(comparison!, "all"));
                  setTypeSelection(buildTypeSelection(comparison!, "all"));
                }}
              >
                全选
              </button>
              <button
                className="btn small"
                onClick={() => {
                  setSelection(buildSelection(comparison!, "none"));
                  setViewSelection(buildViewSelection(comparison!, "none"));
                  setTypeSelection(buildTypeSelection(comparison!, "none"));
                }}
              >
                全不选
              </button>
              <span className="faint">已选 {selectedCount} 项变更</span>
              <div className="grow" />
              <label
                className="check"
                title="PostgreSQL 支持 DDL 事务，任一条失败会整体回滚；MySQL 的 DDL 会隐式提交，无法回滚，失败时会立即停止"
              >
                <input
                  type="checkbox"
                  checked={useTransaction}
                  onChange={(event) => setUseTransaction(event.target.checked)}
                />
                整体事务（失败即回滚）
              </label>
              <button className="btn" onClick={() => void doPreview()} disabled={previewing || selectedCount === 0}>
                {previewing ? "生成中…" : "预览 SQL"}
              </button>
              <button className="btn primary" onClick={() => void doApply()} disabled={applying || selectedCount === 0}>
                {applying ? "执行中…" : "执行同步"}
              </button>
            </div>

            {(job.active || job.update) && (
              <JobProgress
                update={job.update}
                running={job.active}
                onDismiss={job.dismiss}
                onLog={log}
              />
            )}

            {runResult && <RunResultPanel result={runResult} title="结构同步" />}

            <div className="row" style={{ marginTop: 10 }}>
              <input
                type="text"
                placeholder="过滤表名 / 字段名…"
                value={filter}
                onChange={(event) => setFilter(event.target.value)}
                style={{ width: 220 }}
              />
              <label className="check">
                <input
                  type="checkbox"
                  checked={onlyChanges}
                  onChange={(event) => setOnlyChanges(event.target.checked)}
                />
                只显示有差异的表
              </label>
              <div className="grow" />
              <button className="btn small" onClick={() => void runCompare()} disabled={comparing}>
                重新对比
              </button>
            </div>
          </div>

          <div className="section" style={{ padding: 0, borderBottom: "none" }}>
            {visibleTables.length === 0 && (
              <div style={{ padding: 22, textAlign: "center", color: "var(--text-faint)" }}>
                没有符合条件的表
              </div>
            )}
            {visibleTypes.length > 0 && (
              <div className="node">
                <div className="node-head" style={{ background: "var(--bg-subtle)" }}>
                  <span style={{ width: 20 }} />
                  <span className="name">自定义类型（enum / domain）</span>
                  <span className="detail">{visibleTypes.length} 个有差异</span>
                </div>
                <div className="node-body" style={{ paddingLeft: 30 }}>
                  {visibleTypes.map((entry) => (
                    <TypeDiffRow
                      key={entry.name}
                      entry={entry}
                      checked={typeSelection[entry.name] ?? false}
                      onChange={(value) =>
                        setTypeSelection((prev) => ({ ...prev, [entry.name]: value }))
                      }
                    />
                  ))}
                </div>
              </div>
            )}

            {visibleViews.length > 0 && (
              <div className="node">
                <div className="node-head" style={{ background: "var(--bg-subtle)" }}>
                  <span style={{ width: 20 }} />
                  <span className="name">视图 / 物化视图</span>
                  <span className="detail">{visibleViews.length} 个有差异</span>
                </div>
                <div className="node-body" style={{ paddingLeft: 30 }}>
                  {visibleViews.map((view) => (
                    <ViewDiffRow
                      key={view.name}
                      view={view}
                      checked={viewSelection[view.name] ?? false}
                      onChange={(value) =>
                        setViewSelection((prev) => ({ ...prev, [view.name]: value }))
                      }
                    />
                  ))}
                </div>
              </div>
            )}

            {visibleTables.map((table) => (
              <TableDiffNode
                key={table.name}
                table={table}
                sel={selection[table.name]}
                expanded={expanded[table.name] ?? false}
                onToggleExpand={() =>
                  setExpanded((prev) => ({ ...prev, [table.name]: !prev[table.name] }))
                }
                onToggleTable={(enabled) => toggleTable(table.name, enabled)}
                onTogglePrimary={(value) => togglePrimary(table.name, value)}
                onToggleObject={(group, key, value) => toggleObject(table.name, group, key, value)}
              />
            ))}
          </div>
        </>
      )}

      {!diff && (
        <div className="empty">
          <IconEqual size={26} />
          <h2>还没有对比结果</h2>
          <p>选择源与目标数据库后点击「开始对比」，这里会列出所有结构差异。</p>
        </div>
      )}

      {preview && (
        <SqlPreviewModal
          title="结构同步 SQL 预览"
          statements={preview.map((statement) => statement.sql)}
          onClose={() => setPreview(null)}
          onLog={log}
        />
      )}
    </div>
  );
}

interface NodeProps {
  table: TableDiff;
  sel: TableSel | undefined;
  expanded: boolean;
  onToggleExpand: () => void;
  onToggleTable: (enabled: boolean) => void;
  onTogglePrimary: (value: boolean) => void;
  onToggleObject: (
    group: "columns" | "indexes" | "foreignKeys",
    key: string,
    value: boolean,
  ) => void;
}

function TableDiffNode({
  table,
  sel,
  expanded,
  onToggleExpand,
  onToggleTable,
  onTogglePrimary,
  onToggleObject,
}: NodeProps) {
  const wholeTable = table.kind === "new" || table.kind === "missing";
  const changedObjects =
    table.columns.filter((c) => c.kind !== "identical").length +
    table.indexes.filter((i) => i.kind !== "identical").length +
    table.foreignKeys.filter((f) => f.kind !== "identical").length +
    (table.primaryKey.kind !== "identical" ? 1 : 0);

  return (
    <div className="node">
      <div className="node-head">
        {!wholeTable && (
          <button className="icon-btn" onClick={onToggleExpand} title={expanded ? "收起" : "展开"}>
            {expanded ? <IconChevronDown size={12} /> : <IconChevronRight size={12} />}
          </button>
        )}
        {wholeTable && <span style={{ width: 20 }} />}

        <input
          type="checkbox"
          checked={sel?.enabled ?? false}
          onChange={(event) => onToggleTable(event.target.checked)}
          style={{ accentColor: "var(--accent)" }}
        />

        <span className="name" style={{ fontFamily: "var(--mono)" }}>
          {table.name}
        </span>
        <span className={`badge ${changeTone(table.kind) ?? ""}`}>{KIND_LABEL[table.kind]}</span>
        {table.targetName && table.targetName !== table.name && (
          <span className="detail">→ {table.targetName}</span>
        )}
        {!wholeTable && <span className="detail">{changedObjects} 项差异</span>}
        {wholeTable && (
          <span className="detail">
            {table.kind === "new" ? "将在目标库创建整表" : "将从目标库删除整表"}
          </span>
        )}
      </div>

      {expanded && !wholeTable && sel && (
        <div className="node-body">
          {table.columns
            .filter((c) => c.kind !== "identical")
            .map((column) => (
              <ObjectRow
                key={`col-${column.name}`}
                name={column.name}
                kind={column.kind}
                checked={sel.columns[column.name] ?? false}
                onChange={(value) => onToggleObject("columns", column.name, value)}
                detail={columnDetail(column)}
              />
            ))}

          {table.primaryKey.kind !== "identical" && (
            <ObjectRow
              name="PRIMARY KEY"
              kind={table.primaryKey.kind}
              checked={sel.primaryKey}
              onChange={onTogglePrimary}
              prefix="PK"
              detail={`${table.primaryKey.target.join(", ") || "(无)"} → ${
                table.primaryKey.source.join(", ") || "(无)"
              }`}
            />
          )}

          {table.indexes
            .filter((i) => i.kind !== "identical")
            .map((index) => (
              <ObjectRow
                key={`idx-${index.name}`}
                name={index.name}
                kind={index.kind}
                checked={sel.indexes[index.name] ?? false}
                onChange={(value) => onToggleObject("indexes", index.name, value)}
                detail={indexDetail(index)}
                prefix={index.source?.isUnique || index.target?.isUnique ? "UNIQ" : "IDX"}
              />
            ))}

          {table.foreignKeys
            .filter((f) => f.kind !== "identical")
            .map((fk) => (
              <ObjectRow
                key={`fk-${fk.name}`}
                name={fk.name}
                kind={fk.kind}
                checked={sel.foreignKeys[fk.name] ?? false}
                onChange={(value) => onToggleObject("foreignKeys", fk.name, value)}
                detail={fkDetail(fk)}
                prefix="FOREIGN KEY"
              />
            ))}
        </div>
      )}
    </div>
  );
}

function columnDetail(column: ColumnDiff) {
  if (column.kind === "new") return column.source?.fullType ?? "";
  if (column.kind === "missing") return column.target?.fullType ?? "";
  return diffDetail(column.differences);
}

function indexDetail(index: IndexDiff) {
  if (index.kind === "new") {
    return `${index.source?.columns.join(", ") ?? ""}${index.source?.isUnique ? " · 唯一" : ""}`;
  }
  if (index.kind === "missing") return index.target?.columns.join(", ") ?? "";
  return diffDetail(index.differences);
}

function fkDetail(fk: ForeignKeyDiff) {
  const describe = (value: typeof fk.source) =>
    value ? `${value.columns.join(", ")} → ${value.refTable}(${value.refColumns.join(", ")})` : "";
  if (fk.kind === "new") return describe(fk.source);
  if (fk.kind === "missing") return describe(fk.target);
  return diffDetail(fk.differences);
}

interface ObjectRowProps {
  name: string;
  kind: ChangeKind;
  checked: boolean;
  onChange: (value: boolean) => void;
  detail: string;
  prefix?: string;
}

function ObjectRow({ name, kind, checked, onChange, detail, prefix }: ObjectRowProps) {
  return (
    <div className="obj-row">
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
        style={{ accentColor: "var(--accent)", marginLeft: 0 }}
      />
      <span className="badge" style={{ minWidth: 34, justifyContent: "center" }}>
        {prefix ?? ""}
      </span>
      <span className="obj-name">{name}</span>
      <span className={`badge ${changeTone(kind) ?? ""}`}>{KIND_LABEL[kind]}</span>
      <span className="obj-detail" title={detail}>
        {detail}
      </span>
    </div>
  );
}

function ViewDiffRow({
  view,
  checked,
  onChange,
}: {
  view: ViewDiff;
  checked: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <div className="obj-row" title={view.differences.map((d) => d.field).join(", ")}>
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
        style={{ accentColor: "var(--accent)", marginLeft: 0 }}
      />
      <span className="badge" style={{ minWidth: 34, justifyContent: "center" }}>
        {view.source?.kind === "materialized" ? "MAT" : "VIEW"}
      </span>
      <span className="obj-name">{view.name}</span>
      <span className={`badge ${changeTone(view.kind) ?? ""}`}>{KIND_LABEL[view.kind]}</span>
      <span className="obj-detail">
        {view.kind === "changed" && view.differences.length > 0
          ? `变更：${view.differences.map((d) => d.field).join("、")}`
          : ""}
      </span>
    </div>
  );
}

function TypeDiffRow({
  entry,
  checked,
  onChange,
}: {
  entry: TypeDiff;
  checked: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <div className="obj-row" title={entry.differences.map((d) => d.field).join(", ")}>
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
        style={{ accentColor: "var(--accent)", marginLeft: 0 }}
      />
      <span className="badge" style={{ minWidth: 34, justifyContent: "center" }}>
        {entry.source?.kind === "domain" ? "DOM" : "ENUM"}
      </span>
      <span className="obj-name">{entry.name}</span>
      <span className={`badge ${changeTone(entry.kind) ?? ""}`}>{KIND_LABEL[entry.kind]}</span>
      <span className="obj-detail">
        {entry.kind === "changed"
          ? `变更：${entry.differences.map((d) => d.field).join("、")}`
          : (entry.source ?? entry.target)?.labels.join(", ") ?? ""}
      </span>
    </div>
  );
}
