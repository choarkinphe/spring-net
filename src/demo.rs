use axum::{Router, response::IntoResponse, routing::get};
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

pub struct DemoHandle {
    pub http_port: u16,
    pub tcp_port: u16,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}
impl DemoHandle {
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        for mut task in self.tasks.drain(..) {
            if tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}
pub async fn start(cancel: CancellationToken) -> Result<(u16, u16), String> {
    let demo = start_tracked(cancel).await?;
    // Compatibility for test fixtures: their cancellation token owns lifetime.
    Ok((demo.http_port, demo.tcp_port))
}
pub async fn start_tracked(cancel: CancellationToken) -> Result<DemoHandle, String> {
    let http = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let tcp = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let http_port = http.local_addr().map_err(|e| e.to_string())?.port();
    let tcp_port = tcp.local_addr().map_err(|e| e.to_string())?.port();
    let c = cancel.clone();
    let http_task = tokio::spawn(async move {
        let app = Router::new()
            .route(
                "/probe",
                get(|| async {
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    "AoNet demo only\n"
                }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    "slow demo\n"
                }),
            )
            .route(
                "/limited",
                get(|| async {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "30")],
                        "demo rate limit",
                    )
                        .into_response()
                }),
            );
        let _ = axum::serve(http, app)
            .with_graceful_shutdown(c.cancelled_owned())
            .await;
    });
    let c = cancel.clone();
    let tcp_task = tokio::spawn(async move {
        let mut workers = JoinSet::new();
        loop {
            tokio::select! {
                _=c.cancelled()=>break,
                Some(_)=workers.join_next(),if !workers.is_empty()=>{},
                a=tcp.accept(),if workers.len()<1100=>{
                    if let Ok((mut s,_))=a{let c=c.clone();workers.spawn(async move{let mut b=[0;1];tokio::select!{_=c.cancelled()=>{},_=s.read(&mut b)=>{}}});}
                }
            }
        }
        workers.abort_all();
        while workers.join_next().await.is_some() {}
    });
    Ok(DemoHandle {
        http_port,
        tcp_port,
        cancel,
        tasks: vec![http_task, tcp_task],
    })
}
