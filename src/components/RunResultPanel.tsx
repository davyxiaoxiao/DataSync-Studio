import { useState } from "react";
import type { SyncRunResult } from "../types";
import { IconChevronDown, IconChevronRight } from "./Icons";

export function RunResultPanel({ result, title }: { result: SyncRunResult; title: string }) {
  const [expanded, setExpanded] = useState(result.failed > 0);
  const tone = result.failed > 0 ? "var(--red)" : "var(--green)";

  return (
    <div
      className="card"
      style={{ borderColor: result.failed > 0 ? "#f3c2c2" : "#b7dfc3", marginTop: 10 }}
    >
      <div className="card-title" style={{ color: tone }}>
        <button
          className="icon-btn"
          onClick={() => setExpanded((v) => !v)}
          title={expanded ? "收起" : "展开"}
        >
          {expanded ? <IconChevronDown size={12} /> : <IconChevronRight size={12} />}
        </button>
        {title}：{result.message}
        <span className="badge" style={{ marginLeft: 4 }}>
          {result.elapsedMs} ms
        </span>
      </div>

      {(result.warnings ?? []).length > 0 && (
        <div className="warnbox" style={{ marginTop: 8 }}>
          {(result.warnings ?? []).map((warning, index) => (
            <div key={index}>{warning}</div>
          ))}
        </div>
      )}

      {expanded && (
        <div style={{ maxHeight: 260, overflow: "auto" }}>
          {(result.failed > 0 ? result.failures : result.sample).map((outcome, index) => (
            <div key={index} style={{ marginBottom: 6 }}>
              <div className="row" style={{ gap: 6 }}>
                <span className={`badge ${outcome.ok ? "new" : "missing"}`}>
                  {outcome.ok ? "成功" : "失败"}
                </span>
                <span className="faint">{outcome.elapsedMs} ms</span>
                {outcome.ok && <span className="faint">影响 {outcome.rowsAffected} 行</span>}
              </div>
              <pre
                style={{
                  margin: "3px 0 0",
                  padding: "5px 8px",
                  background: "var(--bg-subtle)",
                  border: "1px solid var(--border)",
                  borderRadius: 5,
                  fontFamily: "var(--mono)",
                  fontSize: 11,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                }}
              >
                {outcome.sql}
              </pre>
              {outcome.error && <div className="errbox" style={{ marginTop: 4 }}>{outcome.error}</div>}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
