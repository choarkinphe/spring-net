use super::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

fn roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            include_bytes!("diagnostics/fixtures/ca.der").to_vec(),
        ))
        .unwrap();
    roots
}
fn server(alpn: &[u8]) -> quinn::Endpoint {
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(
            include_bytes!("diagnostics/fixtures/server.der").to_vec(),
        )],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            include_bytes!("diagnostics/fixtures/server-key.der").to_vec(),
        )),
    )
    .unwrap();
    tls.alpn_protocols = vec![alpn.to_vec()];
    tls.max_early_data_size = 0;
    tls.send_tls13_tickets = 0;
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    config.migration(false);
    quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap()
}

async fn successful(host: &str) {
    let endpoint = server(b"aonet-probe");
    let address = endpoint.local_addr().unwrap();
    let fixture = endpoint.clone();
    let task = tokio::spawn(async move {
        let connection = fixture.accept().await.unwrap().await.unwrap();
        let handshake = connection
            .handshake_data()
            .unwrap()
            .downcast::<quinn::crypto::rustls::HandshakeData>()
            .unwrap();
        assert_eq!(handshake.protocol.as_deref(), Some(&b"aonet-probe"[..]));
        assert!(!connection.send_datagram(Vec::new().into()).is_ok());
        connection.closed().await;
        let stats = connection.stats();
        assert_eq!(
            stats.frame_rx.stream, 0,
            "probe must not send application stream bytes"
        );
        assert_eq!(
            stats.frame_rx.datagram, 0,
            "probe must not send application datagrams"
        );
        assert_eq!(stats.frame_rx.path_challenge, 0, "probe must not migrate");
        assert_eq!(fixture.stats().accepted_handshakes, 1);
    });
    let checked = check_with_roots(
        host,
        address,
        "aonet-probe",
        &CancellationToken::new(),
        roots(),
    )
    .await;
    assert_eq!(checked.status, "completed", "{}", checked.output);
    assert!(checked.output.contains("socket 已释放"));
    assert!(checked.output.contains("ALPN aonet-probe"));
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    endpoint.close(0u32.into(), b"done");
}

#[tokio::test]
async fn success_has_exact_alpn_no_application_traffic_and_releases_resources() {
    successful("diagnostic.test").await;
    successful("127.0.0.1").await;
}

#[tokio::test]
async fn certificate_name_and_trust_are_not_bypassed() {
    for (host, trust) in [
        ("wrong.test", roots()),
        (
            "diagnostic.test",
            RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        ),
    ] {
        let endpoint = server(b"aonet-probe");
        let fixture = endpoint.clone();
        let task = tokio::spawn(async move {
            let _ = fixture.accept().await.unwrap().await;
        });
        let checked = check_with_roots(
            host,
            endpoint.local_addr().unwrap(),
            "aonet-probe",
            &CancellationToken::new(),
            trust,
        )
        .await;
        assert_eq!(checked.status, "error", "{}", checked.output);
        assert!(checked.output.contains("socket 已释放"));
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        endpoint.close(0u32.into(), b"done");
    }
}

#[tokio::test]
async fn alpn_mismatch_fails_and_releases_resources() {
    let endpoint = server(b"other-protocol");
    let fixture = endpoint.clone();
    let task = tokio::spawn(async move {
        let _ = fixture.accept().await.unwrap().await;
    });
    let checked = check_with_roots(
        "diagnostic.test",
        endpoint.local_addr().unwrap(),
        "aonet-probe",
        &CancellationToken::new(),
        roots(),
    )
    .await;
    assert_eq!(checked.status, "error", "{}", checked.output);
    assert!(checked.output.contains("socket 已释放"));
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    endpoint.close(0u32.into(), b"done");
}

#[tokio::test(start_paused = true)]
async fn no_response_times_out_at_eight_seconds_and_reaps_drivers() {
    let blackhole = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = blackhole.local_addr().unwrap();
    let started = Instant::now();
    let checked = check(
        "diagnostic.test",
        address,
        "aonet-probe",
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(checked.status, "timeout", "{}", checked.output);
    assert!(started.elapsed() >= HANDSHAKE_TIMEOUT);
    assert!(started.elapsed() <= HANDSHAKE_TIMEOUT + Duration::from_millis(200));
    assert!(checked.output.contains("socket 已释放"));
    let mut data = [0u8; 4096];
    let (_, client) = blackhole.recv_from(&mut data).await.unwrap();
    // A bound port can be immediately reused only after the client's real OS
    // socket is closed; no SO_REUSEADDR is enabled on either socket.
    let rebound = std::net::UdpSocket::bind(client).unwrap();
    assert_eq!(rebound.local_addr().unwrap(), client);
}

#[tokio::test]
async fn cancellation_after_initial_packet_releases_port_before_return() {
    let blackhole = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = blackhole.local_addr().unwrap();
    let cancel = CancellationToken::new();
    let child = cancel.clone();
    let task =
        tokio::spawn(async move { check("diagnostic.test", address, "aonet-probe", &child).await });
    let mut data = [0u8; 4096];
    let (_, client) = tokio::time::timeout(Duration::from_secs(2), blackhole.recv_from(&mut data))
        .await
        .unwrap()
        .unwrap();
    cancel.cancel();
    let checked = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checked.status, "cancelled", "{}", checked.output);
    assert!(checked.output.contains("socket 已释放"));
    std::net::UdpSocket::bind(client).unwrap();
}

#[tokio::test]
async fn pre_cancel_and_invalid_alpn_send_nothing() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = peer.local_addr().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        check("diagnostic.test", address, "aonet-probe", &cancel)
            .await
            .status,
        "cancelled"
    );
    for alpn in ["", "two protocols", "\n", &"x".repeat(256)] {
        assert_eq!(
            check("diagnostic.test", address, alpn, &CancellationToken::new())
                .await
                .status,
            "error"
        );
    }
    let mut data = [0u8; 4096];
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.recv_from(&mut data))
            .await
            .is_err()
    );
}

async fn probe_socket(peer: SocketAddr) -> (Arc<ProbeSocket>, tokio::sync::oneshot::Receiver<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(peer).await.unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    (
        Arc::new(ProbeSocket {
            socket,
            peer,
            budget: Mutex::new(Budget::default()),
            violated: CancellationToken::new(),
            _released: SocketReleased(Some(tx)),
        }),
        rx,
    )
}
fn transmit(peer: SocketAddr, data: &[u8]) -> Transmit<'_> {
    Transmit {
        destination: peer,
        contents: data,
        ecn: None,
        segment_size: None,
        src_ip: None,
    }
}

#[tokio::test]
async fn different_ip_or_port_is_rejected_without_a_packet() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (socket, released) = probe_socket(peer.local_addr().unwrap()).await;
    for destination in [
        "127.0.0.2:12345".parse().unwrap(),
        "127.0.0.1:12345".parse().unwrap(),
    ] {
        assert!(
            socket
                .try_send(&transmit(destination, b"must not send"))
                .is_err()
        );
    }
    assert_eq!(socket.budget.lock().unwrap().sent_bytes, 0);
    assert!(socket.violated.is_cancelled());
    drop(socket);
    released.await.unwrap();
}

#[tokio::test]
async fn send_byte_datagram_and_segmentation_budgets_are_enforced() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = peer.local_addr().unwrap();
    let (socket, released) = probe_socket(address).await;
    let mut packet = transmit(address, b"x");
    packet.segment_size = Some(1);
    assert!(socket.try_send(&packet).is_err());
    packet.segment_size = None;
    socket.budget.lock().unwrap().sent_datagrams = SEND_DATAGRAMS;
    assert!(socket.try_send(&packet).is_err());
    {
        let mut budget = socket.budget.lock().unwrap();
        budget.sent_datagrams = 0;
        budget.sent_bytes = SEND_BYTES;
    }
    assert!(socket.try_send(&packet).is_err());
    assert_eq!(socket.budget.lock().unwrap().sent_datagrams, 0);
    drop(socket);
    released.await.unwrap();
}

async fn receive(socket: &ProbeSocket) -> io::Result<usize> {
    let mut bytes = [0u8; 4096];
    let mut bufs = [IoSliceMut::new(&mut bytes)];
    let mut meta = [RecvMeta::default()];
    std::future::poll_fn(|cx| socket.poll_recv(cx, &mut bufs, &mut meta)).await
}

#[tokio::test]
async fn receive_is_connected_bounded_and_detects_oversize() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (socket, released) = probe_socket(peer.local_addr().unwrap()).await;
    let address = socket.local_addr().unwrap();
    stranger.send_to(b"spoofed", address).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), receive(&socket))
            .await
            .is_err()
    );
    peer.send_to(&[0; MAX_DATAGRAM + 100], address)
        .await
        .unwrap();
    assert!(receive(&socket).await.is_err());
    socket.budget.lock().unwrap().received_datagrams = RECEIVE_DATAGRAMS;
    assert!(receive(&socket).await.is_err());
    {
        let mut budget = socket.budget.lock().unwrap();
        budget.received_datagrams = 0;
        budget.received_bytes = RECEIVE_BYTES;
    }
    assert!(receive(&socket).await.is_err());
    drop(socket);
    released.await.unwrap();
}

#[tokio::test]
async fn shutdown_awaits_task_destruction_and_socket_drop_including_poller_owner() {
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for _ in 0..10 {
        let (socket, released) = probe_socket(peer.local_addr().unwrap()).await;
        let local = socket.local_addr().unwrap();
        let runtime = TrackedRuntime::default();
        let poller = socket.clone().create_io_poller();
        runtime.spawn(Box::pin(async move {
            let _owner = (socket, poller);
            std::future::pending::<()>().await;
        }));
        assert!(runtime.shutdown().await);
        released.await.unwrap();
        std::net::UdpSocket::bind(local).unwrap();
        assert!(runtime.tasks.lock().unwrap().1.is_empty());
    }
}
