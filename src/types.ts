export type Dialect = "mysql" | "postgres";

export interface ConnectionConfig {
  id: string;
  name: string;
  dialect: Dialect;
  host: string;
  port: number;
  username: string;
  database?: string | null;
  /** disable | prefer | require */
  sslMode: string;
  group?: string | null;
  color?: string | null;
  remark?: string | null;
  savePassword: boolean;
  password?: string | null;
  hasPassword: boolean;
}

export interface NamespaceInfo {
  name: string;
  charset?: string | null;
  collation?: string | null;
  isSystem: boolean;
}

export interface TableSummary {
  name: string;
  tableType: string;
  comment?: string | null;
  rows?: number | null;
}

export interface ConnectionInfo {
  serverVersion: string;
  dialect: Dialect;
  namespaces: NamespaceInfo[];
}

export interface TestConnectionResult {
  ok: boolean;
  serverVersion?: string | null;
  elapsedMs: number;
  error?: string | null;
}

export interface ColumnInfo {
  name: string;
  ordinal: number;
  dataType: string;
  fullType: string;
  nullable: boolean;
  default?: string | null;
  isAutoIncrement: boolean;
  autoKind?: string | null;
  /** A computed column: derived on write, never writable. */
  isGenerated: boolean;
  generationExpression?: string | null;
  /** "stored" | "virtual" */
  generationKind?: string | null;
  isUnsigned: boolean;
  comment?: string | null;
  charset?: string | null;
  collation?: string | null;
  /** Set for a user-defined type (PostgreSQL enum/domain). */
  udtName?: string | null;
}

export interface IndexInfo {
  name: string;
  columns: string[];
  isUnique: boolean;
  indexType?: string | null;
}

export interface ForeignKeyInfo {
  name: string;
  columns: string[];
  refSchema?: string | null;
  refTable: string;
  refColumns: string[];
  onUpdate: string;
  onDelete: string;
}

export interface TableStructure {
  name: string;
  comment?: string | null;
  engine?: string | null;
  charset?: string | null;
  collation?: string | null;
  primaryKey: string[];
  columns: ColumnInfo[];
  indexes: IndexInfo[];
  foreignKeys: ForeignKeyInfo[];
}

export type ChangeKind = "new" | "missing" | "changed" | "identical";

export interface FieldChange {
  field: string;
  source?: string | null;
  target?: string | null;
}

export interface ColumnDiff {
  name: string;
  kind: ChangeKind;
  source?: ColumnInfo | null;
  target?: ColumnInfo | null;
  differences: FieldChange[];
}

export interface IndexDiff {
  name: string;
  kind: ChangeKind;
  source?: IndexInfo | null;
  target?: IndexInfo | null;
  differences: FieldChange[];
}

export interface ForeignKeyDiff {
  name: string;
  kind: ChangeKind;
  source?: ForeignKeyInfo | null;
  target?: ForeignKeyInfo | null;
  differences: FieldChange[];
}

export interface PrimaryKeyDiff {
  kind: ChangeKind;
  source: string[];
  target: string[];
}

export interface TableDiff {
  name: string;
  targetName?: string | null;
  kind: ChangeKind;
  sourceComment?: string | null;
  targetComment?: string | null;
  columns: ColumnDiff[];
  primaryKey: PrimaryKeyDiff;
  indexes: IndexDiff[];
  foreignKeys: ForeignKeyDiff[];
}

export interface DiffSummary {
  newTables: number;
  missingTables: number;
  changedTables: number;
  identicalTables: number;
  newColumns: number;
  missingColumns: number;
  changedColumns: number;
  indexChanges: number;
  foreignKeyChanges: number;
  primaryKeyChanges: number;
  newTypes: number;
  missingTypes: number;
  changedTypes: number;
  newViews: number;
  missingViews: number;
  changedViews: number;
  identicalViews: number;
}

export interface ViewStructure {
  name: string;
  /** "view" | "materialized" */
  kind: string;
  definition: string;
  comment?: string | null;
  dependencies: string[];
}

export interface ViewDiff {
  name: string;
  kind: ChangeKind;
  source?: ViewStructure | null;
  target?: ViewStructure | null;
  differences: FieldChange[];
}

export interface CustomType {
  name: string;
  /** "enum" | "domain" */
  kind: string;
  labels: string[];
  baseType?: string | null;
  domainNullable: boolean;
  domainDefault?: string | null;
  constraints: string[];
  comment?: string | null;
}

export interface TypeDiff {
  name: string;
  kind: ChangeKind;
  source?: CustomType | null;
  target?: CustomType | null;
  differences: FieldChange[];
}

export interface SchemaDiff {
  sourceNamespace: string;
  targetNamespace: string;
  sourceDialect: Dialect;
  targetDialect: Dialect;
  tables: TableDiff[];
  views: ViewDiff[];
  types: TypeDiff[];
  summary: DiffSummary;
}

export interface CompareSchemaResult {
  compareId: string;
  diff: SchemaDiff;
  warnings: string[];
}

export interface DdlStatement {
  table: string;
  category: string;
  object: string;
  sql: string;
}

export interface StatementOutcome {
  sql: string;
  ok: boolean;
  rowsAffected: number;
  error?: string | null;
  elapsedMs: number;
}

export interface SyncRunResult {
  total: number;
  succeeded: number;
  failed: number;
  elapsedMs: number;
  message: string;
  failures: StatementOutcome[];
  sample: StatementOutcome[];
  /** Non-fatal notes, e.g. a table that will fail on a non-empty target. */
  warnings: string[];
  cancelled: boolean;
  /** A transaction was used and rolled back, so the target is unchanged. */
  rolledBack: boolean;
  /** Whether the engine could run the plan atomically. */
  transactional: boolean;
  /** Statements never attempted because execution stopped at a failure. */
  skipped: number;
}

export interface SchemaSqlPreview {
  statements: DdlStatement[];
  warnings: string[];
}

/** Mirrors `jobs::ProgressUpdate`; delivered on the `sync://progress` event. */
export interface ProgressUpdate {
  jobId: string;
  stage: string;
  label: string;
  processed: number;
  total?: number | null;
  detail: string;
  finished: boolean;
  cancelled: boolean;
}

export interface DropTablesPreview {
  statements: DdlStatement[];
  /** Views reading from the tables being dropped; they are not removed for you. */
  dependentViews: string[];
  warnings: string[];
}

export interface Cell {
  kind: string;
  text?: string | null;
}

export interface ColumnMeta {
  name: string;
  dataType: string;
  kind: string;
  nullable: boolean;
  primaryKey: boolean;
}

export interface TableData {
  columns: ColumnMeta[];
  rows: Cell[][];
  primaryKey: string[];
  limit: number;
  offset: number;
  hasMore: boolean;
  elapsedMs: number;
  sql: string;
}

export interface ColumnPair {
  source: string;
  target: string;
}

export interface KeyValue {
  column: string;
  value: Cell;
}

export interface CellChange {
  column: string;
  before: Cell;
  after: Cell;
}

export interface RowChange {
  key: KeyValue[];
  cells: CellChange[];
}

export interface DataDiffSummary {
  sourceRows: number;
  targetRows: number;
  inserts: number;
  updates: number;
  deletes: number;
  unchanged: number;
}

export interface DataDiffResult {
  planId: string;
  columns: ColumnPair[];
  keyColumns: ColumnPair[];
  summary: DataDiffSummary;
  insertPreview: RowChange[];
  updatePreview: RowChange[];
  deletePreview: RowChange[];
  previewTruncated: boolean;
  sqlPreview: string[];
  warnings: string[];
}

export interface TableSelection {
  name: string;
  wholeTable: boolean;
  columns: string[];
  primaryKey: boolean;
  indexes: string[];
  foreignKeys: string[];
}

export interface SyncSelection {
  tables: TableSelection[];
  views: string[];
  types: string[];
}

export interface TableEndpoint {
  connectionId: string;
  namespace: string;
  table: string;
}
