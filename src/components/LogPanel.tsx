import { useEffect, useRef } from "react";
import { useStore } from "../store";
import { IconChevronDown, IconChevronRight, IconTrash } from "./Icons";

export function LogPanel() {
  const { logs, clearLogs, logOpen, setLogOpen } = useStore();
  const bodyRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (logOpen && bodyRef.current) {
      bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
    }
  }, [logs, logOpen]);

  const errorCount = logs.filter((l) => l.level === "err").length;

  return (
    <div className="logpanel">
      <div className="logpanel-head">
        <button
          className="icon-btn"
          onClick={() => setLogOpen(!logOpen)}
          title={logOpen ? "收起日志" : "展开日志"}
        >
          {logOpen ? <IconChevronDown size={12} /> : <IconChevronRight size={12} />}
        </button>
        <span style={{ fontWeight: 600 }}>消息日志</span>
        <span>{logs.length} 条</span>
        {errorCount > 0 && <span className="err-text">{errorCount} 个错误</span>}
        <div style={{ flex: 1 }} />
        <button className="icon-btn" title="清空日志" onClick={clearLogs}>
          <IconTrash size={12} />
        </button>
      </div>
      {logOpen && (
        <div className="logpanel-body" ref={bodyRef}>
          {logs.length === 0 && <div className="faint">暂无日志</div>}
          {logs.map((entry) => (
            <div key={entry.id} className={`logline ${entry.level}`}>
              <span className="ts">{entry.time}</span>
              <span className="msg">{entry.message}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
