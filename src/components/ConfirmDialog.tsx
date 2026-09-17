import { useEffect, type ReactNode } from "react";

interface Props {
  title: string;
  message?: ReactNode;
  /** Extra content between the message and the buttons. */
  children?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  busy?: boolean;
  error?: string | null;
  onConfirm: () => void;
  onClose: () => void;
}

/**
 * A plain in-app confirmation dialog.
 *
 * `window.confirm` is not dependable inside a webview (WKWebView has no native
 * prompt panel), so every confirmation in the app goes through this component.
 */
export function ConfirmDialog({
  title,
  message,
  children,
  confirmLabel = "确定",
  cancelLabel = "取消",
  danger,
  busy,
  error,
  onConfirm,
  onClose,
}: Props) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="overlay"
      onMouseDown={(event) => event.target === event.currentTarget && onClose()}
    >
      <div className="modal" style={{ width: 520 }}>
        <div className="modal-head">
          <span>{title}</span>
          <button className="icon-btn" onClick={onClose} title="关闭">
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
              <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        <div className="modal-body">
          {message && <div style={{ lineHeight: 1.7 }}>{message}</div>}
          {children}
          {error && (
            <div className="errbox" style={{ marginTop: 12 }}>
              {error}
            </div>
          )}
        </div>

        <div className="modal-foot">
          <div className="spacer" />
          <button className="btn" onClick={onClose} disabled={busy}>
            {cancelLabel}
          </button>
          <button
            className={danger ? "btn danger" : "btn primary"}
            onClick={onConfirm}
            disabled={busy}
          >
            {busy ? "处理中…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
