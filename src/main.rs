use aonet::{
    api,
    service::{AppService, ServiceConfig},
};
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
static EXIT_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    EXIT_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}
fn install_signals() {
    #[cfg(unix)]
    // SAFETY: handler performs only an atomic store and has process lifetime.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}
async fn shutdown_signal() {
    #[cfg(unix)]
    while !EXIT_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    #[cfg(not(unix))]
    std::future::pending::<()>().await;
}
fn monitor_parent(shutdown: CancellationToken) {
    // A blocking OS thread, not Tokio's blocking pool: it must not delay runtime exit.
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buffer = [0u8; 64];
        let mut input = std::io::stdin().lock();
        loop {
            match input.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    shutdown.cancel();
                    break;
                }
                Ok(_) => {}
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_signals();
    let shutdown = CancellationToken::new();
    let managed = std::env::var("AONET_MANAGED").as_deref() == Ok("1");
    if managed {
        monitor_parent(shutdown.clone());
    }
    let instance_id =
        std::env::var("AONET_INSTANCE_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    if instance_id.len() > 128
        || !instance_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err("无效实例标识".into());
    }
    let root = std::env::var_os("AONET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let data = std::env::var_os("AONET_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("data"));
    let port: u16 = std::env::var("AONET_PORT")
        .unwrap_or("9876".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|e| format!("无法监听 127.0.0.1:{port}，不会终止其他进程：{e}"))?;
    let port = listener.local_addr()?.port();
    let auth_file = std::env::var_os("AONET_AUTH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("config/authorized-targets.json"));
    let service = Arc::new(
        AppService::initialize(
            ServiceConfig {
                root,
                data,
                auth_file,
                control_port: Some(port),
            },
            shutdown.clone(),
        )
        .await?,
    );
    let state = Arc::new(api::AppState {
        service,
        token: uuid::Uuid::new_v4().to_string(),
        instance_id: instance_id.clone(),
        port,
    });
    if shutdown.is_cancelled() {
        return Ok(());
    }
    println!(
        "AONET_READY {}",
        serde_json::json!({"protocol":1,"port":port,"instance_id":instance_id})
    );
    use std::io::Write;
    std::io::stdout().flush()?;
    println!(
        "AoNet {}\nGUI: http://127.0.0.1:{port}\n本机演示不代表真实外网承载能力；公共网站仅低频探测。",
        env!("CARGO_PKG_VERSION")
    );
    let signal = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        signal.cancel();
    });
    let signal = shutdown.clone();
    use std::future::IntoFuture;
    let server = axum::serve(listener, api::router(state.clone()))
        .with_graceful_shutdown(signal.cancelled_owned())
        .into_future();
    tokio::pin!(server);
    let result = tokio::select! {
        result = &mut server => result.map_err(|e|e.to_string()),
        _ = shutdown.cancelled() => {
            state.service.shutdown_and_wait().await;
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut server).await
                .map_err(|_| "服务退出超时".to_string()).and_then(|r| r.map_err(|e|e.to_string()))
        }
    };
    shutdown.cancel();
    state.service.shutdown_and_wait().await;
    result?;
    Ok(())
}
