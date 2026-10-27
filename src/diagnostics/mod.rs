mod model;
pub(crate) mod path;
pub(crate) mod policy;
#[cfg(test)]
mod tests;
mod transport;

pub use model::{DiagnosticConfig, DiagnosticSnapshot, DiagnosticStage, DiagnosticStages};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, oneshot},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

type Shared = Arc<Mutex<DiagnosticSnapshot>>;
type Resolver = Arc<dyn Fn(String, u16) -> Result<Vec<SocketAddr>, String> + Send + Sync>;
struct Active {
    snapshot: Shared,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}
#[derive(Default)]
struct State {
    current: Option<Active>,
    last_start: Option<Instant>,
    hosts: HashMap<String, Instant>,
}

pub struct DiagnosticManager {
    state: AsyncMutex<State>,
    control_port: Option<u16>,
    shutdown: CancellationToken,
    resolver: Resolver,
    dns_budget: Duration,
}
impl DiagnosticManager {
    pub fn new(control_port: Option<u16>, shutdown: CancellationToken) -> Self {
        Self {
            state: AsyncMutex::new(State::default()),
            control_port,
            shutdown,
            dns_budget: Duration::from_secs(5),
            resolver: Arc::new(|host, port| {
                let iter = (host.as_str(), port)
                    .to_socket_addrs()
                    .map_err(|e| e.to_string())?;
                // At most 65 entries retained; the sentinel makes overflow fail closed.
                Ok(iter.take(65).collect())
            }),
        }
    }
    pub async fn snapshot(&self) -> Option<DiagnosticSnapshot> {
        self.state
            .lock()
            .await
            .current
            .as_ref()
            .map(|a| a.snapshot.lock().unwrap().clone())
    }
    pub async fn existing_request(
        &self,
        config: &DiagnosticConfig,
    ) -> Result<Option<DiagnosticSnapshot>, String> {
        let config = policy::normalize(config.clone(), self.control_port)?;
        let state = self.state.lock().await;
        duplicate(&state, &config)
    }
    /// `allowed_loopback` must contain only currently-owned, explicitly enabled
    /// self-test listener sockets, supplied by service code, never request data.
    pub async fn start(
        &self,
        config: DiagnosticConfig,
        allowed_loopback: Vec<SocketAddr>,
    ) -> Result<DiagnosticSnapshot, String> {
        let config = policy::normalize(config, self.control_port)?;
        let mut state = self.state.lock().await;
        if let Some(snapshot) = duplicate(&state, &config)? {
            return Ok(snapshot);
        }
        if self.shutdown.is_cancelled() {
            return Err("Application is shutting down".into());
        }
        if state.current.as_ref().is_some_and(|a| {
            a.snapshot.lock().unwrap().active() || a.task.as_ref().is_some_and(|t| !t.is_finished())
        }) {
            return Err("Diagnostic is running, stopping or draining".into());
        }
        if let Some(task) = state.current.as_mut().and_then(|a| a.task.take()) {
            task.await.map_err(|e| e.to_string())?;
        }
        let now = Instant::now();
        if state
            .last_start
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(5))
        {
            return Err("Diagnostic global cooldown: at least 5 seconds between starts".into());
        }
        state
            .hosts
            .retain(|_, t| now.duration_since(*t) < Duration::from_secs(30));
        if state.hosts.contains_key(&config.target) {
            return Err("Diagnostic host cooldown: at least 30 seconds between starts".into());
        }
        // Validate literal addresses synchronously, without performing DNS or traffic.
        if let Ok(ip) = config.target.parse() {
            policy::validate_addresses(
                &config,
                vec![SocketAddr::new(ip, config.port)],
                &allowed_loopback,
            )?;
        }
        state.last_start = Some(now);
        state.hosts.insert(config.target.clone(), now);
        let snapshot = DiagnosticSnapshot::new(config.clone());
        let returned = snapshot.clone();
        let shared = Arc::new(Mutex::new(snapshot));
        let cancel = self.shutdown.child_token();
        let worker_shared = shared.clone();
        let worker_cancel = cancel.clone();
        let shutdown = self.shutdown.clone();
        let resolver = self.resolver.clone();
        let dns_budget = self.dns_budget;
        let task = tokio::spawn(async move {
            execute(
                worker_shared,
                config,
                allowed_loopback,
                worker_cancel,
                shutdown,
                resolver,
                dns_budget,
            )
            .await;
        });
        state.current = Some(Active {
            snapshot: shared,
            cancel,
            task: Some(task),
        });
        Ok(returned)
    }
    pub async fn stop(&self, id: &str) -> Result<DiagnosticSnapshot, String> {
        let state = self.state.lock().await;
        let a = state.current.as_ref().ok_or("No diagnostic")?;
        let mut snapshot = a.snapshot.lock().unwrap();
        if snapshot.id != id {
            return Err("Diagnostic UUID does not match the latest run".into());
        }
        if snapshot.active() {
            if snapshot.status != "draining" {
                snapshot.status = "stopping".into();
            }
            snapshot.updated_at = stamp();
            a.cancel.cancel();
        }
        Ok(snapshot.clone())
    }
    pub async fn settle(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if let Some(a) = state.current.as_mut() {
            if a.snapshot.lock().unwrap().active()
                || a.task.as_ref().is_some_and(|t| !t.is_finished())
            {
                return Err("Diagnostic is running, stopping or draining".into());
            }
            if let Some(task) = a.task.take() {
                task.await.map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub async fn shutdown_and_wait(&self) {
        self.shutdown.cancel();
        let task = {
            let mut state = self.state.lock().await;
            state.current.as_mut().and_then(|a| {
                a.cancel.cancel();
                a.task.take()
            })
        };
        // Never abort: route child must be reaped and streams dropped. Only the
        // uninterruptible resolver thread has a bounded shutdown exception.
        if let Some(task) = task
            && let Err(error) = task.await
        {
            eprintln!("diagnostic worker failed during shutdown: {error}");
        }
    }
    pub async fn export(&self, id: &str) -> Result<Value, String> {
        let snapshot = self.snapshot().await.ok_or("No diagnostic report")?;
        if snapshot.id != id {
            return Err("Diagnostic UUID does not match the latest run".into());
        }
        Ok(
            json!({"kind":"diagnostic","version":1,"snapshot_at":stamp(),"partial":snapshot.active(),"snapshot":snapshot}),
        )
    }
}
fn duplicate(
    state: &State,
    config: &DiagnosticConfig,
) -> Result<Option<DiagnosticSnapshot>, String> {
    if let Some(a) = &state.current {
        let snapshot = a.snapshot.lock().unwrap();
        if snapshot.request_id == config.request_id {
            if snapshot.config != *config {
                return Err(
                    "request_id already belongs to a different normalized configuration".into(),
                );
            }
            return Ok(Some(snapshot.clone()));
        }
    }
    Ok(None)
}
fn stamp() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn update(shared: &Shared, f: impl FnOnce(&mut DiagnosticSnapshot)) {
    let mut s = shared.lock().unwrap();
    f(&mut s);
    s.updated_at = stamp();
}
fn stage(
    status: &str,
    started: Instant,
    error: Option<String>,
    observations: Value,
) -> DiagnosticStage {
    DiagnosticStage {
        status: status.into(),
        elapsed_ms: Some(started.elapsed().as_millis() as u64),
        error,
        observations,
    }
}
fn finish(shared: &Shared, status: &str) {
    update(shared, |s| {
        for stage in [
            &mut s.stages.dns,
            &mut s.stages.route,
            &mut s.stages.tcp,
            &mut s.stages.tls,
        ] {
            if stage.status == "pending" || stage.status == "running" {
                stage.status = if status == "cancelled" {
                    "cancelled"
                } else {
                    "skipped"
                }
                .into();
                stage.observations = json!({"reason":"Earlier stage prevented execution, or cancellation requested"});
            }
        }
        s.status = status.into();
        s.finished_at = Some(stamp());
    });
}

async fn dns(
    shared: &Shared,
    config: &DiagnosticConfig,
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
    resolver: Resolver,
    budget: Duration,
) -> Result<Vec<SocketAddr>, ()> {
    let started = Instant::now();
    update(shared, |s| s.stages.dns.status = "running".into());
    if let Ok(ip) = config.target.parse() {
        let addrs = vec![SocketAddr::new(ip, config.port)];
        update(shared, |s| {
            s.stages.dns = stage(
                "skipped",
                started,
                None,
                json!({"method":"IP literal; DNS not applicable","addresses":[ip.to_string()]}),
            )
        });
        return Ok(addrs);
    }
    let (tx, mut rx) = oneshot::channel();
    let host = config.target.clone();
    let port = config.port;
    let handle = match std::thread::Builder::new()
        .name("aonet-system-dns".into())
        .spawn(move || {
            let result = resolver(host, port);
            let _ = tx.send(result);
        }) {
        Ok(handle) => handle,
        Err(error) => {
            update(shared, |s| {
                s.stages.dns = stage(
                    "error",
                    started,
                    Some(error.to_string()),
                    json!({"method":"system resolver"}),
                )
            });
            return Err(());
        }
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        _ = tokio::time::sleep(budget) => None,
        result = &mut rx => Some(result),
    };
    if let Some(result) = result {
        let _ = handle.join();
        let result = result.map_err(|e| e.to_string()).and_then(|v| v);
        return match result {
            Ok(addresses) => {
                update(shared, |s| {
                    s.stages.dns = stage(
                        "success",
                        started,
                        None,
                        json!({"method":"system resolver (hosts/cache included)","addresses":addresses.iter().map(|a|a.ip().to_string()).collect::<Vec<_>>()}),
                    )
                });
                Ok(addresses)
            }
            Err(error) => {
                update(shared, |s| {
                    s.stages.dns = stage(
                        "error",
                        started,
                        Some(error),
                        json!({"method":"system resolver"}),
                    )
                });
                Err(())
            }
        };
    }
    let cancelled = cancel.is_cancelled();
    update(shared, |s| {
        s.status = "draining".into();
        s.stages.dns = stage(
            if cancelled { "cancelled" } else { "timeout" },
            started,
            None,
            json!({"method":"system resolver","outstanding":true,"reason":"OS resolver cannot be interrupted; late result will be discarded"}),
        );
    });
    let ended = tokio::select! {
        result = &mut rx => { let _ = result; true }
        _ = shutdown.cancelled() => tokio::time::timeout(Duration::from_secs(2),&mut rx).await.is_ok(),
    };
    if ended {
        let _ = handle.join();
        update(shared, |s| {
            s.stages.dns.observations["outstanding"] = json!(false)
        });
    } else {
        // Dropping a std thread handle detaches only this uninterruptible OS
        // resolver at process shutdown; never during ordinary cancel/timeout.
        drop(handle);
        let warning = "OS DNS resolver has not ended after the 2s shutdown grace; its thread remains alive until process exit, not cancelled";
        eprintln!("{warning}");
        update(shared, |s| s.warnings.push(warning.into()));
    }
    Err(())
}

async fn execute(
    shared: Shared,
    config: DiagnosticConfig,
    allowed_loopback: Vec<SocketAddr>,
    cancel: CancellationToken,
    shutdown: CancellationToken,
    resolver: Resolver,
    dns_budget: Duration,
) {
    if cancel.is_cancelled() {
        finish(&shared, "cancelled");
        return;
    }
    let addresses = match dns(&shared, &config, &cancel, &shutdown, resolver, dns_budget).await {
        Ok(addresses) => addresses,
        Err(()) => {
            finish(
                &shared,
                if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                },
            );
            return;
        }
    };
    if cancel.is_cancelled() {
        finish(&shared, "cancelled");
        return;
    }
    let addresses = match policy::validate_addresses(&config, addresses, &allowed_loopback) {
        Ok(addresses) => addresses,
        Err(error) => {
            update(&shared, |s| {
                s.stages.dns.status = "blocked".into();
                s.stages.dns.error = Some(error);
            });
            finish(&shared, "failed");
            return;
        }
    };
    let address = addresses[0];
    update(&shared, |s| {
        s.selected_ip = Some(address.ip().to_string());
        s.stages.dns.observations["selected_ip"] = json!(address.ip().to_string());
        s.stages.dns.observations["addresses"] = json!(
            addresses
                .iter()
                .map(|a| a.ip().to_string())
                .collect::<Vec<_>>()
        );
        s.stages.dns.observations["untested_addresses"] = json!(
            addresses
                .iter()
                .skip(1)
                .map(|a| a.ip().to_string())
                .collect::<Vec<_>>()
        );
        s.stages.route.status = "running".into();
    });
    let proxy = if let Some(proxy_config) = &config.proxy {
        update(&shared, |s| {
            s.limitations.push("代理模式：路由只观测本机到代理，不是到目标的路由；代理 TCP 与 CONNECT 分开计时，CONNECT 成功不独立证明目标 TCP 时延。TLS 在同一隧道保留原始目标身份校验；不直连回退、不读取环境代理。代理 DNS/CONNECT 各 5 秒。".into());
        });
        match crate::proxy::resolve_proxy(proxy_config, None, &cancel, Duration::from_secs(5)).await
        {
            Ok(proxy) => Some(proxy),
            Err(error) => {
                update(&shared, |s| {
                    s.stages.route.status = "skipped".into();
                    s.stages.route.observations =
                        json!({"subject":"proxy","reason":"Proxy resolution did not complete"});
                    s.stages.tcp.status = if cancel.is_cancelled() {
                        "cancelled"
                    } else {
                        "error"
                    }
                    .into();
                    s.stages.tcp.error = Some(error.to_string());
                    s.stages.tcp.observations = json!({"method":"explicit HTTP CONNECT","phase":"proxy_resolution","target_tcp_independently_observed":false});
                });
                // Shared resolution cannot interrupt OS DNS. Keep diagnostics
                // reserved until all outstanding proxy lookups drain rather
                // than claiming cancellation ended a resolver thread.
                if crate::proxy::proxy_dns_pending() > 0 {
                    update(&shared, |s| s.status = "draining".into());
                    let drain = async {
                        while crate::proxy::proxy_dns_pending() > 0 {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    };
                    tokio::pin!(drain);
                    let ended = tokio::select! {
                        _ = &mut drain => true,
                        _ = shutdown.cancelled() => tokio::time::timeout(Duration::from_secs(2), &mut drain).await.is_ok(),
                    };
                    if !ended {
                        update(&shared, |s| {
                            s.warnings.push("Proxy OS DNS has not ended after the 2s shutdown grace; its thread remains alive, not cancelled".into())
                        });
                    }
                }
                finish(
                    &shared,
                    if cancel.is_cancelled() {
                        "cancelled"
                    } else {
                        "failed"
                    },
                );
                return;
            }
        }
    } else {
        None
    };
    let route_address = proxy
        .as_ref()
        .map(|proxy| proxy.addresses()[0])
        .unwrap_or(address);
    // Self-tests and local proxies never invoke route tools against loopback.
    let mut route = if route_address.ip().is_loopback() {
        DiagnosticStage {
            status: "skipped".into(),
            observations: json!({"reason":"Owned local self-test: route not applicable"}),
            ..Default::default()
        }
    } else {
        path::trace(route_address.ip(), &cancel).await
    };
    if proxy.is_some() {
        route.observations["subject"] = json!("proxy");
        route.observations["proxy_addr"] = json!(route_address);
        route.observations["target_route_observed"] = json!(false);
        if route_address.ip().is_loopback() {
            route.observations["reason"] = json!("Local proxy endpoint: route not applicable");
        }
    }
    update(&shared, |s| s.stages.route = route);
    if cancel.is_cancelled() {
        finish(&shared, "cancelled");
        return;
    }
    transport::probe(&shared, &config, address, proxy.as_ref(), &cancel).await;
    finish(
        &shared,
        if cancel.is_cancelled() {
            "cancelled"
        } else {
            "completed"
        },
    );
}
