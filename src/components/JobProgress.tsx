import { api } from "../api";
import type { ProgressUpdate } from "../jobs";
import { IconClose } from "./Icons";

interface Props {
  update: ProgressUpdate | null;
  running: boolean;
  onDismiss: () => void;
  onLog: (level: "info" | "ok" | "warn" | "err", message: string) => void;
}

/**
 * Long operations report progress on a shared event channel. An operation with
 * an unknown length (a streaming comparison does not know the row count up
 * front) renders an indeterminate bar with live counters instead.
 */
export function JobProgress({ update, running, onDismiss, onLog }: Props) {
  if (!update) return null;

  const total = update.total ?? null;
  const determinate = total !== null && total > 0;
  const percent = determinate
    ? Math.min(100, Math.round((update.processed / total!) * 100))
    : null;

  const cancel = async () => {
    try {
      const accepted = await api.cancelJob(update.jobId);
      if (accepted) {
        onLog("warn", "已请求取消，正在收尾…");
      } else {
        onLog("warn", "该任务已结束，无需取消");
      }
    } catch (error) {
      onLog("err", `取消失败: ${String(error)}`);
    }
  };

  return (
    <div className="progress-card">
      <div className="progress-head">
        <span className="progress-label">{update.label}</span>
        <span className="faint">
          {determinate
            ? `${update.processed} / ${total}`
            : update.processed > 0
              ? `已处理 ${update.processed}`
              : ""}
        </span>
        <div className="grow" />
        {update.cancelled && <span className="badge missing">取消中</span>}
        {running && !update.cancelled && (
          <button className="btn small danger" onClick={() => void cancel()}>
            取消
          </button>
        )}
        {!running && (
          <button className="icon-btn" title="关闭" onClick={onDismiss}>
            <IconClose size={11} />
          </button>
        )}
      </div>

      <div className={`progress-track ${determinate ? "" : "indeterminate"}`}>
        <div
          className="progress-fill"
          style={determinate ? { width: `${percent}%` } : undefined}
        />
      </div>

      {update.detail && <div className="progress-detail">{update.detail}</div>}
    </div>
  );
}
