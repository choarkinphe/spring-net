use crate::{
    model::{RunConfig, SiteList},
    service::AppService,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc};
pub struct AppState {
    pub service: Arc<AppService>,
    pub token: String,
    pub instance_id: String,
    pub port: u16,
}
impl std::ops::Deref for AppState {
    type Target = AppService;
    fn deref(&self) -> &Self::Target {
        &self.service
    }
}
type App = Arc<AppState>;
type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(e: String) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({"error":e})))
}
async fn guard(State(s): State<App>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let valid = [
        format!("127.0.0.1:{}", s.port),
        format!("localhost:{}", s.port),
    ];
    if !valid.iter().any(|h| h == host) {
        return (StatusCode::FORBIDDEN, "Host 不允许").into_response();
    }
    if let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
        && origin != format!("http://{host}")
    {
        return (StatusCode::FORBIDDEN, "Origin 不允许").into_response();
    }
    if req
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|h| h == "cross-site")
    {
        return (StatusCode::FORBIDDEN, "禁止跨站访问").into_response();
    }
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        if req
            .headers()
            .get("x-aonet-token")
            .and_then(|h| h.to_str().ok())
            != Some(s.token.as_str())
        {
            return (StatusCode::FORBIDDEN, "缺少有效会话令牌").into_response();
        }
        if !req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| h.starts_with("application/json"))
        {
            return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "只接受 JSON").into_response();
        }
    }
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert("content-security-policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    response
}
pub fn router(state: App) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/styles.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/styles.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route("/transport.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/transport.js")) }))
        .route("/site-catalog.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/site-catalog.js")) }))
        .route("/catalog-controller.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/catalog-controller.js")) }))
        .route("/application-catalog.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/application-catalog.js")) }))
        .route("/application-catalog.css", get(|| async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../web/application-catalog.css")) }))
        .route("/network-tools.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/network-tools.js")) }))
        .route("/network-tools.css", get(|| async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../web/network-tools.css")) }))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/api/health",
            get(|State(s): State<App>| async move {
                Json(json!({"app":"aonet","version":env!("CARGO_PKG_VERSION"),"ready":!s.shutdown.is_cancelled(),"instance_id":s.instance_id}))
            }),
        )
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/state", get(snapshot))
        .route("/api/runs", post(start))
        .route("/api/stop", post(stop))
        .route("/api/proxy-health", get(proxy_health))
        .route("/proxy-health.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/proxy-health.js")) }))
        .route("/proxy-health.css", get(|| async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../web/proxy-health.css")) }))
        .route("/api/proxy-settings", get(proxy_settings).put(save_proxy_settings))
        .route("/system-settings.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/system-settings.js")) }))
        .route("/system-settings.css", get(|| async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../web/system-settings.css")) }))
        .route("/api/site-catalog", get(site_catalog).put(save_site_catalog))
        .route("/api/network-tools/info", get(network_info))
        .route("/api/network-tools/state", get(network_state))
        .route("/api/network-tools/start", post(start_network_tool))
        .route("/api/network-tools/stop", post(stop_network_tool))
        .route("/api/monitor/state", get(monitor_state))
        .route("/api/monitor/config", post(monitor_save))
        .route("/api/monitor/start", post(monitor_start))
        .route("/api/monitor/heartbeat", post(monitor_heartbeat))
        .route("/api/monitor/stop", post(monitor_stop))
        .route("/api/monitor/pause", post(monitor_pause))
        .route("/app-shell.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/app-shell.js")) }))
        .route("/batch-tools.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/batch-tools.js")) }))
        .route("/batch-tools.css", get(|| async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../web/batch-tools.css")) }))
        .route("/batch-tasks.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../web/batch-tasks.js")) }))
        .route("/api/batch-tasks",get(batch_tasks_list).post(batch_tasks_create))
        .route("/api/batch-tasks/:id",get(batch_tasks_get).put(batch_tasks_update))
        .route("/api/batch-tasks/:id/preflight",post(batch_tasks_preflight))
        .route("/api/batch-tasks/:id/executions",get(batch_execution_list).post(batch_execution_start))
        .route("/api/batch-executions/active",get(batch_execution_active))
        .route("/api/batch-executions/:id",get(batch_execution_get))
        .route("/api/batch-executions/:id/stop",post(batch_execution_stop))
        .route("/api/batch-executions/:id/retry-archive",post(batch_execution_retry_archive))
        .route("/api/catalog-batch/info",get(catalog_batch_info))
        .route("/api/catalog-batch/start",post(catalog_batch_start))
        .route("/api/catalog-batch/state",get(catalog_batch_state))
        .route("/api/catalog-batch/stop",post(catalog_batch_stop))
        .route("/api/batch", post(start_batch))
        .route("/api/batch/state", get(batch_state))
        .route("/api/batch/stop", post(stop_batch))
        .route("/api/batch/report", get(batch_report))
        .route("/api/diagnostics", post(start_diagnostic))
        .route("/api/diagnostics/state", get(diagnostic_state))
        .route("/api/diagnostics/stop", post(stop_diagnostic))
        .route("/api/diagnostics/report", get(diagnostic_report))
        .route("/api/lists", put(save_lists))
        .route("/api/workspace", post(workspace))
        .route("/api/history/export", get(history_export))
        .route("/api/events", get(events))
        .route("/api/export.json", get(export_json))
        .route("/api/export.csv", get(export_csv))
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}
async fn proxy_health(State(s): State<App>) -> Json<Value> {
    Json(json!(s.service.proxy_health_state()))
}
async fn proxy_settings(State(s): State<App>) -> Json<Value> {
    Json(json!(s.service.proxy_settings()))
}
async fn save_proxy_settings(
    State(s): State<App>,
    Json(request): Json<crate::proxy_settings::SaveRequest>,
) -> ApiResult {
    s.service
        .save_proxy_settings(request)
        .await
        .map(|snapshot| Json(json!(snapshot)))
        .map_err(error)
}
async fn catalog_batch_info(State(s): State<App>) -> Json<Value> {
    let mut info = s.service.catalog_batch_info().await;
    info["version"] = json!(env!("CARGO_PKG_VERSION"));
    info["token"] = json!(s.token);
    Json(info)
}
async fn catalog_batch_start(
    State(s): State<App>,
    Json(request): Json<crate::catalog_batch::Request>,
) -> ApiResult {
    s.service
        .start_catalog_batch(request)
        .await
        .map(|s| Json(json!(s)))
        .map_err(error)
}
async fn catalog_batch_state(
    State(s): State<App>,
    axum::extract::Query(request): axum::extract::Query<crate::catalog_batch::StateRequest>,
) -> ApiResult {
    s.service
        .catalog_batch_state(request)
        .await
        .map(|s| Json(json!(s)))
        .map_err(error)
}
async fn catalog_batch_stop(
    State(s): State<App>,
    Json(request): Json<crate::catalog_batch::StopRequest>,
) -> ApiResult {
    s.service
        .stop_catalog_batch(request)
        .await
        .map(|s| Json(json!(s)))
        .map_err(error)
}
async fn site_catalog(State(s): State<App>) -> Json<Value> {
    Json(json!(s.service.site_catalog()))
}
async fn save_site_catalog(
    State(s): State<App>,
    Json(request): Json<crate::site_catalog::SaveRequest>,
) -> ApiResult {
    s.service
        .save_site_catalog(request)
        .await
        .map(|snapshot| Json(json!(snapshot)))
        .map_err(error)
}
async fn network_info(State(s): State<App>) -> Json<Value> {
    Json(json!({"version":env!("CARGO_PKG_VERSION"),"token":s.token}))
}
async fn network_state(State(s): State<App>) -> Json<Value> {
    Json(json!(s.service.network_state().await))
}
async fn start_network_tool(
    State(s): State<App>,
    Json(request): Json<crate::network_tools::Request>,
) -> ApiResult {
    s.service
        .start_network_tool(request)
        .await
        .map(|snapshot| Json(json!(snapshot)))
        .map_err(error)
}
async fn stop_network_tool(State(s): State<App>, Json(request): Json<DiagnosticStop>) -> ApiResult {
    s.service
        .stop_network_tool(&request.id)
        .await
        .map(|snapshot| Json(json!(snapshot)))
        .map_err(error)
}
async fn bootstrap(State(s): State<App>) -> Json<Value> {
    Json(s.service.bootstrap(&s.token).await)
}
async fn snapshot(State(s): State<App>) -> Json<Value> {
    Json(json!(s.engine.snapshot().await))
}
async fn start(State(s): State<App>, Json(config): Json<RunConfig>) -> ApiResult {
    s.service
        .start(config)
        .await
        .map(|r| Json(json!(r)))
        .map_err(error)
}
async fn stop(State(s): State<App>) -> Json<Value> {
    Json(json!(s.engine.stop().await))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorSave {
    expected_revision: u64,
    targets: Vec<crate::monitor::MonitorTarget>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorStart {
    request_id: String,
    revision: u64,
    #[serde(default)]
    proxy: Option<crate::proxy::ProxyConfig>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorGeneration {
    generation: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorPause {
    generation: String,
    target_id: String,
    paused: bool,
}
async fn monitor_state(State(s): State<App>) -> Json<Value> {
    Json(s.service.monitor_state())
}
async fn monitor_save(State(s): State<App>, Json(r): Json<MonitorSave>) -> ApiResult {
    s.service
        .monitor_save(r.expected_revision, r.targets)
        .await
        .map(Json)
        .map_err(error)
}
async fn monitor_start(State(s): State<App>, Json(r): Json<MonitorStart>) -> ApiResult {
    s.service
        .monitor_start(
            &r.request_id,
            r.revision,
            r.proxy,
            crate::monitor::MonitorLifetime::Browser,
        )
        .await
        .map(Json)
        .map_err(error)
}
async fn monitor_heartbeat(State(s): State<App>, Json(r): Json<MonitorGeneration>) -> ApiResult {
    s.service
        .monitor_heartbeat(&r.generation)
        .map(Json)
        .map_err(error)
}
async fn monitor_pause(State(s): State<App>, Json(r): Json<MonitorPause>) -> ApiResult {
    s.service
        .monitor_pause(&r.generation, &r.target_id, r.paused)
        .map(Json)
        .map_err(error)
}
async fn monitor_stop(State(s): State<App>, Json(r): Json<MonitorGeneration>) -> ApiResult {
    s.service
        .monitor_stop(&r.generation)
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_state(State(s): State<App>) -> Json<Value> {
    Json(json!(s.service.batch_state().await))
}
async fn start_batch(
    State(s): State<App>,
    Json(r): Json<crate::batch_service::RunBatchRequest>,
) -> ApiResult {
    s.service
        .start_batch(r)
        .await
        .map(|x| Json(json!(x)))
        .map_err(error)
}
async fn stop_batch(State(s): State<App>, Json(r): Json<DiagnosticStop>) -> ApiResult {
    s.service
        .stop_batch(&r.id)
        .await
        .map(|x| Json(json!(x)))
        .map_err(error)
}
async fn batch_report(
    State(s): State<App>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(id) = query.get("id") else {
        return (StatusCode::BAD_REQUEST, "缺少批次 ID").into_response();
    };
    match s.service.export_run("batch", Some(id)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/json; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"aonet-batch.json\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, message).into_response(),
    }
}
async fn diagnostic_state(State(s): State<App>) -> Json<Value> {
    Json(json!(s.service.diagnostic_state().await))
}
async fn start_diagnostic(
    State(s): State<App>,
    Json(config): Json<crate::diagnostics::DiagnosticConfig>,
) -> ApiResult {
    s.service
        .start_diagnostic(config)
        .await
        .map(|d| Json(json!(d)))
        .map_err(error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticStop {
    id: String,
}
async fn stop_diagnostic(State(s): State<App>, Json(request): Json<DiagnosticStop>) -> ApiResult {
    s.service
        .stop_diagnostic(&request.id)
        .await
        .map(|d| Json(json!(d)))
        .map_err(error)
}
async fn diagnostic_report(
    State(s): State<App>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(id) = query.get("id") else {
        return (StatusCode::BAD_REQUEST, "缺少诊断 ID").into_response();
    };
    match s.service.export_run("diagnostic", Some(id)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/json; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"aonet-diagnostic.json\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, message).into_response(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListUpdate {
    lists: Vec<SiteList>,
}
async fn save_lists(State(s): State<App>, Json(update): Json<ListUpdate>) -> ApiResult {
    s.service.save_lists(update.lists).await.map_err(error)?;
    Ok(bootstrap(State(s)).await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceRequest {
    operation: String,
    #[serde(default)]
    payload: Value,
}
async fn workspace(State(s): State<App>, Json(request): Json<WorkspaceRequest>) -> ApiResult {
    s.service
        .workspace(&request.operation, request.payload, &s.token)
        .await
        .map(Json)
        .map_err(error)
}
async fn history_export(
    State(s): State<App>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(id) = query.get("id") else {
        return (StatusCode::BAD_REQUEST, "缺少运行 ID").into_response();
    };
    let kind = query.get("kind").map(String::as_str).unwrap_or("json");
    match s.service.export_run(kind, Some(id)).await {
        Ok(data) => (
            [(
                header::CONTENT_TYPE,
                if kind == "csv" {
                    "text/csv; charset=utf-8"
                } else {
                    "application/json; charset=utf-8"
                },
            )],
            data,
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}
async fn events(
    State(s): State<App>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut tick=tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select!{_=s.shutdown.cancelled()=>break,_=tick.tick()=>{}}
            let snapshot=s.engine.snapshot().await;
            yield Ok(Event::default().event("snapshot").data(serde_json::to_string(&snapshot).unwrap_or("null".into())));
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}
async fn export_json(State(s): State<App>) -> Response {
    match s.service.export("json").await {
        Ok(data) => (
            [
                (header::CONTENT_TYPE, "application/json; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"aonet-result.json\"",
                ),
            ],
            data,
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}
async fn export_csv(State(s): State<App>) -> Response {
    match s.service.export("csv").await {
        Ok(data) => (
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"aonet-result.csv\"",
                ),
            ],
            data,
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

async fn batch_tasks_list(State(s): State<App>) -> Json<Value> {
    Json(s.service.batch_tasks_list())
}
async fn batch_tasks_create(
    State(s): State<App>,
    Json(r): Json<crate::batch_task_service::CreateRequest>,
) -> ApiResult {
    s.service
        .batch_tasks_create(r)
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_tasks_get(State(s): State<App>, Path(id): Path<String>) -> ApiResult {
    s.service
        .batch_tasks_get(crate::batch_task_service::TaskRequest { task_id: id })
        .map(Json)
        .map_err(error)
}
async fn batch_tasks_update(
    State(s): State<App>,
    Path(id): Path<String>,
    Json(r): Json<crate::batch_task_service::UpdateRequest>,
) -> ApiResult {
    if id != r.task_id {
        return Err(error("path/task_id mismatch".into()));
    }
    s.service
        .batch_tasks_update(r)
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_tasks_preflight(
    State(s): State<App>,
    Path(id): Path<String>,
    Json(r): Json<crate::batch_task_service::PreflightRequest>,
) -> ApiResult {
    if id != r.task_id {
        return Err(error("path/task_id mismatch".into()));
    }
    s.service
        .batch_tasks_preflight(r)
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_execution_list(State(s): State<App>, Path(id): Path<String>) -> ApiResult {
    s.service
        .batch_execution_list(crate::batch_task_service::TaskRequest { task_id: id })
        .map(Json)
        .map_err(error)
}
async fn batch_execution_start(
    State(s): State<App>,
    Path(id): Path<String>,
    Json(r): Json<crate::batch_task_service::StartRequest>,
) -> ApiResult {
    if id != r.task_id {
        return Err(error("path/task_id mismatch".into()));
    }
    s.service
        .batch_execution_start(r)
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_execution_active(State(s): State<App>) -> ApiResult {
    s.service
        .batch_execution_active()
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_execution_get(State(s): State<App>, Path(id): Path<String>) -> ApiResult {
    s.service
        .batch_execution_get(crate::batch_task_service::ExecutionRequest { execution_id: id })
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_execution_stop(
    State(s): State<App>,
    Path(id): Path<String>,
    Json(r): Json<crate::batch_task_service::ExecutionRequest>,
) -> ApiResult {
    if id != r.execution_id {
        return Err(error("path/execution_id mismatch".into()));
    }
    s.service
        .batch_execution_stop(r)
        .await
        .map(Json)
        .map_err(error)
}
async fn batch_execution_retry_archive(
    State(s): State<App>,
    Path(id): Path<String>,
    Json(r): Json<crate::batch_task_service::ExecutionRequest>,
) -> ApiResult {
    if id != r.execution_id {
        return Err(error("path/execution_id mismatch".into()));
    }
    s.service
        .batch_execution_retry_archive(r)
        .await
        .map(Json)
        .map_err(error)
}
