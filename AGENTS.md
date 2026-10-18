# AGENTS.md

`nbs-fractal-web` 的知识库：把 NBS 歌曲转成 Minecraft litematic / TEC-NBS 的 Web 服务（Rust/axum 后端 + Alpine 静态前端，静态资源在编译期内嵌进单一二进制）。本文件只记录能支撑判断的硬知识，不逐条复述可从代码读到的细节。

## 1. 项目结构与技术栈

| 路径 | 说明 | 入库 |
|---|---|---|
| `src/lib.rs` | 库根：只声明模块（`pub mod server` / `pub mod terminal`） | 是 |
| `src/server.rs` | 领域逻辑：handler、multipart 解析、八种模式构建、流式输出、内嵌静态资源 | 是 |
| `src/main.rs` | 二进制入口：终端重开、axum 路由与 layer 装配、监听与浏览器唤起 | 是 |
| `src/terminal.rs` | 独立模块：无控制终端时把自己放进终端窗口重开；只依赖 `std` | 是 |
| `Cargo.toml` | 依赖与 release profile | 是 |
| `assets/` | 全部静态资源，`include_dir!` 编译期内嵌；**有目录结构**（`css/`、`js/`、`img/`），不平摊 | 是 |
| `assets/index.html` | 单页前端 | 是 |
| `assets/css/app.css` | 手写 CSS，outro 设计语言 | 是 |
| `assets/css/catppuccin.css` | vendored 调色板（`@catppuccin/palette` 1.8.0） | 是 |
| `assets/js/app.js` | Alpine 状态与逻辑（`function app()`） | 是 |
| `assets/js/alpine.min.js` | Alpine 运行时，vendored | 是 |
| `assets/img/favicon.svg`、`assets/robots.txt` | 静态 | 是 |
| `LICENSE` | AGPL-3.0-only | 是 |

`.gitignore`：`/target`。

后端 `axum` + `tokio`（`sync` feature）+ `tokio-stream` + `tower-http` + `include_dir` + `mime_guess` + `tracing`；转换核心是上游 crate `nbs-fractal`（git 依赖）与 `rsnbs`（crates.io）。**没有** `rustmatica`、`tempfile`（改用上游流式 writer 直出网络）。前端无构建步骤、无打包器。

## 2. 上游依赖（关键）

上游仓库在 workspace 根**之外**，工具读不到，须走终端 `git -C ../nbs-fractal …` / `git -C ../rsnbs …`（**不要 `cd`**）。

- `../nbs-fractal`：`Cargo.toml` 里是 **git 依赖**（`{ git = "https://github.com/omninbs/nbs-fractal" }`，无 rev 锁；实际锁定 rev 见 `Cargo.lock`）。本地 checkout 在分支 `dev`，CLI 在 `src/bin/cli.rs`。当前对应 `ee10743`。
- `../rsnbs`：**crates.io 依赖**（`rsnbs = "0.1.7"`，已解析到 `0.1.8`），分支 `main`。当前 `58f3c73`（tag `v0.1.8`）。

`nbs-fractal` 的模块分工（大重构后）：`analysis`（把音符面分解成平移等价类）、`layout`（八种布局与通用组合子）、`schematic`（**只导出流式 litematic writer 的 `Metadata`**）。别把 `Metadata` 写成 `layout::Metadata`。

`Layout<Ctx = ()>` 是查询接口，方法名带 `*_with(pos, ctx)` 后缀（父级声明 `Ctx` 并向下传借用）；`()` 上下文的布局经 blanket impl 自动获得 `LayoutExt`，即无 `_with` 的普通查询加输出操作：`as_litematic(metadata, sink)` 是**流式**写（当前唯一出口），`as_rustmatica` 已 deprecated（物化整份投影，仅测试对比用）。组合子（`Arranged` / `Overlaid` / `Reverse` / `EvenlyArranged` / `Clipped` 等）的 `new` 多一个尾参 `ctx: &Ctx`；web 用的都是自带 ctx 的成品布局，构造时不传。`TorrentLayout::new(notes, tracks, omit_empty)` 已吸收原 `fill`（后者转私有）。

## 3. HTTP 接口

- 监听 `127.0.0.1:3021`；静态资源**内嵌**（`include_dir!`），`static_handler` 按 url 镜像路径直查 `ASSETS.get_file(path)`（`/` → `index.html`）。**改 `assets/` 后必须重新 `cargo build -r`**，否则跑的还是旧二进制。
- 桌面双击即用：没有控制终端时先用本机终端模拟器重开自己（`relaunch_in_terminal`），随后用 `open` crate 拉起系统默认浏览器；无头环境（无 `DISPLAY` / `WAYLAND_DISPLAY`）不重开、不打开浏览器，照常后台服务。生命周期由终端掌握（关窗口 / Ctrl+C）。
- `POST /api/generate`（`multipart/form-data`）：

| 字段 | 含义 | 默认 |
|---|---|---|
| `file` | 输入 `.nbs` | 必填 |
| `mode` | `compact` / `linear` / `fishbone` / `cascade` / `torrent` / `decompose` / `match` / `tapped` | `compact` |
| `coarse` | compact 重复器延迟粗糙度 1-4（0 = 不限） | 0 |
| `wrap` | 每行最大列数，0 = 不换行 | linear 0，其余 16 |
| `gap` | 相邻行/轨间距，0 = 交错 | 0 |
| `floor` | `none` / `full` / `gravity` | none |
| `layers` | decompose 层数预算（TEC 数），0 = 不限 | 3 |
| `rules` | match 偏移规则，每行一条，按非数字切分 | 空 |
| `ticks` | torrent 只保留此前红石刻数的音符，空 = 不限 | 空 |
| `tracks` | torrent 轨道位置，每行一个 `x,y`，按「非数字且非 `-`」切分；相对轴，东为负 x、上为正 y | 空（用内置轨道集） |
| `all_tracks` | torrent 是否加载所有轨道（含从不发声的） | 关 |
| `halves` / `breaks` / `no_breaks` | 拆分开关 | 关 |
| `no_balance` | tapped 是否关闭播放行对齐（true = 每行保持基准音高） | 关 |

- **成功响应是流**：`application/octet-stream`、`Transfer-Encoding: chunked`、**无** `Content-Length`，边生成边发。下载名 `generated_<mode>.litematic`（出图模式）或 `generated_<mode>.tec.nbs`（`decompose` / `match`）。
- **任务日志**：`task received`（mode、file、input_bytes 及全部参数）、`task finished`（output_bytes、elapsed_ms）、`task failed`（带 error）；缺文件记 `task rejected`，客户端断流记 `debug`。不记录乐谱内容。
- 失败：参数缺失在开流前返回纯文本 400；**生成中途失败只能中断连接**（流已开始，状态码改不了），错误走 `tracing::error!`。
- 请求体上限 100 MiB（`RequestBodyLimitLayer`）。

## 4. 八种模式与上游 CLI 对齐

上游 CLI 的 8 个子命令与 web 模式**一一对应**，语义完全一致。

| 模式 | 输出 | 专属参数 |
|---|---|---|
| `compact` | litematic | `coarse` |
| `linear` | litematic | |
| `fishbone` | litematic | `gap`（原生刻度，不 retick） |
| `cascade` | litematic | 上游已标 **Deprecated**（推荐 torrent），web 侧仍只标 `experimental` |
| `torrent` | litematic | `ticks` / `tracks` / `all_tracks` |
| `decompose` | **NBS** | `layers` |
| `match` | **NBS** | `rules` |
| `tapped` | litematic | `no_balance` |

`wrap` / `gap` / `floor` 只在出图的模式上出现（`floor.gravity` 仅 `linear` / `fishbone` / `cascade`），前端由 `modes[].fields` 声明显隐。**不要**把 `decompose` / `match` 做成端到端语法糖（那会让前端冒出它俩不接受的布局参数）。

管线分两段：

```text
# 第一段：decompose / match  →  中间 NBS（分组）
NBS ──(按原始文件刻度匹配)──► TEC 分组
     ──(pack_layers + concat：每组独立层，组间留 1 空层)──► NBS

# 第二段：tapped  →  litematic
分组 NBS ──(split_by_layer_gaps：先在原生刻度上切回分组)──►
          ──(逐组 rescale_to_redstone_tick 成 (Tick,Event) 流 + BoundedTec::restore)──► 还原共享 scatter 的 TEC
          ──(TappedLayout + Floor)──► litematic
```

匹配在**原始文件刻度**上进行，红石刻换算推迟到 tapped 阶段。第二段**必须**先切组再逐组 rescale：若先整体 rescale 再 `collect` 成 `Notes`，会按位置折叠，丢掉落在同一红石刻上的多重性（`K (+) S` 因子）。

### 4.1 刻意偏离：`coarse < 4` 兜底

上游 `layout/tapped.rs` 的 `Tap::new` 断言 scatter 最小间距 ≥ 8（即 `repeater_coarse >= 4`），否则 **panic**。web 侧仅把 `min_gap()` 为 `Some(gap)` 且 `gap < 8` 的分组**逐组**退回无偏移 TEC，避免整单失败；`min_gap() == None`（无 scatter）是合法情形，不触发。上游 CLI 在同输入下会 panic，行为不同是预期内的。

上游 `decompose` 的粒度预算是 `redstone_step(&song, 8)`：先把它换算成**原始刻度**下的 `min_step`，再传给 `reuse_flow` / `plan_to_tecs`（上游 `89d44b6`）。web 的 `build_decompose` 已按同一逻辑跟进。tapped 侧的兜底仍保留，因为任意上传的 NBS 不经过这层过滤。

`restore` 的组合爆炸由上游 `9b90457` 修复：不再枚举子集，而是先取所有 head 支撑集的**全局交集**，再尝试删除单个刻度，均不成立则取 `{0}`。复杂度降为多项式，web 侧原有的 `check_restore` 预判已移除，tapped 直接调用 `BoundedTec::restore`。注意该算法的解不再是严格「最宽」，可能比旧版窄。

### 4.2 `panic = "abort"` 不可开

tapped 路径依赖 `spawn_blocking` 隔离上游 panic；开 abort 会杀掉整个进程。

### 4.3 后端函数地图（`src/server.rs`；入口 `main` 在 `src/main.rs`）

| 函数 | 作用 |
|---|---|
| `main` | 入口：终端重开 → 路由与 layer 装配（body limit + 压缩）→ 打开浏览器 → `axum::serve` |
| `static_handler` | 内嵌资源按 url 镜像路径直查 |
| `generate` | multipart 解析 → `BuildArgs` → `spawn_blocking` 流式生成 |
| `parse_params` / `parse_num` / `parse_bool` / `parse_floor` / `parse_rules` / `parse_tracks` | 文本参数解析 |
| `build_compact` / `build_linear` / `build_fishbone` / `build_cascade` / `build_torrent` | 直接构造布局，不经 TEC |
| `build_decompose` | `redstone_step` 换算 `min_step` → `reuse_flow` → `plan_to_tecs`（吸收低于粒度的层回残差），写**中间 NBS** |
| `build_match` | 按 `rules` 抽取得 TEC 分组，写**中间 NBS** |
| `write_tec_groups` | `concat` 打包到独立层、重置 `song.layers`，写 NBS 到 sink |
| `build_tapped_mode` / `tapped_from_grouped` | tapped 核心：`split_by_layer_gaps` → 逐组 rescale → `restore` → `coarse < 4` 兜底 → `TappedLayout`（`balanced = !no_balance`） |
| `build_schematic` | 按 `Floor` 包裹布局，`as_litematic` 流式写进 sink |
| `terminal::relaunch_in_terminal` / `terminal::wait_for_enter` | 无终端时用本机终端模拟器重开自己；出错时在终端里暂停（`src/terminal.rs`） |
| `ChannelWriter` / `stream_response` | 同步 writer → 有界 channel → HTTP 流 |
| `tec_notes` / `compact_lines` / `split_halves` | 展开与拆轨辅助 |

### 4.4 流式输出（网络流）

生成结果不落内存缓冲、不落临时文件，一路推到网络：

```text
spawn_blocking ─► Layout::as_litematic(metadata, sink)
              ─► schematic::write 把 sink 包进 GzEncoder
              ─► sink = ChannelWriter（攒够 CHUNK_SIZE 就 send）
              ─► mpsc channel（容量 4）
              ─► Body::from_stream(ReceiverStream)
```

- 上游 `Layout::as_litematic` 是**同步** `Write`，且刻意不引异步依赖；async 适配（channel + 阻塞 writer）是 web 侧的职责，别要求上游改成 `Read`/`Stream`。
- `CHUNK_SIZE = 64 KiB`：**按字节**攒块，在途内存上界 ≈ `channel 容量 × CHUNK_SIZE`（≈ 256 KiB），而非不可控的压缩块数（压缩块大小无定数、也无语义边界）。
- `ChannelWriter` 实现 `Write`：`write` 攒缓冲、满则 `blocking_send`；`Drop` 兜底发出不足一块的尾巴（少了它尾部字节会丢）。channel 满时生成线程阻塞 → 网络慢则自然反压节流。
- 生成放 `tokio::task::spawn_blocking`（CPU 密集 + 隔离上游 panic）。

## 5. 前端设计语言（outro）

来源 `/home/cmy/Desktop/outro`（preact + Tailwind v4 + `@catppuccin/tailwindcss`）。手写等价 CSS，**不要**引入 Tailwind 或任何构建步骤。

- **配色**：Catppuccin `Mocha`（深，默认）/ `Latte`（亮），走 `prefers-color-scheme`。调色板 vendored 为 `assets/css/catppuccin.css`（`@catppuccin/palette` 1.8.0，提供 `--ctp-*`）；`app.css` 的 `:root` 只做 `--ctp-*` → 语义变量（`--base`/`--text`/`--mauve`/`--red`/`--green`/`--yellow` …）映射，**不要**再手写十六进制。
- **分档**：容器查询挂在 `.page`：`< 30rem` 窄（贴边）、`>= 30rem`、`>= 64rem` 宽。
- **版心**：`--standard: 60rem`；宽档 `--wide: 90rem`。
- **内容双栏 5 : 7**（左 文件/模式，右 参数/生成）。
- **页脚**：两栏 **7 : 5**（brand : nav），`min-height: 14rem`，宽度跟随 `--wide`。
- **链接**：默认无下划线，hover 才有。
- **字号**：区间 `1rem ~ 1.5rem`。
- **图标**：内联 SVG（`stroke="currentColor"`），不用字体符号。
- **动效**：全站唯一一份 `--motion`（只淡不位移）。
- **破折号**：界面文案禁用 em/en dash。
- 主按钮带静态 `disabled` + `:disabled="!file || loading"`，消除首帧闪烁。
- `.notice` 窄档缩进用 `margin-inline`；`.notice` 为通用基类，按语义着色（error 红 / success 绿 / info 黄），由 `modes[].experimental` 驱动实验性提示。

### 5.1 前端机制（`assets/js/app.js`）

- `modes[]` 用 `fields` 声明每模式用到哪些参数字段、`gravity` 声明有无 gravity 档；模板一律 `x-show="show('字段')"`，**不要**在模板里写模式判断。提交字段由 `submit[]` 驱动 `FormData`。
- 模式同步到 `#<mode>` hash（可分享链接），`hashchange` 支持前进/后退。
- 首屏：内联关键样式 + `.boot` 加载页（加载页样式**内联在 head**，不走 CSS，避免样式到达时闪烁），`x-cloak` 藏主界面，`init()` 里 `$nextTick` 后置 `ready` 触发淡入。
- generate 用 `fetch('./api/generate')` + `res.blob()`：前端仍把整个响应读进内存再生成下载链接（要真流式落盘需别的手段）。
- 各模式 `desc` 取上游 `cli.rs` doc 注释首句**原文**，不要改写。

## 6. 协作约定

- 交流用中文，简洁，善用表格；先说明计划再执行。
- **回复必须短且结构化**：用户常常只看到最后一段，长篇大论等于没说。先给结论或结果，再给必要细节；能表格就别用段落，能一行就别用一节。
- **尽可能减少工具调用次数**：独立调用一次性集中（并行），先规划再动手；调用次数过多是主要性能瓶颈。
- 代码块带语言标记。
- commit message：单行 ≤ 50 字符、英文、祈使句、`<type>: <description>`、无结尾标点。
- 不要改上游 `nbs-fractal` / `rsnbs`。
- **划清界限（教训）**：只做被明确要求的事，不擅自扩大动作范围。
  - `commit`、`push`、及一切改写本地/远程历史的操作（`reset`、`revert`、`--force` 等）都**必须**先拿到用户的明确指令。
  - 「收拾一下」「整理一下」「清理一下」这类模糊措辞**不**授权 commit/push，只允许清理临时文件、整理工作区之类可逆的本地动作。
  - 推送是**不可逆**的对外发布：不确定就先问，宁可停下。

## 7. 同步上游流程

1. 取上游提交与差异，重点看 CLI 的选项 / doc 与 `layout` 模块的公开 API：
   ```sh
   git -C ../nbs-fractal fetch --all --prune
   git -C ../nbs-fractal --no-pager log --oneline -n 12
   git -C ../nbs-fractal --no-pager diff --stat <last>..origin/main
   ```
2. 适配 `src/server.rs`（**导入路径**、函数签名、管线）与 `assets/js/app.js`（`modes[].desc`）+ `assets/index.html`（help 文本），保证选项与文档一致。纯 doc-comment 变更只影响 `modes[].desc`。
3. `CARGO_NET_GIT_FETCH_WITH_CLI=true cargo update -p nbs-fractal` 后 `cargo build -r` 验证。

### 7.1 端到端验证

上游两仓库都没有可用的 `.nbs` fixture。可用 `../nbs-fractal/out/` 的产物（`d.nbs`、`k.nbs` 等），或临时造一首（`rsnbs::Song` + `save_nbs`，用完即删）。

`tapped` 的输入必须是**已分组** NBS（每个 TEC 占一组连续层，组间空层）：先用本服务或上游 `decompose` / `match` 存出 `.nbs`，再上传选 `tapped`。普通 NBS 会整首并成一组。

起服务跑各模式（先 `pkill` 清端口），收尾 `pkill` 并删除临时文件。验证 litematic 用 `file` 看是否为 gzip、`gzip -t` 严格校验完整性（CRC/ISIZE 能抓出丢尾）。无头 Chromium 在 `/usr/bin/chromium`；**截图要直接输出到项目目录内**（`read_file` 读不到根外路径），看完删除。

## 8. 构建与部署

- **Linux**：`cargo build -r` → `target/release/nbs-fractal-web`。
- **Windows（交叉编译）**：`CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build -r --target x86_64-pc-windows-gnu` → `target/x86_64-pc-windows-gnu/release/nbs-fractal-web.exe`（约 13 MB，gcc 运行库已静态链接，仅依赖系统 DLL，可单文件分发）。需 `x86_64-pc-windows-gnu` target 与 mingw；本机无 msvc 工具链。
- **沙箱**：git/cargo 网络操作需 `CARGO_NET_GIT_FETCH_WITH_CLI=true`。
- **反代**（nginx）：前端全用相对路径，**Web 路径必须以 `/nbs/` 结尾**，否则基准退化到站点根：
  ```nginx
  location = /nbs { return 301 /nbs/; }
  location /nbs/ {
      proxy_pass http://127.0.0.1:3021/;   # 尾斜杠去掉前缀
      client_max_body_size 100m;
      proxy_read_timeout 600s;             # 默认 60s 会 502/504
      proxy_buffering off;                 # 保留流式，别被 nginx 攒起来
  }
  ```

## 9. 已知坑

- 端口固定 `3021`，**不做顺延**（反代依赖固定地址）；占用时打印错误并以非零码退出（终端里会等回车）。
- 上游 `decompose` 标注 **Experimental**，输出可能再变。
- **分支是 `main`**（不是 `master`），远程 `git@github.com:omninbs/nbs-fractal-web.git`。
- `CompressionLayer` 会二次压缩已是 gzip 的 litematic：其默认 predicate 只豁免 gRPC / 图片 / SSE（且 body ≥ 32 B），`application/octet-stream` 不在内；litematic 的 gzip 是文件格式内部的，`Content-Encoding` 为空，故照压（实测省约 3%，纯耗 CPU）。目前保留未处理。
- 资源**编译期内嵌**，改 `assets/` 必须重新 `cargo build -r`。
- Alpine `x-if` 的 `<template>` 只能单根元素；`x-transition` 对 `x-if` 无效，须配合 `x-cloak`。
- `container-type` 会让元素成为 `position:fixed` 的包含块：**勿**去掉 `.page`；容器查询也**不能**挂 `main`（有 `max-width`）。
- 内联首屏样式与 `<meta name="theme-color">` **保留裸十六进制**（须早于 CSS 到达、值须一致）——刻意设计，勿「优化」。
- `git status` 在沙箱易报 index.lock 冲突，加 `--no-optional-locks`。
- `git commit && git push` 连写时，若此前已 commit（工作区干净），`git add` 报 `nothing to commit` 使整条链失败、push 未执行；先确认 HEAD 与工作区状态，再单独 push。
- 终端 `rm -f` 组合命令曾被安全规则拦截；分步执行。
