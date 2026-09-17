import { useCallback, useMemo, useState } from "react";
import { api, errorMessage } from "./api";
import { ConnectionDialog } from "./components/ConnectionDialog";
import { LogPanel } from "./components/LogPanel";
import { Sidebar } from "./components/Sidebar";
import { TabBar } from "./components/TabBar";
import { Toolbar } from "./components/Toolbar";
import { StoreProvider, useStore } from "./store";
import type { ConnectionConfig } from "./types";
import { DataSyncView } from "./views/DataSyncView";
import { StructureSyncView } from "./views/StructureSyncView";
import { TableDataView } from "./views/TableDataView";
import { WelcomeView } from "./views/WelcomeView";

interface DialogState {
  open: boolean;
  config: ConnectionConfig | null;
}

function Shell() {
  const {
    connections,
    tabs,
    activeTabId,
    selectedConnectionId,
    log,
    reloadConnections,
    connect,
    closeTabsForConnection,
  } = useStore();

  const [dialog, setDialog] = useState<DialogState>({ open: false, config: null });

  const selectedConnection = useMemo(
    () =>
      connections.find((config) => config.id === selectedConnectionId) ??
      connections[0] ??
      null,
    [connections, selectedConnectionId],
  );

  const openNew = useCallback(() => setDialog({ open: true, config: null }), []);
  const openEdit = useCallback(
    (config: ConnectionConfig) => setDialog({ open: true, config }),
    [],
  );

  const removeConnection = useCallback(
    async (config: ConnectionConfig) => {
      const confirmed = window.confirm(
        `确定要删除连接「${config.name}」吗？该操作会同时删除已保存的密码。`,
      );
      if (!confirmed) return;
      try {
        await api.deleteConnection(config.id);
        closeTabsForConnection(config.id);
        await reloadConnections();
        log("ok", `已删除连接 ${config.name}`);
      } catch (error) {
        log("err", `删除连接失败: ${errorMessage(error)}`);
      }
    },
    [closeTabsForConnection, log, reloadConnections],
  );

  return (
    <div className="app">
      <Toolbar
        onNewConnection={openNew}
        selectedConnection={selectedConnection}
        onEditConnection={openEdit}
      />

      <div className="app-body">
        <Sidebar
          onNewConnection={openNew}
          onEditConnection={openEdit}
          onDeleteConnection={(config) => void removeConnection(config)}
        />

        <div className="main-area">
          <TabBar />
          <div className="view-host">
            {tabs.map((tab) => (
              <div
                key={tab.id}
                className="view-slot"
                style={{ display: tab.id === activeTabId ? "flex" : "none" }}
              >
                {tab.kind === "welcome" && <WelcomeView />}
                {tab.kind === "structure" && <StructureSyncView />}
                {tab.kind === "data" && (
                  <DataSyncView
                    key={`${tab.id}:${tab.connectionId ?? ""}:${tab.namespace ?? ""}:${tab.table ?? ""}`}
                    initial={
                      tab.connectionId && tab.namespace && tab.table
                        ? {
                            connectionId: tab.connectionId,
                            namespace: tab.namespace,
                            table: tab.table,
                          }
                        : undefined
                    }
                  />
                )}
                {tab.kind === "table" && tab.connectionId && tab.namespace && tab.table && (
                  <TableDataView
                    connectionId={tab.connectionId}
                    namespace={tab.namespace}
                    table={tab.table}
                    onLog={log}
                  />
                )}
              </div>
            ))}
          </div>
          <LogPanel />
        </div>
      </div>

      {dialog.open && (
        <ConnectionDialog
          initial={dialog.config}
          onClose={() => setDialog({ open: false, config: null })}
          onSaved={(config, isNew) => {
            setDialog({ open: false, config: null });
            log("ok", `${isNew ? "已保存新连接" : "已更新连接"} ${config.name}`);
            void reloadConnections().then(() => {
              if (isNew) void connect(config.id);
            });
          }}
        />
      )}
    </div>
  );
}

export default function App() {
  return (
    <StoreProvider>
      <Shell />
    </StoreProvider>
  );
}
