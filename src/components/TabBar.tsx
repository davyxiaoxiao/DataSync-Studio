import { useState } from "react";
import { useStore, type Tab } from "../store";
import { ContextMenu, menuAt, type MenuState } from "./ContextMenu";
import {
  IconClose,
  IconCompare,
  IconDatabase,
  IconTable,
} from "./Icons";

function TabIcon({ kind }: { kind: Tab["kind"] }) {
  switch (kind) {
    case "structure":
      return <IconCompare size={12} />;
    case "data":
      return <IconCompare size={12} />;
    case "table":
      return <IconTable size={12} />;
    default:
      return <IconDatabase size={12} />;
  }
}

export function TabBar() {
  const { tabs, activeTabId, setActiveTab, closeTab, closeOtherTabs, closeAllTabs } =
    useStore();
  const [menu, setMenu] = useState<MenuState | null>(null);

  return (
    <div className="tabbar">
      {tabs.map((tab) => (
        <div
          key={tab.id}
          className={`tab ${tab.id === activeTabId ? "active" : ""}`}
          onClick={() => setActiveTab(tab.id)}
          onContextMenu={(event) =>
            setMenu(
              menuAt(event, [
                {
                  label: "关闭",
                  run: () => closeTab(tab.id),
                },
                {
                  label: "关闭其他",
                  disabled: tabs.length <= 1,
                  run: () => closeOtherTabs(tab.id),
                },
                {
                  label: "全部关闭",
                  disabled: tabs.every((entry) => entry.kind === "welcome"),
                  run: () => closeAllTabs(),
                },
              ]),
            )
          }
          onAuxClick={(event) => {
            // Middle click closes, as it does everywhere else.
            if (event.button === 1) closeTab(tab.id);
          }}
        >
          <TabIcon kind={tab.kind} />
          <span className="title" title={tab.title}>
            {tab.title}
          </span>
          <button
            className="close"
            onClick={(event) => {
              event.stopPropagation();
              closeTab(tab.id);
            }}
            title="关闭"
          >
            <IconClose size={10} />
          </button>
        </div>
      ))}

      {menu && <ContextMenu state={menu} onClose={() => setMenu(null)} />}
    </div>
  );
}
