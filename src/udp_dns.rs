//! A single bounded UDP DNS exchange, not the system resolver or a port scanner.
//! Callers validate/pin the server address. Answer addresses are never connected.
use crate::network_checks::CheckResult;
use hickory_proto::{
    op::{Message, MessageType, OpCode, Query, ResponseCode},
    rr::{DNSClass, Name, RecordType},
};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, time::Duration};
use tokio::{net::UdpSocket, time::Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum QueryType {
    A,
    #[serde(rename = "AAAA")]
    Aaaa,
}
impl QueryType {
    fn record_type(self) -> RecordType {
        match self {
            Self::A => RecordType::A,
            Self::Aaaa => RecordType::AAAA,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub query_name: String,
    pub record_type: QueryType,
}
impl Parameters {
    pub fn normalize(mut self) -> Result<Self, String> {
        let input = self.query_name.trim();
        if input.is_empty()
            || input.len() > 253
            || input.chars().any(|c| c.is_control() || c.is_whitespace())
            || input.contains(['/', ':', '@', '?', '#', '*', '\\', '%'])
        {
            return Err("DNS 查询名须为单个合法域名，不接受网址、通配或控制字符".into());
        }
        let input = input.strip_suffix('.').unwrap_or(input);
        let domain = match url::Host::parse(input).map_err(|_| "DNS 查询名无效")? {
            url::Host::Domain(domain) => domain,
            _ => return Err("DNS 查询名须为域名，不是 IP 地址".into()),
        };
        if domain.len() > 253
            || domain.split('.').any(|s| {
                s.is_empty()
                    || s.len() > 63
                    || s.starts_with('-')
                    || s.ends_with('-')
                    || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err("DNS 查询名标签无效".into());
        }
        self.query_name = domain.to_ascii_lowercase();
        Ok(self)
    }
}
fn result(status: &'static str, text: String) -> CheckResult {
    CheckResult {
        status,
        output: text.clone(),
        summary: text,
    }
}
fn expected_query(parameters: &Parameters) -> Result<Query, String> {
    let name =
        Name::from_ascii(format!("{}.", parameters.query_name)).map_err(|e| e.to_string())?;
    Ok(Query::query(name, parameters.record_type.record_type()))
}
fn response(bytes: &[u8], id: u16, query: &Query) -> Result<Message, String> {
    if bytes.len() < 12 || bytes.len() > 512 {
        return Err("DNS 报文长度不在 12～512 字节内".into());
    }
    let count = |offset| u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
    if count(4) != 1 || u32::from(count(6)) + u32::from(count(8)) + u32::from(count(10)) > 64 {
        return Err("DNS 问题数或记录数超限".into());
    }
    let message = Message::from_vec(bytes).map_err(|e| format!("DNS 报文解析失败：{e}"))?;
    if message.id() != id
        || message.message_type() != MessageType::Response
        || message.op_code() != OpCode::Query
        || message.queries().len() != 1
        || message.queries()[0].name() != query.name()
        || message.queries()[0].query_type() != query.query_type()
        || message.queries()[0].query_class() != DNSClass::IN
    {
        return Err("DNS 响应身份或问题不匹配".into());
    }
    Ok(message)
}

pub(crate) async fn check(
    address: SocketAddr,
    parameters: &Parameters,
    cancel: &CancellationToken,
) -> CheckResult {
    check_with_budget(address, parameters, cancel, Duration::from_secs(5)).await
}
async fn check_with_budget(
    address: SocketAddr,
    parameters: &Parameters,
    cancel: &CancellationToken,
    budget: Duration,
) -> CheckResult {
    let started = Instant::now();
    let deadline = started + budget;
    let parameters = match parameters.clone().normalize() {
        Ok(parameters) => parameters,
        Err(e) => return result("error", e),
    };
    let query = match expected_query(&parameters) {
        Ok(query) => query,
        Err(e) => return result("error", e),
    };
    // UUID v4 uses the OS CSPRNG. DNS requires a 16-bit transaction identity.
    let random = uuid::Uuid::new_v4();
    let id = u16::from_be_bytes([random.as_bytes()[0], random.as_bytes()[1]]);
    let mut message = Message::new();
    message
        .set_id(id)
        .set_message_type(MessageType::Query)
        .set_op_code(OpCode::Query)
        .set_recursion_desired(true)
        .add_query(query.clone());
    let bytes = match message.to_vec() {
        Ok(bytes) if bytes.len() <= 512 => bytes,
        _ => return result("error", "无法生成有界 DNS 查询".into()),
    };
    let operation = async {
        let bind = if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind).await.map_err(|e| e.to_string())?;
        socket.connect(address).await.map_err(|e| e.to_string())?;
        if cancel.is_cancelled() {
            return Err::<String, String>("已停止，未发送查询".into());
        }
        socket.send(&bytes).await.map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 513];
        let mut discarded = 0;
        loop {
            let length = socket.recv(&mut buffer).await.map_err(|e| e.to_string())?;
            match response(&buffer[..length], id, &query) {
                Ok(message) => {
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    let description = if message.truncated() {
                        "响应标记 TC 截断，未回退 TCP".to_owned()
                    } else if message.response_code() == ResponseCode::NoError
                        && message.answers().is_empty()
                    {
                        "NOERROR / NODATA（响应未含答案，不递归追踪）".into()
                    } else {
                        message.response_code().to_string()
                    };
                    let mut output = format!(
                        "UDP DNS 收到匹配响应：{address}；{elapsed:.2} ms\n{} {:?}；{description}\n",
                        parameters.query_name, parameters.record_type
                    );
                    for answer in message.answers().iter().take(64) {
                        output.push_str(&format!("{answer}\n"));
                    }
                    output.push_str(&format!("一次 UDP 查询；已忽略 {discarded} 份无效报文；无重试、无TCP回退、不连接答案地址、不验证DNSSEC；并非应用业务可用性。"));
                    return Ok(output);
                }
                Err(_) => {
                    discarded += 1;
                    if discarded >= 16 {
                        return Err("收到16份无效/超长DNS响应，已停止接收；未重试".into());
                    }
                }
            }
        }
    };
    // The operation owns the socket; cancelling/dropping it closes it before return.
    tokio::select! {
        biased;
        _ = cancel.cancelled() => result("cancelled", "UDP DNS 已停止，socket已释放；未重试".into()),
        _ = tokio::time::sleep_until(deadline) => result("timeout", "UDP DNS 未在期限内收到有效响应；不等于端口关闭，未重试或回退TCP".into()),
        received = operation => match received {
            Ok(text) => result("completed", text),
            Err(e) => result("error", format!("UDP DNS：{e}；socket已释放，不代表所有UDP业务不可达")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::rr::{
        RData, Record,
        rdata::{A, AAAA},
    };
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn query_validation_is_bounded_and_does_not_resolve() {
        let p = Parameters {
            query_name: "EXAMPLE.invalid.".into(),
            record_type: QueryType::A,
        }
        .normalize()
        .unwrap();
        assert_eq!(p.query_name, "example.invalid");
        for name in [
            "",
            "*.invalid",
            "http://example.invalid",
            "a..b",
            "127.0.0.1",
            "a b",
            "a\nb",
        ] {
            assert!(
                Parameters {
                    query_name: name.into(),
                    record_type: QueryType::A
                }
                .normalize()
                .is_err()
            );
        }
    }
    fn reply(query: &Message, code: ResponseCode) -> Message {
        let mut r = Message::new();
        r.set_id(query.id())
            .set_message_type(MessageType::Response)
            .set_op_code(OpCode::Query)
            .set_response_code(code)
            .add_query(query.queries()[0].clone());
        r
    }
    #[tokio::test]
    async fn receives_a_aaaa_and_error_codes_with_one_datagram() {
        for kind in [QueryType::A, QueryType::Aaaa] {
            for code in [
                ResponseCode::NoError,
                ResponseCode::NXDomain,
                ResponseCode::ServFail,
            ] {
                let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let address = server.local_addr().unwrap();
                let worker = tokio::spawn(async move {
                    let mut buf = [0; 513];
                    let (n, peer) = server.recv_from(&mut buf).await.unwrap();
                    let query = Message::from_vec(&buf[..n]).unwrap();
                    assert_eq!(query.queries().len(), 1);
                    assert!(query.extensions().is_none());
                    let mut r = reply(&query, code);
                    if code == ResponseCode::NoError {
                        let data = match kind {
                            QueryType::A => RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
                            QueryType::Aaaa => RData::AAAA(AAAA(Ipv6Addr::LOCALHOST)),
                        };
                        r.add_answer(Record::from_rdata(
                            query.queries()[0].name().clone(),
                            60,
                            data,
                        ));
                    }
                    server.send_to(&r.to_vec().unwrap(), peer).await.unwrap();
                    assert!(
                        tokio::time::timeout(Duration::from_millis(30), server.recv_from(&mut buf))
                            .await
                            .is_err()
                    );
                });
                let p = Parameters {
                    query_name: "fixture.invalid".into(),
                    record_type: kind,
                };
                let response = check(address, &p, &CancellationToken::new()).await;
                assert_eq!(response.status, "completed", "{}", response.output);
                assert!(response.output.contains(&code.to_string()));
                worker.await.unwrap();
            }
        }
    }
    #[tokio::test]
    async fn ignores_wrong_identity_question_and_oversize_before_valid_response() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let worker = tokio::spawn(async move {
            let mut buf = [0; 513];
            let (n, peer) = server.recv_from(&mut buf).await.unwrap();
            let q = Message::from_vec(&buf[..n]).unwrap();
            let mut wrong_id = reply(&q, ResponseCode::NoError);
            wrong_id.set_id(q.id().wrapping_add(1));
            server
                .send_to(&wrong_id.to_vec().unwrap(), peer)
                .await
                .unwrap();
            let mut wrong_question = reply(&q, ResponseCode::NoError);
            wrong_question.queries_mut()[0] =
                Query::query(Name::from_ascii("other.invalid.").unwrap(), RecordType::A);
            server
                .send_to(&wrong_question.to_vec().unwrap(), peer)
                .await
                .unwrap();
            server.send_to(&[0; 700], peer).await.unwrap();
            let mut valid = reply(&q, ResponseCode::NoError);
            valid.set_truncated(true);
            server
                .send_to(&valid.to_vec().unwrap(), peer)
                .await
                .unwrap();
        });
        let p = Parameters {
            query_name: "fixture.invalid".into(),
            record_type: QueryType::A,
        };
        let response = check(address, &p, &CancellationToken::new()).await;
        assert_eq!(response.status, "completed");
        assert!(response.output.contains("TC 截断"));
        assert!(response.output.contains("3 份"));
        worker.await.unwrap();
    }
    #[tokio::test]
    async fn timeout_and_cancellation_do_not_retry() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let p = Parameters {
            query_name: "fixture.invalid".into(),
            record_type: QueryType::A,
        };
        let response = check_with_budget(
            address,
            &p,
            &CancellationToken::new(),
            Duration::from_millis(30),
        )
        .await;
        assert_eq!(response.status, "timeout");
        let mut bytes = [0; 513];
        let (_, peer) = server.recv_from(&mut bytes).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), server.recv_from(&mut bytes))
                .await
                .is_err()
        );
        assert!(UdpSocket::bind(peer).await.is_ok());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(check(address, &p, &cancel).await.status, "cancelled");
    }
    #[tokio::test]
    async fn rejects_wrong_source_and_bounds_invalid_packets() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let other = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let worker = tokio::spawn(async move {
            let mut buf = [0; 513];
            let (n, peer) = server.recv_from(&mut buf).await.unwrap();
            let q = Message::from_vec(&buf[..n]).unwrap();
            let good = reply(&q, ResponseCode::NoError).to_vec().unwrap();
            other.send_to(&good, peer).await.unwrap();
            for _ in 0..16 {
                server.send_to(&[0; 12], peer).await.unwrap();
            }
        });
        let p = Parameters {
            query_name: "fixture.invalid".into(),
            record_type: QueryType::A,
        };
        let response = check(address, &p, &CancellationToken::new()).await;
        assert_eq!(response.status, "error");
        assert!(response.output.contains("16份"));
        worker.await.unwrap();
    }
    #[test]
    fn rejects_malicious_packets_and_wrong_headers() {
        let p = Parameters {
            query_name: "fixture.invalid".into(),
            record_type: QueryType::A,
        };
        let query = expected_query(&p).unwrap();
        assert!(response(&[], 1, &query).is_err());
        assert!(response(&[0; 513], 1, &query).is_err());
        let mut pointer_loop = vec![0, 1, 0x80, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 1, 0, 1];
        assert!(response(&pointer_loop, 1, &query).is_err());
        pointer_loop[4] = 0xff;
        pointer_loop[5] = 0xff;
        assert!(response(&pointer_loop, 1, &query).is_err());
    }
}
