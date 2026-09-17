import { useStore } from "../store";
import type { ConnectionConfig } from "../types";
import {
  IconCompare,
  IconDatabase,
  IconEdit,
  IconMinus,
  IconPlus,
  IconRefresh,
  IconSpinner,
} from "./Icons";

interface Props {
  onNewConnection: () => void;
  selectedConnection: ConnectionConfig | null;
  onEditConnection: (config: ConnectionConfig) => void;
}

export function Toolbar({ onNewConnection, selectedConnection, onEditConnection }: Props) {
  const {
    connections,
    sessions,
    openTab,
    toggleConnection,
    disconnect,
    reloadConnections,
    log,
  } = useStore();
  const connectedCount = Object.values(sessions).filter((s) => s.status === "connected").length;
  const connecting = Object.values(sessions).some((s) => s.status === "connecting");
  const current = selectedConnection ?? connections[0] ?? null;
  const status = current ? sessions[current.id]?.status ?? "idle" : "idle";

  return (
    <div className="toolbar">
      <div className="brand">
        <IconDatabase size={15} />
        DataSync Studio
      </div>

      <button className="tbtn" onClick={onNewConnection} title="新建数据库连接">
        <span className="icon">
          <IconPlus size={13} />
        </span>
        新建连接
      </button>

      <button
        className="tbtn"
        disabled={!current}
        onClick={() => current && onEditConnection(current)}
        title="编辑当前连接"
      >
        <span className="icon">
          <IconEdit size={13} />
        </span>
        编辑
      </button>

      <button
        className="tbtn"
        disabled={!current || status === "connecting"}
        onClick={() => {
          if (!current) return;
          if (status === "connected") {
            void disconnect(current.id);
          } else {
            void toggleConnection(current.id);
          }
        }}
      >
        <span className={status === "connecting" ? "icon spin" : "icon"}>
          {status === "connecting" ? <IconSpinner size={13} /> : <IconDatabase size={13} />}
        </span>
        {status === "connected" ? "断开" : "连接"}
      </button>

      <button className="tbtn" onClick={() => void reloadConnections()} title="重新加载连接配置">
        <span className="icon">
          <IconRefresh size={13} />
        </span>
        刷新
      </button>

      <div className="sep" />

      <button
        className="tbtn"
        onClick={() => openTab({ kind: "structure", title: "结构同步" })}
        title="对比并同步表结构"
      >
        <span className="icon">
          <IconCompare size={13} />
        </span>
        结构同步
      </button>

      <button
        className="tbtn"
        onClick={() => openTab({ kind: "data", title: "数据同步" })}
        title="对比并同步表数据"
      >
        <span className="icon">
          <IconCompare size={13} />
        </span>
        数据同步
      </button>

      <div className="spacer" />

      <span className="muted" style={{ fontSize: 11.5 }}>
        {connections.length} 个连接 · 已连接 {connectedCount}
      </span>
      {(connecting || connectedCount > 0) && (
        <button
          className="tbtn"
          onClick={() => {
            for (const config of connections) {
              if (sessions[config.id]?.status === "connected") void disconnect(config.id);
            }
            log("info", "已断开全部连接");
          }}
        >
          <span className="icon">
            <IconMinus size={13} />
          </span>
          全部断开
        </button>
      )}
    </div>
  );
}
