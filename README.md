# local-query / lselect

用类似 SQL 的语法查询本地 CSV、TSV、JSON 数组和 JSON Lines（JSONL/NDJSON）文件。

## 安装与运行

```bash
cargo install --path .
lselect --help

# 或直接在项目内运行
cargo run -- "* from './data.csv' limit 10"
```

查询作为一个完整的命令行参数传入，外层使用双引号，防止 shell 展开 `*`。
支持省略 `SELECT`；以下两种写法等价：

```bash
lselect "* from '../../xxxx.csv' where name = 2"
lselect "SELECT * FROM '../../xxxx.csv' WHERE name = 2"
```

路径相对于运行命令时的工作目录解析。无空格路径可以不加引号；推荐始终使用单引号。
路径或字符串中包含单引号时，按 SQL 规则写成两个单引号，例如 `'./it''s data.csv'`。

## AI 查询 Skill

项目内提供 [local-file-query Skill](skills/local-file-query/SKILL.md)，包含查询流程、SQL 模板和常见错误排查。
Skill 源文件统一在 `skills/local-file-query/` 中维护，随项目提交和分发。

在项目根目录安装命令，并将 Skill 复制到本机 Codex Skills 目录：

```bash
cargo install --path . --force
mkdir -p "${CODEX_HOME:-$HOME/.codex}/skills/local-file-query/agents"
cp skills/local-file-query/SKILL.md "${CODEX_HOME:-$HOME/.codex}/skills/local-file-query/SKILL.md"
cp skills/local-file-query/agents/openai.yaml "${CODEX_HOME:-$HOME/.codex}/skills/local-file-query/agents/openai.yaml"
```

上述复制会更新本机同名 Skill。项目中的 Skill 更新后，可重新执行复制命令。
也可以把 [SKILL.md](skills/local-file-query/SKILL.md) 提供给其他支持 Skill 的 AI 工具使用。

调用示例：

```text
$local-file-query 帮我统计 /path/logs 目录中各个 level 的日志数量
```

## CSV / TSV

默认首行是表头，按列名查询；CSV 单元格输出时保留为字符串。

```bash
lselect "name, age from './users.csv' where age >= 18 AND name != '张三' limit 20"
lselect "* from './users.tsv' where age >= 18"
```

无表头时必须指定 `--no-header`，自动生成从 1 开始的列名；第一行仍是数据：

```bash
lselect --no-header "column1, column3 from './data.csv' where column2 = 2"
```

- 表头必须非空且不重复；字段名大小写敏感，原样保留空格。
- 支持 CSV 标准引号、字段内逗号和换行；每条记录必须与表头/首条记录列数一致。
- 与数字比较时尝试将 CSV 文本转为数字，非数字文本产生 `NULL`（未知），不匹配筛选。
- 与字符串比较时按原始文本比较：`id = 2` 可以匹配 `002`，`id = '002'` 仅匹配 `002`。
- 空单元格是空字符串，不是 `NULL`。

## JSON 数组

```json
[
  {"name":"张三","age":20,"user":{"city":"上海"},"active":true},
  {"name":"李四","age":25,"active":false}
]
```

```bash
lselect "name, user.city AS city from './users.json' where age >= 18"
lselect "* from './users.json' where active = true"
lselect "name from './users.json' where user.city IS NULL"
```

每个数组元素是一条记录，必须是对象，或编码了对象的 JSON 字符串。根节点必须是数组。
原始 JSON 数字、字符串、布尔值、对象、数组和 `null` 在输出中保留其类型。

## JSONL / NDJSON

每行一个完整 JSON 对象，允许空白行；不支持一个对象跨多行：

```jsonl
{"name":"张三","age":20}
{"name":"李四","age":25}
```

```bash
lselect "name from './users.jsonl' where age >= 18 limit 10"
lselect --format jsonl "* from './events.txt' where active = true"
```

也接受每行被额外 JSON 编码一层的对象字符串，例如：

```json
"{\"name\":\"张三\",\"age\":20}"
```

通过扩展名推断 `csv`、`tsv`、`json`、`jsonl`、`ndjson`，未知扩展名使用
`--format csv|tsv|json|jsonl` 显式指定。`.json` 中存放 JSONL 时也需要指定格式。
`--no-header` 仅适用于 CSV/TSV。

## 查询文件夹

`FROM` 默认读取单个文件；显式使用 `FROM FOLDER` 读取目录，目录路径必须加单引号或双引号：

```bash
# 合并日志目录中的记录，查询不同日志等级
lselect --format jsonl "DISTINCT level FROM FOLDER '/Users/meow.zang/Downloads/logs' LIMIT 10"

# SQL 中使用双引号路径时，需要转义 shell 外层双引号
lselect --format jsonl "level, COUNT(*) AS n FROM FOLDER \"./logs\" GROUP BY level ORDER BY n DESC"

# 递归读取子目录
lselect --recursive "* FROM FOLDER './data' WHERE age >= 18 LIMIT 20"
```

- 默认仅查询目录直属文件；`--recursive` 包含子目录，只能用于 `FROM FOLDER`。
- 跳过隐藏文件、隐藏目录和符号链接。文件按路径字典序读取，未排序查询的行顺序由此确定。
- 未指定 `--format` 时，只读取 `.csv/.tsv/.json/.jsonl/.ndjson`，其他扩展名跳过。
  查询 `.log/.txt` 或无扩展名文件时使用 `--format jsonl` 等显式格式；这时所有非隐藏普通文件
  都按指定格式读取，因此目录中应只放该格式的数据文件。空目录或没有可识别文件时结果为空。
- 支持同一目录内混合 JSON 数组和 JSONL，或混合 CSV 和 TSV。
  CSV/TSV 与 JSON/JSONL 混在一起时会报错，避免同一查询中的文本/数字比较规则发生歧义。
- 每个 CSV/TSV 文件独立解析表头，按字段名对齐，表头顺序可以不同；缺失字段为 `NULL`。
  `--no-header` 对每个文件生效，列名都从 `column1` 开始。
- WHERE、GROUP BY、HAVING、DISTINCT、ORDER BY、LIMIT 作用于所有文件合并后的记录。
  普通查询达到 LIMIT 后停止读取后续文件；分组、聚合、排序和去重查询需要读完所有文件。
- 非法数据默认报错并标明文件路径及记录位置，不会静默跳过。`LIMIT 0` 验证目录和文件格式列表，
  不打开目录内的数据文件。目录不存在或把目录用于普通 FROM 时会报错。

## 查询规则

支持：

- `SELECT *`、指定字段、`AS` 别名、嵌套字段 `user.city`，以及 `SELECT DISTINCT` 去重。
- 数值表达式 `+ - * / %`，包括聚合结果之间的运算。
- `= != <> < <= > >=`，`AND OR NOT`，括号，`IS NULL / IS NOT NULL`。
- 字符串、数字（包括负数、小数）、布尔值、`NULL`。
- `COUNT / SUM / AVG / MIN / MAX` 聚合函数、`GROUP BY`、`HAVING`。
- `ORDER BY` 多字段排序、`ASC / DESC`、`NULLS FIRST / NULLS LAST`。
- 非负整数 `LIMIT`，限制分组、筛选、去重和排序之后的最终输出条数。

缺失字段视为 `NULL`。比较 `NULL` 的结果为未知，只有条件为 `true` 的记录会被输出。
例如查缺失字段应该用 `IS NULL`，不能用 `= NULL`。
JSON 字段类型不隐式转换：数字 `20` 与字符串 `'20'` 分开查询。
不兼容类型以及对象/数组参与比较时产生未知；对象/数组仍可以直接选出。
数字比较使用十进制精度，不先转换成浮点数，因此不会把相邻的大整数混为一谈。

字段名本身含点号、空格或是保留字时，使用 SQL 双引号；在 shell 外层双引号中需要转义：

```bash
lselect "\"user.city\" from './users.json' where \"user.city\" = '上海'"
```

`user.city` 查询嵌套属性，而 `"user.city"` 查询名字恰好为 `user.city` 的顶层属性。
投影中的嵌套字段默认输出键名为 `user.city`，可以用 `AS city` 改名。输出键不得重复。

支持单文件或单个目录查询，不支持 JOIN、子查询、窗口函数、普通标量函数、
数组下标、表别名、OFFSET、DISTINCT ON 及旧原型的 `regex:` 语法。不支持的 SQL 会报错。
Excel 和任意非结构化 TXT 暂不支持。

## 分组、聚合和排序

```bash
# 多字段排序；可以按未输出的输入字段排序
lselect "name, age from './users.json' order by age desc, name asc limit 10"

# 先过滤输入，再按城市分组，筛选符合条件的组，最后排序
lselect "city, COUNT(*) AS n, SUM(amount) AS total, AVG(amount) AS avg_amount from './orders.csv' where amount > 0 group by city having n >= 2 AND total > 100 order by total desc limit 10"

# 全文件聚合；不写 GROUP BY 时，把通过 WHERE 的记录视为一个组
lselect "COUNT(*) AS rows, MIN(age) AS youngest, MAX(age) AS oldest from './users.jsonl'"

# 去重、表达式、嵌套字段和多字段分组
lselect "DISTINCT user.city AS city from './users.json' order by city"
lselect "user.city AS city, active, COUNT(*) AS n from './users.json' group by user.city, active having COUNT(*) > 1 order by n desc"
lselect "price * quantity AS amount from './orders.csv' order by amount desc"
```

执行顺序为 `WHERE → GROUP BY/聚合 → HAVING → SELECT → DISTINCT → ORDER BY → LIMIT`。

- `COUNT(*)` 统计记录数；`COUNT(field)` 忽略缺失和 `null`。CSV 空字符串仍计数。
- `SUM/AVG/MIN/MAX` 忽略 `null`。空集合的 `COUNT` 为 0，其他聚合为 `null`；
  无分组键的全局聚合仍输出一行，而空输入的 `GROUP BY` 不产生分组。
- 支持 `COUNT(DISTINCT field)` 等单参数聚合去重。不支持嵌套聚合、`COUNT(DISTINCT *)`、
  函数内的 `FILTER`、`ORDER BY` 或 `OVER`。
- `SUM/AVG` 和算术运算要求数字；CSV 文本可转成数字，非数字文本（包括空字符串）报错。
  JSON 字符串不隐式转成数字。除数为 0 报错；运算有 `null` 时返回 `null`。
  加减乘及求和使用十进制运算；除法及平均值中的无限小数按 BigDecimal 默认精度舍入。
- `MIN/MAX` 返回选中的原始值，因此 CSV 的结果仍为字符串。CSV 排序及 `MIN/MAX`
  按数字比较可解析为数字的文本，其余文本按字典序比较；数字排在非数字文本前面。
- `GROUP BY` 支持多个字段/表达式、嵌套字段、SELECT 别名及从 1 开始的选择项序号。
  缺失字段与 `null` 归入同一组；JSON 数字 `1`、`1.0`、`1e0` 视为同一个值。
  CSV 分组、`SELECT DISTINCT` 和 `COUNT(DISTINCT field)` 保留文本差异，`002` 与 `2` 不合并。
- 分组查询中，SELECT、HAVING 和 ORDER BY 引用的字段必须在分组键内，或位于聚合函数中。
  分组查询不能直接 `SELECT *`，WHERE 和 GROUP BY 不能使用聚合函数。
- `HAVING` 可以使用聚合表达式或 SELECT 别名。没有 GROUP BY 的 HAVING 按全局单组处理。
- `ORDER BY` 支持 SELECT 别名、选择项序号、输入字段、表达式和聚合表达式。
  例如 `ORDER BY 2 DESC` 按第二个选择项排序；序号不能引用 `*`。
  使用 `SELECT DISTINCT` 时，排序表达式须出现在 SELECT 中（`SELECT DISTINCT *` 除外）。
- GROUP BY、HAVING 和 ORDER BY 中，SELECT 别名优先于同名输入字段。
  SELECT 自身以及聚合函数的参数中，字段名仍指输入字段，不引用其他选择项的别名。
- 默认 `ASC NULLS LAST`，`DESC NULLS FIRST`，可显式覆盖。排序键相同时保留输入/分组首次出现的顺序。
  JSON 混合类型按布尔、数字、字符串、数组、对象的顺序排序；不同类型不会隐式互转。

若省略 SELECT 后，查询以负号开头，请加 `--` 结束选项解析，例如：

```bash
lselect -- "-age AS negative_age from './users.csv'"
```

## 输出、性能与错误

标准输出为 JSON 数组，可重定向保存；标准错误输出错误详情，失败退出码为 1。

```bash
lselect "* from './users.jsonl' where age >= 18" > result.json
```

所有输入格式均按记录读取。普通筛选/投影查询流式输出，内存使用主要取决于单条记录大小；
达到 `LIMIT` 后立即停止，因此不会校验后续记录。
分组、聚合、排序和 DISTINCT 查询会暂存通过 WHERE 的记录，读完整个输入后再计算结果，
这类查询的内存使用随匹配数据量增长，LIMIT 不会提前截断输入；当前没有磁盘溢写。
输出对象的键顺序不保证与选择顺序一致。`LIMIT 0` 只验证查询与文件能否打开。
解析过程中遇到坏记录会失败，JSONL 错误包含行号，JSON 语法错误包含行列位置。
由于输出是流式的，中途失败可能已经输出部分数据，重定向时应检查退出码再使用结果。

## 开发验证

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
