import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api, errorMessage } from "./api";
import type {
  SavedQuery, ConnectionConfig, Dialect, NamespaceInfo } from "./types";

export type LogLevel = "info" | "ok" | "warn" | "err" | "sql";

export interface LogEntry {
  id: number;
  level: LogLevel;
  message: string;
  time: string;
}

export interface TableNode {
  name: string;
  tableType: string;
  comment?: string | null;
  rows?: number | null;
}

export interface NamespaceNode {
  name: string;
  loaded: boolean;
  loading: boolean;
  tables: TableNode[];
  expanded: boolean;
  /** Whether the 表 / 视图 / 查询 group folders are open. */
  tablesExpanded: boolean;
  viewsExpanded: boolean;
  queriesExpanded: boolean;
  queriesLoaded: boolean;
  queries: SavedQuery[];
}

export interface Session {
  connectionId: string;
  status: "idle" | "connecting" | "connected" | "error";
  serverVersion?: string;
  error?: string;
  expanded: boolean;
  namespaces: NamespaceNode[];
}

export type TabKind = "welcome" | "table" | "structure" | "data" | "query";

export interface Tab {
  id: string;
  kind: TabKind;
  title: string;
  connectionId?: string;
  namespace?: string;
  table?: string;
  /** For `kind: "query"`: the `.sql` file the tab edits. */
  path?: string;
}

interface StoreValue {
  connections: ConnectionConfig[];
  sessions: Record<string, Session>;
  logs: LogEntry[];
  tabs: Tab[];
  activeTabId: string;
  logOpen: boolean;
  selectedConnectionId: string | null;

  log: (level: LogLevel, message: string) => void;
  clearLogs: () => void;
  setLogOpen: (open: boolean) => void;

  reloadConnections: () => Promise<void>;
  connect: (connectionId: string) => Promise<void>;
  disconnect: (connectionId: string) => Promise<void>;
  toggleConnection: (connectionId: string) => void;
  toggleNamespace: (connectionId: string, namespace: string) => void;
  refreshTables: (connectionId: string, namespace: string) => Promise<void>;
  /** Open or close one of the 表 / 视图 / 查询 folders inside a database. */
  toggleNamespaceGroup: (
    connectionId: string,
    namespace: string,
    group: "tables" | "views" | "queries",
  ) => void;
  /** Re-read the .sql files belonging to a connection. */
  loadQueries: (connectionId: string, namespace: string) => Promise<void>;

  selectConnection: (id: string | null) => void;
  /** The database the toolbar's 查询 button acts on. */
  selectedNamespace: { connectionId: string; namespace: string } | null;
  selectNamespace: (connectionId: string, namespace: string) => void;
  closeTabsForConnection: (connectionId: string) => void;
  openTab: (tab: Omit<Tab, "id"> & { id?: string }) => string;
  closeTab: (id: string) => void;
  /** Rename a tab in place, e.g. to show an unsaved marker. */
  updateTabTitle: (id: string, title: string) => void;
  /** Keep only this tab. */
  closeOtherTabs: (id: string) => void;
  /** Close everything, leaving a welcome tab so the window is never blank. */
  closeAllTabs: () => void;
  setActiveTab: (id: string) => void;
}

const StoreContext = createContext<StoreValue | null>(null);

let logSeq = 0;

function nowLabel() {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function StoreProvider({ children }: { children: ReactNode }) {
  const [connections, setConnections] = useState<ConnectionConfig[]>([]);
  const [sessions, setSessions] = useState<Record<string, Session>>({});
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [tabs, setTabs] = useState<Tab[]>([
    { id: "welcome", kind: "welcome", title: "欢迎" },
  ]);
  const [activeTabId, setActiveTabId] = useState("welcome");
  const [logOpen, setLogOpen] = useState(true);
  const [selectedConnectionId, setSelectedConnectionId] = useState<string | null>(null);
  const [selectedNamespace, setSelectedNamespace] = useState<{
    connectionId: string;
    namespace: string;
  } | null>(null);

  const connectionsRef = useRef<ConnectionConfig[]>([]);
  connectionsRef.current = connections;
  const tabsRef = useRef<Tab[]>([]);
  tabsRef.current = tabs;

  const log = useCallback((level: LogLevel, message: string) => {
    logSeq += 1;
    const entry: LogEntry = {
      id: logSeq,
      level,
      message,
      time: nowLabel(),
    };
    setLogs((prev) => [...prev.slice(-600), entry]);
  }, []);

  const reloadConnections = useCallback(async () => {
    try {
      const list = await api.listConnections();
      setConnections(list);
      setSessions((prev) => {
        const next: Record<string, Session> = {};
        for (const config of list) {
          next[config.id] = prev[config.id] ?? {
            connectionId: config.id,
            status: "idle",
            expanded: false,
            namespaces: [],
          };
        }
        return next;
      });
    } catch (error) {
      log("err", `读取连接配置失败: ${errorMessage(error)}`);
    }
  }, [log]);

  useEffect(() => {
    void reloadConnections();
  }, [reloadConnections]);

  const patchSession = useCallback(
    (connectionId: string, patch: Partial<Session>) => {
      setSessions((prev) => {
        const base: Session = prev[connectionId] ?? {
          connectionId,
          status: "idle",
          expanded: false,
          namespaces: [],
        };
        return { ...prev, [connectionId]: { ...base, ...patch } };
      });
    },
    [],
  );

  const loadTables = useCallback(
    async (connectionId: string, namespace: string) => {
      setSessions((prev) => {
        const session = prev[connectionId];
        if (!session) return prev;
        return {
          ...prev,
          [connectionId]: {
            ...session,
            namespaces: session.namespaces.map((ns) =>
              ns.name === namespace ? { ...ns, loading: true } : ns,
            ),
          },
        };
      });
      try {
        const tables = await api.listTables(connectionId, namespace);
        setSessions((prev) => {
          const session = prev[connectionId];
          if (!session) return prev;
          return {
            ...prev,
            [connectionId]: {
              ...session,
              namespaces: session.namespaces.map((ns) =>
                ns.name === namespace
                  ? { ...ns, loading: false, loaded: true, tables, expanded: true }
                  : ns,
              ),
            },
          };
        });
      } catch (error) {
        setSessions((prev) => {
          const session = prev[connectionId];
          if (!session) return prev;
          return {
            ...prev,
            [connectionId]: {
              ...session,
              namespaces: session.namespaces.map((ns) =>
                ns.name === namespace ? { ...ns, loading: false } : ns,
              ),
            },
          };
        });
        log("err", `读取 ${namespace} 表列表失败: ${errorMessage(error)}`);
      }
    },
    [log],
  );

  const connect = useCallback(
    async (connectionId: string) => {
      const config = connectionsRef.current.find((c) => c.id === connectionId);
      const label = config?.name ?? connectionId;
      patchSession(connectionId, { status: "connecting", error: undefined });
      log("info", `正在连接 ${label} ...`);
      const started = Date.now();
      try {
        const info = await api.openConnection(connectionId);
        setSessions((prev) => {
          const session = prev[connectionId];
          return {
            ...prev,
            [connectionId]: {
              connectionId,
              status: "connected",
              serverVersion: info.serverVersion,
              expanded: true,
              namespaces: info.namespaces.map((ns: NamespaceInfo) => {
                const previous = session?.namespaces.find((n) => n.name === ns.name);
                return (
                  previous ?? {
                    name: ns.name,
                    loaded: false,
                    loading: false,
                    tables: [],
                    expanded: false,
                    tablesExpanded: true,
                    viewsExpanded: true,
                    queriesExpanded: false,
                    queriesLoaded: false,
                    queries: [],
                  }
                );
              }),
            },
          };
        });
        log(
          "ok",
          `已连接 ${label}（${config?.dialect === "postgres" ? "PostgreSQL" : "MySQL"} ${info.serverVersion}，${Date.now() - started} ms）`,
        );
      } catch (error) {
        patchSession(connectionId, { status: "error", error: errorMessage(error) });
        log("err", `连接 ${label} 失败: ${errorMessage(error)}`);
      }
    },
    [log, patchSession],
  );

  const disconnect = useCallback(
    async (connectionId: string) => {
      const config = connectionsRef.current.find((c) => c.id === connectionId);
      try {
        await api.closeConnection(connectionId);
      } catch (error) {
        log("warn", `断开连接时出错: ${errorMessage(error)}`);
      }
      patchSession(connectionId, { status: "idle", namespaces: [], expanded: false });
      log("info", `已断开 ${config?.name ?? connectionId}`);
    },
    [log, patchSession],
  );

  const toggleConnection = useCallback(
    (connectionId: string) => {
      const session = sessions[connectionId];
      if (session?.status === "connected") {
        patchSession(connectionId, { expanded: !session.expanded });
        return;
      }
      void connect(connectionId);
    },
    [connect, patchSession, sessions],
  );

  const toggleNamespace = useCallback(
    (connectionId: string, namespace: string) => {
      const session = sessions[connectionId];
      const node = session?.namespaces.find((n) => n.name === namespace);
      if (!node) return;
      if (node.loaded) {
        setSessions((prev) => ({
          ...prev,
          [connectionId]: {
            ...prev[connectionId],
            namespaces: prev[connectionId].namespaces.map((ns) =>
              ns.name === namespace ? { ...ns, expanded: !ns.expanded } : ns,
            ),
          },
        }));
        return;
      }
      void loadTables(connectionId, namespace);
    },
    [loadTables, sessions],
  );

  const refreshTables = useCallback(
    async (connectionId: string, namespace: string) => {
      await loadTables(connectionId, namespace);
    },
    [loadTables],
  );

  const loadQueries = useCallback(
    async (connectionId: string, namespace: string) => {
      try {
        const all = await api.listQueries(connectionId);
        // A query with an empty namespace is not tied to one database, so it
        // shows up under every database of that connection.
        const mine = all.filter(
          (query) => query.namespace === "" || query.namespace === namespace,
        );
        setSessions((prev) => {
          const session = prev[connectionId];
          if (!session) return prev;
          return {
            ...prev,
            [connectionId]: {
              ...session,
              namespaces: session.namespaces.map((ns) =>
                ns.name === namespace
                  ? { ...ns, queries: mine, queriesLoaded: true, queriesExpanded: true }
                  : ns,
              ),
            },
          };
        });
      } catch (error) {
        log("err", `读取查询列表失败: ${errorMessage(error)}`);
      }
    },
    [log],
  );

  const toggleNamespaceGroup = useCallback(
    (connectionId: string, namespace: string, group: "tables" | "views" | "queries") => {
      const session = sessions[connectionId];
      const node = session?.namespaces.find((n) => n.name === namespace);
      if (!node) return;
      // Opening 查询 for the first time has to read the folder.
      if (group === "queries" && !node.queriesLoaded && !node.queriesExpanded) {
        void loadQueries(connectionId, namespace);
        return;
      }
      const key =
        group === "tables"
          ? "tablesExpanded"
          : group === "views"
            ? "viewsExpanded"
            : "queriesExpanded";
      setSessions((prev) => ({
        ...prev,
        [connectionId]: {
          ...prev[connectionId],
          namespaces: prev[connectionId].namespaces.map((ns) =>
            ns.name === namespace ? { ...ns, [key]: !ns[key] } : ns,
          ),
        },
      }));
    },
    [loadQueries, sessions],
  );

  const openTab = useCallback((tab: Omit<Tab, "id"> & { id?: string }) => {
    // The sync wizards are singletons: reopening one focuses (and re-seeds) the
    // existing tab instead of stacking duplicates. Table tabs are keyed by their
    // target so the same table always focuses the same tab.
    const singleton =
      tab.kind === "welcome" || tab.kind === "structure" || tab.kind === "data";
    // A query tab is keyed by its file so the same snippet focuses one tab.
    const id =
      tab.id ??
      (singleton
        ? tab.kind
        : tab.kind === "query"
          ? `query:${tab.path ?? ""}`
          : `${tab.kind}:${tab.connectionId ?? ""}:${tab.namespace ?? ""}:${tab.table ?? ""}`);
    setTabs((prev) => {
      const index = prev.findIndex((t) => t.id === id);
      if (index >= 0) {
        const next = [...prev];
        next[index] = { ...next[index], ...tab, id };
        return next;
      }
      return [...prev, { ...tab, id }];
    });
    setActiveTabId(id);
    return id;
  }, []);

  const closeTabsForConnection = useCallback((connectionId: string) => {
    setTabs((prev) => {
      const next = prev.filter(
        (tab) =>
          tab.connectionId !== connectionId ||
          tab.kind === "welcome" ||
          tab.kind === "structure" ||
          tab.kind === "data",
      );
      return next.length > 0 ? next : [{ id: "welcome", kind: "welcome", title: "欢迎" }];
    });
    setActiveTabId((current) => {
      const stillThere = tabsRef.current.find(
        (tab) =>
          tab.id === current &&
          (tab.connectionId !== connectionId ||
            tab.kind === "welcome" ||
            tab.kind === "structure" ||
            tab.kind === "data"),
      );
      return stillThere ? current : "welcome";
    });
  }, []);

  const closeTab = useCallback(
    (id: string) => {
      setTabs((prev) => {
        const next = prev.filter((t) => t.id !== id);
        if (next.length === 0) {
          setActiveTabId("welcome");
          return [{ id: "welcome", kind: "welcome", title: "欢迎" }];
        }
        setActiveTabId((current) =>
          current === id ? next[next.length - 1].id : current,
        );
        return next;
      });
    },
    [],
  );

  const updateTabTitle = useCallback((id: string, title: string) => {
    setTabs((prev) =>
      prev.map((tab) => (tab.id === id && tab.title !== title ? { ...tab, title } : tab)),
    );
  }, []);

  const closeOtherTabs = useCallback((id: string) => {
    const target = tabsRef.current.find((tab) => tab.id === id);
    if (!target) return;
    setTabs([target]);
    setActiveTabId(id);
  }, []);

  const closeAllTabs = useCallback(() => {
    // An empty tab strip would leave nothing to look at, so the welcome tab is
    // always the last one standing.
    setTabs([{ id: "welcome", kind: "welcome", title: "欢迎" }]);
    setActiveTabId("welcome");
  }, []);

  const value = useMemo<StoreValue>(
    () => ({
      connections,
      sessions,
      logs,
      tabs,
      activeTabId,
      logOpen,
      selectedConnectionId,
      log,
      clearLogs: () => setLogs([]),
      setLogOpen,
      reloadConnections,
      connect,
      disconnect,
      toggleConnection,
      toggleNamespace,
      refreshTables,
      selectConnection: setSelectedConnectionId,
      selectedNamespace,
      selectNamespace: (connectionId: string, namespace: string) =>
        setSelectedNamespace({ connectionId, namespace }),
      closeTabsForConnection,
      openTab,
      closeTab,
      updateTabTitle,
      closeOtherTabs,
      closeAllTabs,
      toggleNamespaceGroup,
      loadQueries,
      setActiveTab: setActiveTabId,
    }),
    [
      connections,
      sessions,
      logs,
      tabs,
      activeTabId,
      logOpen,
      selectedConnectionId,
      selectedNamespace,
      log,
      reloadConnections,
      connect,
      disconnect,
      toggleConnection,
      toggleNamespace,
      refreshTables,
      openTab,
      closeTab,
      updateTabTitle,
      closeOtherTabs,
      closeAllTabs,
      closeTabsForConnection,
      toggleNamespaceGroup,
      loadQueries,
    ],
  );

  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}

export function useStore(): StoreValue {
  const context = useContext(StoreContext);
  if (!context) throw new Error("useStore must be used inside StoreProvider");
  return context;
}

export function dialectLabel(dialect: Dialect | undefined): string {
  return dialect === "postgres" ? "PostgreSQL" : "MySQL";
}
