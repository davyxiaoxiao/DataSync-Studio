# DataSync Studio

一个仿 Navicat 的 macOS 数据库同步工具，支持 **MySQL** 与 **PostgreSQL**：

- **连接管理**：保存 / 编辑 / 测试 / 删除连接，密码加密存储
- **库表浏览**：模式（数据库）→ 表树、表数据分页浏览
- **结构同步**：对比两个库的表 / 列 / 索引 / 外键 / 主键差异，勾选后生成 DDL 并执行（支持跨数据库类型 Type Mapping）
- **数据同步**：按主键匹配，计算新增 / 修改 / 删除行，批量生成 DML 并事务化执行

技术栈：**Tauri 2 + React 19 + TypeScript + Rust (sqlx)**。Rust 侧负责全部数据库通信与 SQL 生成，前端只做展示与选择。

---

## 快速开始

```bash
# 依赖安装（首次）
pnpm install

# 开发模式（会打开应用窗口）
pnpm tauri dev

# 打包 .app，产物在 src-tauri/target/release/bundle/macos/
pnpm tauri build

# 需要 DMG 安装包时（DMG 打包依赖 Finder 自动化，需在正常桌面会话下运行）
pnpm tauri build --bundles dmg
```

> 如果本机 pnpm / cargo 的全局缓存目录不可写，可先 `source dev-env.sh`，
> 它会把所有工具缓存重定向到仓库内的 `.cache/`。

### 前置要求

- macOS（Apple Silicon 或 Intel）
- Node.js 18+ / pnpm
- Rust 稳定版 + Xcode Command Line Tools

---

## 使用流程

### 1. 新建连接

工具栏「新建连接」→ 选择 MySQL / PostgreSQL → 填写主机、端口、用户名、密码、数据库。

- 密码使用 **AES-256-GCM** 加密后保存在配置目录，主密钥存放在 **macOS 钥匙串**。
  钥匙串不可用时回退为配置目录下的 `.masterkey`（0600 权限）。
- 「测试连接」会在保存前验证连通性并显示服务端版本。

### 2. 连接与浏览

左侧双击连接建立会话，展开后即可浏览模式与表。点击表名打开数据浏览标签页。

### 3. 结构同步

1. 工具栏「结构同步」
2. 左侧选 **源（结构基准）**，右侧选 **目标（将被修改）**
3. 「开始对比」→ 得到差异树
4. 勾选要应用的变更（默认已排除所有删除类操作）
5. 「预览 SQL」检查 DDL，或直接「执行同步」

支持的操作：

| 类别 | 支持 |
| --- | --- |
| 表 | 新建表、删除表 |
| 列 | 新增、删除、修改（类型 / 可空 / 默认值 / 自增 / 注释 / 字符集） |
| 主键 | 新增、删除、修改 |
| 索引 | 新增、删除、修改（含唯一性） |
| 外键 | 新增、删除、修改（含 ON DELETE / ON UPDATE） |

执行顺序经过设计以避免依赖冲突：
先删除失效外键 → 删除表 → 创建表（含主键）→ 删除索引 → 修改主键 → 删除列 → 修改列 → 新增列 → 新增索引 → 新增外键。

### 4. 数据同步

1. 工具栏「数据同步」
2. 选源表与目标表（可跨库、跨数据库类型）
3. 「开始对比」→ 得到新增 / 修改 / 删除统计与逐行明细
4. 选择要执行的动作（**删除默认关闭**），可选是否使用事务
5. 「预览 SQL」或「执行同步」

匹配规则：默认使用**目标表主键**，并要求源表存在同名字段；字段映射默认按同名自动建立。
单表对比行数默认上限 200000，超出会给出提示。

---

## 架构

```
src/                       前端（React + TypeScript）
  api.ts                   IPC 封装（类型安全）
  types.ts                 Rust DTO 的 TS 镜像
  store.tsx                连接会话 / 标签页 / 日志状态
  components/              工具栏、连接树、标签页、日志、连接对话框、端点选择器、SQL 预览
  views/                   欢迎页、表数据、结构同步、数据同步
src-tauri/                 Rust 后端
  src/dialect.rs           方言抽象：标识符引用、字符串/字节字面量、布尔、表选项
  src/store.rs             连接配置持久化 + 主密钥 + AES-256-GCM 加解密
  src/db/mod.rs            连接池注册表、查询执行、脚本执行、事务执行
  src/db/value.rs          跨驱动单元格解码（Value / Cell / ColumnKind）
  src/schema/introspect.rs MySQL / PostgreSQL 结构内省
  src/schema/diff.rs       结构对比引擎
  src/schema/ddl.rs        跨方言类型映射 + DDL 生成
  src/data/mod.rs          数据读取计划、行差异计算、DML 生成
  src/commands.rs          Tauri 命令层
  src/tests/               纯逻辑单元测试
```

### 关键设计

**归一化结构模型**：MySQL 的 `information_schema` 与 PostgreSQL 的 `pg_catalog` 各自被读取为
统一的 `TableStructure`（列 / 主键 / 索引 / 外键），对比引擎只处理这一种模型，因此
「MySQL ↔ PostgreSQL 跨类型对比」不需要单独的代码路径。

**类型映射**：跨方言对比时先把源类型映射到目标方言再比较，因此 `int(11)` 与 `integer`
不会被视为差异，而 `varchar(64)` 与 `varchar(128)` 会。内置常见类型的双向映射（见
`ddl.rs::map_type`），未知类型退化为 `text` / `longtext`。

**值解码**：不依赖驱动的结果集类型推断，而是使用内省得到的列类型决定如何解码；
PostgreSQL 上驱动无法原生解码的类型（`uuid`、`inet`、数组、枚举、域等）会自动以
`col::text` 取出，避免整个查询失败。

**字面量而非绑定参数**：DML 由生成的字面量构成，因此「预览 SQL」与实际执行的内容
完全一致；转义按方言分别处理（MySQL 转义反斜杠，PostgreSQL 用标准 `''` 并转义 NUL）。

**计划缓存**：对比结果缓存在后端（结构对比与数据同步各一份）。预览与执行使用同一份
快照，避免两次计算之间数据变化导致执行内容与预览不符。

**启动降级**：配置目录不可写时回退到临时目录、主密钥无法持久化时使用临时密钥，
两者都会在界面上给出警告，而不是让应用启动失败。

---

## 测试

```bash
cd src-tauri && cargo test
```

21 个纯逻辑测试，无需数据库，覆盖：跨方言类型映射、结构差异分类、`int(11)` 显示宽度、
跨方言等价类型判定、MySQL / PostgreSQL 的 `CREATE TABLE` 生成、变更计划顺序、
选择集过滤（含「全不选」语义）、DML 字面量转义与批量 INSERT、**INSERT 字段映射与改名的
位置正确性**、数值规范化比较、行级差异计算、SQL 脚本切分、主密钥跨重启稳定性。

### 真实数据库端到端测试

```bash
cp .env.live.example .env.live    # 填入 host / port / user / password
./scripts/run-live-tests.sh
```

测试会走完应用使用的同一条代码路径（内省 → 对比 → 生成 DDL/DML → 执行），
并在服务器上创建再删除名为 `datasync_live_*` 的临时库 / 模式：

1. 在源端建两张带主键、唯一键、索引、外键、JSON、BLOB、布尔、DECIMAL、DATETIME 的表
2. 空目标端对比 → 生成 DDL → 执行 → 再对比，断言「已完全一致」
3. 源端制造结构漂移（加列 / 加宽列 / 删列 / 改类型 / 加索引 / 改默认值）→ 同步 → 再断言一致
4. 源端插入数据 → 数据同步 → 再对比断言 0 新增 / 0 修改 / 0 删除
5. 源端 UPDATE + DELETE → 再同步 → 再断言一致

只配置 MySQL 或只配置 PostgreSQL 时跑单引擎流程；两者都配置时会额外跑
**MySQL → PostgreSQL 跨库迁移**，覆盖类型映射与跨引擎取值比较。

未配置 `.env.live` 时这些测试会自动跳过，`cargo test` 仍然全绿。

**当前状态**：MySQL→MySQL、PostgreSQL→PostgreSQL、MySQL→PostgreSQL 三条链路均已在
真实服务器上跑通（结构新建、结构漂移同步、数据新增/更新/删除同步、外键与索引）。

---

## 已知限制

- 只支持 MySQL 与 PostgreSQL；视图 / 物化视图 / 外部表会被跳过并提示
- 数据同步为**全量对比**（按主键排序后读入内存），默认上限 20 万行；超大表需要分批策略
- 结构同步不做数据搬迁（例如给已有表加非空列且无默认值时，目标表已有数据会失败）
- 不支持自定义类型（PostgreSQL enum/domain、MySQL SET 语义）的等价映射，统一退化为文本
- 分区表按普通表处理
- `pnpm tauri build` 默认只产出 `.app`；DMG 需要 Finder 自动化权限，故未列为默认目标
