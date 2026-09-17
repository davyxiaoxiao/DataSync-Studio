import { useStore, type Tab } from "../store";
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
  const { tabs, activeTabId, setActiveTab, closeTab } = useStore();

  return (
    <div className="tabbar">
      {tabs.map((tab) => (
        <div
          key={tab.id}
          className={`tab ${tab.id === activeTabId ? "active" : ""}`}
          onClick={() => setActiveTab(tab.id)}
          onAuxClick={(event) => {
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
    </div>
  );
}
