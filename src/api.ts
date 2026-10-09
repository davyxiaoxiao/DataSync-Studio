import { invoke } from "@tauri-apps/api/core";
import type {
  ColumnInfo,
  CellUpdateResult,
  ColumnMapping,
  CompletionCatalog,
  ColumnPair,
  FilterGroup,
  ForeignKeyInfo,
  ExcelImportPlan,
  ExcelSheetPreview,
  ExportColumn,
  ExportOptions,
  ExportOutcome,
  ImportOutcome,
  ImportStrategy,
  IndexInfo,
  CompareSchemaResult,
  ConnectionConfig,
  ConnectionInfo,
  DataDiffResult,
  NamespaceInfo,
  SchemaDiff,
  DropTablesPreview,
  SchemaSqlPreview,
  RowKeyValue,
  SavedQuery,
  SortSpec,
  SqlCheckResult,
  SqlRunResult,
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
    filters: FilterGroup | null = null,
    sort: SortSpec[] = [],
  ) =>
    invoke<TableData>("preview_table_data", {
      connectionId,
      namespace,
      table,
      limit,
      offset,
      filters,
      sort,
    }),

  /** Apply one cell edit; the previous value guards against concurrent change. */
  updateTableCell: (request: {
    connectionId: string;
    namespace: string;
    table: string;
    key: RowKeyValue[];
    column: string;
    value?: string | null;
    previous?: string | null;
  }) => invoke<CellUpdateResult>("update_table_cell", { request }),

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
    /** Wrap the whole DDL plan in a transaction (PostgreSQL only). */
    useTransaction?: boolean;
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
    /** PostgreSQL: refresh materialised views once the rows are written. */
    refreshMaterializedViews?: boolean;
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

  previewExcel: (
    path: string,
    sheet?: string | null,
    headerRow?: number | null,
    sample?: number,
  ) =>
    invoke<ExcelSheetPreview>("preview_excel", { path, sheet, headerRow, sample }),

  planExcelImport: (
    connectionId: string,
    namespace: string,
    table: string,
    path: string,
    sheet?: string | null,
    headerRow?: number | null,
  ) =>
    invoke<ExcelImportPlan>("plan_excel_import", {
      connectionId,
      namespace,
      table,
      path,
      sheet,
      headerRow,
    }),

  importExcel: (request: {
    connectionId: string;
    namespace: string;
    table: string;
    path: string;
    sheet?: string | null;
    headerRow?: number | null;
    mapping: ColumnMapping[];
    options?: {
      skipInvalid?: boolean;
      useTransaction?: boolean;
      batchSize?: number;
      strategy?: ImportStrategy;
    };
    jobId?: string;
  }) => invoke<ImportOutcome>("import_excel", { request }),

  /** Rows in a table, so the export dialog can warn before starting. */
  countTableRows: (connectionId: string, namespace: string, table: string) =>
    invoke<number | null>("count_table_rows", { connectionId, namespace, table }),

  exportExcel: (request: {
    connectionId: string;
    namespace: string;
    table: string;
    path: string;
    columns: ExportColumn[];
    options?: ExportOptions;
    jobId?: string;
  }) => invoke<ExportOutcome>("export_excel", { request }),

  listQueries: (connectionId: string) =>
    invoke<SavedQuery[]>("list_queries", { connectionId }),

  createQuery: (request: { connectionId: string; namespace: string; name: string }) =>
    invoke<SavedQuery>("create_query", { request }),

  readQuery: (request: { path: string }) =>
    invoke<SavedQuery>("read_query", { request }),

  saveQuery: (request: { path: string; sql: string }) =>
    invoke<SavedQuery>("save_query", { request }),

  renameQuery: (path: string, name: string) =>
    invoke<SavedQuery>("rename_query", { path, name }),

  duplicateQuery: (request: { path: string }) =>
    invoke<SavedQuery>("duplicate_query", { request }),

  deleteQuery: (request: { path: string }) =>
    invoke<void>("delete_query", { request }),

  /** The folder the .sql files live in, for revealing it in the file manager. */
  queriesDirectory: (connectionId: string) =>
    invoke<string>("queries_directory", { connectionId }),

  /** Validate a script with the server without running it. */
  checkSql: (request: { connectionId: string; namespace: string; sql: string }) =>
    invoke<SqlCheckResult>("check_sql", { request }),

  /** Table / view / column / database names for editor completion. */
  completionCatalog: (connectionId: string, namespace: string) =>
    invoke<CompletionCatalog>("completion_catalog", { connectionId, namespace }),

  runSql: (request: {
    connectionId: string;
    namespace: string;
    sql: string;
    jobId?: string;
  }) => invoke<SqlRunResult>("run_sql", { request }),

  previewDropTables: (connectionId: string, namespace: string, tables: string[]) =>
    invoke<DropTablesPreview>("preview_drop_tables", { connectionId, namespace, tables }),

  applyDropTables: (request: {
    connectionId: string;
    namespace: string;
    tables: string[];
    jobId?: string;
  }) => invoke<SyncRunResult>("apply_drop_tables", { request }),

  renameTable: (request: {
    connectionId: string;
    namespace: string;
    table: string;
    newName: string;
    jobId?: string;
  }) => invoke<SyncRunResult>("rename_table", { request }),

  previewCreateTable: (request: {
    connectionId: string;
    namespace: string;
    table: string;
    columns: ColumnInfo[];
    primaryKey?: string[];
    indexes?: IndexInfo[];
  }) => invoke<SchemaSqlPreview>("preview_create_table", { request }),

  createTable: (request: {
    connectionId: string;
    namespace: string;
    table: string;
    columns: ColumnInfo[];
    primaryKey?: string[];
    indexes?: IndexInfo[];
    jobId?: string;
  }) => invoke<SyncRunResult>("create_table", { request }),

  previewAlterTable: (
    connectionId: string,
    namespace: string,
    table: string,
    columns: ColumnInfo[],
    renames: ColumnRename[] = [],
    indexes?: IndexInfo[],
    foreignKeys?: ForeignKeyInfo[],
  ) =>
    invoke<SchemaSqlPreview>("preview_alter_table", {
      connectionId,
      namespace,
      table,
      columns,
      renames,
      indexes,
      foreignKeys,
    }),

  applyAlterTable: (request: {
    connectionId: string;
    namespace: string;
    table: string;
    columns: ColumnInfo[];
    renames?: ColumnRename[];
    indexes?: IndexInfo[];
    foreignKeys?: ForeignKeyInfo[];
    jobId?: string;
  }) => invoke<SyncRunResult>("apply_alter_table", { request }),
};

/** A column whose name was edited in the table designer. */
export interface ColumnRename {
  from: string;
  to: string;
}

export interface StorageInfo {
  dir: string;
  warning?: string | null;
}

export type { SchemaDiff };
