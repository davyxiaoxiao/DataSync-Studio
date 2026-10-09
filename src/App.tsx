import { useCallback, useMemo, useState } from "react";
import { api, errorMessage } from "./api";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { ConnectionDialog } from "./components/ConnectionDialog";
import { DropTablesDialog } from "./components/DropTablesDialog";
import { ExcelExportDialog } from "./components/ExcelExportDialog";
import { ExcelImportDialog } from "./components/ExcelImportDialog";
import { LogPanel } from "./components/LogPanel";
import { Sidebar, type TableRef } from "./components/Sidebar";
import { TableEditorDialog } from "./components/TableEditorDialog";
import { TabBar } from "./components/TabBar";
import { Toolbar } from "./components/Toolbar";
import { StoreProvider, useStore } from "./store";
import type { ConnectionConfig } from "./types";
import { DataSyncView } from "./views/DataSyncView";
import { StructureSyncView } from "./views/StructureSyncView";
import { QueryView } from "./views/QueryView";
import { TableDataView } from "./views/TableDataView";
import { WelcomeView } from "./views/WelcomeView";

interface DialogState {
  open: boolean;
  config: ConnectionConfig | null;
}

/// One place for every table-level dialog, so only one can be open at a time.
type TableAction =
  | { kind: "designer"; ref: TableRef }
  | { kind: "rename"; ref: TableRef }
  | { kind: "drop"; refs: TableRef[] }
  | { kind: "importExcel"; ref: TableRef }
  | {
      kind: "renameQuery";
      query: { connectionId: string; namespace: string; name: string; path: string };
    }
  | { kind: "exportExcel"; ref: TableRef }
  | { kind: "deleteConnection"; config: ConnectionConfig };

function RenameQueryDialog({
  query,
  onClose,
  onDone,
  onLog,
}: {
  query: { name: string; path: string };
  onClose: () => void;
  onDone: (renamed: { name: string; path: string }) => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}) {
  const [name, setName] = useState(query.name);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const renamed = await api.renameQuery(query.path, name.trim());
      onLog("ok", `已重命名为 ${renamed.name}`);
      onDone({ name: renamed.name, path: renamed.path });
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `重命名失败: ${errorMessage(err)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <ConfirmDialog
      title="重命名查询"
      busy={busy}
      error={error}
      confirmLabel="重命名"
      onClose={onClose}
      onConfirm={() => {
        if (name.trim() && name.trim() !== query.name) void submit();
      }}
      message={
        <div className="field">
          <label>
            将 <span className="mono">{query.name}</span> 重命名为
          </label>
          <input
            type="text"
            value={name}
            autoFocus
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && name.trim() && name.trim() !== query.name) {
                void submit();
              }
            }}
          />
          <div className="faint" style={{ marginTop: 6 }}>
            会同时重命名磁盘上的 .sql 文件
          </div>
        </div>
      }
    />
  );
}

function RenameTableDialog({
  target,
  onClose,
  onDone,
  onLog,
}: {
  target: TableRef;
  onClose: () => void;
  onDone: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}) {
  const [name, setName] = useState(target.table);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const outcome = await api.renameTable({
        connectionId: target.connectionId,
        namespace: target.namespace,
        table: target.table,
        newName: name.trim(),
      });
      if (outcome.failed > 0) {
        setError(outcome.failures[0]?.error ?? outcome.message);
        onLog("err", outcome.message);
      } else {
        onLog("ok", `已将 ${target.table} 重命名为 ${name.trim()}`);
        onDone();
      }
    } catch (err) {
      setError(errorMessage(err));
      onLog("err", `重命名失败: ${errorMessage(err)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <ConfirmDialog
      title="重命名表"
      busy={busy}
      error={error}
      confirmLabel="重命名"
      onClose={onClose}
      onConfirm={() => {
        if (name.trim() && name.trim() !== target.table) void submit();
      }}
      message={
        <div className="field">
          <label>
            将 <span className="mono">{target.namespace}.{target.table}</span> 重命名为
          </label>
          <input
            type="text"
            value={name}
            autoFocus
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && name.trim() && name.trim() !== target.table) {
                void submit();
              }
            }}
          />
        </div>
      }
    />
  );
}

function Shell() {
  const {
    connections,
    tabs,
    updateTabTitle,
    activeTabId,
    selectedConnectionId,
    log,
    reloadConnections,
    connect,
    closeTabsForConnection,
    closeTab,
    openTab,
    loadQueries,
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

  const [tableAction, setTableAction] = useState<TableAction | null>(null);

  const removeConnection = useCallback(
    (config: ConnectionConfig) =>
      setTableAction({ kind: "deleteConnection", config }),
    [],
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
          onDeleteConnection={removeConnection}
          onEditTable={(ref) => setTableAction({ kind: "designer", ref })}
          onRenameTable={(ref) => setTableAction({ kind: "rename", ref })}
          onDropTables={(refs) => setTableAction({ kind: "drop", refs })}
          onImportExcel={(ref) => setTableAction({ kind: "importExcel", ref })}
          onExportExcel={(ref) => setTableAction({ kind: "exportExcel", ref })}
          onRenameQuery={(query) => setTableAction({ kind: "renameQuery", query })}
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
                {tab.kind === "query" && tab.connectionId && tab.namespace && tab.path && (
                  <QueryView
                    connectionId={tab.connectionId}
                    namespace={tab.namespace}
                    path={tab.path}
                    onLog={log}
                    onTitleChange={(title, dirty) => {
                      // The unsaved marker lives in the tab title.
                      updateTabTitle(tab.id, dirty ? `${title} •` : title);
                    }}
                  />
                )}
              </div>
            ))}
          </div>
          <LogPanel />
        </div>
      </div>

      {tableAction?.kind === "deleteConnection" && (
        <ConfirmDialog
          title="删除连接"
          danger
          confirmLabel="删除"
          message={
            <div>
              确定要删除连接「{tableAction.config.name}」吗？
              <div className="faint" style={{ marginTop: 6 }}>
                该操作会同时删除已保存的密码，且不可恢复。
              </div>
            </div>
          }
          onClose={() => setTableAction(null)}
          onConfirm={async () => {
            const config = tableAction.config;
            setTableAction(null);
            try {
              await api.deleteConnection(config.id);
              closeTabsForConnection(config.id);
              await reloadConnections();
              log("ok", `已删除连接 ${config.name}`);
            } catch (error) {
              log("err", `删除连接失败: ${errorMessage(error)}`);
            }
          }}
        />
      )}

      {tableAction?.kind === "rename" && (
        <RenameTableDialog
          target={tableAction.ref}
          onClose={() => setTableAction(null)}
          onDone={() => {
            const target = tableAction.ref;
            // The tab still points at the old name.
            for (const tab of tabs) {
              if (
                tab.kind === "table" &&
                tab.connectionId === target.connectionId &&
                tab.namespace === target.namespace &&
                tab.table === target.table
              ) {
                closeTab(tab.id);
              }
            }
            setTableAction(null);
            window.dispatchEvent(new CustomEvent("datasync:refresh-tables"));
          }}
          onLog={log}
        />
      )}

      {tableAction?.kind === "drop" && (
        <DropTablesDialog
          refs={tableAction.refs}
          onClose={() => setTableAction(null)}
          onDone={() => {
            for (const ref of tableAction.refs) {
              for (const tab of tabs) {
                if (
                  tab.kind === "table" &&
                  tab.connectionId === ref.connectionId &&
                  tab.namespace === ref.namespace &&
                  tab.table === ref.table
                ) {
                  closeTab(tab.id);
                }
              }
            }
            setTableAction(null);
            window.dispatchEvent(new CustomEvent("datasync:refresh-tables"));
          }}
          onLog={log}
        />
      )}

      {tableAction?.kind === "importExcel" && (
        <ExcelImportDialog
          target={tableAction.ref}
          onClose={() => setTableAction(null)}
          onImported={() => {
            // The table's contents changed, so an open grid should reload.
            window.dispatchEvent(new CustomEvent("datasync:refresh-data"));
          }}
          onLog={log}
        />
      )}

      {tableAction?.kind === "renameQuery" && (
        <RenameQueryDialog
          query={tableAction.query}
          onClose={() => setTableAction(null)}
          onDone={(renamed) => {
            setTableAction(null);
            // Reopen under the new name so the tab is not left pointing at a
            // file that no longer exists.
            closeTab(`query:${tableAction.query.path}`);
            openTab({
              kind: "query",
              title: renamed.name,
              connectionId: tableAction.query.connectionId,
              namespace: tableAction.query.namespace,
              path: renamed.path,
            });
            void loadQueries(tableAction.query.connectionId, tableAction.query.namespace);
          }}
          onLog={log}
        />
      )}

      {tableAction?.kind === "exportExcel" && (
        <ExcelExportDialog
          target={tableAction.ref}
          onClose={() => setTableAction(null)}
          onLog={log}
        />
      )}

      {tableAction?.kind === "designer" && (
        <TableEditorDialog
          connectionId={tableAction.ref.connectionId}
          namespace={tableAction.ref.namespace}
          table={tableAction.ref.table}
          onClose={() => setTableAction(null)}
          onSaved={() => {
            window.dispatchEvent(new CustomEvent("datasync:refresh-tables"));
          }}
          onLog={log}
        />
      )}

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
