// MeowCanvas 入口:后台线程跑 Axum 服务,主线程跑 wry 桌面窗口。
// 服务编译期把前端静态资源打包进二进制 (rust-embed),运行时单文件即可启动。
use std::path::PathBuf;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

mod api;
mod server;
mod store;

fn main() -> anyhow::Result<()> {
    // 初始化日志:默认 info,屏蔽 tao 事件循环的已知警告噪音
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .filter_module("tao::platform_impl::platform::event_loop::runner", log::LevelFilter::Error)
        .init();

    // 数据目录:可执行文件同级 data/ (便携式桌面应用)
    let data_dir = resolve_data_dir();
    log::info!("数据目录: {}", data_dir.display());

    // 后台线程:tokio runtime + Axum 服务
    let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => { log::error!("创建 tokio runtime 失败: {e}"); return; }
        };
        runtime.block_on(async {
            match server::spawn_server(&data_dir) {
                Ok(s) => { let _ = port_tx.send(s.port); }
                Err(e) => { log::error!("启动服务失败: {e}"); return; }
            }
            // 保持 runtime 常驻,服务在后台持续运行
            std::future::pending::<()>().await;
        });
    });

    let port = port_rx.recv().map_err(|e| anyhow::anyhow!("服务线程未返回端口: {e}"))?;
    let url = format!("http://127.0.0.1:{port}/");
    log::info!("桌面窗口加载 {url}");

    // 主线程:wry 桌面窗口
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("MeowCanvas - 无限画布")
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(LogicalSize::new(960.0, 640.0))
        .build(&event_loop)?;

    let _webview = WebViewBuilder::new(&window)
        .with_url(&url)
        .with_devtools(cfg!(debug_assertions))
        .build()?;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}

/// 解析数据目录:优先可执行文件所在目录的 data/ 子目录
fn resolve_data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join("data");
        }
    }
    PathBuf::from("data")
}
