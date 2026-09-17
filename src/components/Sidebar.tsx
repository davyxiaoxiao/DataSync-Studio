import { useMemo, useState, type MouseEvent } from "react";
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

interface Props {
  onNewConnection: () => void;
  onEditConnection: (config: ConnectionConfig) => void;
  onDeleteConnection: (config: ConnectionConfig) => void;
}

interface MenuState {
  x: number;
  y: number;
  items: { label: string; danger?: boolean; disabled?: boolean; run: () => void }[];
}

export function Sidebar({ onNewConnection, onEditConnection, onDeleteConnection }: Props) {
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

  const openMenu = (
    event: MouseEvent,
    items: MenuState["items"],
  ) => {
    event.preventDefault();
    event.stopPropagation();
    setMenu({ x: event.clientX, y: event.clientY, items });
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
                                className="tnode"
                                style={{ paddingLeft: 42 }}
                                title={table.comment ?? table.name}
                                onClick={() => {
                                  if (isView) {
                                    log("warn", "视图暂不支持数据浏览");
                                    return;
                                  }
                                  openTab({
                                    kind: "table",
                                    title: table.name,
                                    connectionId: config.id,
                                    namespace: ns.name,
                                    table: table.name,
                                  });
                                }}
                                onContextMenu={(event) =>
                                  openMenu(event, [
                                    {
                                      label: "浏览数据",
                                      run: () =>
                                        openTab({
                                          kind: "table",
                                          title: table.name,
                                          connectionId: config.id,
                                          namespace: ns.name,
                                          table: table.name,
                                        }),
                                    },
                                    {
                                      label: "作为数据同步源…",
                                      run: () =>
                                        openTab({
                                          kind: "data",
                                          title: "数据同步",
                                          connectionId: config.id,
                                          namespace: ns.name,
                                          table: table.name,
                                        }),
                                    },
                                  ])
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
        <span>{connections.length} 个连接</span>
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
            {menu.items.map((item, index) => (
              <div
                key={index}
                onClick={() => {
                  if (item.disabled) return;
                  setMenu(null);
                  item.run();
                }}
                style={{
                  padding: "5px 10px",
                  borderRadius: 5,
                  cursor: "default",
                  color: item.disabled
                    ? "var(--text-faint)"
                    : item.danger
                      ? "var(--red)"
                      : "var(--text)",
                  background: "transparent",
                }}
                onMouseEnter={(event) => {
                  if (!item.disabled) event.currentTarget.style.background = "var(--bg-hover)";
                }}
                onMouseLeave={(event) => {
                  event.currentTarget.style.background = "transparent";
                }}
              >
                {item.label}
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
