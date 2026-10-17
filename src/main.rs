use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::Multipart;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use nbs_fractal::analysis::reuse::{plan_to_tecs, reuse_flow};
use nbs_fractal::analysis::{BoundedTec, TePlane, TransEqClass};
use nbs_fractal::layout::linear::{MultiLinearLayout, StackedLinearLayout};
use nbs_fractal::layout::{LayoutExt, WithFloor, torrent::TorrentLayout};
#[allow(deprecated)]
use nbs_fractal::layout::{cascade::CascadeLayout, compact::MultiCompactLayout};
use nbs_fractal::layout::{fishbone::MultiFishboneLayout, tapped::TappedLayout};
use nbs_fractal::schematic::Metadata;
use rsnbs::note::{Note, NoteStreamExt, Notes, Tone};
use rsnbs::song::{Layer, Song};
use rsnbs::types::{Position, Tick, TimeAnchor};
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;

const DEFAULT_PORT: u16 = 3021;

/// 前端静态资源，编译期整体嵌入二进制，路径镜像 url（见 [`static_handler`]）
static ASSETS: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/assets");

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().without_time().init();

    // 严格绑定默认端口：反代部署依赖固定地址，不能静默换端口
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, DEFAULT_PORT)).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("Failed to bind 127.0.0.1:{DEFAULT_PORT}: {err}");
            std::process::exit(1);
        }
    };
    let url = format!("http://127.0.0.1:{DEFAULT_PORT}/");

    let app = Router::new()
        .route("/api/generate", post(generate))
        .fallback(static_handler);

    let app = app
        .layer(RequestBodyLimitLayer::new(100 * 1024 * 1024))
        .layer(CompressionLayer::new().zstd(true).gzip(true).deflate(true));

    tracing::info!("🚀 Server started at: {url}");
    tracing::info!("Press Ctrl+C to stop (closing this window also stops it).");
    tokio::spawn(async move {
        // 等 serve 真正开始接受连接
        tokio::time::sleep(Duration::from_millis(300)).await;
        // 打不开也不影响服务：无头环境会在这里失败，没必要大惊小怪
        if let Err(err) = open::that(&url) {
            tracing::debug!("Could not open the browser ({err}), visit {url} manually");
        }
    });
    axum::serve(listener, app).await.unwrap();
}

/// 从嵌入资源返回前端文件（`assets/` 目录结构与 url 镜像）
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(file) = ASSETS.get_file(path) else {
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
    tracks: String,
    all_tracks: bool,
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
            tracks: String::new(),
            all_tracks: false,
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
    tracks: String,
    all_tracks: bool,
    halves: bool,
    breaks: bool,
    no_breaks: bool,
}

/// Floor platform mode（与 CLI 的 Floor 枚举一致）
#[derive(Clone, Copy, PartialEq, Debug)]
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
        for t in line.split(|c: char| !c.is_ascii_digit()) {
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
        _ => {
            tracing::warn!(
                mode = %params.mode,
                file = %params.file_name,
                "task rejected: no file uploaded"
            );
            return bad_request("No file uploaded".to_string());
        }
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
        tracks: params.tracks,
        all_tracks: params.all_tracks,
        halves: params.halves,
        breaks: params.breaks,
        no_breaks: params.no_breaks,
    };
    let mode = params.mode;

    tracing::info!(
        mode = %mode,
        file = %args.name,
        input_bytes = file_bytes.len(),
        coarse = args.coarse,
        wrap = args.wrap,
        gap = args.gap,
        floor = ?args.floor,
        layers = args.layers,
        ticks = ?args.ticks,
        all_tracks = args.all_tracks,
        halves = args.halves,
        breaks = args.breaks,
        no_breaks = args.no_breaks,
        rules = args.rules.lines().count(),
        tracks = args.tracks.lines().count(),
        "task received"
    );

    // 边生成边发送：有界 channel 给生成线程背压，响应头先于字节。
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(4);
    let report = tx.clone();
    let task_written = Arc::new(AtomicU64::new(0));
    let task_mode = mode.clone();
    let task_file = args.name.clone();
    let report_mode = mode;
    let report_file = args.name.clone();

    // 生成是 CPU 密集的同步过程，交给阻塞线程池：既不拖住 runtime，
    // 也隔离上游可能的 panic（tapped 路径）。
    let task = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let started = Instant::now();
        let sink = ChannelWriter::new(tx, task_written.clone());
        let result = (|| {
            let song = Song::parse(&mut &file_bytes[..])
                .map_err(|e| format!("Failed to read NBS file: {e}"))?;
            match task_mode.as_str() {
                "fishbone" => build_fishbone(song, args, sink),
                "linear" => build_linear(song, args, sink),
                "cascade" => build_cascade(song, args, sink),
                "torrent" => build_torrent(song, args, sink),
                "decompose" => build_decompose(song, args, sink),
                "match" => build_match(song, args, sink),
                "tapped" => build_tapped_mode(song, args, sink),
                _ => build_compact(song, args, sink),
            }
        })();
        let output_bytes = task_written.load(Ordering::Relaxed);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(()) => tracing::info!(
                mode = %task_mode,
                file = %task_file,
                output_bytes,
                elapsed_ms,
                "task finished"
            ),
            Err(error) => tracing::error!(
                mode = %task_mode,
                file = %task_file,
                output_bytes,
                elapsed_ms,
                error = %error,
                "task failed"
            ),
        }
        result
    });

    // 流一旦开始，状态码就无法再改；失败只能中断流并记日志。
    tokio::spawn(async move {
        let error = match task.await {
            Ok(Ok(())) => return,
            // 已在任务线程里记过
            Ok(Err(e)) => e,
            Err(e) => {
                let message = format!("Task panicked: {e}");
                tracing::error!(
                    mode = %report_mode,
                    file = %report_file,
                    "{message}"
                );
                message
            }
        };
        let _ = report.send(Err(io::Error::other(error))).await;
    });

    stream_response(ReceiverStream::new(rx), download_name)
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
        } else if name == "tracks" {
            if !p.tracks.is_empty() {
                p.tracks.push('\n');
            }
            p.tracks.push_str(&field.text().await.unwrap_or_default());
        } else if name == "all_tracks" {
            p.all_tracks = parse_bool(field.text().await.unwrap_or_default());
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

/// 流式下载响应：响应头先发，字节边生成边来。
fn stream_response(
    stream: ReceiverStream<Result<Bytes, io::Error>>,
    download_name: &str,
) -> Response {
    Response::builder()
        .status(200)
        .header("Content-Type", "application/octet-stream")
        .header(
            "Content-Disposition",
            format!("attachment; filename=\"{download_name}\""),
        )
        .body(Body::from_stream(stream))
        .unwrap()
}

/// 每个网络分块的字节数：够大以摊薄系统调用与 channel 唤醒，够小以让首字节早流。
const CHUNK_SIZE: usize = 64 * 1024;

/// 把同步 writer 的字节攒成定长块推入异步 channel，形成带背压的流。
///
/// 生成线程在 channel 满时阻塞，于是被网络消费速度自然节流；攒块让在途内存
/// 变成「channel 容量 × CHUNK_SIZE」，而不是不可控的压缩块数。
struct ChannelWriter {
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
    buf: Vec<u8>,
    written: Arc<AtomicU64>,
}

impl ChannelWriter {
    fn new(tx: mpsc::Sender<Result<Bytes, io::Error>>, written: Arc<AtomicU64>) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(CHUNK_SIZE),
            written,
        }
    }

    /// 发送已攒下的字节（若有）。
    fn send_chunk(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::take(&mut self.buf);
        self.tx.blocking_send(Ok(Bytes::from(chunk))).map_err(|_| {
            tracing::debug!(
                output_bytes = self.written.load(Ordering::Relaxed),
                "client closed the response stream"
            );
            io::Error::new(io::ErrorKind::BrokenPipe, "response stream closed")
        })
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        self.written.fetch_add(buf.len() as u64, Ordering::Relaxed);
        if self.buf.len() >= CHUNK_SIZE {
            self.send_chunk()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_chunk()
    }
}

/// 收尾时把不足一块的尾巴发出，否则它会随 writer 一起被丢弃。
impl Drop for ChannelWriter {
    fn drop(&mut self) {
        let _ = self.send_chunk();
    }
}

/// 按 Floor 模式包裹布局，流式写进 `sink`（与 CLI 的 write_schematic 一致）
fn build_schematic<L: LayoutExt, W: Write>(
    layout: L,
    floor: Floor,
    description: String,
    sink: W,
) -> Result<(), String> {
    const NAME: &str = "Note Block Track Schematic";
    const AUTHOR: &str = "nbs-fractal";
    let metadata = Metadata {
        name: NAME,
        description: &description,
        author: AUTHOR,
    };
    match floor {
        Floor::None => layout.as_litematic(metadata, sink),
        _ => WithFloor::new(layout, floor.full()).as_litematic(metadata, sink),
    }
    .map_err(|e| format!("Failed to write litematic: {e}"))
}

/// Compact 模式：按需分组，每组拆成 redstone 轨道的紧凑布局（与 CLI 一致）
fn build_compact<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
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
    build_schematic(layout, Floor::None, description, sink)
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
fn build_linear<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
    let notes: Notes = song.notes.rescale_to_game_tick(song.header.tempo).collect();
    let song_length = notes.tick_count();
    let tracks: Vec<Notes> = match args.no_breaks {
        true => vec![notes],
        false => notes.split_by_layer_gaps(),
    };
    let description = format!("Sectional from {}", args.name);
    if let Some(wrap) = NonZero::new(args.wrap) {
        let layout =
            StackedLinearLayout::new(tracks, Some(wrap), args.gap, args.floor.full(), song_length);
        build_schematic(layout, Floor::None, description, sink)
    } else {
        let layout = MultiLinearLayout::new(tracks, args.gap, song_length);
        build_schematic(layout, args.floor, description, sink)
    }
}

/// Cascade 模式：每个红石刻各占一行，整首从同一起点扫出（先按层音量与立体声混音，与 CLI 一致）
#[allow(deprecated)]
fn build_cascade<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
    let notes = song.resolved_notes();
    let layout = CascadeLayout::new(notes);
    let description = format!("Cascade from {}", args.name);
    build_schematic(layout, args.floor, description, sink)
}

/// Torrent 模式：整个隧道按刻逐片铺出（可选只保留前 N 刻、自定义轨道，与 CLI 一致）
fn build_torrent<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
    let limit = args.ticks.unwrap_or(u32::MAX);
    let notes = song
        .resolved_notes()
        .filter(|(pos, _)| pos.into_tick() < limit);
    let tracks = parse_tracks(&args.tracks)?;
    let tracks = if tracks.is_empty() {
        TorrentLayout::DEFAULT_TRACKS.to_vec()
    } else {
        tracks
    };
    let layout = TorrentLayout::new(notes, tracks, !args.all_tracks);
    let description = format!("Torrent from {}", args.name);
    build_schematic(layout, Floor::None, description, sink)
}

/// 解析轨道中心：每行一个，按非数字但符号除外切分，坐标相对轴（东为负 x、上为正 y），与 CLI 的 `--track` 一致
fn parse_tracks(text: &str) -> Result<Vec<(i32, i32)>, String> {
    let mut tracks = Vec::new();
    for line in text.lines() {
        let digits: Vec<&str> = line
            .split(|c: char| !c.is_ascii_digit() && c != '-')
            .filter(|t| !t.is_empty())
            .collect();
        if digits.is_empty() {
            continue;
        }
        let [x, y] = digits.as_slice() else {
            return Err(format!("expected two numbers: {line}"));
        };
        let x = x.parse().map_err(|_| format!("invalid track: {x}"))?;
        let y = y.parse().map_err(|_| format!("invalid track: {y}"))?;
        tracks.push((x, y));
    }
    Ok(tracks)
}

/// Fishbone 模式：以原生刻度直接投影鱼骨布局（不 retick，与上游一致）
fn build_fishbone<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
    let notes: Notes = song.notes;
    let song_length = notes.tick_count();
    let tracks: Vec<Notes> = match args.no_breaks {
        true => vec![notes],
        false => notes.split_by_layer_gaps(),
    };
    let layout = MultiFishboneLayout::new(tracks, args.gap, song_length);
    let description = format!("Fishbone from {}", args.name);
    build_schematic(layout, args.floor, description, sink)
}

/// Decompose 模式：按复用流拆成 TEC 分组，写出**中间 NBS**（上游 `decompose` 的等价实现）。
///
/// 只吃 `layers`：每个 TEC 占一组连续层，组间留空层。中间 NBS 可再上传给 `tapped` 出图。
fn build_decompose<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
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
    write_tec_groups(song, tecs, "Decompose", sink)
}

/// Match 模式：按手动指定的偏移切成 TEC 分组，写出**中间 NBS**（上游 `match` 的等价实现）。
///
/// 只吃 `rules`：每条规则一个 TEC，组间留空层。中间 NBS 可再上传给 `tapped` 出图。
fn build_match<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
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

    write_tec_groups(song, tecs, "Match", sink)
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
fn build_tapped_mode<W: Write>(song: Song, args: BuildArgs, sink: W) -> Result<(), String> {
    let description = format!("Tapped from {}", args.name);
    tapped_from_grouped(song.notes, song.header.tempo, &args, description, sink)
}

/// 把 TEC 分组写成中间 NBS（上游 CLI 的 `write_tec_groups` 等价实现）。
///
/// 每个 TEC 的展开各自占一组连续层，组间由 `concat` 留一个空层；原始层分配被丢弃。
fn write_tec_groups<W: Write>(
    mut song: Song,
    tecs: Vec<BoundedTec<Tone>>,
    label: &str,
    mut sink: W,
) -> Result<(), String> {
    let notes: Notes<Position, Note> = tecs.into_iter().map(tec_notes).concat().collect();
    let layers = notes.layer_count() as usize;
    song.notes = notes;
    song.layers = vec![Layer::default(); layers];
    song.header.song_name = format!("{label} from {}", song.header.song_name);
    song.write(&mut sink)
        .map_err(|e| format!("Failed to write NBS: {e}"))
}

/// 从已分组的 Notes 还原 TEC 并投影为 tapped 布局（上游 `tapped` 子命令的核心）
fn tapped_from_grouped<W: Write>(
    notes: Notes<Position, Note>,
    tempo: f32,
    args: &BuildArgs,
    description: String,
    sink: W,
) -> Result<(), String> {
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
    build_schematic(layout, Floor::None, description, sink)
}
