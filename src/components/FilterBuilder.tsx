import { useState } from "react";
import type { ColumnMeta, Combinator, FilterGroup, FilterNode, FilterOp } from "../types";

/**
 * A condition tree editor.
 *
 * The tree mirrors the SQL: each group combines its members with 全部（AND）or
 * 任一（OR）, and groups nest, so `a = 1 AND (b = 2 OR c = 3)` is expressible.
 */

const OP_LABELS: Record<FilterOp, string> = {
  eq: "=",
  ne: "≠",
  gt: ">",
  ge: "≥",
  lt: "<",
  le: "≤",
  contains: "包含",
  startsWith: "开头是",
  endsWith: "结尾是",
  in: "在列表中",
  isNull: "为 NULL",
  notNull: "不为 NULL",
};

const TEXT_ONLY: FilterOp[] = ["contains", "startsWith", "endsWith"];

const ALL_OPS: FilterOp[] = [
  "eq",
  "ne",
  "gt",
  "ge",
  "lt",
  "le",
  "contains",
  "startsWith",
  "endsWith",
  "in",
  "isNull",
  "notNull",
];

export function operatorsFor(kind: string | undefined): FilterOp[] {
  const text = kind === "str" || kind === "text" || kind === "json";
  return text ? ALL_OPS : ALL_OPS.filter((op) => !TEXT_ONLY.includes(op));
}

export function needsValue(operator: FilterOp): boolean {
  return operator !== "isNull" && operator !== "notNull";
}

export interface DraftCondition {
  key: string;
  kind: "condition";
  column: string;
  operator: FilterOp;
  value: string;
}

export interface DraftGroup {
  key: string;
  kind: "group";
  combinator: Combinator;
  children: DraftNode[];
}

export type DraftNode = DraftCondition | DraftGroup;

let seq = 0;
const nextKey = () => `f-${++seq}`;

export function newDraftCondition(column: string): DraftCondition {
  return { key: nextKey(), kind: "condition", column, operator: "eq", value: "" };
}

export function newDraftGroup(): DraftGroup {
  return { key: nextKey(), kind: "group", combinator: "and", children: [] };
}

/**
 * Convert the editor state into the shape the backend expects.
 *
 * Rows that are still being filled in are dropped rather than sent: a condition
 * with no value would either be rejected or, worse, silently match nothing.
 */
export function toWireGroup(group: DraftGroup): FilterGroup | null {
  const children: FilterNode[] = [];
  for (const child of group.children) {
    if (child.kind === "condition") {
      if (!child.column) continue;
      if (needsValue(child.operator) && child.value === "") continue;
      children.push({
        kind: "condition",
        condition: {
          column: child.column,
          operator: child.operator,
          value: needsValue(child.operator) ? child.value : null,
        },
      });
    } else {
      const nested = toWireGroup(child);
      if (nested) children.push({ kind: "group", group: nested });
    }
  }
  return children.length > 0 ? { combinator: group.combinator, children } : null;
}

/** How many real conditions the tree currently holds. */
export function countConditions(group: DraftGroup): number {
  let total = 0;
  for (const child of group.children) {
    if (child.kind === "condition") {
      if (child.column && (!needsValue(child.operator) || child.value !== "")) total += 1;
    } else {
      total += countConditions(child);
    }
  }
  return total;
}

interface Props {
  columns: ColumnMeta[];
  value: DraftGroup;
  onChange: (next: DraftGroup) => void;
  onApply: () => void;
  onClear: () => void;
  busy?: boolean;
}

export function FilterBuilder({ columns, value, onChange, onApply, onClear, busy }: Props) {
  const total = countConditions(value);

  return (
    <div style={{ marginTop: 8 }}>
      <GroupEditor
        group={value}
        columns={columns}
        depth={0}
        onChange={onChange}
        removable={false}
      />
      <div className="row" style={{ gap: 6, marginTop: 6 }}>
        <button className="btn primary small" onClick={onApply} disabled={busy}>
          查询
        </button>
        <button className="btn small" onClick={onClear} disabled={busy}>
          清除
        </button>
        <span className="faint">
          {total === 0 ? "尚未填写条件" : `共 ${total} 个条件`}
        </span>
      </div>
    </div>
  );
}

interface GroupProps {
  group: DraftGroup;
  columns: ColumnMeta[];
  depth: number;
  onChange: (next: DraftGroup) => void;
  removable: boolean;
}

function GroupEditor({ group, columns, depth, onChange, removable }: GroupProps) {
  const [collapsed, setCollapsed] = useState(false);
  const first = columns[0]?.name ?? "";

  const patch = (changes: Partial<DraftGroup>) => onChange({ ...group, ...changes });

  const replaceChild = (key: string, next: DraftNode) =>
    patch({ children: group.children.map((child) => (child.key === key ? next : child)) });

  return (
    <div className={`filter-group ${depth > 0 ? "nested" : ""}`}>
      <div className="row" style={{ gap: 6 }}>
        <span className="faint">满足</span>
        <select
          value={group.combinator}
          style={{ width: 76 }}
          onChange={(event) =>
            patch({ combinator: event.target.value as Combinator })
          }
        >
          <option value="and">全部</option>
          <option value="or">任一</option>
        </select>
        <span className="faint">条件</span>

        <button
          className="btn small"
          onClick={() => patch({ children: [...group.children, newDraftCondition(first)] })}
        >
          + 条件
        </button>
        <button
          className="btn small"
          onClick={() => patch({ children: [...group.children, newDraftGroup()] })}
        >
          + 分组
        </button>
        {group.children.length > 0 && (
          <button className="btn small" onClick={() => patch({ children: [] })}>
            清空本组
          </button>
        )}
        {depth > 0 && (
          <button className="btn small" onClick={() => setCollapsed((value) => !value)}>
            {collapsed ? "展开" : "收起"}
          </button>
        )}

        <div className="grow" />
        {removable && (
          <span className="faint" title="删除这个分组">
            分组
          </span>
        )}
      </div>

      {!collapsed && (
        <div style={{ marginTop: 6 }}>
          {group.children.length === 0 && (
            <div className="faint" style={{ padding: "2px 0 2px 4px" }}>
              这个分组还没有条件，不会参与查询
            </div>
          )}
          {group.children.map((child) =>
            child.kind === "condition" ? (
              <ConditionRow
                key={child.key}
                condition={child}
                columns={columns}
                onChange={(next) => replaceChild(child.key, next)}
                onRemove={() =>
                  patch({ children: group.children.filter((item) => item.key !== child.key) })
                }
              />
            ) : (
              <div className="row" key={child.key} style={{ alignItems: "flex-start" }}>
                <div style={{ flex: 1 }}>
                  <GroupEditor
                    group={child}
                    columns={columns}
                    depth={depth + 1}
                    onChange={(next) => replaceChild(child.key, next)}
                    removable
                  />
                </div>
                <button
                  className="icon-btn"
                  title="删除该分组"
                  style={{ marginTop: 4 }}
                  onClick={() =>
                    patch({ children: group.children.filter((item) => item.key !== child.key) })
                  }
                >
                  <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
                    <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
                  </svg>
                </button>
              </div>
            ),
          )}
        </div>
      )}
    </div>
  );
}

interface RowProps {
  condition: DraftCondition;
  columns: ColumnMeta[];
  onChange: (next: DraftCondition) => void;
  onRemove: () => void;
}

function ConditionRow({ condition, columns, onChange, onRemove }: RowProps) {
  const meta = columns.find((column) => column.name === condition.column);
  const operators = operatorsFor(meta?.kind);
  const patch = (changes: Partial<DraftCondition>) => onChange({ ...condition, ...changes });

  return (
    <div className="row filter-row" style={{ gap: 6 }}>
      <select
        value={condition.column}
        style={{ width: 170 }}
        onChange={(event) => patch({ column: event.target.value })}
      >
        {columns.map((column) => (
          <option key={column.name} value={column.name}>
            {column.name}
          </option>
        ))}
      </select>
      <select
        value={condition.operator}
        style={{ width: 108 }}
        onChange={(event) => patch({ operator: event.target.value as FilterOp })}
      >
        {operators.map((op) => (
          <option key={op} value={op}>
            {OP_LABELS[op]}
          </option>
        ))}
      </select>
      {needsValue(condition.operator) ? (
        <input
          type="text"
          value={condition.value}
          placeholder={condition.operator === "in" ? "用逗号分隔多个值" : "值"}
          style={{ width: 220 }}
          onChange={(event) => patch({ value: event.target.value })}
        />
      ) : (
        <span className="faint" style={{ width: 220 }}>
          无需填写值
        </span>
      )}
      <button className="icon-btn" title="删除该条件" onClick={onRemove}>
        <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
          <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
        </svg>
      </button>
    </div>
  );
}
