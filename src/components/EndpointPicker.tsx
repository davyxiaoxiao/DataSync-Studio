import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorMessage } from "../api";
import { useStore, dialectLabel } from "../store";
import type { NamespaceInfo, TableSummary } from "../types";
import { IconRefresh } from "./Icons";

export interface EndpointValue {
  connectionId: string;
  namespace: string;
  table?: string;
}

interface Props {
  title: string;
  accent: string;
  value: EndpointValue;
  withTable?: boolean;
  disabled?: boolean;
  onChange: (value: EndpointValue) => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

export function EndpointPicker({
  title,
  accent,
  value,
  withTable,
  disabled,
  onChange,
  onLog,
}: Props) {
  const { connections } = useStore();
  const [namespaces, setNamespaces] = useState<Record<string, NamespaceInfo[]>>({});
  const [tables, setTables] = useState<Record<string, TableSummary[]>>({});
  const [loadingNamespaces, setLoadingNamespaces] = useState(false);
  const [loadingTables, setLoadingTables] = useState(false);

  const connection = useMemo(
    () => connections.find((c) => c.id === value.connectionId) ?? null,
    [connections, value.connectionId],
  );

  const loadNamespaces = useCallback(
    async (connectionId: string, force = false) => {
      if (!connectionId) return;
      if (!force && namespaces[connectionId]) return;
      setLoadingNamespaces(true);
      try {
        const list = await api.listNamespaces(connectionId);
        setNamespaces((prev) => ({ ...prev, [connectionId]: list }));
      } catch (error) {
        onLog("err", `读取数据库列表失败: ${errorMessage(error)}`);
      } finally {
        setLoadingNamespaces(false);
      }
    },
    [namespaces, onLog],
  );

  const loadTables = useCallback(
    async (connectionId: string, namespace: string, force = false) => {
      const key = `${connectionId}::${namespace}`;
      if (!connectionId || !namespace) return;
      if (!force && tables[key]) return;
      setLoadingTables(true);
      try {
        const list = await api.listTables(connectionId, namespace);
        setTables((prev) => ({ ...prev, [key]: list }));
      } catch (error) {
        onLog("err", `读取表列表失败: ${errorMessage(error)}`);
      } finally {
        setLoadingTables(false);
      }
    },
    [tables, onLog],
  );

  // Pick a sensible default connection on first render.
  useEffect(() => {
    if (!value.connectionId && connections.length > 0) {
      onChange({ connectionId: connections[0].id, namespace: "", table: undefined });
    }
  }, [connections, onChange, value.connectionId]);

  useEffect(() => {
    if (value.connectionId) void loadNamespaces(value.connectionId);
  }, [value.connectionId, loadNamespaces]);

  const available = namespaces[value.connectionId];

  // Default the namespace once the list arrives.
  useEffect(() => {
    if (!available || value.namespace) return;
    const preferred =
      available.find((ns) => !ns.isSystem && ns.name === connection?.database) ??
      available.find((ns) => !ns.isSystem) ??
      available[0];
    if (preferred) {
      onChange({
        connectionId: value.connectionId,
        namespace: preferred.name,
        table: undefined,
      });
    }
  }, [available, connection?.database, onChange, value.connectionId, value.namespace]);

  useEffect(() => {
    if (withTable && value.connectionId && value.namespace) {
      void loadTables(value.connectionId, value.namespace);
    }
  }, [withTable, value.connectionId, value.namespace, loadTables]);

  const tableList = tables[`${value.connectionId}::${value.namespace}`] ?? [];

  return (
    <div className="card" style={{ borderLeft: `3px solid ${accent}` }}>
      <div className="card-title">
        <span style={{ color: accent }}>{title}</span>
        {connection && <span className="badge accent">{dialectLabel(connection.dialect)}</span>}
        <div style={{ flex: 1 }} />
        <button
          className="icon-btn"
          title="刷新列表"
          disabled={disabled || !value.connectionId}
          onClick={() => {
            if (!value.connectionId) return;
            void loadNamespaces(value.connectionId, true);
            if (value.namespace) void loadTables(value.connectionId, value.namespace, true);
          }}
        >
          <IconRefresh size={12} />
        </button>
      </div>

      <div className="row">
        <select
          value={value.connectionId}
          disabled={disabled}
          onChange={(event) =>
            onChange({ connectionId: event.target.value, namespace: "", table: undefined })
          }
          style={{ minWidth: 150 }}
        >
          <option value="">选择连接…</option>
          {connections.map((config) => (
            <option key={config.id} value={config.id}>
              {config.name}
            </option>
          ))}
        </select>

        <select
          value={value.namespace}
          disabled={disabled || !value.connectionId || loadingNamespaces}
          onChange={(event) =>
            onChange({ ...value, namespace: event.target.value, table: undefined })
          }
          style={{ minWidth: 150 }}
        >
          <option value="">
            {loadingNamespaces ? "载入中…" : connection?.dialect === "postgres" ? "选择模式…" : "选择数据库…"}
          </option>
          {(available ?? []).map((ns) => (
            <option key={ns.name} value={ns.name}>
              {ns.name}
              {ns.isSystem ? "（系统）" : ""}
            </option>
          ))}
        </select>

        {withTable && (
          <select
            value={value.table ?? ""}
            disabled={disabled || !value.namespace || loadingTables}
            onChange={(event) => onChange({ ...value, table: event.target.value })}
            style={{ minWidth: 170 }}
          >
            <option value="">{loadingTables ? "载入中…" : "选择表…"}</option>
            {tableList
              .filter((table) => !table.tableType.toUpperCase().includes("VIEW"))
              .map((table) => (
                <option key={table.name} value={table.name}>
                  {table.name}
                </option>
              ))}
          </select>
        )}
      </div>
    </div>
  );
}
