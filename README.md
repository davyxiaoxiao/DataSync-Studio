# DataSync Studio

一个仿 Navicat 的 macOS 数据库同步工具，支持 **MySQL** 与 **PostgreSQL**。

> **只支持同类型同步**：MySQL → MySQL、PostgreSQL → PostgreSQL。
> 跨类型（MySQL → PostgreSQL 或反向）会被明确拒绝——两者的类型、生成表达式、
> 视图 SQL 都不同，机械转换无法保证正确。界面上目标连接会按源连接的类型自动过滤。

- **连接管理**：保存 / 编辑 / 测试 / 删除连接，密码加密存储
- **库表浏览**：模式（数据库）→ 表树、表数据分页浏览
- **结构同步**：对比两个库的表 / 列 / 索引 / 外键 / 主键差异，勾选后生成 DDL 并执行
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
| 列 | 新增、删除、修改（类型 / 可空 / 默认值 / 自增 / 注释 / 字符集 / 生成列表达式）；向已有数据的表新增非空列时会自动用占位值回填再删掉默认值 |
| 主键 | 新增、删除、修改 |
| 索引 | 新增、删除、修改（含唯一性） |
| 外键 | 新增、删除、修改（含 ON DELETE / ON UPDATE） |
| 视图 | 新增、删除、修改定义（同类型数据库之间；跨类型会被拒绝） |
| 物化视图 | 同上，PostgreSQL 使用 `CREATE MATERIALIZED VIEW ... WITH DATA` |
| 自定义类型 | PostgreSQL `ENUM` / `DOMAIN`：新建、删除；枚举加值（含位置）、域的可空/默认值/CHECK 约束修改 |

执行顺序经过设计以避免依赖冲突：
先删除失效外键 → 删除表 → 创建表（含主键）→ 删除索引 → 修改主键 → 删除列 → 修改列 → 新增列 → 新增索引 → 新增外键。

### 4. 数据同步

1. 工具栏「数据同步」
2. 选源表与目标表（可跨库、跨数据库类型）
3. 「开始对比」→ 得到新增 / 修改 / 删除统计与逐行明细
4. 选择要执行的动作（**删除默认关闭**），可选是否使用事务
5. 「预览 SQL」或「执行同步」

匹配规则：默认使用**目标表主键**，并要求源表存在同名字段；字段映射默认按同名自动建立。
对比字段必须非空（主键天然满足）。

**流式对比**：两侧都按主键排序、用 keyset 分页逐页读取并做归并 join，内存占用与表大小
无关（只与差异行数相关），因此**不再有比较行数上限**。排序统一强制为字节序
（MySQL `CAST(col AS BINARY)`、PostgreSQL `COLLATE "C"`），所以即使两端表的 collation
不同（例如 `utf8mb4_general_ci` 对 `utf8mb4_bin`），归并也不会错位。

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
  src/schema/ddl.rs        DDL 生成与列定义渲染
  src/data/mod.rs          数据读取计划、行差异计算、DML 生成
  src/commands.rs          Tauri 命令层
  src/tests/               纯逻辑单元测试
```

### 关键设计

**归一化结构模型**：MySQL 的 `information_schema` 与 PostgreSQL 的 `pg_catalog` 各自被读取为
统一的 `TableStructure`（列 / 主键 / 索引 / 外键），对比与 DDL 生成只处理这一种模型，
两个引擎共用同一套代码路径。

**视图依赖处理**：视图定义里的 schema 限定会被重写到目标库（否则目标视图会去读**源库**的表——
两边数据一致时完全看不出来）。当某张表发生变化时，依赖它的视图（含未被勾选的、以及视图套视图的
情况）会先按逆依赖序删除、再按依赖序重建，因为 PostgreSQL 不允许直接修改被视图引用的列。

**新增非空列**：向已有数据的表加一个非空且无默认值的列，PostgreSQL 会直接拒绝。工具会先用
类型对应的占位值（数值 `0`、文本 `''`、布尔 `false`、时间 `1970-01-01`、枚举取第一个标签等）
建立该列并回填现有行，随后立即 `DROP DEFAULT`，并在日志里说明用了什么占位值。无法推断占位值的
类型（如 `point`）会明确告警而不是猜一个值。

**自定义类型（仅 PostgreSQL）**：`ENUM` 与 `DOMAIN` 会先于使用它们的表创建、晚于引用它们的表删除。
枚举只支持增加值（PostgreSQL 本身不支持删除枚举值）；域的基础类型变更同样不支持，这两种情况
会明确告警而不是生成无法执行的 DDL。列上的自定义类型会被**显式限定 schema**，因为目标 schema
不一定在连接的 `search_path` 里。

**物化视图刷新**：PostgreSQL 的物化视图在创建时快照数据，所以数据同步完成后会自动
`REFRESH MATERIALIZED VIEW`（可在界面上关闭），否则它会一直读到创建那一刻的旧数据。

**生成列保真**：MySQL 的 `STORED` / `VIRTUAL GENERATED` 与 PostgreSQL 的
`GENERATED ALWAYS AS (...) STORED` 会被识别并原样重建。数据同步把这些列排除在
INSERT / UPDATE 之外（值由目标端自己计算），因此既不会被降级成普通列，也不会被服务端拒绝。

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

64 个无需数据库的测试（含 18 个 IPC 契约测试），覆盖：跨方言类型映射、结构差异分类、`int(11)` 显示宽度、
跨方言等价类型判定、MySQL / PostgreSQL 的 `CREATE TABLE` 生成、变更计划顺序、
选择集过滤（含「全不选」语义）、DML 字面量转义与批量 INSERT、**INSERT 字段映射与改名的
位置正确性**、数值规范化比较、行级差异计算、SQL 脚本切分、主密钥跨重启稳定性、
视图/类型 diff 与依赖排序、非空列占位值。

另有一组 **IPC 契约测试**：把每个 DTO 序列化成 JSON，逐一断言前端依赖的 camelCase 字段与
枚举拼写（`"mysql"` / `"missing"` / `"int"` / `"bytes"` …），并反向验证前端发来的 payload
能被 Rust 结构体反序列化。这弥补了「TS 类型是手写镜像、编译期无人校验」的缺口。

### 真实数据库端到端测试

```bash
cp .env.live.example .env.live    # 填入 host / port / user / password
./scripts/run-live-tests.sh
```

测试会走完应用使用的同一条代码路径（内省 → 对比 → 生成 DDL/DML → 执行），
并在服务器上创建再删除名为 `datasync_live_*` 的临时库 / 模式：

1. 在源端建两张表，覆盖主键、唯一键、索引、外键、JSON、BLOB、布尔、DECIMAL、
   DATETIME 以及 STORED 生成列
2. 空目标端对比 → 生成 DDL → 执行 → 再对比，断言「已完全一致」
3. 源端制造结构漂移（加列 / 加宽列 / 删列 / 改类型 / 加索引 / 改默认值）→ 同步 → 再断言一致
4. 源端插入数据 → 数据同步 → 再对比断言 0 新增 / 0 修改 / 0 删除
5. 源端 UPDATE + DELETE → 再同步 → 再断言一致
6. 断言目标端的生成列被真正重算（`amount * 2`），而不是被拷贝或留空
7. 源端建视图、视图套视图、PostgreSQL 物化视图 → 同步 → 断言目标端视图可查询、
   不引用源库、且行数与源端一致（物化视图需正确刷新）
8. PostgreSQL：建 `ENUM` 与 `DOMAIN`、并在表上使用该枚举 → 同步 → 断言枚举值、
   域的基础类型与 CHECK 约束都到了目标端；漂移阶段给枚举加值 → 再断言已传播
9. 数据同步完成后，再往源表加一个非空无默认值的列 → 同步 → 断言成功、
   占位默认值已被删除、现有行已回填
10. 表数据浏览：分页、`has_more`、列 kind 与网格对齐、PostgreSQL 的
    `uuid` / `inet` / `text[]` / 枚举等需 `::text` 转换的类型、MySQL 的 TEXT 列

只配置 MySQL 或只配置 PostgreSQL 时跑单引擎流程；两者都配置时会额外验证
**跨类型同步被正确拒绝**。

未配置 `.env.live` 时这些测试会自动跳过，`cargo test` 仍然全绿。

**当前状态**：MySQL→MySQL 与 PostgreSQL→PostgreSQL 两条链路均已在真实服务器上跑通
（结构新建、结构漂移同步、生成列保真与目标端重算、视图与物化视图同步、
PostgreSQL enum/domain 类型同步、数据新增/更新/删除同步、外键与索引），
跨类型请求被拒绝、流式分页与跨页增量也有真库测试覆盖。

---

## 已知限制

- 只支持 MySQL 与 PostgreSQL；外部表会被跳过并提示
- 视图同步不保留 `DEFINER` / `SQL SECURITY` / `ALGORITHM`，会以当前用户重建
- 数据同步的**差异语句**会全部保留在内存中以便「预览即执行」；差异行数极大时内存随之增长
- 不支持跨类型同步（见开头说明），两端必须是同一引擎
- 不支持删除 PostgreSQL 枚举值、修改域的基础类型（引擎本身不支持），会告警提示手工处理
- MySQL 的 `SET` 类型仍按普通文本处理
- 分区表按普通表处理
- `pnpm tauri build` 默认只产出 `.app`；DMG 需要 Finder 自动化权限，故未列为默认目标
