import { useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { useStore, dialectLabel, type NamespaceNode } from "../store";
import { api, errorMessage } from "../api";
import { ContextMenu, menuAt, type MenuState } from "./ContextMenu";
import type { ConnectionConfig } from "../types";
import {
  IconChevronDown,
  IconChevronRight,
  IconDatabase,
  IconEdit,
  IconFile,
  IconFolder,
  IconPlus,
  IconPlug,
  IconRefresh,
  IconTable,
  IconUnplug,
  IconView,
} from "./Icons";

let scratchSeq = 0;

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
  onRenameQuery: (query: {
    connectionId: string;
    namespace: string;
    name: string;
    path: string;
  }) => void;
}

const refKey = (ref: TableRef) => `${ref.connectionId}::${ref.namespace}::${ref.table}`;
const sameScope = (ref: TableRef, other: TableRef) =>
  ref.connectionId === other.connectionId && ref.namespace === other.namespace;

export function Sidebar({
  onNewConnection,
  onEditConnection,
  onDeleteConnection,
  onEditTable,
  onRenameTable,
  onDropTables,
  onImportExcel,
  onExportExcel,
  onRenameQuery,
}: Props) {
  const {
    connections,
    sessions,
    log,
    toggleConnection,
    toggleNamespace,
    toggleNamespaceGroup,
    loadQueries,
    selectNamespace,
    refreshTables,
    disconnect,
    openTab,
    closeTab,
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

  const keyword = filter.trim().toLowerCase();

  const visible = useMemo(() => {
    if (!keyword) return connections;
    return connections.filter((config) => {
      if (config.name.toLowerCase().includes(keyword)) return true;
      if (`${config.host}:${config.port}`.toLowerCase().includes(keyword)) return true;
      const session = sessions[config.id];
      return session?.namespaces.some((ns) =>
        ns.tables.some((table) => table.name.toLowerCase().includes(keyword)),
      );
    });
  }, [connections, keyword, sessions]);

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

  const openMenu = (event: MouseEvent, items: MenuState["items"]) =>
    setMenu(menuAt(event, items));

  /** A temporary query: nothing is written until the user saves it. */
  const openScratchQuery = (connectionId: string, namespace: string) => {
    scratchSeq += 1;
    openTab({
      id: `query:scratch:${scratchSeq}`,
      kind: "query",
      title: `临时查询 ${scratchSeq}`,
      connectionId,
      namespace,
    });
  };

  /** Non-view tables of a namespace, honouring the search box. */
  const tablesOf = (ns: NamespaceNode) =>
    ns.tables
      .filter((table) => !table.tableType.toUpperCase().includes("VIEW"))
      .filter((table) => !keyword || table.name.toLowerCase().includes(keyword));

  const viewsOf = (ns: NamespaceNode) =>
    ns.tables
      .filter((table) => table.tableType.toUpperCase().includes("VIEW"))
      .filter((table) => !keyword || table.name.toLowerCase().includes(keyword));

  /** One of the 表 / 视图 / 查询 folder rows. */
  const groupRow = (options: {
    label: string;
    count: number;
    expanded: boolean;
    indent: number;
    onToggle: () => void;
    menu: MenuState["items"];
  }) => (
    <div
      className="tnode"
      style={{ paddingLeft: options.indent }}
      onClick={options.onToggle}
      onContextMenu={(event) => openMenu(event, options.menu)}
    >
      <span className="twisty">
        {options.expanded ? <IconChevronDown size={12} /> : <IconChevronRight size={12} />}
      </span>
      <span style={{ color: "#c99700", display: "inline-flex" }}>
        <IconFolder size={13} />
      </span>
      <span className="label">{options.label}</span>
      <span className="meta">{options.count}</span>
    </div>
  );

  const revealQueries = async (connectionId: string) => {
    try {
      const dir = await api.queriesDirectory(connectionId);
      const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
      await revealItemInDir(dir);
    } catch (error) {
      log("err", `打开查询目录失败: ${errorMessage(error)}`);
    }
  };

  const queryMenu = (
    connectionId: string,
    namespace: string,
    query: { name: string; path: string },
  ): MenuState["items"] => [
    {
      label: "打开",
      run: () =>
        openTab({
          kind: "query",
          title: query.name,
          connectionId,
          namespace,
          path: query.path,
        }),
    },
    {
      label: "重命名…",
      // The rename dialog lives in App; `window.prompt` is not dependable inside
      // a webview.
      run: () =>
        onRenameQuery({ connectionId, namespace, name: query.name, path: query.path }),
    },
    {
      label: "复制一份",
      run: () =>
        void (async () => {
          try {
            const copy = await api.duplicateQuery({ path: query.path });
            await loadQueries(connectionId, namespace);
            log("ok", `已复制为 ${copy.name}`);
          } catch (error) {
            log("err", `复制失败: ${errorMessage(error)}`);
          }
        })(),
    },
    {
      label: "在访达中显示",
      run: () => void revealQueries(connectionId),
    },
    {
      label: "删除",
      danger: true,
      run: () =>
        void (async () => {
          try {
            await api.deleteQuery({ path: query.path });
            closeTab(`query:${query.path}`);
            await loadQueries(connectionId, namespace);
            log("ok", `已删除查询 ${query.name}`);
          } catch (error) {
            log("err", `删除失败: ${errorMessage(error)}`);
          }
        })(),
    },
  ];

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
                        onClick={() => {
                          selectConnection(config.id);
                          selectNamespace(config.id, ns.name);
                          toggleNamespace(config.id, ns.name);
                        }}
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

                      {ns.expanded && (
                        <>
                          {groupRow({
                            label: "表",
                            count: tablesOf(ns).length,
                            expanded: ns.tablesExpanded,
                            indent: 42,
                            onToggle: () =>
                              toggleNamespaceGroup(config.id, ns.name, "tables"),
                            menu: [
                              {
                                label: "刷新表列表",
                                run: () => void refreshTables(config.id, ns.name),
                              },
                            ],
                          })}
                          {ns.tablesExpanded &&
                            tablesOf(ns).map((table) => (
                              <div
                                key={table.name}
                                className={`tnode ${
                                  selectedTables.some(
                                    (entry) =>
                                      entry.connectionId === config.id &&
                                      entry.namespace === ns.name &&
                                      entry.table === table.name,
                                  )
                                    ? "selected"
                                    : ""
                                }`}
                                style={{ paddingLeft: 58 }}
                                title={`${table.comment ?? table.name}\n单击=浏览数据，⌘/Ctrl+单击=多选，Shift+单击=范围选择，右键=更多操作`}
                                onClick={(event) =>
                                  handleTableClick(
                                    {
                                      connectionId: config.id,
                                      namespace: ns.name,
                                      table: table.name,
                                    },
                                    event,
                                  )
                                }
                                onContextMenu={(event) =>
                                  openMenu(
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
                                <span style={{ color: "#4a7ebb", display: "inline-flex" }}>
                                  <IconTable size={13} />
                                </span>
                                <span className="label">{table.name}</span>
                                {table.rows != null && table.rows >= 0 && (
                                  <span className="meta">≈{table.rows}</span>
                                )}
                              </div>
                            ))}

                          {groupRow({
                            label: "视图",
                            count: viewsOf(ns).length,
                            expanded: ns.viewsExpanded,
                            indent: 42,
                            onToggle: () =>
                              toggleNamespaceGroup(config.id, ns.name, "views"),
                            menu: [
                              {
                                label: "刷新表列表",
                                run: () => void refreshTables(config.id, ns.name),
                              },
                            ],
                          })}
                          {ns.viewsExpanded &&
                            viewsOf(ns).map((view) => (
                              <div
                                key={view.name}
                                className="tnode"
                                style={{ paddingLeft: 58 }}
                                title={view.comment ?? view.name}
                                onClick={() =>
                                  log(
                                    "info",
                                    `视图 ${ns.name}.${view.name} 暂不支持数据浏览，可用结构同步处理`,
                                  )
                                }
                                onContextMenu={(event) =>
                                  openMenu(event, [
                                    {
                                      label: "视图暂不支持编辑",
                                      disabled: true,
                                      run: () => undefined,
                                    },
                                  ])
                                }
                              >
                                <span className="twisty leaf" />
                                <span style={{ color: "var(--purple)", display: "inline-flex" }}>
                                  <IconView size={13} />
                                </span>
                                <span className="label">{view.name}</span>
                              </div>
                            ))}

                          {groupRow({
                            label: "查询",
                            count: ns.queries.length,
                            expanded: ns.queriesExpanded,
                            indent: 42,
                            onToggle: () =>
                              toggleNamespaceGroup(config.id, ns.name, "queries"),
                            menu: [
                              {
                                label: "新建查询",
                                run: () => {
                                  selectConnection(config.id);
                                  selectNamespace(config.id, ns.name);
                                  openScratchQuery(config.id, ns.name);
                                },
                              },
                              {
                                label: "刷新查询列表",
                                run: () => void loadQueries(config.id, ns.name),
                              },
                              {
                                label: "在访达中打开查询目录",
                                run: () => void revealQueries(config.id),
                              },
                            ],
                          })}
                          {ns.queriesExpanded &&
                            (ns.queries.length === 0 ? (
                              <div
                                className="tnode"
                                style={{ paddingLeft: 58 }}
                                onClick={() => {
                                  selectConnection(config.id);
                                  selectNamespace(config.id, ns.name);
                                  openScratchQuery(config.id, ns.name);
                                }}
                                title="查询在保存后才会出现在这里；点击打开一个临时查询"
                              >
                                <span className="faint">还没有保存的查询</span>
                              </div>
                            ) : (
                              ns.queries.map((query) => (
                                <div
                                  key={query.id}
                                  className="tnode"
                                  style={{ paddingLeft: 58 }}
                                  title={`${query.path}\n双击打开编辑`}
                                  onClick={() =>
                                    openTab({
                                      kind: "query",
                                      title: query.name,
                                      connectionId: config.id,
                                      namespace: ns.name,
                                      path: query.path,
                                    })
                                  }
                                  onContextMenu={(event) =>
                                    openMenu(event, queryMenu(config.id, ns.name, query))
                                  }
                                >
                                  <span className="twisty leaf" />
                                  <span style={{ color: "#c99700", display: "inline-flex" }}>
                                    <IconFile size={13} />
                                  </span>
                                  <span className="label">{query.name}</span>
                                </div>
                              ))
                            ))}
                        </>
                      )}
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

      {menu && <ContextMenu state={menu} onClose={() => setMenu(null)} />}
    </div>
  );
}
