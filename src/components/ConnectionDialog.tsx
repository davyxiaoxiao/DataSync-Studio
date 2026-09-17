import { useEffect, useState } from "react";
import { api, errorMessage } from "../api";
import type { ConnectionConfig, Dialect, TestConnectionResult } from "../types";

interface Props {
  initial: ConnectionConfig | null;
  onClose: () => void;
  onSaved: (config: ConnectionConfig, isNew: boolean) => void;
}

const COLORS = ["#1f6feb", "#1a7f37", "#cf222e", "#b45309", "#7c3aed", "#0f766e"];

function blankConfig(dialect: Dialect, index: number): ConnectionConfig {
  return {
    id: "",
    name: `新建连接 ${index}`,
    dialect,
    host: "127.0.0.1",
    port: dialect === "postgres" ? 5432 : 3306,
    username: dialect === "postgres" ? "postgres" : "root",
    database: "",
    sslMode: "prefer",
    group: "",
    color: COLORS[0],
    remark: "",
    savePassword: true,
    password: "",
    hasPassword: false,
  };
}

export function ConnectionDialog({ initial, onClose, onSaved }: Props) {
  const [draft, setDraft] = useState<ConnectionConfig>(
    initial ?? blankConfig("mysql", 1),
  );
  const [testing, setTesting] = useState(false);
  const [testingLabel, setTestingLabel] = useState("");
  const [result, setResult] = useState<TestConnectionResult | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const isNew = !initial;

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const patch = (partial: Partial<ConnectionConfig>) =>
    setDraft((prev) => ({ ...prev, ...partial }));

  const changeDialect = (dialect: Dialect) => {
    const wasDefaultPort =
      draft.port === (draft.dialect === "postgres" ? 5432 : 3306) || draft.port === 0;
    patch({
      dialect,
      port: wasDefaultPort ? (dialect === "postgres" ? 5432 : 3306) : draft.port,
      username:
        draft.username === "root" || draft.username === "postgres"
          ? dialect === "postgres"
            ? "postgres"
            : "root"
          : draft.username,
    });
    setResult(null);
  };

  const runTest = async () => {
    setTesting(true);
    setResult(null);
    setTestingLabel(draft.dialect === "postgres" ? "PostgreSQL" : "MySQL");
    try {
      const outcome = await api.testConnection(draft);
      setResult(outcome);
    } catch (error) {
      setResult({
        ok: false,
        elapsedMs: 0,
        error: errorMessage(error),
      });
    } finally {
      setTesting(false);
    }
  };

  const save = async () => {
    if (!draft.name.trim()) {
      setSaveError("请填写连接名称");
      return;
    }
    if (!draft.host.trim()) {
      setSaveError("请填写主机地址");
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const saved = await api.saveConnection({
        ...draft,
        port: Number(draft.port) || (draft.dialect === "postgres" ? 5432 : 3306),
      });
      onSaved(saved, isNew);
    } catch (error) {
      setSaveError(errorMessage(error));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="overlay" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="modal" style={{ width: 620 }}>
        <div className="modal-head">
          <span>{isNew ? "新建连接" : `编辑连接 · ${initial?.name}`}</span>
          <button className="icon-btn" onClick={onClose} title="关闭">
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
              <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
            </svg>
          </button>
        </div>

        <div className="modal-body">
          <div className="row" style={{ marginBottom: 14 }}>
            <div className="pill-tabs">
              <button
                className={draft.dialect === "mysql" ? "active" : ""}
                onClick={() => changeDialect("mysql")}
              >
                MySQL
              </button>
              <button
                className={draft.dialect === "postgres" ? "active" : ""}
                onClick={() => changeDialect("postgres")}
              >
                PostgreSQL
              </button>
            </div>
            <div className="grow" />
            <div className="row" style={{ gap: 4 }}>
              {COLORS.map((color) => (
                <button
                  key={color}
                  onClick={() => patch({ color })}
                  title="连接颜色标记"
                  style={{
                    width: 18,
                    height: 18,
                    borderRadius: "50%",
                    background: color,
                    border:
                      draft.color === color ? "2px solid var(--text)" : "1px solid rgba(0,0,0,.15)",
                    padding: 0,
                    cursor: "default",
                  }}
                />
              ))}
            </div>
          </div>

          <div className="form-grid">
            <label>连接名</label>
            <input
              type="text"
              className="full"
              value={draft.name}
              onChange={(event) => patch({ name: event.target.value })}
              placeholder="例如：本地 MySQL"
            />

            <label>主机</label>
            <input
              type="text"
              value={draft.host}
              onChange={(event) => patch({ host: event.target.value })}
            />
            <label>端口</label>
            <input
              type="number"
              value={draft.port}
              onChange={(event) => patch({ port: Number(event.target.value) })}
            />

            <label>用户名</label>
            <input
              type="text"
              value={draft.username}
              onChange={(event) => patch({ username: event.target.value })}
            />
            <label>密码</label>
            <input
              type="password"
              value={draft.password ?? ""}
              onChange={(event) => patch({ password: event.target.value })}
              placeholder={initial?.hasPassword ? "留空则沿用已保存密码" : ""}
            />

            <label>数据库</label>
            <input
              type="text"
              value={draft.database ?? ""}
              onChange={(event) => patch({ database: event.target.value })}
              placeholder={draft.dialect === "postgres" ? "postgres" : "可选"}
            />
            <label>SSL</label>
            <select
              value={draft.sslMode}
              onChange={(event) => patch({ sslMode: event.target.value })}
            >
              <option value="disable">禁用</option>
              <option value="prefer">优先（自动协商）</option>
              <option value="require">强制</option>
            </select>

            <label>备注</label>
            <input
              type="text"
              className="full"
              value={draft.remark ?? ""}
              onChange={(event) => patch({ remark: event.target.value })}
            />

            <div />
            <label className="check" style={{ justifySelf: "start" }}>
              <input
                type="checkbox"
                checked={draft.savePassword}
                onChange={(event) => patch({ savePassword: event.target.checked })}
              />
              保存密码到 macOS 钥匙串
            </label>
          </div>

          {result && (
            <div style={{ marginTop: 14 }}>
              {result.ok ? (
                <div
                  style={{
                    padding: "8px 12px",
                    borderRadius: 6,
                    background: "var(--green-soft)",
                    color: "var(--green)",
                    border: "1px solid #b7dfc3",
                  }}
                >
                  连接成功 · {testingLabel} {result.serverVersion} · {result.elapsedMs} ms
                </div>
              ) : (
                <div className="errbox">{result.error}</div>
              )}
            </div>
          )}

          {saveError && (
            <div className="errbox" style={{ marginTop: 14 }}>
              {saveError}
            </div>
          )}
        </div>

        <div className="modal-foot">
          <button className="btn" onClick={() => void runTest()} disabled={testing}>
            {testing ? "测试中…" : "测试连接"}
          </button>
          <div className="spacer" />
          <button className="btn" onClick={onClose}>
            取消
          </button>
          <button className="btn primary" onClick={() => void save()} disabled={saving}>
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
