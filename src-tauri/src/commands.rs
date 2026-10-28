use crate::DesktopState;
use aonet::model::{Run, RunConfig, SiteList};
use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn proxy_health(
    state: State<'_, DesktopState>,
) -> Result<aonet::proxy_health::Snapshot, String> {
    Ok(state.service().await?.proxy_health_state())
}
#[tauri::command]
pub async fn proxy_settings(
    state: State<'_, DesktopState>,
) -> Result<aonet::proxy_settings::Snapshot, String> {
    Ok(state.service().await?.proxy_settings())
}
#[tauri::command]
pub async fn save_proxy_settings(
    state: State<'_, DesktopState>,
    request: aonet::proxy_settings::SaveRequest,
) -> Result<aonet::proxy_settings::Snapshot, String> {
    state.service().await?.save_proxy_settings(request).await
}
#[tauri::command]
pub async fn catalog_batch_info(state: State<'_, DesktopState>) -> Result<Value, String> {
    Ok(state.service().await?.catalog_batch_info().await)
}
#[tauri::command]
pub async fn catalog_batch_start(
    state: State<'_, DesktopState>,
    request: aonet::catalog_batch::Request,
) -> Result<aonet::catalog_batch::Snapshot, String> {
    state.service().await?.start_catalog_batch(request).await
}
#[tauri::command]
pub async fn catalog_batch_state(
    state: State<'_, DesktopState>,
    request: aonet::catalog_batch::StateRequest,
) -> Result<aonet::catalog_batch::StateResponse, String> {
    state.service().await?.catalog_batch_state(request).await
}
#[tauri::command]
pub async fn catalog_batch_stop(
    state: State<'_, DesktopState>,
    request: aonet::catalog_batch::StopRequest,
) -> Result<aonet::catalog_batch::Snapshot, String> {
    state.service().await?.stop_catalog_batch(request).await
}
#[tauri::command]
pub async fn site_catalog(
    state: State<'_, DesktopState>,
) -> Result<aonet::site_catalog::Snapshot, String> {
    Ok(state.service().await?.site_catalog())
}
#[tauri::command]
pub async fn save_site_catalog(
    state: State<'_, DesktopState>,
    request: aonet::site_catalog::SaveRequest,
) -> Result<aonet::site_catalog::Snapshot, String> {
    state.service().await?.save_site_catalog(request).await
}
#[tauri::command]
pub async fn network_info(state: State<'_, DesktopState>) -> Result<Value, String> {
    let _ = state.service().await?;
    Ok(serde_json::json!({"version":env!("CARGO_PKG_VERSION")}))
}
#[tauri::command]
pub async fn network_state(
    state: State<'_, DesktopState>,
) -> Result<Option<aonet::network_tools::Snapshot>, String> {
    Ok(state.service().await?.network_state().await)
}
#[tauri::command]
pub async fn network_start(
    state: State<'_, DesktopState>,
    request: aonet::network_tools::Request,
) -> Result<aonet::network_tools::Snapshot, String> {
    state.service().await?.start_network_tool(request).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkStop {
    id: String,
}
#[tauri::command]
pub async fn network_stop(
    state: State<'_, DesktopState>,
    request: NetworkStop,
) -> Result<aonet::network_tools::Snapshot, String> {
    state.service().await?.stop_network_tool(&request.id).await
}
#[tauri::command]
pub async fn bootstrap(state: State<'_, DesktopState>) -> Result<Value, String> {
    Ok(state.service().await?.bootstrap("desktop-local-ipc").await)
}
#[tauri::command]
pub async fn state(state: State<'_, DesktopState>) -> Result<Option<Run>, String> {
    Ok(state.service().await?.engine.snapshot().await)
}
#[tauri::command]
pub async fn start_run(state: State<'_, DesktopState>, config: RunConfig) -> Result<Run, String> {
    state.service().await?.start(config).await
}
#[tauri::command]
pub async fn stop_run(state: State<'_, DesktopState>) -> Result<Option<Run>, String> {
    Ok(state.service().await?.engine.stop().await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorSave {
    expected_revision: u64,
    targets: Vec<aonet::monitor::MonitorTarget>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorStart {
    request_id: String,
    revision: u64,
    #[serde(default)]
    proxy: Option<aonet::proxy::ProxyConfig>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorGeneration {
    generation: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorPause {
    generation: String,
    target_id: String,
    paused: bool,
}
#[tauri::command]
pub async fn monitor_state(state: State<'_, DesktopState>) -> Result<Value, String> {
    Ok(state.service().await?.monitor_state())
}
#[tauri::command]
pub async fn monitor_save(
    state: State<'_, DesktopState>,
    request: MonitorSave,
) -> Result<Value, String> {
    state
        .service()
        .await?
        .monitor_save(request.expected_revision, request.targets)
        .await
}
#[tauri::command]
pub async fn monitor_start(
    state: State<'_, DesktopState>,
    request: MonitorStart,
) -> Result<Value, String> {
    state
        .service()
        .await?
        .monitor_start(
            &request.request_id,
            request.revision,
            request.proxy,
            aonet::monitor::MonitorLifetime::Desktop,
        )
        .await
}
#[tauri::command]
pub async fn monitor_heartbeat(
    state: State<'_, DesktopState>,
    request: MonitorGeneration,
) -> Result<Value, String> {
    state
        .service()
        .await?
        .monitor_heartbeat(&request.generation)
}
#[tauri::command]
pub async fn monitor_stop(
    state: State<'_, DesktopState>,
    request: MonitorGeneration,
) -> Result<Value, String> {
    state
        .service()
        .await?
        .monitor_stop(&request.generation)
        .await
}
#[tauri::command]
pub async fn monitor_pause(
    state: State<'_, DesktopState>,
    request: MonitorPause,
) -> Result<Value, String> {
    state
        .service()
        .await?
        .monitor_pause(&request.generation, &request.target_id, request.paused)
}
#[tauri::command]
pub async fn diagnostic_state(
    state: State<'_, DesktopState>,
) -> Result<Option<aonet::diagnostics::DiagnosticSnapshot>, String> {
    Ok(state.service().await?.diagnostic_state().await)
}
#[tauri::command]
pub async fn start_diagnostic(
    state: State<'_, DesktopState>,
    config: aonet::diagnostics::DiagnosticConfig,
) -> Result<aonet::diagnostics::DiagnosticSnapshot, String> {
    state.service().await?.start_diagnostic(config).await
}
#[tauri::command]
pub async fn stop_diagnostic(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<aonet::diagnostics::DiagnosticSnapshot, String> {
    state.service().await?.stop_diagnostic(&id).await
}
#[tauri::command]
pub async fn batch_state(
    state: State<'_, DesktopState>,
) -> Result<Option<aonet::batch_service::BatchSnapshot>, String> {
    Ok(state.service().await?.batch_state().await)
}
#[tauri::command]
pub async fn batch_start(
    state: State<'_, DesktopState>,
    request: aonet::batch_service::RunBatchRequest,
) -> Result<aonet::batch_service::BatchSnapshot, String> {
    state.service().await?.start_batch(request).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchStop {
    request_id: String,
}
#[tauri::command]
pub async fn batch_stop(
    state: State<'_, DesktopState>,
    request: BatchStop,
) -> Result<aonet::batch_service::BatchSnapshot, String> {
    state.service().await?.stop_batch(&request.request_id).await
}
#[tauri::command]
pub async fn save_lists(
    state: State<'_, DesktopState>,
    lists: Vec<SiteList>,
) -> Result<Value, String> {
    let service = state.service().await?;
    service.save_lists(lists).await?;
    Ok(service.bootstrap("desktop-local-ipc").await)
}

#[tauri::command]
pub async fn workspace(
    state: State<'_, DesktopState>,
    operation: String,
    payload: Value,
) -> Result<Value, String> {
    state
        .service()
        .await?
        .workspace(&operation, payload, "desktop-local-ipc")
        .await
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportKind {
    Json,
    Csv,
    Lists,
    Diagnostic,
    Batch,
}
#[tauri::command]
pub async fn import_lists_dialog(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<Value>, String> {
    let _service = state.service().await?;
    tauri::async_runtime::spawn_blocking(move || {
        let Some(selected) = app
            .dialog()
            .file()
            .add_filter("AoNet JSON", &["json"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = selected.into_path().map_err(|e| e.to_string())?;
        let bytes = aonet::storage::read_bounded(&path, 1_048_576)?;
        let value = serde_json::from_slice(&bytes).map_err(|e| format!("无效列表 JSON：{e}"))?;
        Ok(Some(value))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn export_dialog(
    app: AppHandle,
    state: State<'_, DesktopState>,
    kind: ExportKind,
    id: Option<String>,
) -> Result<bool, String> {
    let kind = match kind {
        ExportKind::Json => "json",
        ExportKind::Csv => "csv",
        ExportKind::Lists => "lists",
        ExportKind::Diagnostic => "diagnostic",
        ExportKind::Batch => "batch",
    };
    let content = state
        .service()
        .await?
        .export_run(kind, id.as_deref())
        .await?;
    let extension = if kind == "csv" { "csv" } else { "json" };
    let filename = if kind == "lists" {
        "aonet-lists.json".to_owned()
    } else if kind == "diagnostic" {
        format!(
            "aonet-diagnostic-{}.json",
            id.as_deref().ok_or("缺少诊断 ID")?
        )
    } else if kind == "batch" {
        format!("aonet-batch-{}.json", id.as_deref().ok_or("缺少批次 ID")?)
    } else {
        format!("aonet-result.{extension}")
    };
    tauri::async_runtime::spawn_blocking(move || {
        let Some(selected) = app
            .dialog()
            .file()
            .add_filter("AoNet export", &[extension])
            .set_file_name(&filename)
            .blocking_save_file()
        else {
            return Ok(false);
        };
        let path = selected.into_path().map_err(|e| e.to_string())?;
        // The path comes only from the native user decision, never from a command argument.
        aonet::storage::atomic_bytes(&path, &content)?;
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}
