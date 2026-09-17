import { invoke } from "@tauri-apps/api/core";
import type {
  ColumnPair,
  CompareSchemaResult,
  ConnectionConfig,
  ConnectionInfo,
  DataDiffResult,
  NamespaceInfo,
  SchemaDiff,
  SchemaSqlPreview,
  SyncRunResult,
  SyncSelection,
  TableData,
  TableEndpoint,
  TableStructure,
  TableSummary,
  TestConnectionResult,
} from "./types";

/** Tauri surfaces command errors as plain strings. */
export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

export const api = {
  listConnections: () => invoke<ConnectionConfig[]>("list_connections"),

  saveConnection: (config: ConnectionConfig) =>
    invoke<ConnectionConfig>("save_connection", { config }),

  deleteConnection: (connectionId: string) =>
    invoke<void>("delete_connection", { connectionId }),

  reorderConnections: (ids: string[]) => invoke<void>("reorder_connections", { ids }),

  testConnection: (config: ConnectionConfig) =>
    invoke<TestConnectionResult>("test_connection", { config }),

  openConnection: (connectionId: string) =>
    invoke<ConnectionInfo>("open_connection", { connectionId }),

  closeConnection: (connectionId: string) =>
    invoke<void>("close_connection", { connectionId }),

  listNamespaces: (connectionId: string) =>
    invoke<NamespaceInfo[]>("list_namespaces", { connectionId }),

  listTables: (connectionId: string, namespace: string) =>
    invoke<TableSummary[]>("list_tables", { connectionId, namespace }),

  getTableStructure: (connectionId: string, namespace: string, table: string) =>
    invoke<TableStructure>("get_table_structure", { connectionId, namespace, table }),

  previewTableData: (
    connectionId: string,
    namespace: string,
    table: string,
    limit = 200,
    offset = 0,
  ) =>
    invoke<TableData>("preview_table_data", {
      connectionId,
      namespace,
      table,
      limit,
      offset,
    }),

  compareSchema: (request: {
    sourceConnectionId: string;
    sourceNamespace: string;
    targetConnectionId: string;
    targetNamespace: string;
    ignoreComments?: boolean;
    ignoreCase?: boolean;
    compareCharset?: boolean;
    tables?: string[] | null;
    jobId?: string;
  }) => invoke<CompareSchemaResult>("compare_schema", { request }),

  previewSchemaSql: (compareId: string, selection?: SyncSelection | null) =>
    invoke<SchemaSqlPreview>("preview_schema_sql", { compareId, selection: selection ?? null }),

  applySchemaSync: (request: {
    compareId: string;
    selection?: SyncSelection | null;
    stopOnError?: boolean;
    jobId?: string;
  }) => invoke<SyncRunResult>("apply_schema_sync", { request }),

  compareData: (request: {
    source: TableEndpoint;
    target: TableEndpoint;
    keyColumns?: ColumnPair[];
    columnMap?: ColumnPair[];
    /** Page size for streaming comparison and rows per INSERT statement. */
    batchSize?: number;
    jobId?: string;
  }) => invoke<DataDiffResult>("compare_data", { request }),

  applyDataSync: (request: {
    planId: string;
    connectionId: string;
    insert?: boolean;
    update?: boolean;
    delete?: boolean;
    useTransaction?: boolean;
    jobId?: string;
  }) => invoke<SyncRunResult>("apply_data_sync", { request }),

  runQuery: (connectionId: string, sql: string) =>
    invoke<SyncRunResult>("run_query", { connectionId, sql }),

  exportTextFile: (path: string, content: string) =>
    invoke<void>("export_text_file", { path, content }),

  appStorageDir: () => invoke<StorageInfo>("app_storage_dir"),

  clearCaches: () => invoke<void>("clear_caches"),

  /** Ask a running job to stop; returns false when it already finished. */
  cancelJob: (jobId: string) => invoke<boolean>("cancel_job", { jobId }),
};

export interface StorageInfo {
  dir: string;
  warning?: string | null;
}

export type { SchemaDiff };
