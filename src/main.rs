use axum::Router;
use axum::extract::Multipart;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use nbs_fractal::analysis::reuse::{plan_to_tecs, reuse_flow};
use nbs_fractal::analysis::{BoundedTec, TePlane, TransEqClass};
use nbs_fractal::schematic::{CascadeLayout, StackedLinearLayout, TappedLayout, TorrentLayout};
use nbs_fractal::schematic::{Layout, WithFloor};
use nbs_fractal::schematic::{MultiCompactLayout, MultiFishboneLayout, MultiLinearLayout};
use rsnbs::note::{Note, NoteStreamExt, Notes, Tone};
use rsnbs::song::{Layer, Song};
use rsnbs::types::{Position, Tick, TimeAnchor};
use rustmatica::Litematic;
use std::collections::BTreeSet;
use std::num::NonZero;
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;

const SERVER_ADDR: &str = "127.0.0.1:3021";

/// 前端静态资源，编译期整体嵌入二进制，路径镜像 url（见 [`static_handler`]）
static ASSETS: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/assets");

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().without_time().init();
    let listener = TcpListener::bind(SERVER_ADDR).await.unwrap();

    let app = Router::new()
        .route("/api/generate", post(generate))
        .fallback(static_handler);

    let app = app
        .layer(RequestBodyLimitLayer::new(100 * 1024 * 1024))
        .layer(CompressionLayer::new().zstd(true).gzip(true).deflate(true));

    tracing::info!("🚀 Server started at: http://{}", SERVER_ADDR);
    axum::serve(listener, app).await.unwrap();
}

/// 从嵌入资源返回前端文件
async fn static_handler(uri: Uri) -> Response {
    // 资源文件平铺在 `assets/`，url 仍按子目录组织，这里做一次映射
    let name = match uri.path() {
        "/" | "/index.html" => "index.html",
        "/css/app.css" => "app.css",
        "/css/catppuccin.css" => "catppuccin.css",
        "/js/app.js" => "app.js",
        "/js/alpine.min.js" => "alpine.min.js",
        "/img/favicon.svg" => "favicon.svg",
        "/robots.txt" => "robots.txt",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some(file) = ASSETS.get_file(name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(file.path()).first_or_octet_stream();
    ([(header::CONTENT_TYPE, mime.to_string())], file.contents()).into_response()
}

/// multipart 表单解析结果
struct Params {
    file_bytes: Option<Vec<u8>>,
    file_name: String,
    mode: String,
    coarse: u32,
    wrap: usize,
    wrap_set: bool,
    gap: u32,
    floor: String,
    layers: usize,
    rules: String,
    ticks: Option<u32>,
    halves: bool,
    breaks: bool,
    no_breaks: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            file_bytes: None,
            file_name: String::from("song.nbs"),
            mode: String::from("compact"),
            coarse: 0,
            wrap: 0,
            wrap_set: false,
            gap: 0,
            floor: String::new(),
            layers: 2,
            rules: String::new(),
            ticks: None,
            halves: false,
            breaks: false,
            no_breaks: false,
        }
    }
}

impl Params {
    /// wrap 为空时视为未设置（保留默认值），与 CLI 一致
    fn set_wrap(&mut self, text: String) {
        if !text.is_empty() {
            self.wrap = text.parse().unwrap_or(0);
            self.wrap_set = true;
        }
    }

    fn set_layers(&mut self, text: String) {
        if !text.is_empty() {
            self.layers = text.parse().unwrap_or(2);
        }
    }
}

/// 各构建模式共享的参数
struct BuildArgs {
    name: String,
    coarse: u32,
    wrap: usize,
    gap: u32,
    floor: Floor,
    layers: usize,
    rules: String,
    ticks: Option<u32>,
    halves: bool,
    breaks: bool,
    no_breaks: bool,
}

/// Floor platform mode（与 CLI 的 Floor 枚举一致）
#[derive(Clone, Copy, PartialEq)]
enum Floor {
    None,
    Full,
    Gravity,
}

impl Floor {
    /// Full-coverage flag for layouts that always carry a floor
    fn full(self) -> bool {
        matches!(self, Floor::Full)
    }
}

fn parse_floor(s: &str) -> Floor {
    match s {
        "full" | "true" => Floor::Full,
        "gravity" => Floor::Gravity,
        _ => Floor::None,
    }
}

/// 解析规则文本：每行一条规则，规则内偏移以 / , 空格分隔（与 CLI 一致）
fn parse_rules(text: &str) -> Result<Vec<Vec<Tick>>, String> {
    let mut rules = Vec::new();
    for line in text.lines() {
        let mut ticks = Vec::new();
        for t in line.split(|c: char| matches!(c, '/' | ',' | ' ')) {
            if t.is_empty() {
                continue;
            }
            ticks.push(t.parse().map_err(|_| format!("invalid tick: {t}"))?);
        }
        if !ticks.is_empty() {
            rules.push(ticks);
        }
    }
    Ok(rules)
}

async fn generate(multipart: Multipart) -> Response {
    let params = parse_params(multipart).await;

    let file_bytes = match params.file_bytes {
        Some(data) if !data.is_empty() => data,
        _ => return bad_request("No file uploaded".to_string()),
    };

    let download_name = match params.mode.as_str() {
        "fishbone" => "generated_fishbone.litematic",
        "linear" => "generated_linear.litematic",
        "cascade" => "generated_cascade.litematic",
        "torrent" => "generated_torrent.litematic",
        "decompose" => "generated_decompose.tec.nbs",
        "match" => "generated_match.tec.nbs",
        "tapped" => "generated_tapped.litematic",
        _ => "generated_compact.litematic",
    };

    let args = BuildArgs {
        name: params.file_name,
        coarse: params.coarse,
        wrap: params.wrap,
        gap: params.gap,
        floor: parse_floor(&params.floor),
        layers: params.layers,
        rules: params.rules,
        ticks: params.ticks,
        halves: params.halves,
        breaks: params.breaks,
        no_breaks: params.no_breaks,
    };
    let mode = params.mode;

    let result = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let song = Song::parse(&mut &file_bytes[..])
            .map_err(|e| format!("Failed to read NBS file: {e}"))?;
        match mode.as_str() {
            "fishbone" => build_fishbone(song, args),
            "linear" => build_linear(song, args),
            "cascade" => build_cascade(song, args),
            "torrent" => build_torrent(song, args),
            "decompose" => build_decompose(song, args),
            "match" => build_match(song, args),
            "tapped" => build_tapped_mode(song, args),
            _ => build_compact(song, args),
        }
    })
    .await;

    match result {
        Ok(Ok(bytes)) => download_response(bytes, download_name),
        Ok(Err(e)) => err_response(e),
        Err(e) => err_response(format!("Task failed: {e}")),
    }
}

async fn parse_params(multipart: Multipart) -> Params {
    let mut p = Params::default();
    let mut multipart = multipart;
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            p.file_name = field.file_name().unwrap_or("song.nbs").to_string();
            p.file_bytes = Some(field.bytes().await.unwrap().to_vec());
        } else if name == "mode" {
            p.mode = field.text().await.unwrap_or_default();
        } else if name == "coarse" {
            p.coarse = parse_num(field.text().await.unwrap_or_default(), 0);
        } else if name == "wrap" {
            p.set_wrap(field.text().await.unwrap_or_default());
        } else if name == "gap" {
            p.gap = parse_num(field.text().await.unwrap_or_default(), 0);
        } else if name == "floor" {
            p.floor = field.text().await.unwrap_or_default();
        } else if name == "layers" {
            p.set_layers(field.text().await.unwrap_or_default());
        } else if name == "rules" {
            if !p.rules.is_empty() {
                p.rules.push('\n');
            }
            p.rules.push_str(&field.text().await.unwrap_or_default());
        } else if name == "ticks" {
            p.ticks = field.text().await.unwrap_or_default().parse().ok();
        } else if name == "halves" {
            p.halves = parse_bool(field.text().await.unwrap_or_default());
        } else if name == "breaks" {
            p.breaks = parse_bool(field.text().await.unwrap_or_default());
        } else if name == "no_breaks" {
            p.no_breaks = parse_bool(field.text().await.unwrap_or_default());
        }
    }
    if !p.wrap_set {
        p.wrap = match p.mode.as_str() {
            "linear" => 0,
            _ => 16,
        };
    }
    p
}

fn parse_num(text: String, default: u32) -> u32 {
    text.parse().unwrap_or(default)
}

/// 解析开关字段：空串、"false"、"0"、"off" 视为关
fn parse_bool(text: String) -> bool {
    !matches!(text.as_str(), "" | "false" | "0" | "off")
}

fn bad_request(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
}

fn err_response(msg: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
}

fn download_response(bytes: Vec<u8>, download_name: &str) -> Response {
    Response::builder()
        .status(200)
        .header("Content-Type", "application/octet-stream")
        .header(
            "Content-Disposition",
            format!("attachment; filename=\"{download_name}\""),
        )
        .body(axum::body::Body::from(bytes))
        .unwrap()
}

/// 按 Floor 模式包裹布局（与 CLI 的 build_schematic 一致）
fn build_schematic<L: Layout>(layout: L, floor: Floor, description: String) -> Litematic {
    const AUTHOR: &str = "nbs-fractal";
    match floor {
        Floor::None => layout.as_litematic(description, AUTHOR),
        _ => WithFloor::new(layout, floor.full()).as_litematic(description, AUTHOR),
    }
}

/// 把 litematic 序列化到内存字节（替代写临时文件）
fn litematic_bytes(litematic: Litematic) -> Result<Vec<u8>, String> {
    litematic
        .to_bytes()
        .map_err(|e| format!("Failed to write litematic: {e}"))
}

/// Compact 模式：按需分组，每组拆成 redstone 轨道的紧凑布局（与 CLI 一致）
fn build_compact(song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    let tempo = song.header.tempo;
    let coarse = NonZero::new(args.coarse);
    let groups = match args.breaks {
        true => song.notes.split_by_layer_gaps(),
        false => vec![song.notes],
    };
    let tracks = groups
        .into_iter()
        .flat_map(|group| compact_lines(group, tempo, args.halves, coarse));
    let layout =
        MultiCompactLayout::new(tracks, NonZero::new(args.wrap), args.gap, args.floor.full());
    let description = format!("Sectional from {}", args.name);
    litematic_bytes(build_schematic(layout, Floor::None, description))
}

/// 把一组音符重采样后拆成带粗糙度标签的 redstone 轨道。
///
/// `halves` 时重采样到游戏刻（20 t/s），共用同一红石刻的两个游戏刻各占一轨；
/// 否则重采样到红石刻（10 t/s），折叠到单轨。
fn compact_lines(
    group: Notes,
    tempo: f32,
    halves: bool,
    coarse: Option<NonZero<Tick>>,
) -> impl Iterator<Item = (Vec<(Tick, Note)>, Option<NonZero<Tick>>)> {
    let rate = if halves { 20 } else { 10 };
    let notes = group.rescale_to_tick_rate(tempo, rate);
    let notes = notes.map(|(pos, note)| (pos.into_tick(), note));
    let lines: Vec<Vec<(Tick, Note)>> = match halves {
        true => split_halves(notes),
        false => vec![notes.collect()],
    };
    lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .map(move |line| (line, coarse))
}

/// 按游戏刻奇偶把音符流拆到两条 redstone 轨道上
fn split_halves<I, T>(notes: I) -> Vec<Vec<(Tick, T)>>
where
    I: IntoIterator<Item = (Tick, T)>,
{
    let mut lines: [Vec<(Tick, T)>; 2] = Default::default();
    for (tick, note) in notes {
        lines[(tick % 2) as usize].push((tick / 2, note));
    }
    lines.into_iter().collect()
}

/// Linear 模式：默认按层间距切分（`no_breaks` 时保持单轨），再按 3 层一组拆分
fn build_linear(song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    let notes: Notes = song.notes.rescale_to_game_tick(song.header.tempo).collect();
    let song_length = notes.tick_count();
    let tracks: Vec<Notes> = match args.no_breaks {
        true => vec![notes],
        false => notes.split_by_layer_gaps(),
    };
    let description = format!("Sectional from {}", args.name);
    let litematic = if let Some(wrap) = NonZero::new(args.wrap) {
        let layout =
            StackedLinearLayout::new(tracks, Some(wrap), args.gap, args.floor.full(), song_length);
        build_schematic(layout, Floor::None, description)
    } else {
        let layout = MultiLinearLayout::new(tracks, args.gap, song_length);
        build_schematic(layout, args.floor, description)
    };
    litematic_bytes(litematic)
}

/// Cascade 模式：每个红石刻各占一行，整首从同一起点扫出（先按层音量与立体声混音，与 CLI 一致）
fn build_cascade(song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    let notes = song.resolved_notes();
    let layout = CascadeLayout::new(notes);
    let description = format!("Cascade from {}", args.name);
    litematic_bytes(build_schematic(layout, args.floor, description))
}

/// Torrent 模式：整个隧道按刻逐片铺出（可选只保留前 N 刻，与 CLI 一致）
fn build_torrent(song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    let limit = args.ticks.unwrap_or(u32::MAX);
    let notes = song
        .resolved_notes()
        .filter(|(pos, _)| pos.into_tick() < limit);
    let layout = TorrentLayout::new(notes);
    let description = format!("Torrent from {}", args.name);
    litematic_bytes(build_schematic(layout, Floor::None, description))
}

/// Fishbone 模式：以原生刻度直接投影鱼骨布局（不 retick，与上游一致）
fn build_fishbone(song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    let notes: Notes = song.notes;
    let song_length = notes.tick_count();
    let tracks: Vec<Notes> = match args.no_breaks {
        true => vec![notes],
        false => notes.split_by_layer_gaps(),
    };
    let layout = MultiFishboneLayout::new(tracks, args.gap, song_length);
    let description = format!("Fishbone from {}", args.name);
    litematic_bytes(build_schematic(layout, args.floor, description))
}

/// Decompose 模式：按复用流拆成 TEC 分组，写出**中间 NBS**（上游 `decompose` 的等价实现）。
///
/// 只吃 `layers`：每个 TEC 占一组连续层，组间留空层。中间 NBS 可再上传给 `tapped` 出图。
fn build_decompose(mut song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    // 在原始文件刻度上匹配：中间产物（分组）保留原始时序
    let all_plane: TePlane<Tone> = song
        .notes
        .iter()
        .map(|(pos, note)| (pos.into_tick(), note.tone))
        .collect();
    // 层数预算：0 = 无预算（与 CLI 一致）
    let max_layers = match args.layers {
        0 => usize::MAX,
        n => n,
    };
    let (plan, _total_reuse, residual) = reuse_flow(&all_plane, 6, max_layers);
    // 中继器粒度限制：`min_gap < 8`（repeater coarse < 4）的层被吸收回残差，
    // 保证产物能被 tapped 安全消费；残差本身作为无偏移层保留。
    let (tecs, skipped) = plan_to_tecs(plan, residual);
    if skipped > 0 {
        tracing::info!(
            "decompose: skipped {skipped} layer(s) below the tapped granularity, absorbed into residual"
        );
    }
    let tecs: Vec<BoundedTec<Tone>> = tecs.into_iter().map(BoundedTec::new).collect();
    write_tec_groups(&mut song, tecs, "Decompose")
}

/// Match 模式：按手动指定的偏移切成 TEC 分组，写出**中间 NBS**（上游 `match` 的等价实现）。
///
/// 只吃 `rules`：每条规则一个 TEC，组间留空层。中间 NBS 可再上传给 `tapped` 出图。
fn build_match(mut song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    // 在原始文件刻度上按规则抽取：中间产物（分组）保留原始时序
    let mut residual: TePlane<Tone> = song
        .notes
        .iter()
        .map(|(pos, note)| (pos.into_tick(), note.tone))
        .collect();

    // 每条规则一个 TEC（BTreeSet 天然排序去重，与 CLI 一致）
    let mut tecs: Vec<BoundedTec<Tone>> = Vec::new();
    for rule in parse_rules(&args.rules)? {
        let scatter: BTreeSet<_> = rule.into_iter().filter_map(NonZero::new).collect();
        tecs.push(BoundedTec::extract_from(&mut residual, scatter));
    }

    // 剩余部分保留为无偏移 TEC
    if !residual.is_empty() {
        tecs.push(BoundedTec::new(TransEqClass::new(
            Default::default(),
            residual,
        )));
    }

    write_tec_groups(&mut song, tecs, "Match")
}

/// 将一个 TEC 的展开各自打包到独立层（与上游 CLI 的 `tec_notes` 一致）
fn tec_notes(tec: BoundedTec<Tone>) -> Notes<Position, Note> {
    let points = tec
        .into_inner()
        .expand()
        .into_points()
        .map(|(tick, tone)| (tick, Note::from(tone)));
    points.pack_layers().collect()
}

/// Tapped 模式：直接把已分组的 NBS（`decompose`/`match` 的中间产物）投影为延迟线
fn build_tapped_mode(song: Song, args: BuildArgs) -> Result<Vec<u8>, String> {
    let description = format!("Tapped from {}", args.name);
    let schematic = tapped_from_grouped(song.notes, song.header.tempo, &args, description);
    litematic_bytes(schematic)
}

/// 把 TEC 分组写成中间 NBS（上游 CLI 的 `write_tec_groups` 等价实现）。
///
/// 每个 TEC 的展开各自占一组连续层，组间由 `concat` 留一个空层；原始层分配被丢弃。
fn write_tec_groups(
    song: &mut Song,
    tecs: Vec<BoundedTec<Tone>>,
    label: &str,
) -> Result<Vec<u8>, String> {
    let notes: Notes<Position, Note> = tecs.into_iter().map(tec_notes).concat().collect();
    let layers = notes.layer_count() as usize;
    song.notes = notes;
    song.layers = vec![Layer::default(); layers];
    song.header.song_name = format!("{label} from {}", song.header.song_name);
    let mut buf = Vec::new();
    song.write(&mut buf)
        .map_err(|e| format!("Failed to write NBS: {e}"))?;
    Ok(buf)
}

/// 从已分组的 Notes 还原 TEC 并投影为 tapped 布局（上游 `tapped` 子命令的核心）
fn tapped_from_grouped(
    notes: Notes<Position, Note>,
    tempo: f32,
    args: &BuildArgs,
    description: String,
) -> Litematic {
    // 先在原生刻度上按空层切回 TEC 分组，再逐组把 `(Tick, Event)` 流换算到红石刻。
    // 若先整体 rescale 再 collect 成 `Notes`，会按位置折叠，丢掉落在同一红石刻上的
    // 多重性（即 `K (+) S` 因子）。
    let restored: Vec<BoundedTec<Tone>> = notes
        .split_by_layer_gaps()
        .into_iter()
        .map(|group| {
            let group: Notes<Position, Tone> = group
                .into_iter()
                .map(|(pos, note)| (pos, note.tone))
                .collect();
            let plane: TePlane<Tone> = group
                .rescale_to_redstone_tick(tempo)
                .map(|(pos, tone)| (pos.into_tick(), tone))
                .collect();
            BoundedTec::restore(&plane)
        })
        .collect();

    // 上游 tapped 的硬约束：scatter 最小间距 < 8（repeater coarse < 4）无法布线。
    // 这类分组逐组退回无偏移 TEC（不压缩复用），避免整单失败（上游 CLI 在此会 panic）。
    // 注意 `min_gap() == None`（无 scatter，即无重复模式）是合法情形，照常布线。
    let wired: Vec<BoundedTec<Tone>> = restored
        .into_iter()
        .map(|tec| match tec.min_gap() {
            Some(gap) if gap < 8 => {
                let plane = tec.into_inner().expand();
                BoundedTec::new(TransEqClass::new(BTreeSet::new(), plane))
            }
            _ => tec,
        })
        .collect();

    let layout = TappedLayout::new(wired, NonZero::new(args.wrap), args.gap, args.floor.full());
    build_schematic(layout, Floor::None, description)
}
