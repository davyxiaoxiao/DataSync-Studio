import { useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { useStore, dialectLabel } from "../store";
import type { ConnectionConfig } from "../types";
import {
  IconChevronDown,
  IconChevronRight,
  IconDatabase,
  IconEdit,
  IconFolder,
  IconPlus,
  IconPlug,
  IconRefresh,
  IconTable,
  IconUnplug,
  IconView,
} from "./Icons";

export interface TableRef {
  connectionId: string;
  namespace: string;
  table: string;
}

interface Props {
  onNewConnection: () => void;
  onEditConnection: (config: ConnectionConfig) => void;
  onDeleteConnection: (config: ConnectionConfig) => void;
  onEditTable: (ref: TableRef) => void;
  onRenameTable: (ref: TableRef) => void;
  onDropTables: (refs: TableRef[]) => void;
  onImportExcel: (ref: TableRef) => void;
  onExportExcel: (ref: TableRef) => void;
}

const refKey = (ref: TableRef) => `${ref.connectionId}::${ref.namespace}::${ref.table}`;
const sameScope = (ref: TableRef, other: TableRef) =>
  ref.connectionId === other.connectionId && ref.namespace === other.namespace;

export interface MenuItem {
  label: string;
  danger?: boolean;
  disabled?: boolean;
  /** A leading glyph, used for the import submenu's file type. */
  hint?: string;
  /** When present the entry opens a nested list instead of running. */
  children?: MenuItem[];
  run?: () => void;
}

interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

export function Sidebar({
  onNewConnection,
  onEditConnection,
  onDeleteConnection,
  onEditTable,
  onRenameTable,
  onDropTables,
  onImportExcel,
  onExportExcel,
}: Props) {
  const {
    connections,
    sessions,
    log,
    toggleConnection,
    toggleNamespace,
    refreshTables,
    disconnect,
    openTab,
    reloadConnections,
    selectedConnectionId,
    selectConnection,
  } = useStore();
  const [filter, setFilter] = useState("");
  const [menu, setMenu] = useState<MenuState | null>(null);
  // Tables are selected within one connection+namespace, which is what a batch
  // operation can act on.
  const [selectedTables, setSelectedTables] = useState<TableRef[]>([]);
  const lastClicked = useRef<string | null>(null);

  const visible = useMemo(() => {
    const keyword = filter.trim().toLowerCase();
    if (!keyword) return connections;
    return connections.filter((config) => {
      if (config.name.toLowerCase().includes(keyword)) return true;
      if (`${config.host}:${config.port}`.toLowerCase().includes(keyword)) return true;
      const session = sessions[config.id];
      return session?.namespaces.some((ns) =>
        ns.tables.some((table) => table.name.toLowerCase().includes(keyword)),
      );
    });
  }, [connections, filter, sessions]);

  // A rename or drop elsewhere in the app invalidates the cached table list, so
  // every namespace that is already loaded is refreshed.
  useEffect(() => {
    const onRefresh = () => {
      for (const config of connections) {
        const session = sessions[config.id];
        if (!session) continue;
        for (const ns of session.namespaces) {
          if (ns.loaded) void refreshTables(config.id, ns.name);
        }
      }
    };
    window.addEventListener("datasync:refresh-tables", onRefresh);
    return () => window.removeEventListener("datasync:refresh-tables", onRefresh);
  }, [connections, sessions, refreshTables]);

  const openMenu = (
    event: MouseEvent,
    items: MenuState["items"],
  ) => {
    event.preventDefault();
    event.stopPropagation();
    setMenu({ x: event.clientX, y: event.clientY, items });
  };

  /** Visible tables of a namespace, in tree order — the basis for range select. */
  const visibleTablesOf = (connectionId: string, namespace: string): TableRef[] => {
    const session = sessions[connectionId];
    const node = session?.namespaces.find((ns) => ns.name === namespace);
    if (!node) return [];
    const keyword = filter.trim().toLowerCase();
    return node.tables
      .filter((table) => !keyword || table.name.toLowerCase().includes(keyword))
      .filter((table) => !table.tableType.toUpperCase().includes("VIEW"))
      .map((table) => ({ connectionId, namespace, table: table.name }));
  };

  const handleTableClick = (ref: TableRef, event: MouseEvent) => {
    const key = refKey(ref);
    if (event.metaKey || event.ctrlKey) {
      setSelectedTables((prev) => {
        const scope = prev.filter((entry) => sameScope(entry, ref));
        return scope.some((entry) => refKey(entry) === key)
          ? scope.filter((entry) => refKey(entry) !== key)
          : [...scope, ref];
      });
    } else if (event.shiftKey && lastClicked.current) {
      const ordered = visibleTablesOf(ref.connectionId, ref.namespace);
      const from = ordered.findIndex((entry) => refKey(entry) === lastClicked.current);
      const to = ordered.findIndex((entry) => refKey(entry) === key);
      if (from >= 0 && to >= 0) {
        const [start, end] = from <= to ? [from, to] : [to, from];
        setSelectedTables(ordered.slice(start, end + 1));
        return;
      }
      setSelectedTables([ref]);
    } else {
      setSelectedTables([ref]);
      // Preserve the long-standing behaviour: a plain click also opens the data.
      openTab({
        kind: "table",
        title: ref.table,
        connectionId: ref.connectionId,
        namespace: ref.namespace,
        table: ref.table,
      });
    }
    lastClicked.current = key;
  };

  /** The tables a context-menu action applies to. */
  const actionTargets = (ref: TableRef): TableRef[] => {
    const key = refKey(ref);
    const inSelection = selectedTables.some((entry) => refKey(entry) === key);
    return inSelection && selectedTables.length > 1 ? selectedTables : [ref];
  };

  const tableMenu = (ref: TableRef) => {
    const targets = actionTargets(ref);
    const many = targets.length > 1;
    return [
      {
        label: "浏览数据",
        run: () =>
          openTab({
            kind: "table",
            title: ref.table,
            connectionId: ref.connectionId,
            namespace: ref.namespace,
            table: ref.table,
          }),
      },
      { label: "编辑表…", disabled: many, run: () => onEditTable(ref) },
      { label: "重命名…", disabled: many, run: () => onRenameTable(ref) },
      {
        label: many ? `删除选中的 ${targets.length} 张表` : "删除表",
        danger: true,
        run: () => onDropTables(targets),
      },
      {
        label: "导入",
        disabled: many,
        children: [
          {
            label: "从 Excel 导入…",
            hint: "xlsx",
            run: () => onImportExcel(ref),
          },
          {
            label: "CSV 导入",
            disabled: true,
            run: () => undefined,
          },
        ],
      },
      {
        label: "导出",
        disabled: many,
        children: [
          {
            label: "导出 Excel…",
            hint: "xlsx",
            run: () => onExportExcel(ref),
          },
          {
            label: "CSV 导出",
            disabled: true,
            run: () => undefined,
          },
        ],
      },
      {
        label: "作为数据同步源…",
        disabled: many,
        run: () =>
          openTab({
            kind: "data",
            title: "数据同步",
            connectionId: ref.connectionId,
            namespace: ref.namespace,
            table: ref.table,
          }),
      },
    ];
  };

  const connectionMenu = (config: ConnectionConfig) => {
    const session = sessions[config.id];
    const connected = session?.status === "connected";
    return [
      {
        label: connected ? "断开连接" : "连接",
        run: () => (connected ? void disconnect(config.id) : void toggleConnection(config.id)),
      },
      {
        label: "在编辑器中打开",
        disabled: !connected,
        run: () => {
          openTab({
            kind: "data",
            title: `数据同步 · ${config.name}`,
            connectionId: config.id,
          });
        },
      },
      {
        label: "结构同步…",
        run: () => openTab({ kind: "structure", title: "结构同步" }),
      },
      { label: "编辑连接…", run: () => onEditConnection(config) },
      { label: "删除连接", danger: true, run: () => onDeleteConnection(config) },
    ];
  };

  return (
    <div className="sidebar" onClick={() => menu && setMenu(null)}>
      <div className="sidebar-head">
        <span>连接</span>
        <div className="head-actions">
          <button className="icon-btn" title="新建连接" onClick={onNewConnection}>
            <IconPlus size={13} />
          </button>
          <button
            className="icon-btn"
            title="刷新连接列表"
            onClick={() => void reloadConnections()}
          >
            <IconRefresh size={12} />
          </button>
        </div>
      </div>

      <div style={{ padding: "6px 8px 4px" }}>
        <input
          type="text"
          placeholder="搜索连接或表…"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
          style={{ width: "100%", height: 24 }}
        />
      </div>

      <div className="tree">
        {visible.length === 0 && (
          <div className="tree-empty">
            还没有连接配置
            <br />
            <button className="btn small" style={{ marginTop: 10 }} onClick={onNewConnection}>
              新建连接
            </button>
          </div>
        )}

        {visible.map((config) => {
          const session = sessions[config.id];
          const status = session?.status ?? "idle";
          const expanded = session?.expanded ?? false;
          const keyword = filter.trim().toLowerCase();

          return (
            <div key={config.id}>
              <div
                className={`tnode ${selectedConnectionId === config.id ? "selected" : ""}`}
                style={{ paddingLeft: 6 }}
                onClick={() => selectConnection(config.id)}
                onDoubleClick={() => void toggleConnection(config.id)}
                onContextMenu={(event) => openMenu(event, connectionMenu(config))}
                title={`${config.username}@${config.host}:${config.port}${config.database ? `/${config.database}` : ""}`}
              >
                <span
                  className="twisty"
                  onClick={(event) => {
                    event.stopPropagation();
                    void toggleConnection(config.id);
                  }}
                >
                  {expanded ? <IconChevronDown size={12} /> : <IconChevronRight size={12} />}
                </span>
                <span className={`dot ${status === "connected" ? "on" : "off"}`} />
                <span style={{ color: config.color ?? "var(--accent)", display: "inline-flex" }}>
                  <IconDatabase size={13} />
                </span>
                <span className="label">{config.name}</span>
                <span className="meta">{dialectLabel(config.dialect)}</span>
                {status === "connecting" && <span className="meta">连接中…</span>}
                {status === "error" && (
                  <span className="meta err-text" title={session?.error}>
                    失败
                  </span>
                )}
                <span className="fav">
                  <button
                    className="icon-btn"
                    title="编辑"
                    onClick={(event) => {
                      event.stopPropagation();
                      onEditConnection(config);
                    }}
                  >
                    <IconEdit size={11} />
                  </button>
                </span>
              </div>

              {expanded && status === "error" && (
                <div
                  className="tnode"
                  style={{ paddingLeft: 32, color: "var(--red)", height: "auto", padding: "3px 8px 6px 32px" }}
                >
                  {session?.error}
                </div>
              )}

              {expanded &&
                status === "connected" &&
                session?.namespaces
                  .filter(
                    (ns) =>
                      !keyword ||
                      ns.name.toLowerCase().includes(keyword) ||
                      ns.tables.some((table) => table.name.toLowerCase().includes(keyword)),
                  )
                  .map((ns) => (
                    <div key={ns.name}>
                      <div
                        className="tnode"
                        style={{ paddingLeft: 22 }}
                        onClick={() => toggleNamespace(config.id, ns.name)}
                        onContextMenu={(event) =>
                          openMenu(event, [
                            {
                              label: "刷新表列表",
                              run: () => void refreshTables(config.id, ns.name),
                            },
                            {
                              label: "结构同步…",
                              run: () =>
                                openTab({
                                  kind: "structure",
                                  title: "结构同步",
                                }),
                            },
                            {
                              label: "数据同步…",
                              run: () =>
                                openTab({
                                  kind: "data",
                                  title: "数据同步",
                                }),
                            },
                          ])
                        }
                      >
                        <span className="twisty">
                          {ns.expanded ? <IconChevronDown size={12} /> : <IconChevronRight size={12} />}
                        </span>
                        <span style={{ color: "#c99700", display: "inline-flex" }}>
                          <IconFolder size={13} />
                        </span>
                        <span className="label">{ns.name}</span>
                        {ns.loading && <span className="meta">载入中…</span>}
                        {ns.loaded && <span className="meta">{ns.tables.length}</span>}
                      </div>

                      {ns.expanded &&
                        ns.tables
                          .filter(
                            (table) =>
                              !keyword || table.name.toLowerCase().includes(keyword),
                          )
                          .map((table) => {
                            const isView = table.tableType.toUpperCase().includes("VIEW");
                            return (
                              <div
                                key={table.name}
                                className={`tnode ${
                                  !isView &&
                                  selectedTables.some(
                                    (entry) =>
                                      entry.connectionId === config.id &&
                                      entry.namespace === ns.name &&
                                      entry.table === table.name,
                                  )
                                    ? "selected"
                                    : ""
                                }`}
                                style={{ paddingLeft: 42 }}
                                title={
                                  isView
                                    ? (table.comment ?? table.name)
                                    : `${table.comment ?? table.name}\n单击=浏览数据，⌘/Ctrl+单击=多选，Shift+单击=范围选择，右键=更多操作`
                                }
                                onClick={(event) => {
                                  if (isView) {
                                    log(
                                      "info",
                                      `视图 ${ns.name}.${table.name} 暂不支持数据浏览`,
                                    );
                                    return;
                                  }
                                  handleTableClick(
                                    {
                                      connectionId: config.id,
                                      namespace: ns.name,
                                      table: table.name,
                                    },
                                    event,
                                  );
                                }}
                                onContextMenu={(event) =>
                                  isView
                                    ? openMenu(event, [
                                        {
                                          label: "视图暂不支持编辑",
                                          disabled: true,
                                          run: () => undefined,
                                        },
                                      ])
                                    : openMenu(
                                        event,
                                        tableMenu({
                                          connectionId: config.id,
                                          namespace: ns.name,
                                          table: table.name,
                                        }),
                                      )
                                }
                              >
                                <span className="twisty leaf" />
                                <span
                                  style={{
                                    color: isView ? "var(--purple)" : "#4a7ebb",
                                    display: "inline-flex",
                                  }}
                                >
                                  {isView ? <IconView size={13} /> : <IconTable size={13} />}
                                </span>
                                <span className="label">{table.name}</span>
                                {table.rows != null && table.rows >= 0 && (
                                  <span className="meta">≈{table.rows}</span>
                                )}
                              </div>
                            );
                          })}
                    </div>
                  ))}
            </div>
          );
        })}
      </div>

      <div className="statusbar" style={{ borderTop: "1px solid var(--border)" }}>
        {selectedTables.length > 1 ? (
          <span className="badge accent" title="右键可批量删除">
            已选中 {selectedTables.length} 张表
          </span>
        ) : (
          <span>{connections.length} 个连接</span>
        )}
        <div className="spacer" />
        <button
          className="icon-btn"
          title="全部断开"
          onClick={() => {
            for (const config of connections) {
              if (sessions[config.id]?.status === "connected") void disconnect(config.id);
            }
          }}
        >
          <IconUnplug size={12} />
        </button>
        <button
          className="icon-btn"
          title="连接全部"
          onClick={() => {
            for (const config of connections) {
              if (sessions[config.id]?.status !== "connected") void toggleConnection(config.id);
            }
          }}
        >
          <IconPlug size={12} />
        </button>
      </div>

      {menu && (
        <>
          <div
            style={{ position: "fixed", inset: 0, zIndex: 200 }}
            onClick={() => setMenu(null)}
            onContextMenu={(event) => {
              event.preventDefault();
              setMenu(null);
            }}
          />
          <div
            style={{
              position: "fixed",
              left: menu.x,
              top: menu.y,
              zIndex: 201,
              minWidth: 168,
              background: "#fff",
              border: "1px solid var(--border-strong)",
              borderRadius: 7,
              boxShadow: "0 10px 26px rgba(15,20,28,.18)",
              padding: 4,
            }}
          >
            {menu.items.map((item, index) =>
              item.children ? (
                <div key={index} className="menu-entry has-children">
                  <div className="menu-row">
                    <span>{item.label}</span>
                    <span className="faint">▸</span>
                  </div>
                  <div className="menu-sub">
                    {item.children.map((child, childIndex) => (
                      <div
                        key={childIndex}
                        className={`menu-entry ${child.disabled ? "disabled" : ""}`}
                        onClick={() => {
                          if (child.disabled || !child.run) return;
                          setMenu(null);
                          child.run();
                        }}
                      >
                        <div className="menu-row">
                          <span>{child.label}</span>
                          {child.hint && <span className="faint mono">{child.hint}</span>}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              ) : (
                <div
                  key={index}
                  className={`menu-entry ${item.disabled ? "disabled" : ""} ${
                    item.danger ? "danger" : ""
                  }`}
                  onClick={() => {
                    if (item.disabled || !item.run) return;
                    setMenu(null);
                    item.run();
                  }}
                >
                  <div className="menu-row">
                    <span>{item.label}</span>
                  </div>
                </div>
              ),
            )}
          </div>
        </>
      )}
    </div>
  );
}
