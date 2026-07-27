# 图数据库市场格局调研与 RocksGraph 生态位定位

Status: research — 供产品/路线图决策参考，非架构设计文档，不影响代码实现。

调研时间：2026-07。市场数据、产品状态引用见文末"参考来源"，后续更新请一并更新引用时间。

## 一、图数据库市场竞争格局调研

### 1.1 市场总体情况

图数据库市场规模约 42.1 亿美元（2026），预计以 27.19% 的复合增速在 2031 年达到 140.2
亿美元，处于"中度分散"状态——既有超大规模云厂商，也有独立商业化厂商和大量开源/细分玩家。
Neo4j、AWS、TigerGraph 被行业报告列为该市场的"头部玩家"（Star players）。Neo4j 年收入已
超过 2 亿美元，主要靠 AuraDB（托管 SaaS）驱动；TigerGraph 融资 1.717 亿美元，靠 Savanna
云产品主攻金融/电信行业的 OLAP 图分析场景。

**市场增速的驱动因素**（2024-2026 年的新增量）：

| 驱动因素 | 对市场增速的贡献 | 对嵌入式图引擎的关联 |
|---------|----------------|-------------------|
| AI/ML 对知识图谱和 GraphRAG 的需求爆发 | 最大单一增量 | 高 — Agent 本地记忆需要嵌入式方案 |
| 金融欺诈检测和风控的实时图遍历需求 | 稳定增长 | 中 — OLTP 遍历是 RocksGraph 强项 |
| 供应链/物联网的数字孪生 | 快速增长 | 中 — 边缘设备上的本地图推理 |
| 云厂商将图能力内嵌到现有数据库产品中 | 结构性变化 | 低 — 反而是差异化机会 |

这个格局的含义：**头部竞争已经被云厂商和融资过亿的商业化公司卡位**，RocksGraph 作为个人/
小团队项目，正面竞争这一层没有意义。真正值得研究的是市场里还有没有没被这些重资本玩家覆盖
的架构层。

### 1.2 按部署形态分层

| 层级 | 代表产品 | 部署形态 | 查询语言 | 目标客群 | 起步成本（月） |
|------|---------|---------|---------|---------|--------------|
| 云原生托管服务 | AWS Neptune Database / Neptune Analytics、Azure Cosmos DB for Gremlin、GCP Spanner Graph | 全托管，按需付费的云服务 | Gremlin / openCypher / GQL（因产品而异） | 企业客户，愿意用云厂商生态换取免运维 | $50–$500+（最小实例） |
| 独立商业化分布式图数据库 | Neo4j（含 AuraDB）、TigerGraph（含 Savanna） | 自建集群或对应云 SaaS | Cypher（Neo4j）、GSQL（TigerGraph） | 中大型企业，图规模和并发要求高 | AuraDB 免费层 $0，专业版 ~$65/月起 |
| 开源自建分布式图数据库 | JanusGraph、NebulaGraph、HugeGraph、ArangoDB、Memgraph | 自己搭建并运维集群 | Gremlin（JanusGraph/HugeGraph）、nGQL（Nebula）、AQL（Arango）、Cypher（Memgraph） | 有运维能力、想省授权费的团队 | 服务器成本（自运维） |
| 嵌入式/进程内图引擎 | Kùzu（含社区 fork）、FalkorDB / FalkorDBLite、RocksGraph | 链接进宿主进程或作为本地子进程 | Cypher（Kùzu）、Cypher-like（FalkorDB）、Gremlin 风格（RocksGraph） | 单机应用、数据科学、AI Agent 本地记忆场景 | $0（开源库） |
| PostgreSQL 图扩展 | Apache AGE、pgRouting | PostgreSQL 扩展插件 | openCypher（AGE）、SQL | 已使用 PostgreSQL、不希望引入新数据库的团队 | $0（PG 扩展） |

**补充说明——PostgreSQL 图扩展这一层**：

- **Apache AGE** 把 openCypher 查询翻译为 PostgreSQL CTE（递归公用表表达式），底层仍是关系代数的 join。它满足的是"我不想引入新数据库"的心态，而不是"我需要一个真正的图数据库"。在遍历深度超过 3-4 跳时，CTE 展开的性能下降明显。
- **pgRouting** 专注于地理空间网络路由，不是通用图数据库。

这两者不是 RocksGraph 的直接竞品，但值得记住——当用户说"我在用 Postgres 的图扩展"时，实际上是在表达"我不想要一个新服务"，这正是嵌入式图引擎的切入机会。

前三层是重资本、重运维的战场，玩家实力和上一节的市场数据完全对应。**RocksGraph 现在的架构
（单进程、RocksDB 持久化、无网络服务、Gremlin 风格遍历）天然落在第四层**，所以这一层是本文
重点分析的对象。

### 1.3 嵌入式图引擎层的全貌

#### 1.3.1 直接竞品（同为嵌入式图库）

| 产品 | 语言 | 存储模型 | 查询语言 | 许可证 | 持久化 | 进程模型 | GitHub Stars（2026-07 约） |
|------|------|---------|---------|--------|-------|---------|--------------------------|
| **Kùzu（原版，已归档）** | C++ | 列式 | Cypher | MIT | ✅ 持久化 | 真嵌入 | ~5,000（归档前） |
| **LadybugDB（社区 fork）** | C++ | 列式（继承 Kùzu） | Cypher | MIT | ✅ | 真嵌入 | ~200（新建） |
| **Vela Kuzu fork** | C++ | 列式（继承 Kùzu） | Cypher | MIT | ✅ | 真嵌入 | ~350（新建） |
| **FalkorDBLite** | C（Redis 模块） | GraphBLAS 稀疏矩阵 | Cypher-like | SSPL（FalkorDB） | ✅ | **子进程 + Unix socket** | ~7,000（FalkorDB 整体） |
| **RocksGraph** | Rust | RocksDB / LSM | Gremlin 风格 | GPL-2.0+ | ✅ | 真嵌入 | 个人项目阶段 |

**关键观察：**

1. 这一层目前没有一个产品同时满足：①真嵌入（非子进程） ②非 C++（便于绑定） ③Gremlin 语义 ④活跃维护。
   RocksGraph 在 ①③④ 上可以占据唯一位置。
2. Kùzu 的两个 fork 都还很早期（stars 量级说明社区尚未形成），但它们继承了 Kùzu 的代码基础，
   技术成熟度起点不低。如果其中任何一个在 2026 Q4–2027 Q1 获得资本或较大的社区投入，"空当"
   的窗口可能缩短到 12-18 个月。
3. FalkorDBLite 的子进程模式在 Python 生态里分发体验不差（`pip install falkordblite` 自动
   拉取并管理子进程），对不关心"真嵌入 vs 子进程"的用户来说，这个差异不构成选择理由——但这个
   差异对 CLI 工具、Tauri 应用等场景至关重要（子进程在这些场景里是显著的负担）。

#### 1.3.2 邻近竞品（定位重叠但不是直接竞品）

| 产品 | 定位 | 部署形态 | 查询语言 | 与 RocksGraph 的差集 |
|------|------|---------|---------|---------------------|
| **Dgraph** | 分布式图数据库，GraphQL 原生 | 自建集群 / 云托管（Dgraph Cloud 已停） | GraphQL±（DQL） | 分布式，不适合嵌入 |
| **TypeDB** | 知识图谱 + 推理引擎 | 自建集群 | TypeQL | 面向本体推理，不走属性图模型 |
| **SurrealDB** | 多模型（文档+图），Rust 实现 | 嵌入式或分布式 | SurrealQL | 图能力是副功能，不是主业 |
| **PuppyGraph** | 对数据湖/仓库的图查询引擎 | 作为查询层叠加在现有数据湖上 | Gremlin / openCypher | 不自己存数据；依赖 Iceberg/Delta Lake/Hudi |
| **Memgraph** | 内存图数据库，Cypher 兼容 | 自建集群 | Cypher | 在内存，持久化是副本而非主存储 |
| **NetworkX** | Python 图算法库 | 纯内存 Python 对象 | 无查询语言，API 调用 | 无持久化，无查询优化，无并发 |
| **DuckDB + CTE** | 分析型 SQL 数据库，用递归 CTE 模拟图 | 嵌入式 | SQL（CTE） | 不是图数据库，深度遍历性能差 |

**值得关注的：SurrealDB**

SurrealDB 值得单独提，因为它是目前**唯一另一个 Rust 写的、有嵌入模式的、有图能力的数据库**。
但它不是图数据库——它的核心是文档模型，图遍历是其多模型能力的一部分（类似 ArangoDB 将图作为
AQL 的一个维度）。用户选择 SurrealDB 的理由是"一个数据库覆盖文档+图+搜索"，而不是"我需要
最好的图数据库"。这意味着 SurrealDB 和 RocksGraph 不是替代关系，一个用户完全可能在用
SurrealDB 存文档的同时用 RocksGraph 做图遍历。

#### 1.3.3 云厂商嵌入式/轻量方案

| 云厂商 | 轻量方案 | 限制 |
|--------|---------|-----|
| AWS | Neptune 无免费层，无本地开发版 | 最小 db.r6g.large ~$0.50/h |
| Azure | Cosmos DB 有免费层（1000 RU/s，5GB） | 免费层适合实验，但仍是云服务 |
| GCP | 无轻量图方案；Spanner Graph 从 ~$0.90/h 起步 | 最贵，且 Spanner 本身是关系数据库 |

**没有一家云厂商提供"本地开发用的嵌入式图引擎"**。开发者本地测试 Gremlin 遍历的选项只有：
①起一个完整的 JanusGraph/Neptune 实例（重）；②用 Gremlin Server + TinkerGraph（Java，内存
且不持久化）；③没有第三个选项。这是 RocksGraph 可以填补的空位。

### 1.4 全栈竞品特性矩阵

以下覆盖所有与 RocksGraph 存在重叠的产品，按"与 RocksGraph 的竞争关系"排序——越靠近的排越前：

| 产品 | 内核语言 | 嵌入？ | 存储模型 | 查询语言 | 许可 | API 协议 | 写优化 | 读优化 | 分布式？ | 向量支持？ |
|------|---------|-------|---------|---------|------|---------|-------|-------|---------|----------|
| **LadybugDB** | C++ | ✅ | 列式 | Cypher | MIT | C API | 否（列式为批导入优化） | 多跳遍历 | 否 | 否 |
| **Vela Kuzu** | C++ | ✅ | 列式 | Cypher | MIT | C API | 否 | 路径查询 | 否 | 规划中 |
| **FalkorDBLite** | C | ⚠️ 子进程 | 稀疏矩阵 | Cypher-like | SSPL | Redis 协议 | 否 | 稠密聚合 | 否 | ✅ |
| **RocksGraph** | Rust | ✅ 真嵌入 | LSM (RocksDB) | Gremlin 风格 | GPL-2.0+ | Rust/Python API | ✅ SST ingest | 点查+范围扫描 | 否 | 否（可外挂） |
| **SurrealDB** | Rust | ✅ | 自定义 KV | SurrealQL | BSL-1.1 | WS + REST | ✅ | 索引查询 | ✅ | ✅ |
| **Dgraph** | Go | 否 | 自定义 KV | GraphQL± | Apache-2.0 | gRPC + HTTP | ✅ | 索引查询 | ✅ | 否 |
| **Memgraph** | C/C++ | 否 | 内存+WAL | Cypher | BSL | Bolt 协议 | ✅ | 内存全扫描 | ✅ | 否 |
| **JanusGraph** | Java | 否 | 可插拔（Cassandra/HBase/Bigtable） | Gremlin | Apache-2.0 | Gremlin WS | ✅ | 取决于后端 | ✅ | 否 |
| **Neo4j** | Java | 否 | 自定义图存储 | Cypher | GPLv3（社区）/商业 | Bolt + HTTP | ✅ | 索引+遍历 | ✅（企业版） | ✅（企业版） |
| **TypeDB** | Java | 否 | RocksDB | TypeQL | AGPL | gRPC | ✅ | 推理引擎 | ✅ | 否 |

**矩阵的关键解读：**

1. **写优化是 RocksGraph 的结构性优势**。LSM 树天然优化写入，加上已有 SST 直接 ingest 管线（300K+ 边/秒），这一层里只有 RocksGraph 和 JanusGraph（后端是 Cassandra 时）是面向持续写入设计的。这意味着"数据会频繁变化的图"——如用户行为图、实时风控图——是 RocksGraph 比列式和稀疏矩阵引擎更适合的场景。
2. **Gremlin 语义在嵌入式层是独占**。JanusGraph 和 HugeGraph 虽然也走 Gremlin，但它们不是嵌入式的。一个从 JanusGraph 迁移下来的小规模部署，可以直接用 RocksGraph 替代而保持遍历代码不变——这在嵌入式层是唯一的。
3. **向量支持**是目前这一层所有产品都在加速补的能力（Vela fork 在规划，FalkorDB 已支持，SurrealDB 已支持）。RocksGraph 当前没有内置向量索引，但可以通过属性存储向量 + 外挂 ANN 索引库（如 `usearch`、`faiss`）来实现。这不是 v0.1 的优先事项，但如果未来要进入 GraphRAG 领域，这是一个必答题。

### 1.5 影响全行业的两个技术趋势

#### 1.5.1 ISO GQL:2024 — 图查询语言的标准化

2024 年 4 月，ISO 正式发布 **GQL（Graph Query Language）** 标准（ISO/IEC 39075），这是图数据库领域第一个国际标准查询语言。GQL 融合了 openCypher、GSQL 和 SQL 的图扩展，核心语法接近 Cypher 的模式匹配风格。

**对各层玩家的影响：**

- **云厂商**：Neptune 已经在预览版支持 GQL；GCP 的 Spanner Graph 原生走 GQL+SQL 混合。标准化让他们有动力把用户从 Gremlin 迁移走。
- **开源分布式层**：所有走自己查询语言的产品（NebulaGraph 的 nGQL、ArangoDB 的 AQL）要么迁移到 GQL，要么成为"仅限存量"的语言。
- **嵌入式层**：标准化的影响在这里最小——嵌入式的核心价值是"轻"和"近"，查询语言是次要的差异化因素。但 GQL 的普及可能推动更多用户从 Gremlin 转向 Cypher-like 语法，这会缩小"Gremlin 独占"这个差异化的长期价值。
- **对 RocksGraph 的影响**：短期（2026-2027）无影响——Gremlin 存量用户足够多，且 GQL 工具链还远未成熟。中期（2028+）可能需要评估是否添加 GQL 语法前端，但这不是嵌入式的竞争关键。

#### 1.5.2 图+向量融合 — 从 GraphRAG 到多模态知识库

2024-2025 年最显著的趋势是图数据库和向量数据库的边界模糊化：

- Neo4j 在 5.x 版本内置了向量索引，支持 Cypher 中直接做 ANN 搜索
- FalkorDB 把 GraphBLAS 矩阵运算用于混合图+向量查询
- Azure Cosmos DB 在同一引擎里支持 Gremlin + 向量搜索（DiskANN）
- SurrealDB 内置向量类型和索引

**趋势的本质**：图结构（关系）+ 向量嵌入（语义）的组合查询正在成为 RAG 系统的标准模式。单独一个有图能力的产品如果完全不考虑向量，可能在 2027 年后被认为"功能不完整"。

**对 RocksGraph 的影响**：短期不需要内置向量索引（保持嵌入式+图遍历的核心定位），但架构上应该为向量搜索留接口：属性值可以存储 embedding 向量（已有 `Primitive::Bytes` 类型），查询层可以外挂一个 ANN 索引而不是试图在 LSM 里做近邻搜索（那是错误的设计方向）。

### 1.6 开源社区健康度对比

量化评估竞品社区的活跃度，衡量它们是否在"真正增长"还是"名义上活着"：

| 产品 | 核心仓库 commits（近三个月） | 贡献者数（近三个月） | issue 响应 | 文档质量 | 是否有商业公司 |
|------|---------------------------|-------------------|-----------|---------|-------------|
| Neo4j | 极高（商业驱动） | 多（含员工） | 快 | ⭐⭐⭐⭐⭐ | ✅ Neo4j Inc. |
| SurrealDB | 高 | 多 | 快 | ⭐⭐⭐⭐ | ✅ SurrealDB Ltd. |
| FalkorDB | 中 | ~5-10 | 中 | ⭐⭐⭐ | ✅ FalkorDB Ltd. |
| JanusGraph | 中低 | ~5-10 | 慢 | ⭐⭐ | ❌ 纯社区 |
| Kùzu（归档） | 0（已停止） | 0 | — | ⭐⭐⭐⭐ | ❌（被 Apple 收购后解散） |
| LadybugDB | 中（重建中） | ~3-5 | — | ⭐⭐（继承 Kùzu） | ❌ 纯社区 |
| Vela Kuzu fork | 中 | ~2-5 | — | ⭐⭐（继承 Kùzu） | ✅ Vela Partners（VC 支持） |
| RocksGraph | 个人节奏 | 1 | — | ⭐⭐⭐ | ❌ 个人项目 |

**对这个表的解读**：
- 嵌入式这一层现在处于"所有人都在起跑线附近"的状态——没有谁已经占据了不可撼动的位置。
- 但 Vela 的 Kùzu fork 有 VC 资金（意味着可以雇人全职开发），这比纯社区 fork 的生存概率高一到两个数量级，是需要持续关注的信号。
- SurrealDB 的社区增速是这一层最快的——虽然它不是纯图数据库，但它的 Rust 实现 + 嵌入式模式 + 快速增长让它在"用 Rust 写嵌入式数据库"这个叙事上占据了先发位置。

### 1.7 结构性结论（更新）

1. 嵌入式这一层此前唯一称得上"认真做产品"的选手（Kùzu）**在 2025 年 10 月失去了商业主体**，
   接盘的社区 fork 还处于早期、方向未完全收敛的阶段。这是一个真实存在、时间窗口明确的市场
   空当，不是想象出来的蓝海。**但这个窗口不是永久性的——如果 Vela 的 fork 在 2026 Q4 获得
   足够 traction，窗口可能在 12-18 个月内关闭。**
2. 幸存/活跃的邻近选手在架构上都不是"原生进程内库"：FalkorDBLite 是子进程 + IPC，本质上仍是
   服务化架构的轻量包装；NetworkX/SQLite 变通方案不算数据库。**真正意义上"编译进宿主进程、
   无 IPC、无子进程"的嵌入式图数据库，目前市场上几乎是空的。**
3. 查询语言阵营上，嵌入式这一层清一色是 Cypher/Cypher-like（Kùzu、FalkorDB），**没有一个走
   Gremlin/TinkerPop 语义**——而 Gremlin 阵营在托管云服务层（Neptune Database、Cosmos DB
   Gremlin API）和开源自建层（JanusGraph、HugeGraph）都有可观的存量用户，这些用户如果想要
   一个"不用起集群的轻量版"，目前没有语义对得上的嵌入式选项。
4. **写优化能力在嵌入式层是独占优势**。列式和稀疏矩阵引擎都面向"一次导入、反复分析"的工作负载，
   RocksGraph 的 LSM 存储模型天然适合"图会持续变化"的场景。这不是营销层面的差异化，而是存储
   引擎选择带来的结构性差异。
5. AI Agent 本地记忆/RAG 场景的需求是真实的（Vela 的 fork 专门为此转型证明了这一点），但
   **这个细分已经有人在抢**，不是等待被发现的空白，进入这个细分需要明确的差异化，不能只讲
   "本地知识图谱"这个大故事。
6. **云厂商的"开发者本地测试"是一个高度收敛、需求明确的细分**。目前 Neptune/CosmosDB Gremlin
   用户测试遍历的选项极其有限（起云实例太贵太慢、TinkerGraph 不是真持久化），一个兼容 Gremlin
   语义的本地嵌入式替代品能直接满足这个需求。

## 二、RocksGraph 生态位定位

### 2.1 核心定位

**"Gremlin 语义的进程内图引擎"**——RocksDB 支持的、编译进宿主进程运行的持久化图数据库，
承接 Gremlin/TinkerPop 心智模型的用户群，填补 Kùzu 出局后留下的、清一色被 Cypher 系产品
占据的嵌入式图引擎空当。

这个定位不是凭空提出的，而是现有架构选择自然导出的结果：单进程 + RocksDB 持久化 + 无网络
服务（团队已确认不做纯 Rust 后端、优先 PyO3、不运维网络服务）+ `src/gremlin` 已经实现的
Gremlin 风格遍历语义，三者拼起来正好对上这个空当，不需要新增能力去"凑"定位。

**一句话定位（elevator pitch）：**
> "What SQLite did for relational databases, RocksGraph does for graph databases — open it, traverse it, embed it. No server. No cluster. No JVM."

**对标物（reference products）:**
- **SQLite** — 嵌入式数据库的黄金标准，证明了一个好的嵌入方案可以比所有 server 方案加起来用得更多
- **DuckDB** — 嵌入式分析数据库，证明了一个定位精准的嵌入式项目可以在 2-3 年内从零成长到行业影响力
- **Kùzu** — 嵌入式图引擎的先驱，它的轨迹（VC 投资 → 被收购 → 社区 fork 接手）是 RocksGraph 最好的参照系

### 2.2 差异化维度

| 维度 | RocksGraph | 最接近的竞品 | 差异 |
|------|-----------|------------|------|
| 宿主语言集成方式 | 原生 Rust crate，Rust 应用零 FFI 直接嵌入；Python 走 PyO3 | Kùzu 核心是 C++，Rust/Python 都要走 C API 绑定 | 对 Rust 生态（CLI 工具、Tauri 桌面应用、Rust 后端服务）是唯一原生选项，这是当前空白最明确的一块 |
| 存储引擎模型 | RocksDB / LSM，为持续增量写入和批量导入两种模式都做了优化（外部排序 + SST 直接 ingest 管线） | Kùzu 是列式存储，面向静态快照式分析负载；FalkorDB 是稀疏矩阵代数，面向稠密聚合查询 | 更适合"图会持续变化、同时偶尔需要整体刷新"的场景，而不是一次性导入后只读分析 |
| 查询语义 | Gremlin/TinkerPop 风格遍历 | Cypher（Kùzu、FalkorDB） | 承接 Neptune/Cosmos Gremlin API/JanusGraph/HugeGraph 的存量用户心智，Cypher 阵营已经很拥挤 |
| 运维模型 | 纯库，无需部署/运维网络服务 | FalkorDBLite 仍需拉起 Redis 子进程 | 更彻底的"零基础设施"，适合完全不想碰进程管理的场景 |
| 增量写入性能 | LSM 树优化写入路径 + SST bulk ingest | Kùzu 列式存储写入需要重组列文件；FalkorDB 矩阵运算更适合批量聚合 | 高写入吞吐场景（实时行为图、时间序列关系）的结构性优势 |
| 编译产物大小 | `cargo build --release` 生成 ~10-15MB 的静态链接产物（不含 Rust std） | Kùzu C++ 编译产物更大且依赖 C++ runtime；FalkorDBLite 内含完整 Redis | 适合分发（CLI 工具、Tauri 应用） |

### 2.3 成本定位——嵌入式的经济学

嵌入式图引擎相比云图数据库的成本结构完全不同。以下是实际部署场景的粗略对比（2026 年价格）：

| 场景 | 云图数据库方案 | 月成本 | 嵌入式方案 | 月成本 |
|------|-------------|--------|----------|--------|
| 小规模生产（<500GB 数据，单机，<10 QPS） | Cosmos DB（1000 RU/s + 500GB 存储） | ~$350/月 | RocksGraph 自部署在 $20/月 VPS | **~$20/月** |
| 中型生产（1-5TB，20-100 QPS） | Neptune db.r6g.xlarge | ~$380/月 | RocksGraph + $80/月 NVMe 实例 | **~$80/月** |
| 开发/测试环境 | Neptune db.r6g.large（最小） | ~$360/月 | RocksGraph 在开发机上 `Graph::open()` | **$0** |
| CI 集成测试 | Neptune 实例（测试期间启动） | ~$0.50/h | RocksGraph `tempdir()` 零配置 | **$0** |
| 10 个开发者的团队 | 每人一个 Neptune 测试实例 | ~$3,600/月 | 每人 `cargo add rocksgraph` | **$0** |

**结论**：嵌入式图引擎的成本优势不是 20-30%，而是 **一个数量级以上**（在开发和小规模场景中甚至趋近于零）。这不是"便宜一点的 Neptune"，而是"根本不需要为图遍历付费"——和 SQLite 在 Web 服务里替代 PostgreSQL 的逻辑一致。

### 2.4 竞争护城河分析

评估 RocksGraph 如果进入嵌入式市场，哪些优势是竞品难以复制的：

| 护城河 | 强度 | 理由 |
|--------|-----|------|
| **Rust 原生实现** | 中高 | 其他产品都是 C++/C/Java 内核。用 Rust 重写一个 LSM 图引擎的工作量是 C++ 竞品难以"顺带"做的——这会消耗他们大量的工程时间。 |
| **Gremlin 语义覆盖** | 中 | Gremlin 遍历引擎（volcano pipeline、optimizer 规则、端到端物理计划执行）大约 15,000 行 Rust，不是一个周末能加的特性。但 Cypher 系产品如果决定支持 Gremlin，可以通过协议翻译层实现（类似 Neptune 内部做的），会削弱这个壁垒。 |
| **LSM 写入优化** | 中 | SST ingest 管线、外部排序器、degree counter 这些工具是面向持续写入精心设计的，不是简单地"把 RocksDB 当 KV 用"。但 RocksDB 本身谁都可以接入，差异在于 RocksGraph 在 RocksDB 上做的图专用编码层和索引结构设计。 |
| **先发优势** | 低 | 目前是概念上的先发（"还没有 Rust + Gremlin + 嵌入式的图数据库"），但实际发布后才能算真正的先发优势。在发布前，任何一个 Rust 团队都可以启动类似项目。 |
| **社区和生态** | 低（当前） | 个人项目，没有社群。但 Rust 生态的特点是：如果唯一的选择就是你，且设计合理、文档好，开发者会主动靠过来。 |

### 2.5 建议聚焦的细分市场（按优先级）

1. **Rust 应用生态里需要嵌入式图能力的开发者**——CLI 工具、桌面应用（Tauri）、已经用 Rust
   写后端服务的团队。这是差异化最清晰、竞争最小的细分，因为它直接对应"原生 Rust crate"这个
   目前唯一的空白点，不需要和任何人抢地盘。
   - 可量化的 TAM（Total Addressable Market）：Rust 生态月活开发者约 300 万（2025 Rust Survey），其中有图数据库需求的保守估计 2-3%（基于数据库类别 crate 的下载量比例）→ **约 6-9 万潜在用户**。

2. **持续增量更新的中小规模图，且团队已经熟悉/倾向 Gremlin 语义**——例如从 JanusGraph/
   Neptune 縮容下来的场景，或者不想为一个几十 GB 规模的图去起云服务的团队。差异化在于语言
   语义匹配 + LSM 增量写入模型，而不是单纯"我也是嵌入式"。
   - 可量化的 TAM：JanusGraph GitHub stars ~5,000（代表至少同数量级的部署量），其中单机小规模部署的比例保守估计 30-40% → **约 1,500-2,000 团队**可能愿意换成嵌入式方案。

3. **AI Agent 本地知识图谱/RAG 记忆**——需求真实，但已经有 Vela 的 Kùzu fork 在专门做这件
   事，且做出了"并发多写"这样的具体承诺。进入这个细分必须给出比"本地、嵌入式"更具体的差异
   化（例如结合上面两点：Gremlin 风格的显式遍历更适合表达 Agent 需要的关系推理路径，而不是
   Cypher 的模式匹配），否则只是重复别人已经讲过的故事，没有说服力。

### 2.6 需要验证的假设与风险

- **社区 fork 的走向未知**：LadybugDB、Vela 的 fork 现在都还早期，如果其中一个在半年到一年
  内获得资本或社区规模，"Kùzu 留下的空当"这个前提就会改变，需要定期重新评估，不能只调研
  一次就当作长期事实。
- **FalkorDBLite 的子进程模式是否比"真嵌入式"更有分发优势**是不确定的——它绑定了 Redis 已
  有的用户基础和生态位，这种"背靠大树"的分发路径未必比一个独立新库差，需要观察而非假设
  RocksGraph 的"更彻底嵌入式"天然更受欢迎。
- **GQL 标准化可能长期侵蚀 Gremlin 生态的存量**：如果 2027-2028 年 Neptune 和 Cosmos DB
  主推 GQL 而非 Gremlin，新用户学习 Gremlin 的动力会下降。但这个风险有至少 2-3 年的缓冲期，
  且 Gremlin 的遍历式思维和 GQL/Cypher 的声明式模式匹配在认知模型上不同，存量用户不会
  一夜之间全部转走。
- **"真嵌入"的价值主张需要场景验证**：目前关于"真嵌入 vs 子进程"的讨论更多的基于架构直觉，
  而不是用户反馈数据。子进程模式在 Python 生态里已经被 `duckdb` 和 `falkordblite` 验证了
  分发体验不差。需要在实际用户场景中验证"编译进进进程运行"是否真的解决了子进程解决不了的问题。
- **这份定位分析目前停留在市场研究层面，没有对应的产品化投入**（例如还没有面向这些细分市场
  的文档、示例、benchmark 对比）。如果要把定位落地，下一步是把 2.5 节里排第一的细分场景
  转化为具体的 PyO3/Rust 绑定设计文档和面向该场景的示例代码，而不是停在定位陈述上。

## 三、从定位到行动的建议

### 3.1 工程路线图

| 做什么 | 优先级 | 预期效果 |
|--------|-------|---------|
| 将许可证从 GPL-2.0+ 改为 MIT（或 MIT OR Apache-2.0） | **P0** | 许可证与嵌入式定位不冲突的前提条件；当前 GPL 不兼容嵌入使用模式（见第四章分析） |
| 发布 crates.io + `cargo add rocksgraph` 一键可用 | P0 | 验证 Rust 生态的原生嵌入需求——这是最大差异化点 |
| 完成 PyO3 绑定（`pip install rocksgraph`） | P0 | Python 是图数据科学的第一语言，也是 Neptune/CosmosDB Gremlin 用户最集中的生态 |
| 在核心 crate 预留 `ProFeatures` trait 插槽 | P0 | 为未来的商业化模块留好架构接口，当前默认实现为重导出已有明文编解码（零性能开销、零行为变化） |
| 写 "RocksGraph vs Kùzu fork vs FalkorDBLite" 对比 benchmark | P1 | 给开发者在嵌入式选项之间的决策依据 |
| 实现 Gremlin WebSocket 协议子集（够跑 Neptune 用户的基本遍历） | P1 | 实现"本地开发 → Neptune 生产"的无缝切换场景 |
| 实现首个 Pro 模块：加密存储（AES-GCM prop_blob 加密） | P1 | 验证开源/付费分离的代码架构是否可行，同时产出第一个可卖的功能 |
| 写一个完整的 "Build a knowledge graph CLI with RocksGraph + Tauri" 教程 | P2 | 展示嵌入式图引擎在桌面应用的独特价值（这是纯服务端产品永远做不到的） |
| 写 Neptune → RocksGraph 迁移 / 本地测试指南 | P2 | 直接转化现存的 Gremlin 生态用户 |
| 实现 Pro 模块：审计日志 | P2 | 第二个付费功能，验证变现模型的可复制性 |
| 建立付费分发渠道（私有 crate 注册表 或 加密下载 + license key） | P2 | 将 Pro 模块交付给付费用户的基础设施 |

### 3.2 定位落地的关键里程碑

```
T+0 月： 许可证改为 MIT，crates.io 首次发布
T+3 月： PyO3 绑定可用，Python 用户可以 pip install
T+6 月： ProFeatures trait 就位 + 加密模块完成，第一个付费用户
T+12月： Gremlin WS 协议 + 审计日志，Pro 用户 >10
T+18月： 年付费收入达到可衡量水平，决定是否加大投入
```

---

## 四、许可证与商业化设计框架

*本章讨论 RocksGraph 在当前定位下，"如何开源"和"如何获得收入"之间的工程和法律边界设计。*

### 4.1 当前许可证分析：GPL-2.0-or-later 与嵌入式定位互斥

当前 `Cargo.toml` 中的 `license = "GPL-2.0-or-later"` 与第二节的嵌入式定位存在根本冲突。

GPL 的"链接即感染"规则：任何将 GPL 库链接进自身二进制文件的程序，其**整体**都必须以 GPL 发布。对于嵌入型数据库（被编译进宿主的二进制），这意味着：

- `cargo add rocksgraph` 后，用户整个 crate 被迫使用 GPL
- 闭源商业应用无法使用——这与"SQLite of Graph Databases"的定位矛盾
- Rust 生态中的企业项目（尤其是 crates.io 消费方）在进行许可证合规审查时会自动拒绝 GPL 依赖
- CI / 开发工具场景的团队同样不会为测试依赖承担 GPL 风险

**嵌入式数据库的行业标准许可证对比：**

| 产品 | 许可证 | 与嵌入式兼容？ | 盈利模式 |
|------|--------|:---:|---------|
| SQLite | Public Domain | ✅ 零摩擦 | 商业支持 + 加密扩展付费（SEE） |
| DuckDB | MIT | ✅ | VC 投资 + DuckDB Labs 商业合同 |
| libSQL（SQLite fork） | MIT | ✅ | VC 投资（Turso） |
| LevelDB | BSD-3 | ✅ | Google 开源（无盈利需求） |
| LMDB | OpenLDAP（类 BSD） | ✅ | Symas 商业支持 |
| Kùzu（归档前） | MIT | ✅ | VC 投资 |
| SurrealDB | BSL 1.1 → Apache 2.0 | ⚠️ BSL 的"非生产使用"条款对嵌入场景模糊 | VC 投资 + 云托管 |
| RocksGraph（当前） | **GPL-2.0+** | **❌ 互斥** | — |

**结论**：在嵌入式定位下，GPL 不可行。嵌入库的市场惯例是 permissive license（MIT / Apache-2.0 / BSD）。GPL 只适用于独立运行的 server / CLI 工具，不适用于被 link 进入宿主进程的库。

### 4.2 推荐许可证：MIT

推荐将核心库许可证从 GPL-2.0-or-later 改为 **MIT**（或 MIT OR Apache-2.0 双许可）。

理由：
- MIT 是最简洁的 permissive license，允许任意使用、修改、再许可、商用——完全契合嵌入式库的需求
- 所有现有依赖（`rocksdb`、`smol_str`、`base64` 等）的许可证与 MIT 完全兼容
- Rust 生态的主流约定是 MIT/Apache-2.0 双许可，选择 MIT 单许可或双许可都不会产生生态摩擦
- MIT 不阻止你**另外**为 Pro 模块使用商业许可——两种许可可以并存于同一项目的不同 crate 中

### 4.3 开源/付费分离的代码架构

采用**双 crate 架构**：核心 crate 开源（MIT），Pro crate 闭源（商业许可）。两个 crate 之间的关系通过一个在核心 crate 中定义、在 Pro crate 中实现的 trait 来连接。

```
┌─────────────────────────────────────────────────┐
│          rocksgraph（MIT，公开仓库）                │
│  crates.io 发布                                   │
│  接受社区 PR                                      │
│  包含：所有核心图引擎功能                            │
│                                                  │
│  pub trait ProFeatures: Send + Sync {            │
│      fn encode_props(&self, props: &HashMap)     │
│          -> Vec<u8>;                             │
│      fn decode_prop_by_key(&self, blob: &[u8],   │
│          key: u16) -> Option<Primitive>;          │
│      fn audit_write(&self, op: &str,             │
│          element_id: &str);                      │
│  }                                               │
│                                                  │
│  pub struct NoopProFeatures;                     │
│  // ← 免费版永远使用这个：所有方法穿透到           │
│  //    已有明文编解码，零行为变化                   │
│                                                  │
│  pub struct GraphOptions {                       │
│      // ...                                      │
│      pub pro_features: Option<                   │
│          Box<dyn ProFeatures>>                   │
│      // None = 使用 NoopProFeatures              │
│  }                                               │
└─────────────────────────────────────────────────┘
                         │
                         │ 依赖核心 crate
                         ▼
┌─────────────────────────────────────────────────┐
│      rocksgraph-pro（商业许可，私有仓库）            │
│  **不接受外部 PR**                                │
│  **只由作者一人维护**                               │
│  通过私有渠道分发（详见 4.5）                       │
│                                                  │
│  pub struct EncryptedProFeatures {               │
│      cipher: Aes256Gcm,                          │
│      audit: AuditLogger,                         │
│      license: License,                           │
│  }                                               │
│                                                  │
│  impl rocksgraph::ProFeatures                    │
│      for EncryptedProFeatures { ... }            │
│                                                  │
│  pub fn create_pro_features(                     │
│      key: &str                                   │
│  ) -> Result<Box<dyn ProFeatures>, LicenseError> │
│                                                  │
│  内置模块：                                        │
│  ├── 加密 prop_blob（AES-256-GCM）                │
│  ├── 审计日志（write-ahead log CF）               │
│  └── License key 的验证和解密逻辑                  │
└─────────────────────────────────────────────────┘
```

### 4.4 分离边界的设计原则

**核心 crate 只包含一个 trait 定义 + 一个空实现。** 不包含任何加密算法、任何 license key 验证逻辑、任何付费功能的实际代码。这样：

- 核心 crate 可以安心接受社区 PR——贡献者只接触到 MIT 许可的代码
- Pro crate 的代码属于作者独有版权——没有外部贡献者对其代码主张权利
- `ProFeatures` trait 的方法签名设计为**功能注入点**而非**数据管道**——每个方法对应一个明确的可插入功能

**trait 设计的关键约束：**

| 原则 | 理由 |
|------|------|
| trait 方法签名永远返回标准类型（`Vec<u8>`、`Option<Primitive>`、`()`） | 不暴露 Pro crate 的私有类型，保持 ABI 稳定 |
| 默认实现（`NoopProFeatures`）的性能开销为零 | `#[inline]` + 空函数体；编译器会完全优化掉 |
| 不在 trait 里定义 license 验证 | License 验证留在 Pro crate 的构造函数里；核心 crate 不感知 license 的存在 |
| trait 对象使用 `Box<dyn ProFeatures>` 而非泛型 | 避免 `Graph<S, P>` 的双重泛型参数，保持 API 简洁；动态分发在 `encode_props` 路径上的开销相比加密操作本身可忽略 |

### 4.5 Pro 模块的分发方式

Pro crate 不发布到 crates.io，而是通过以下渠道分发给付费用户：

| 渠道 | 适用场景 | 工作量 |
|------|---------|--------|
| **私有 crate 注册表**（如 `cargo vendor` + 加密 tar.gz） | 企业用户通过 `Cargo.toml` 以 path 依赖引入 | 低——脚本自动化打包和加密 |
| **GitHub Releases + license key 解密** | 下载加密的 `.crate` 文件，Pro crate 的 build.rs 用 license key 解密源码 | 中——需要实现一个简单的解密工具 |
| **源代码托管平台私有仓库**（如 GitHub private repo） | 按用户授予访问权限，用户直接从私有 git 依赖拉取 | 低——但用户管理是手动操作 |

**推荐路径**（分阶段）：

```
阶段 1（<10 付费用户）：GitHub Releases + 加密 tarball
  → 作者手动打包、加密、上传到 Release
  → 用户下载后用 license key 解密
  → 手动操作，但用户少时完全够用

阶段 2（10-50 付费用户）：GitHub private repo + 自动 invite
  → Cargo.toml 中写 git = "https://github.com/xxx/rocksgraph-pro"
  → 用户付费后自动授予 repo 访问权限（GitHub App 或 webhook）
  → 需要简单的付费管理后台

阶段 3（50+ 付费用户）：专用分发服务
  → 私有 crate 注册表（如 Cloudsmith / JFrog 免费层）
  → 或自建一个简单的 token-gated proxy
```

### 4.6 定价与收入模型

**三层定价**（参考 Sidekiq 的模型）：

| 层级 | 适用条件 | 价格 | 包含 |
|------|---------|------|------|
| **Free** | 所有人 | $0 | MIT 核心、社区 issue、无 Pro 功能 |
| **Pro** | 个人开发者 或 <10 人团队 & <$2M 年收入 | **$99/年** | 加密存储 + 审计日志 + 预编译二进制 + 优先 issue |
| **Enterprise** | ≥10 人或 ≥$2M 年收入 | **$299/年** | Pro 全部 + 自定义 license key 管理 + 邮件支持 |

**为什么不按实例/按核/按数据量收费：**

嵌入型数据库没有"实例"的概念——用户编译进自己的二进制里，没法像 SaaS 那样计费。按开发者人数或公司规模定价，是因为这是**唯一可审计的维度**（公司的员工数比"这个库在你的二进制里被编译了多少次"更容易验证，而且不需要技术手段）。

**定价锚点参照：**

| 参照产品 | 价格 | 相同逻辑 |
|---------|------|---------|
| Sidekiq Pro | $299/年 | "比你自己整合 Redis + 任务调度省 100 小时" |
| SQLite SEE | $2,000 一次性 | "比自己做 AES 加密 + 合规文档省两周开发" |
| ImageSharp | $999/年（>$1M 收入的公司） | 按客户收入分级 |

RocksGraph Pro 的核心价值主张是：**"比你自己给嵌入的图数据库加加密和审计省 50 小时"**。任何付得起 $99/年的团队，都会选择买而不是造。

**收入预期**（参考 Sidekiq 的公开数据：一个维护者 + $1.2M/年）：

| 阶段 | 付费用户数 | 年收入估算 | 里程碑 |
|------|-----------|-----------|--------|
| 初期（0-12 月） | 0-20 | $0–$6,000 | 验证有人愿意付费 |
| 成长期（12-24 月） | 20-100 | $6,000–$30,000 | 补贴个人开发时间 |
| 可持续期（24-36 月） | 100-500 | $30,000–$150,000 | 可能接近替代全职收入 |

### 4.7 贡献者关系与收入分配

#### 核心库（MIT）的贡献者

- 所有对核心 crate 的 PR 贡献在 MIT 许可下。贡献者无偿贡献，不保留对代码使用方式的任何控制权。
- 作者可以合法地将核心 crate 的代码（包括社区贡献）与 Pro crate 一起分发和销售。
- 社区贡献者不会从 Pro 收入中获得分成——这是 MIT 许可的法律含义，在首次提交 PR 时就已经成立。

#### Pro crate 的贡献

- **不接受外部 PR。** Pro crate 的所有代码只由作者一人编写和维护。
- 这是个人开源商业项目可持续性的核心条件：Pro 代码的版权归属清晰、没有共同作者、没有需要签 CLA 的贡献者。
- 如果有人想为 Pro 功能做贡献，引导他们为核心 trait 接口（`ProFeatures` 的方法签名设计）提建议——这是可以接受社区输入的 MIT 部分。

#### 贡献者认可（非财务）

对于核心库的重要贡献者，在以下方面给予公开认可：
- `AUTHORS.md` 和 README 中署名
- Release notes 中鸣谢
- 商业化公告中感谢

这不是法律义务，而是维持健康开源社区的道义做法。

#### 如果将来成立公司

- 核心库（MIT）的社区贡献无法律障碍——MIT 许可允许商业再许可。
- 作者独自拥有的 Pro crate 版权可以无障碍转让给公司实体。
- 不需要追溯性地向任何贡献者请求许可或重新分配权益。

### 4.8 竞争性 fork 的风险与防护

MIT 许可允许任何人 fork 核心库、添加自己的 Pro 功能、以更低价格或免费分发。这在法律上完全合法。

**防护措施（按有效性排序）：**

| 防护 | 机制 | 有效性 |
|------|------|--------|
| **品牌 + 商标** | 使用 "RocksGraph" 名称和 logo 作为品牌标识。fork 不能用相同的名称——这构成商标侵权（即使没有注册商标，common law trademark 也有一定保护）。 | 中高 |
| **持续维护速度** | 公开的 commit 记录、频繁的 release、响应 issue 的速度——fork 很难在"跟上上游更新"上与你竞争。 | 高 |
| **Pro 功能的不可替代性** | Pro crate 的加密和审计代码不公开，fork 无法简单复制。他们需要自己从零实现——而这正是你卖的产品。 | 高 |
| **合规需求** | 有正式合规部门的公司（>500 人）需要合法的供应商合同。非正式 fork 无法提供这个。 | 高（对目标客户） |
| **网络效应** | 文档、教程、社区问答（Stack Overflow、Discord）围绕官方版本积累——fork 没有这些。 | 中 |

**核心逻辑：不靠技术锁死用户，靠"维护 + 信任 + 合规"让付费成为最便宜的选择。**

---

## 参考来源

- [Graph Database Market Size, Growth & Competitive Landscape — Mordor Intelligence](https://www.mordorintelligence.com/industry-reports/graph-database-market)
- [Kuzu's Legacy and the New Wave of Embedded Graph Databases — gdotv](https://gdotv.com/blog/kuzu-legacy-embedded-graph-database-landscape/)
- [Vela-Engineering/kuzu — embedded graph database for AI agent memory](https://github.com/Vela-Engineering/kuzu)
- [Kùzu 2026 Company Profile — PitchBook](https://pitchbook.com/profiles/company/537968-17)
- [RedisGraph End-of-Life Announcement — Redis](https://redis.io/blog/redisgraph-eol/)
- [FalkorDBLite: Embedded Python Graph Database — FalkorDB Blog](https://www.falkordb.com/blog/falkordblite-embedded-python-graph-database/)
- [Spanner Graph is now GA — Google Cloud Blog](https://cloud.google.com/blog/products/databases/spanner-graph-is-now-ga)
- [The unified graph solution with Spanner Graph and BigQuery Graph — GCP Blog](https://cloud.google.com/blog/products/data-analytics/the-unified-graph-solution-with-spanner-graph-and-bigquery-graph)
- [Neptune Database vs Neptune Analytics — AWS docs](https://docs.aws.amazon.com/neptune-analytics/latest/userguide/what-is-neptune-analytics.html)
- [ISO/IEC 39075:2024 — Information technology — Database languages — GQL](https://www.iso.org/standard/76120.html)
- [Apache AGE — PostgreSQL Graph Extension](https://age.apache.org/)
- [SurrealDB — The ultimate multi-model database](https://surrealdb.com/)
- [Dgraph — native GraphQL database with a graph backend](https://dgraph.io/)
- [Memgraph — in-memory graph database](https://memgraph.com/)
- [2025 Rust Annual Survey — Rust Blog](https://blog.rust-lang.org/2026/02/19/Rust-2025-annual-survey.html)
