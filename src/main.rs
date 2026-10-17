//! 二进制入口：axum 应用装配与服务生命周期。
//!
//! 领域逻辑（模式构建、handler）在库中，这里只负责把路由、layer、监听器和
//! 浏览器唤起串起来，以及桌面双击时把进程放进终端窗口。

use std::net::Ipv4Addr;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use nbs_fractal_web::server::{generate, static_handler};
use nbs_fractal_web::terminal;
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;

const DEFAULT_PORT: u16 = 3021;

#[tokio::main]
async fn main() {
    // 桌面双击（没有控制终端）时，把自己放进终端窗口重开，生命周期交给终端
    if terminal::relaunch_in_terminal() {
        return;
    }
    tracing_subscriber::fmt().without_time().init();

    // 严格绑定默认端口：反代部署依赖固定地址，不做顺延
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, DEFAULT_PORT)).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("Failed to bind 127.0.0.1:{DEFAULT_PORT}: {err}");
            terminal::wait_for_enter();
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
