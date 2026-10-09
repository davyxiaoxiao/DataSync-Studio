import { IconSpinner } from "./Icons";

/**
 * Waiting feedback.
 *
 * Every action that goes to the database gets one of these, because a window
 * that shows nothing while it waits is indistinguishable from one that has
 * frozen — and the user's next move is to force quit it.
 */
export function Loading({
  label = "加载中…",
  detail,
}: {
  label?: string;
  detail?: string;
}) {
  return (
    <div className="loading-block">
      <span className="icon spin">
        <IconSpinner size={18} />
      </span>
      <div>{label}</div>
      {detail && <div className="faint">{detail}</div>}
    </div>
  );
}

/**
 * The same feedback laid over content that is being replaced.
 *
 * Used when there is already something on screen: the rows stay visible but
 * dimmed, so the wait does not blank out context the user was reading.
 */
export function LoadingOverlay({
  label = "加载中…",
  detail,
}: {
  label?: string;
  detail?: string;
}) {
  return (
    <div className="loading-overlay">
      <div className="loading-card">
        <span className="icon spin">
          <IconSpinner size={16} />
        </span>
        <span>{label}</span>
        {detail && <span className="faint">{detail}</span>}
      </div>
    </div>
  );
}

/** A small inline spinner for buttons and tree rows. */
export function InlineLoading({ label }: { label?: string }) {
  return (
    <span className="row" style={{ gap: 6, display: "inline-flex" }}>
      <span className="icon spin">
        <IconSpinner size={11} />
      </span>
      {label && <span className="faint">{label}</span>}
    </span>
  );
}
