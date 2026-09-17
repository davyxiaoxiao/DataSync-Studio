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
import type { ConnectionConfig, Dialect, NamespaceInfo } from "./types";

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
}

export interface Session {
  connectionId: string;
  status: "idle" | "connecting" | "connected" | "error";
  serverVersion?: string;
  error?: string;
  expanded: boolean;
  namespaces: NamespaceNode[];
}

export type TabKind = "welcome" | "table" | "structure" | "data";

export interface Tab {
  id: string;
  kind: TabKind;
  title: string;
  connectionId?: string;
  namespace?: string;
  table?: string;
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

  selectConnection: (id: string | null) => void;
  closeTabsForConnection: (connectionId: string) => void;
  openTab: (tab: Omit<Tab, "id"> & { id?: string }) => string;
  closeTab: (id: string) => void;
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

  const openTab = useCallback((tab: Omit<Tab, "id"> & { id?: string }) => {
    // The sync wizards are singletons: reopening one focuses (and re-seeds) the
    // existing tab instead of stacking duplicates. Table tabs are keyed by their
    // target so the same table always focuses the same tab.
    const singleton =
      tab.kind === "welcome" || tab.kind === "structure" || tab.kind === "data";
    const id =
      tab.id ??
      (singleton
        ? tab.kind
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
      closeTabsForConnection,
      openTab,
      closeTab,
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
      log,
      reloadConnections,
      connect,
      disconnect,
      toggleConnection,
      toggleNamespace,
      refreshTables,
      openTab,
      closeTab,
      closeTabsForConnection,
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
