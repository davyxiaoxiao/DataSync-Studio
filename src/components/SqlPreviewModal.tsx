import { useEffect, useMemo, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { api, errorMessage } from "../api";

interface Props {
  title: string;
  statements: string[];
  notice?: string | null;
  onClose: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

const RENDER_LIMIT = 500;

export function SqlPreviewModal({ title, statements, notice, onClose, onLog }: Props) {
  const [filter, setFilter] = useState("");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const text = useMemo(() => statements.join("\n"), [statements]);

  const visible = useMemo(() => {
    const keyword = filter.trim().toLowerCase();
    const list = keyword
      ? statements.filter((statement) => statement.toLowerCase().includes(keyword))
      : statements;
    return list.slice(0, RENDER_LIMIT);
  }, [statements, filter]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      onLog("ok", `已复制 ${statements.length} 条 SQL 到剪贴板`);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      onLog("warn", "复制失败，请手动全选文本框内容");
    }
  };

  const exportFile = async () => {
    try {
      const path = await save({
        title: "导出 SQL",
        defaultPath: "sync.sql",
        filters: [{ name: "SQL", extensions: ["sql"] }],
      });
      if (!path) return;
      await api.exportTextFile(path, text);
      onLog("ok", `已导出到 ${path}`);
    } catch (error) {
      onLog("err", `导出失败: ${errorMessage(error)}`);
    }
  };

  return (
    <div className="overlay" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="modal" style={{ width: 900, height: "78vh" }}>
        <div className="modal-head">
          <span>
            {title} <span className="badge">{statements.length} 条语句</span>
          </span>
          <button className="icon-btn" onClick={onClose} title="关闭">
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
              <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        <div style={{ padding: "8px 12px", borderBottom: "1px solid var(--border)" }} className="row">
          <input
            type="text"
            placeholder="过滤语句…"
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            style={{ width: 240 }}
          />
          {notice && <span className="warnbox" style={{ padding: "4px 10px" }}>{notice}</span>}
          <div className="grow" />
          <button className="btn small" onClick={() => void copy()}>
            {copied ? "已复制" : "复制全部"}
          </button>
          <button className="btn small" onClick={() => void exportFile()}>
            导出为 .sql
          </button>
        </div>

        <div className="modal-body" style={{ padding: 0, display: "flex", flexDirection: "column" }}>
          <div style={{ overflow: "auto", flex: 1, padding: "10px 12px" }}>
            {visible.map((statement, index) => (
              <pre
                key={index}
                style={{
                  margin: "0 0 8px",
                  padding: "7px 9px",
                  background: "var(--bg-subtle)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  fontFamily: "var(--mono)",
                  fontSize: 11.5,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                }}
              >
                <span style={{ color: "var(--text-faint)", marginRight: 8 }}>
                  {String(index + 1).padStart(4, " ")}
                </span>
                {statement}
              </pre>
            ))}
            {statements.length > RENDER_LIMIT && (
              <div className="faint" style={{ padding: "6px 0" }}>
                仅显示前 {RENDER_LIMIT} 条，复制 / 导出包含全部 {statements.length} 条语句。
              </div>
            )}
            {statements.length === 0 && <div className="faint">没有需要执行的语句。</div>}
          </div>
        </div>

        <div className="modal-foot">
          <span className="faint">按 Esc 关闭</span>
          <div className="spacer" />
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
