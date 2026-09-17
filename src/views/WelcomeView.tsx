import { useEffect, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";

export function WelcomeView() {
  const { connections, openTab, log } = useStore();
  const [storageDir, setStorageDir] = useState("");
  const [storageWarning, setStorageWarning] = useState<string | null>(null);

  useEffect(() => {
    api
      .appStorageDir()
      .then((info) => {
        setStorageDir(info.dir);
        if (info.warning) {
          setStorageWarning(info.warning);
          log("warn", info.warning);
        }
      })
      .catch(() => setStorageDir(""));
  }, [log]);

  return (
    <div className="panel-scroll">
      <div className="section" style={{ padding: "22px 24px" }}>
        <h2 style={{ margin: "0 0 6px", fontSize: 17 }}>DataSync Studio</h2>
        <p className="muted" style={{ margin: 0, lineHeight: 1.8 }}>
          面向 MySQL / PostgreSQL 的结构同步与数据同步工具。左侧双击连接即可建立会话，
          然后使用工具栏的「结构同步」「数据同步」。
        </p>
      </div>

      <div className="section">
        <div className="section-title">快速开始</div>
        <div className="row">
          <button
            className="btn primary"
            onClick={() => openTab({ kind: "structure", title: "结构同步" })}
          >
            结构同步
          </button>
          <button className="btn" onClick={() => openTab({ kind: "data", title: "数据同步" })}>
            数据同步
          </button>
        </div>
        <p className="faint" style={{ marginTop: 10, marginBottom: 0, lineHeight: 1.8 }}>
          结构同步：选择源库与目标库 → 自动对比表 / 列 / 索引 / 外键差异 → 勾选要应用的变更 →
          预览 DDL → 在目标库执行。
          <br />
          数据同步：选择源表与目标表 → 按主键匹配 → 得到新增 / 修改 / 删除清单 →
          预览 DML → 事务化执行。
        </p>
      </div>

      <div className="section">
        <div className="section-title">
          已保存的连接 <span className="hint">{connections.length} 个</span>
        </div>
        {connections.length === 0 ? (
          <div className="faint">还没有连接配置，点击左上角「新建连接」开始。</div>
        ) : (
          <div className="split">
            {connections.map((config) => (
              <div className="card" key={config.id}>
                <div className="card-title">
                  <span
                    style={{
                      width: 8,
                      height: 8,
                      borderRadius: "50%",
                      background: config.color ?? "var(--accent)",
                      display: "inline-block",
                    }}
                  />
                  {config.name}
                  <span className="badge">
                    {config.dialect === "postgres" ? "PostgreSQL" : "MySQL"}
                  </span>
                </div>
                <dl className="kv">
                  <dt>主机</dt>
                  <dd>
                    {config.host}:{config.port}
                  </dd>
                  <dt>用户</dt>
                  <dd>{config.username}</dd>
                  <dt>数据库</dt>
                  <dd>{config.database || "(未指定)"}</dd>
                  <dt>密码</dt>
                  <dd>{config.hasPassword ? "已保存" : "未保存"}</dd>
                </dl>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="section" style={{ borderBottom: "none" }}>
        <div className="section-title">配置存储位置</div>
        {storageWarning && (
          <div className="warnbox" style={{ marginBottom: 10 }}>
            {storageWarning}
          </div>
        )}
        <div className="row">
          <code className="mono" style={{ fontSize: 11.5 }}>
            {storageDir || "读取中…"}
          </code>
          <button
            className="btn small"
            disabled={!storageDir}
            onClick={() => {
              void navigator.clipboard
                .writeText(storageDir)
                .then(() => log("ok", "已复制配置目录路径"))
                .catch(() => log("warn", "复制失败，请手动选择文本"));
            }}
          >
            复制路径
          </button>
        </div>
        <p className="faint" style={{ marginTop: 8, marginBottom: 0 }}>
          连接信息以 JSON 保存，密码使用 AES-256-GCM 加密后存放，主密钥保存在 macOS 钥匙串
          （不可用时回退为配置文件目录下的 600 权限密钥文件）。
        </p>
      </div>
    </div>
  );
}
