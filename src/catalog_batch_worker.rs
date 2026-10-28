//! Bounded round-robin dispatch. No socket driver or request task is detached.
use crate::{
    catalog_batch::{Attempt, Mode, Snapshot, Target},
    policy::{self, Cooldowns},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, net::TcpStream, time::Instant};
use tokio_util::sync::CancellationToken;

pub(crate) struct Worker {
    pub snapshot: Arc<Mutex<Snapshot>>,
    pub targets: Vec<Target>,
    pub cooldowns: Arc<Mutex<Cooldowns>>,
    pub path: PathBuf,
    pub control_port: Option<u16>,
    pub cancel: CancellationToken,
    pub app_shutdown: CancellationToken,
    #[cfg(test)]
    pub test_dns: Option<Arc<TestDns>>,
    #[cfg(test)]
    pub test_proxy_dns: Option<Arc<TestDns>>,
}

/// Test-only uninterruptible resolver thread. Its pending count remains live
/// after the receiving future is cancelled, just like the production OS lookup.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestDns {
    pub pending: std::sync::atomic::AtomicUsize,
    pub timeout: Mutex<Option<Duration>>,
    released: Mutex<bool>,
    wake: std::sync::Condvar,
}
#[cfg(test)]
impl TestDns {
    pub fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
    async fn resolve(self: &Arc<Self>) -> Result<Vec<std::net::SocketAddr>, String> {
        use std::sync::atomic::Ordering;
        self.pending.fetch_add(1, Ordering::SeqCst);
        let resolver = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let mut released = resolver.released.lock().unwrap();
            while !*released {
                released = resolver.wake.wait(released).unwrap();
            }
            resolver.pending.fetch_sub(1, Ordering::SeqCst);
            let _ = send.send(Err("test resolver released".to_string()));
        });
        let timeout = *self.timeout.lock().unwrap();
        if let Some(timeout) = timeout {
            tokio::time::timeout(timeout, receive)
                .await
                .map_err(|_| "DNS 超时".to_string())?
                .map_err(|e| e.to_string())?
        } else {
            receive.await.map_err(|e| e.to_string())?
        }
    }
}
struct Activity {
    snapshot: Arc<Mutex<Snapshot>>,
    site: usize,
    detail: usize,
    socket: bool,
    connecting: bool,
    connected: bool,
    held_since: Option<Instant>,
}
impl Activity {
    fn phase(&mut self, phase: &str) {
        let mut s = self.snapshot.lock().unwrap();
        s.attempts[self.detail].phase = phase.into();
        if phase == "connecting" {
            self.socket = true;
            self.connecting = true;
            s.counts.connection_permits += 1;
            s.counts.connecting += 1;
            s.counts.peak_connection_permits = s
                .counts
                .peak_connection_permits
                .max(s.counts.connection_permits);
        }
        if phase == "holding" {
            self.connecting = false;
            self.connected = true;
            self.held_since = Some(Instant::now());
            s.counts.connecting -= 1;
            s.counts.connected += 1;
            s.counts.peak_connected = s.counts.peak_connected.max(s.counts.connected);
            let site = &mut s.sites[self.site];
            site.tcp_connected += 1;
            site.connected += 1;
            site.peak_connected = site.peak_connected.max(site.connected);
        }
        if phase == "http" && self.connecting {
            self.connecting = false;
            s.counts.connecting -= 1;
        }
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        let mut s = self.snapshot.lock().unwrap();
        s.counts.active -= 1;
        if self.socket {
            s.counts.connection_permits -= 1;
        }
        if self.connecting {
            s.counts.connecting -= 1;
        }
        if self.connected {
            s.counts.connected -= 1;
            s.sites[self.site].connected -= 1;
            s.attempts[self.detail].hold_ms =
                self.held_since.map(|t| t.elapsed().as_secs_f64() * 1000.0);
        }
    }
}
#[derive(Default)]
struct ResultData {
    status: Option<u16>,
    hold_ms: Option<f64>,
    outcome: &'static str,
}
trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

impl Worker {
    fn dns_pending(&self) -> usize {
        #[cfg(test)]
        if self.test_dns.is_some() || self.test_proxy_dns.is_some() {
            return self
                .test_dns
                .iter()
                .chain(self.test_proxy_dns.iter())
                .map(|r| r.pending.load(std::sync::atomic::Ordering::SeqCst))
                .sum();
        }
        policy::target_dns_pending() + crate::proxy::proxy_dns_pending()
    }
    async fn resolve(&self, target: &Target) -> Result<Vec<std::net::SocketAddr>, String> {
        #[cfg(test)]
        if let Some(resolver) = &self.test_dns {
            return resolver.resolve().await;
        }
        policy::resolve(&target.url, target.permit.as_ref(), self.control_port).await
    }
    fn expired(&self) -> bool {
        self.targets
            .iter()
            .filter_map(|t| t.permit.as_ref())
            .any(|p| {
                chrono::DateTime::parse_from_rfc3339(&p.expires_at)
                    .map_or(true, |v| v <= chrono::Utc::now())
            })
    }
    // Called only on this scheduler, before execution admission and without holding
    // an execution/socket slot. All sites bound to a permit share these clocks.
    fn ready(
        &self,
        site: usize,
        next_global: Instant,
        permit_next: &HashMap<String, Instant>,
        active: &[usize],
    ) -> bool {
        let s = self.snapshot.lock().unwrap();
        if Instant::now() < next_global
            || s.counts.active >= s.config.concurrency.min(s.config.connection_limit)
        {
            return false;
        }
        if s.config.mode == Mode::Tcp && active[site] >= s.config.per_site_connections.unwrap() {
            return false;
        }
        drop(s);
        let target = &self.targets[site];
        if policy::host(&target.url)
            .parse::<std::net::IpAddr>()
            .is_err()
            && policy::target_dns_pending() >= 4
        {
            return false;
        }
        if let Some(p) = &target.permit {
            if permit_next.get(&p.id).is_some_and(|t| Instant::now() < *t) {
                return false;
            }
            let n: usize = self
                .targets
                .iter()
                .enumerate()
                .filter(|(_, t)| t.permit.as_ref().is_some_and(|v| v.id == p.id))
                .map(|(i, _)| active[i])
                .sum();
            n < p.max_concurrency
        } else {
            self.cooldowns
                .lock()
                .unwrap()
                .ready(&policy::domain_key(&target.url))
        }
    }
    fn reserve(&self, site: usize) -> Result<(), String> {
        if self.targets[site].permit.is_none() {
            let key = policy::domain_key(&self.targets[site].url);
            let mut cooldowns = self.cooldowns.lock().unwrap();
            if !cooldowns.ready(&key) {
                return Err("public cooldown changed before reservation".into());
            }
            cooldowns
                .reserve_dispatch(&key, &self.path)
                .map_err(|e| format!("cooldown persistence failed: {e}"))?;
        }
        Ok(())
    }
    fn observe_headers(
        &self,
        site: usize,
        status: u16,
        headers: &hyper::HeaderMap,
    ) -> Result<(), String> {
        let target = &self.targets[site];
        if target.permit.is_none() && matches!(status, 429 | 503) {
            let seconds = policy::retry_after(
                headers
                    .get(hyper::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
            );
            self.cooldowns
                .lock()
                .unwrap()
                .reserve(
                    &policy::domain_key(&target.url),
                    Cooldowns::now().saturating_add(seconds.saturating_mul(1000)),
                    &self.path,
                )
                .map_err(|e| format!("cooldown persistence failed: {e}"))?;
        }
        Ok(())
    }
    async fn http(
        &self,
        stream: TcpStream,
        site: usize,
        activity: &mut Activity,
    ) -> Result<ResultData, String> {
        use tokio_rustls::{
            TlsConnector,
            rustls::{self, ClientConfig, RootCertStore, pki_types::ServerName},
        };
        let target = &self.targets[site];
        let url = &target.url;
        let io: Box<dyn Io> = if url.scheme() == "https" {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let mut config = ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let name = ServerName::try_from(policy::host(url)).map_err(|e| e.to_string())?;
            Box::new(
                TlsConnector::from(Arc::new(config))
                    .connect(name, stream)
                    .await
                    .map_err(|e| format!("TLS: {e}"))?,
            )
        } else {
            Box::new(stream)
        };
        activity.phase("http");
        let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
            .max_headers(64)
            .max_buf_size(16 * 1024)
            .handshake(hyper_util::rt::TokioIo::new(io))
            .await
            .map_err(|e| format!("HTTP handshake: {e}"))?;
        let authority = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap()),
            None => url.host_str().unwrap().to_owned(),
        };
        let request = hyper::Request::builder()
            .method("HEAD")
            .uri(&url[url::Position::BeforePath..url::Position::AfterQuery])
            .header(hyper::header::HOST, authority)
            .header(hyper::header::CONNECTION, "close")
            .header(
                hyper::header::USER_AGENT,
                concat!("AoNet/", env!("CARGO_PKG_VERSION"), " catalog-batch"),
            )
            .body(http_body_util::Empty::<hyper::body::Bytes>::new())
            .map_err(|e| e.to_string())?;
        // Both futures are owned here. Poll the response FIRST when a final HEAD
        // and EOF arrive together. Dropping this stack closes the actual IO before
        // the Activity/socket permit is released; there is no spawned driver.
        let response = {
            let response = sender.send_request(request);
            tokio::pin!(response);
            tokio::pin!(connection);
            tokio::select! { biased;
                result = &mut response => result.map_err(|e| format!("HTTP: {e}"))?,
                result = &mut connection => {
                    result.map_err(|e| format!("HTTP driver: {e}"))?;
                    response.await.map_err(|e| format!("HTTP: {e}"))?
                }
            }
        };
        let status = response.status().as_u16();
        if let Err(error) = self.observe_headers(site, status, response.headers()) {
            self.snapshot.lock().unwrap().error = Some(error);
        }
        // Never read the response body or redirect/retry. Every response is a
        // transport observation, including 4xx/5xx; not a business success.
        Ok(ResultData {
            status: Some(status),
            outcome: "http_response",
            ..Default::default()
        })
    }
    async fn attempt(
        &self,
        site: usize,
        detail: usize,
        batch_deadline: Instant,
        proxy: Option<&crate::proxy::ResolvedProxy>,
    ) -> (usize, usize, Result<ResultData, String>, f64) {
        let started = Instant::now();
        let mut activity = Activity {
            snapshot: self.snapshot.clone(),
            site,
            detail,
            socket: false,
            connecting: false,
            connected: false,
            held_since: None,
        };
        let config = self.snapshot.lock().unwrap().config.clone();
        let request_deadline = started + Duration::from_secs(config.request_timeout_secs);
        let operation = async {
            let target = &self.targets[site];
            activity.phase("resolving");
            let addrs = tokio::time::timeout_at(request_deadline, self.resolve(target))
                .await
                .map_err(|_| "attempt timed out")??;
            activity.phase("connecting");
            self.snapshot.lock().unwrap().attempts[detail].target_ip =
                Some(addrs[0].ip().to_string());
            let mut stream = if let Some(proxy) = proxy {
                let tunnel = crate::proxy::connect_resolved_proxy_tunnel(
                    proxy,
                    addrs[0],
                    &self.cancel,
                    request_deadline.saturating_duration_since(Instant::now()),
                )
                .await
                .map_err(|e| e.to_string())?;
                {
                    let mut s = self.snapshot.lock().unwrap();
                    let attempt = &mut s.attempts[detail];
                    attempt.connected_ip = Some(tunnel.proxy_addr.ip().to_string());
                    attempt.proxy_ip = attempt.connected_ip.clone();
                    attempt.proxy_tcp_ms = Some(tunnel.proxy_tcp_duration.as_secs_f64() * 1000.0);
                    attempt.tunnel_ms = Some(tunnel.tunnel_duration.as_secs_f64() * 1000.0);
                    attempt.latency_kind = if config.mode == Mode::Tcp {
                        "proxy_tunnel_establishment"
                    } else {
                        "proxied_http_headers"
                    }
                    .into();
                }
                tunnel.stream
            } else {
                let stream =
                    tokio::time::timeout_at(request_deadline, TcpStream::connect(addrs[0]))
                        .await
                        .map_err(|_| "attempt timed out")?
                        .map_err(|e| format!("TCP connect: {e}"))?;
                let mut s = self.snapshot.lock().unwrap();
                s.attempts[detail].connected_ip = Some(addrs[0].ip().to_string());
                s.attempts[detail].latency_kind = if config.mode == Mode::Tcp {
                    "direct_connection_establishment"
                } else {
                    "direct_http_headers"
                }
                .into();
                stream
            };
            if config.mode == Mode::Tcp {
                self.snapshot.lock().unwrap().attempts[detail].latency_ms =
                    Some(started.elapsed().as_secs_f64() * 1000.0);
                activity.phase("holding");
                let held = Instant::now();
                // Read/discard a bounded buffer only to detect EOF. No payload is
                // ever written, and remote bytes cannot cause unbounded memory.
                let end = held + Duration::from_secs(config.tcp_hold_secs.unwrap() as u64);
                let mut buffer = [0u8; 1024];
                let outcome = loop {
                    tokio::select! { biased;
                        _ = tokio::time::sleep_until(end) => break "tcp_connected",
                        result = stream.read(&mut buffer) => match result {
                            Ok(0) => break "early_closed", Ok(_) => {}, Err(e) => return Err(format!("TCP hold: {e}")),
                        }
                    }
                };
                Ok(ResultData {
                    outcome,
                    hold_ms: Some(held.elapsed().as_secs_f64() * 1000.0),
                    ..Default::default()
                })
            } else {
                tokio::time::timeout_at(request_deadline, self.http(stream, site, &mut activity))
                    .await
                    .map_err(|_| "attempt timed out")?
            }
        };
        let result = tokio::select! { biased;
            // The scheduler cancels siblings at the batch deadline. Preserve the
            // deadline reason instead of misclassifying these as user stops.
            _ = tokio::time::sleep_until(batch_deadline) => Err("batch timed out".into()),
            _ = self.cancel.cancelled() => Err(if Instant::now() >= batch_deadline { "batch timed out" } else { "cancelled" }.into()),
            result = operation => result,
        };
        // The operation future (and all socket/HTTP IO) is dropped before this.
        drop(activity);
        (
            site,
            detail,
            result,
            started.elapsed().as_secs_f64() * 1000.0,
        )
    }
    fn finish(&self, site: usize, detail: usize, result: Result<ResultData, String>, latency: f64) {
        let mut s = self.snapshot.lock().unwrap();
        let tcp = s.config.mode == Mode::Tcp;
        let (outcome, error, status, hold) = match result {
            Ok(v) => (v.outcome, None, v.status, v.hold_ms),
            Err(e) => {
                let outcome = if e == "cancelled" {
                    "cancelled"
                } else if e.contains("timed out") || e == "DNS 超时" {
                    "timed_out"
                } else {
                    "network_error"
                };
                if e.contains("cooldown persistence failed") {
                    s.error = Some(e.clone());
                }
                (outcome, Some(e), None, None)
            }
        };
        let item = &mut s.attempts[detail];
        item.phase = "finished".into();
        item.outcome = Some(outcome.into());
        item.ended_at = Some(chrono::Utc::now().to_rfc3339());
        item.status = status;
        let latency = item.latency_ms.unwrap_or(latency);
        item.latency_ms = Some(latency);
        item.hold_ms = hold.or(item.hold_ms);
        item.error_truncated = error.as_ref().is_some_and(|e| e.len() > 512);
        item.error = error.map(|e| e.chars().take(512).collect());
        let error = item.error.clone();
        s.counts.terminal += 1;
        let site = &mut s.sites[site];
        site.last_status = status;
        site.last_error = error;
        site.last_latency_ms = Some(latency);
        site.latency_samples += 1;
        site.latency_total_ms += latency;
        site.average_latency_ms = Some(site.latency_total_ms / site.latency_samples as f64);
        match outcome {
            "http_response" => {
                site.responses += 1;
                if status.is_some_and(|v| (200..400).contains(&v)) {
                    site.good_status += 1;
                } else if status.is_some_and(|v| v >= 400) {
                    site.bad_status += 1;
                }
            }
            "network_error" => {
                site.network_errors += 1;
                if tcp {
                    site.tcp_failed += 1;
                }
            }
            "timed_out" => site.timed_out += 1,
            "cancelled" => site.cancelled += 1,
            "early_closed" => site.early_closed += 1,
            _ => {}
        }
    }
    pub async fn run(self) {
        let config = self.snapshot.lock().unwrap().config.clone();
        let deadline = Instant::now() + Duration::from_secs(config.batch_timeout_secs);
        let mut next_global = Instant::now();
        let mut permit_next = HashMap::new();
        let mut generated = vec![0usize; self.targets.len()];
        let mut active_sites = vec![0usize; self.targets.len()];
        let mut queued = vec![false; self.targets.len()];
        let mut queue = VecDeque::new();
        let mut cursor = 0;
        let mut final_status = "completed";
        // Admission has already registered a preparing snapshot. Resolve once here,
        // outside the manager lock, so cancellation and real DNS draining stay visible.
        let route = self.snapshot.lock().unwrap().route.clone();
        let proxy_timeout = Duration::from_secs(config.request_timeout_secs.min(5));
        let prepared = async {
            let Some(config) = &route.proxy else {
                return Ok(None);
            };
            #[cfg(test)]
            if let Some(resolver) = &self.test_proxy_dns {
                resolver.resolve().await?;
                return Err("test proxy resolver unexpectedly returned addresses".into());
            }
            let proxy = crate::proxy::resolve_proxy(
                config,
                self.control_port,
                &self.cancel,
                proxy_timeout.min(deadline.saturating_duration_since(Instant::now())),
            )
            .await
            .map_err(|e| e.to_string())?;
            for target in &self.targets {
                if let Some(permit) = &target.permit {
                    let allowed = policy::proxy_route(permit, config, self.control_port)?;
                    policy::validate_proxy_route_addresses(allowed, &proxy)?;
                }
            }
            Ok::<_, String>(Some(proxy))
        };
        let prepared = tokio::select! { biased;
            _ = self.cancel.cancelled() => Err("cancelled".into()),
            _ = tokio::time::sleep_until(deadline) => Err("batch timed out".into()),
            result = prepared => result,
        };
        let proxy = match prepared {
            Ok(proxy) => {
                if !self.cancel.is_cancelled() {
                    self.snapshot.lock().unwrap().status = "running".into();
                }
                proxy
            }
            Err(error) => {
                if !self.cancel.is_cancelled() && Instant::now() < deadline {
                    self.snapshot.lock().unwrap().error = Some(error);
                }
                None
            }
        };
        let mut active = FuturesUnordered::new();
        loop {
            let stopping = self.cancel.is_cancelled()
                || Instant::now() >= deadline
                || self.expired()
                || self.snapshot.lock().unwrap().error.is_some();
            if stopping {
                final_status = if self.snapshot.lock().unwrap().error.is_some() || self.expired() {
                    "failed"
                } else if Instant::now() >= deadline {
                    "timed_out"
                } else {
                    "cancelled"
                };
                if self.expired() {
                    self.snapshot.lock().unwrap().error = Some("bound permit expired".into());
                }
                self.cancel.cancel();
                self.snapshot.lock().unwrap().status = "stopping".into();
                while let Some((site, detail, result, latency)) = active.next().await {
                    active_sites[site] -= 1;
                    self.finish(site, detail, result, latency);
                }
                break;
            }
            // Queue contains only next-attempt descriptors, not an eager Cartesian
            // product. Return last tick's blocked descriptors to the lazy producer;
            // the cursor advances, so even capacity one cannot head-of-line block.
            while let Some(site) = queue.pop_front() {
                queued[site] = false;
            }
            for _ in 0..self.targets.len() {
                if queue.len() >= config.queue_capacity {
                    break;
                }
                let site = cursor;
                cursor = (cursor + 1) % self.targets.len();
                if !queued[site] && generated[site] < config.attempts_per_site {
                    queue.push_back(site);
                    queued[site] = true;
                }
            }
            let mut waiting = 0;
            let candidates: Vec<_> = queue.drain(..).collect();
            for site in candidates {
                queued[site] = false;
                if !self.ready(site, next_global, &permit_next, &active_sites) {
                    waiting += 1;
                    queue.push_back(site);
                    queued[site] = true;
                    continue;
                }
                if let Err(e) = self.reserve(site) {
                    self.snapshot.lock().unwrap().error = Some(e);
                    break;
                }
                let now = Instant::now();
                next_global = now + Duration::from_secs_f64(1.0 / config.start_rate as f64);
                if let Some(p) = &self.targets[site].permit {
                    permit_next.insert(
                        p.id.clone(),
                        now + Duration::from_secs_f64(1.0 / p.max_rps as f64),
                    );
                }
                // Queue production can visit an entire ring in one tick and end
                // at its original cursor. Fairness follows actual admissions,
                // not the producer's last visited site.
                cursor = (site + 1) % self.targets.len();
                generated[site] += 1;
                active_sites[site] += 1;
                let detail = {
                    let mut s = self.snapshot.lock().unwrap();
                    let index = s.attempts.len();
                    let mut item =
                        Attempt::queued(index + 1, s.sites[site].site_id.clone(), generated[site]);
                    item.attempted = true;
                    item.started_at = Some(chrono::Utc::now().to_rfc3339());
                    item.phase = "resolving".into();
                    s.attempts.push(item);
                    s.sites[site].attempted += 1;
                    s.counts.active += 1;
                    // Pending/queue counters are published together below.
                    index
                };
                active.push(self.attempt(site, detail, deadline, proxy.as_ref()));
            }
            {
                let mut s = self.snapshot.lock().unwrap();
                s.counts.queued = queue.len();
                // waiting_admission is a subset of queued, not an extra class.
                s.counts.waiting_admission = waiting.min(queue.len());
                s.counts.pending_generation =
                    s.counts.planned - generated.iter().sum::<usize>() - queue.len();
            }
            if active.is_empty() && generated.iter().all(|n| *n == config.attempts_per_site) {
                if self.snapshot.lock().unwrap().error.is_some() {
                    final_status = "failed";
                }
                break;
            }
            tokio::select! { biased;
                _ = self.cancel.cancelled() => {},
                result = active.next(), if !active.is_empty() => if let Some((site,detail,result,latency)) = result { active_sites[site] -= 1; self.finish(site,detail,result,latency); },
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
        // Normal stop waits for the real resolver. App exit is different: let
        // AppService enforce its bounded DNS grace instead of blocking before it.
        // Keep this snapshot draining (never falsely terminal/idle) on that path.
        if self.dns_pending() != 0 {
            self.snapshot.lock().unwrap().status = "draining".into();
        }
        while self.dns_pending() != 0 {
            tokio::select! { biased;
                _ = self.app_shutdown.cancelled() => {
                    let mut snapshot=self.snapshot.lock().unwrap();
                    snapshot.status="draining".into();
                    snapshot.error=Some("application exiting with OS DNS still draining; resolver was not cancelled".into());
                    return;
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
        let mut s = self.snapshot.lock().unwrap();
        for (site, count) in generated.iter().enumerate() {
            for attempt in count + 1..=config.attempts_per_site {
                let mut item =
                    Attempt::queued(s.attempts.len() + 1, s.sites[site].site_id.clone(), attempt);
                item.phase = "finished".into();
                item.outcome = Some("not_executed".into());
                item.ended_at = Some(chrono::Utc::now().to_rfc3339());
                s.attempts.push(item);
                s.counts.unexecuted += 1;
            }
        }
        s.counts.pending_generation = 0;
        s.counts.queued = 0;
        s.counts.waiting_admission = 0;
        s.status = final_status.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_cooldowns_share_global_clock_and_backoff_is_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cooldowns.json");
        let make_target = |url: &str| Target {
            url: policy::parse_url(url).unwrap(),
            permit: None,
        };
        // This is policy/admission only: no sockets to public targets are opened.
        let request = crate::catalog_batch::Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            catalog_revision: 1,
            group_ids: vec!["g".into()],
            site_ids: vec![],
            resource_selections: None,
            mode: Mode::PublicHttp,
            permit_bindings: vec![],
            concurrency: 4,
            connection_limit: 4,
            queue_capacity: 1,
            attempts_per_site: 1,
            start_rate: 2,
            request_timeout_secs: 1,
            batch_timeout_secs: 1,
            per_site_connections: None,
            tcp_hold_secs: None,
            confirmed_authorized: false,
        };
        let snapshot = Snapshot {
            request_id: request.request_id.clone(),
            config: request,
            route: Default::default(),
            status: "running".into(),
            sites: vec![],
            attempts: vec![],
            counts: Default::default(),
            error: None,
            details_retained: true,
        };
        let worker = Worker {
            snapshot: Arc::new(Mutex::new(snapshot)),
            targets: vec![
                make_target("https://a.example.com/"),
                make_target("https://b.example.com/"),
                make_target("https://other.example.net/"),
            ],
            cooldowns: Arc::new(Mutex::new(Cooldowns::default())),
            path: path.clone(),
            control_port: None,
            cancel: CancellationToken::new(),
            app_shutdown: CancellationToken::new(),
            test_dns: None,
            test_proxy_dns: None,
        };
        worker.reserve(0).unwrap();
        assert!(!worker.ready(1, Instant::now(), &HashMap::new(), &[0, 0, 0]));
        assert!(!worker.ready(2, Instant::now(), &HashMap::new(), &[0, 0, 0]));
        let mut headers = hyper::HeaderMap::new();
        headers.insert(hyper::header::RETRY_AFTER, "120".parse().unwrap());
        worker.observe_headers(0, 429, &headers).unwrap();
        assert!(!Cooldowns::load(&path).unwrap().ready("example.com"));
        let before = std::fs::read(&path).unwrap();
        worker.observe_headers(1, 503, &headers).unwrap();
        assert!(!before.is_empty());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            worker
                .observe_headers(0, 429, &headers)
                .unwrap_err()
                .contains("persistence")
        );
    }
}
