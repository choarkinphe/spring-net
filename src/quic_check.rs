//! One authenticated QUIC handshake to one prevalidated address. The caller owns
//! DNS/address policy and proxy admission; this module never resolves or retries
//! another address. No production loopback exemption is provided here.
use std::{
    future::Future,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use quinn::{
    AsyncTimer, AsyncUdpSocket, Runtime, UdpPoller,
    udp::{RecvMeta, Transmit},
};
use tokio::{net::UdpSocket, task::JoinHandle, time::Instant};
use tokio_rustls::rustls::{self, RootCertStore};
use tokio_util::sync::CancellationToken;

use crate::network_checks::CheckResult;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
const CLEANUP_GRACE: Duration = Duration::from_millis(100);
const SEND_BYTES: usize = 64 * 1024;
const SEND_DATAGRAMS: usize = 64;
const RECEIVE_BYTES: usize = 256 * 1024;
const RECEIVE_DATAGRAMS: usize = 256;
const MAX_DATAGRAM: usize = 2048;
const SCOPE: &str = "仅 QUIC/TLS 握手，不代表 HTTP/3、登录或业务可用；正常证书链/名称校验，Mozilla 根（非系统/企业根）；无 0-RTT、会话复用、应用 stream/datagram、keepalive 或地址迁移；协议可能重传/Retry/发送握手控制包，非单 UDP 包测试";

fn result(status: &'static str, detail: impl AsRef<str>) -> CheckResult {
    let output = format!("{}\n{SCOPE}", detail.as_ref());
    CheckResult {
        status,
        summary: output.clone(),
        output,
    }
}

fn client_config(alpn: &str, roots: RootCertStore) -> Result<quinn::ClientConfig, String> {
    if alpn.is_empty() || alpn.len() > 255 || !alpn.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("ALPN 必须是 1–255 字节非空可见 ASCII 单个协议标识".into());
    }
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| e.to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![alpn.as_bytes().to_vec()];
    tls.enable_early_data = false;
    tls.resumption = rustls::client::Resumption::disabled();
    let crypto =
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(|e| e.to_string())?;
    let mut transport = quinn::TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(0u32.into())
        .max_concurrent_uni_streams(0u32.into())
        .stream_receive_window(0u32.into())
        .receive_window(0u32.into())
        .send_window(0)
        .datagram_receive_buffer_size(None)
        .datagram_send_buffer_size(0)
        .keep_alive_interval(None)
        .mtu_discovery_config(None)
        .initial_mtu(1200)
        .max_idle_timeout(Some(
            HANDSHAKE_TIMEOUT
                .try_into()
                .map_err(|e: quinn::VarIntBoundsExceeded| e.to_string())?,
        ));
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport));
    Ok(config)
}

/// Quinn 0.11.11 spawns the endpoint driver and the single connection driver
/// synchronously in Endpoint::new / connect_with. Retain every JoinHandle, not
/// just abort handles: awaiting an aborted task proves its future was dropped.
#[derive(Debug, Default)]
struct TrackedRuntime {
    tasks: Mutex<(bool, Vec<JoinHandle<()>>)>,
}
impl Runtime for TrackedRuntime {
    fn new_timer(&self, at: std::time::Instant) -> Pin<Box<dyn AsyncTimer>> {
        quinn::TokioRuntime.new_timer(at)
    }
    fn now(&self) -> std::time::Instant {
        quinn::TokioRuntime.now()
    }
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        let mut state = self.tasks.lock().unwrap();
        // Sealing also makes cleanup robust to any future Quinn spawn site.
        if !state.0 {
            state.1.push(tokio::spawn(future));
        }
    }
    fn wrap_udp_socket(&self, _: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Err(io::Error::other(
            "QUIC probe requires its fixed-peer socket",
        ))
    }
}
impl TrackedRuntime {
    async fn shutdown(&self) -> bool {
        let mut tasks = {
            let mut state = self.tasks.lock().unwrap();
            state.0 = true;
            std::mem::take(&mut state.1)
        };
        let until = Instant::now() + CLEANUP_GRACE;
        let mut forced = false;
        for task in &mut tasks {
            if tokio::time::timeout_at(until, &mut *task).await.is_err() {
                forced = true;
                task.abort();
                // No timeout here: do not publish a terminal state until the
                // runtime has actually destroyed the driver future.
                let _ = task.await;
            }
        }
        forced
    }
}

#[derive(Debug, Default)]
struct Budget {
    sent_bytes: usize,
    sent_datagrams: usize,
    received_bytes: usize,
    received_datagrams: usize,
    violation: Option<&'static str>,
}

/// A connected OS UDP socket enforces the fixed peer on receive as well as
/// send. The destination check additionally prevents preferred-address/path
/// traffic even if the protocol stack tries to send it. GSO/GRO are not exposed.
#[derive(Debug)]
struct ProbeSocket {
    socket: UdpSocket,
    peer: SocketAddr,
    budget: Mutex<Budget>,
    violated: CancellationToken,
    // Fields drop in declaration order: signal only AFTER the OS socket closes.
    _released: SocketReleased,
}
#[derive(Debug)]
struct SocketReleased(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for SocketReleased {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl ProbeSocket {
    fn fail(&self, budget: &mut Budget, reason: &'static str) -> io::Error {
        budget.violation.get_or_insert(reason);
        self.violated.cancel();
        io::Error::other(reason)
    }
}
impl AsyncUdpSocket for ProbeSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(ProbePoller(self))
    }
    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        let mut budget = self.budget.lock().unwrap();
        if transmit.destination != self.peer || transmit.segment_size.is_some() {
            return Err(self.fail(&mut budget, "QUIC 拒绝地址迁移/分段发送"));
        }
        if budget.sent_datagrams >= SEND_DATAGRAMS
            || transmit.contents.len() > SEND_BYTES.saturating_sub(budget.sent_bytes)
        {
            return Err(self.fail(&mut budget, "QUIC 发送预算耗尽（64 KiB / 64 数据报）"));
        }
        let n = self.socket.try_send(transmit.contents)?;
        budget.sent_bytes += n;
        budget.sent_datagrams += 1;
        Ok(())
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let mut budget = self.budget.lock().unwrap();
        if budget.received_datagrams >= RECEIVE_DATAGRAMS || budget.received_bytes >= RECEIVE_BYTES
        {
            return Poll::Ready(Err(
                self.fail(&mut budget, "QUIC 接收预算耗尽（256 KiB / 256 数据报）")
            ));
        }
        // The extra byte detects oversize datagrams even when the OS truncates.
        let mut data = [0u8; MAX_DATAGRAM + 1];
        let mut buf = tokio::io::ReadBuf::new(&mut data);
        match self.socket.poll_recv(cx, &mut buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {
                let n = buf.filled().len();
                budget.received_datagrams += 1;
                budget.received_bytes += n;
                if n > MAX_DATAGRAM
                    || budget.received_bytes > RECEIVE_BYTES
                    || bufs.is_empty()
                    || meta.is_empty()
                    || n > bufs[0].len()
                {
                    return Poll::Ready(Err(
                        self.fail(&mut budget, "QUIC 接收数据报超出有界缓冲/预算")
                    ));
                }
                bufs[0][..n].copy_from_slice(buf.filled());
                meta[0] = RecvMeta {
                    addr: self.peer,
                    len: n,
                    stride: n,
                    ecn: None,
                    dst_ip: None,
                };
                Poll::Ready(Ok(1))
            }
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}
#[derive(Debug)]
struct ProbePoller(Arc<ProbeSocket>);
impl UdpPoller for ProbePoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.socket.poll_send_ready(cx)
    }
}

pub(crate) async fn check(
    host: &str,
    address: SocketAddr,
    alpn: &str,
    cancel: &CancellationToken,
) -> CheckResult {
    check_with_roots(
        host,
        address,
        alpn,
        cancel,
        RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    )
    .await
}

async fn check_with_roots(
    host: &str,
    address: SocketAddr,
    alpn: &str,
    cancel: &CancellationToken,
    roots: RootCertStore,
) -> CheckResult {
    let started = Instant::now();
    if cancel.is_cancelled() {
        return result("cancelled", "QUIC 已停止；未创建 socket");
    }
    let config = match client_config(alpn, roots) {
        Ok(config) => config,
        Err(e) => return result("error", format!("QUIC 配置失败：{e}")),
    };
    let socket = match std::net::UdpSocket::bind(if address.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .and_then(|s| {
        s.connect(address)?;
        s.set_nonblocking(true)?;
        Ok(s)
    })
    .and_then(UdpSocket::from_std)
    {
        Ok(socket) => socket,
        Err(e) => return result("error", format!("QUIC UDP socket 创建失败：{e}")),
    };
    let violated = CancellationToken::new();
    let (released_tx, released_rx) = tokio::sync::oneshot::channel();
    let socket = Arc::new(ProbeSocket {
        socket,
        peer: address,
        budget: Mutex::new(Budget::default()),
        violated: violated.clone(),
        _released: SocketReleased(Some(released_tx)),
    });
    let runtime = Arc::new(TrackedRuntime::default());
    let endpoint = match quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        socket,
        runtime.clone(),
    ) {
        Ok(endpoint) => endpoint,
        Err(e) => {
            runtime.shutdown().await;
            let _ = released_rx.await;
            return result("error", format!("QUIC endpoint 创建失败：{e}"));
        }
    };
    let (mut status, mut detail) = match endpoint.connect_with(config, address, host) {
        Err(e) => ("error", format!("QUIC 无法发起握手：{e}")),
        Ok(connecting) => {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => ("cancelled", "QUIC 已停止".into()),
                _ = violated.cancelled() => ("error", "QUIC socket 预算/固定地址限制触发".into()),
                _ = tokio::time::sleep_until(started + HANDSHAKE_TIMEOUT) => ("timeout", "QUIC 握手超时（8 秒）；不等于 UDP 端口关闭".into()),
                connected = connecting => match connected {
                    Err(e) => ("error", format!("QUIC 握手失败：{e}")),
                    Ok(connection) => {
                        let matches = connection.handshake_data()
                            .and_then(|d| d.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
                            .is_some_and(|d| d.protocol.as_deref() == Some(alpn.as_bytes()));
                        connection.close(0u32.into(), b"handshake probe finished");
                        drop(connection);
                        if matches { ("completed", format!("QUIC 握手成功：{address}；身份 {host}；ALPN {alpn}；{:.2} ms", started.elapsed().as_secs_f64() * 1000.0)) }
                        else { ("error", "QUIC 协商 ALPN 不匹配或缺失".into()) }
                    }
                }
            }
        }
    };
    endpoint.close(0u32.into(), b"handshake probe finished");
    drop(endpoint);
    let forced = runtime.shutdown().await;
    // The witness fires from ProbeSocket::drop, after its UdpSocket field is
    // dropped. It therefore proves OS socket release (not merely wait_idle).
    if tokio::time::timeout(CLEANUP_GRACE, released_rx)
        .await
        .is_err()
    {
        // This is an invariant failure, not a success/timeout result: preserve
        // the worker's draining state rather than publishing a false terminal.
        std::future::pending::<()>().await;
    }
    if violated.is_cancelled() {
        status = "error";
        detail.push_str("；固定地址/收发预算限制触发");
    }
    detail.push_str(if forced {
        "；清理宽限到期，已强制本地关闭并 await 全部 driver，socket 已释放"
    } else {
        "；全部 driver 已退出，socket 已释放"
    });
    result(status, detail)
}

#[cfg(test)]
#[path = "quic_check_tests.rs"]
mod tests;
