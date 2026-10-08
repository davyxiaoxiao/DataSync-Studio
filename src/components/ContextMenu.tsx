import type { MouseEvent as ReactMouseEvent } from "react";

/**
 * A right-click menu.
 *
 * Extracted because the sidebar tree and the tab strip both need one, including
 * the submenu behaviour, and two copies of this markup would drift.
 */

export interface MenuItem {
  label: string;
  danger?: boolean;
  disabled?: boolean;
  /** A trailing hint, used for file types in the import/export submenus. */
  hint?: string;
  /** When present the entry opens a nested list instead of running. */
  children?: MenuItem[];
  run?: () => void;
}

export interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

/** Open a menu at the pointer, keeping it inside the window. */
export function menuAt(event: ReactMouseEvent, items: MenuItem[]): MenuState {
  event.preventDefault();
  event.stopPropagation();
  // Roughly the menu's own size; enough to stop it hanging off the edge.
  const width = 190;
  const height = items.length * 26 + 12;
  return {
    x: Math.min(event.clientX, Math.max(window.innerWidth - width - 4, 4)),
    y: Math.min(event.clientY, Math.max(window.innerHeight - height - 4, 4)),
    items,
  };
}

interface Props {
  state: MenuState;
  onClose: () => void;
}

export function ContextMenu({ state, onClose }: Props) {
  const renderEntry = (item: MenuItem, key: string, nested: boolean) => (
    <div
      key={key}
      className={`menu-entry ${item.disabled ? "disabled" : ""} ${
        item.danger ? "danger" : ""
      } ${item.children ? "has-children" : ""} ${nested ? "nested" : ""}`}
      onClick={(event) => {
        if (item.children) return;
        event.stopPropagation();
        if (item.disabled || !item.run) return;
        onClose();
        item.run();
      }}
    >
      <div className="menu-row">
        <span>{item.label}</span>
        {item.hint && <span className="faint mono">{item.hint}</span>}
        {item.children && <span className="faint">▸</span>}
      </div>
      {item.children && (
        <div className="menu-sub">
          {item.children.map((child, index) =>
            renderEntry(child, `${key}-${index}`, true),
          )}
        </div>
      )}
    </div>
  );

  return (
    <>
      {/* Clicking anywhere else, or right-clicking again, dismisses the menu. */}
      <div
        style={{ position: "fixed", inset: 0, zIndex: 200 }}
        onClick={onClose}
        onContextMenu={(event) => {
          event.preventDefault();
          onClose();
        }}
      />
      <div
        style={{
          position: "fixed",
          left: state.x,
          top: state.y,
          zIndex: 201,
          minWidth: 168,
          background: "#fff",
          border: "1px solid var(--border-strong)",
          borderRadius: 7,
          boxShadow: "0 10px 26px rgba(15,20,28,.18)",
          padding: 4,
        }}
      >
        {state.items.map((item, index) => renderEntry(item, String(index), false))}
      </div>
    </>
  );
}
