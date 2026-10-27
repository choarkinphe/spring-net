use super::DiagnosticConfig;
use std::net::{IpAddr, SocketAddr};

pub(super) fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}

pub(crate) fn normalize(
    mut c: DiagnosticConfig,
    control_port: Option<u16>,
) -> Result<DiagnosticConfig, String> {
    if uuid::Uuid::parse_str(&c.request_id)
        .map_err(|_| "request_id must be a canonical UUID")?
        .to_string()
        != c.request_id
    {
        return Err("request_id must be a canonical UUID".into());
    }
    if c.port == 0 || Some(c.port) == control_port {
        return Err("AoNet control port or zero port is not permitted".into());
    }
    if c.target.is_empty()
        || c.target.len() > 2048
        || c.target
            .chars()
            .any(|v| v.is_control() || v.is_whitespace())
        || c.target.contains(['\\', '%', '*', '?', '#', '@'])
    {
        return Err("Target must be one exact host/IP or a credential-free HTTP(S) URL".into());
    }
    let host = if c.target.contains("://") {
        let u = crate::policy::parse_url(&c.target)?;
        if let Some(port) = u.port()
            && port != c.port
        {
            return Err("URL port differs from selected port".into());
        }
        crate::policy::host(&u)
    } else {
        c.target
            .strip_prefix('[')
            .and_then(|v| v.strip_suffix(']'))
            .unwrap_or(&c.target)
            .to_owned()
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        c.target = canonical_ip(ip).to_string();
    } else {
        if host.len() > 253
            || host.ends_with('.')
            || host.starts_with('-')
            || host
                .bytes()
                .any(|v| !v.is_ascii_alphanumeric() && v != b'.' && v != b'-')
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
            })
            || host.bytes().all(|v| v.is_ascii_digit() || v == b'.')
        {
            return Err(
                "Invalid exact DNS hostname; ranges, options and IPv6 zones are not allowed".into(),
            );
        }
        // URL normalization catches alternate numeric IPv4 forms such as 127.1 and 0x7f000001.
        let u = url::Url::parse(&format!("http://{host}/")).map_err(|_| "Invalid hostname")?;
        if let Some(url::Host::Ipv4(ip)) = u.host() {
            c.target = ip.to_string();
        } else {
            c.target = host.to_ascii_lowercase();
        }
        if c.target.parse::<IpAddr>().is_err() && !c.allow_lan && !c.target.contains('.') {
            return Err("Single-label names require explicit LAN scope".into());
        }
    }
    if let Some(proxy) = &c.proxy {
        c.proxy = Some(
            crate::proxy::normalize_proxy_config(proxy, control_port)
                .map_err(|error| error.to_string())?,
        );
        if c.target.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return Err("A proxy cannot be used with an owned loopback diagnostic target".into());
        }
    }
    Ok(c)
}

pub(crate) fn validate_addresses(
    c: &DiagnosticConfig,
    addresses: Vec<SocketAddr>,
    allowed_loopback: &[SocketAddr],
) -> Result<Vec<SocketAddr>, String> {
    if addresses.is_empty() || addresses.len() > 64 {
        return Err("System DNS returned zero or more than 64 addresses".into());
    }
    let mut result = Vec::new();
    for address in addresses {
        let ip = canonical_ip(address.ip());
        let a = SocketAddr::new(ip, c.port);
        let owned_loopback = c.proxy.is_none()
            && ip.is_loopback()
            && allowed_loopback
                .iter()
                .any(|owned| owned.port() == c.port && canonical_ip(owned.ip()) == ip);
        let lan = match ip {
            IpAddr::V4(v) => v.is_private(),
            IpAddr::V6(v) => v.segments()[0] & 0xfe00 == 0xfc00,
        };
        // Alibaba metadata lies in CGNAT. Also explicitly exclude Azure's
        // virtual platform IP and AWS's ULA metadata IP even in LAN scope.
        // Link-local metadata and other special addresses stay blocked.
        let platform_metadata = ip == IpAddr::V4(std::net::Ipv4Addr::new(168, 63, 129, 16))
            || ip
                == IpAddr::V6(std::net::Ipv6Addr::new(
                    0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254,
                ));
        if platform_metadata
            || !owned_loopback && !(crate::policy::public_ip(ip) || c.allow_lan && lan)
        {
            return Err(format!(
                "Address {ip} is outside the selected scope (loopback, metadata, link-local and special-use addresses are blocked)"
            ));
        }
        if !result.contains(&a) {
            result.push(a);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(target: &str, lan: bool) -> DiagnosticConfig {
        DiagnosticConfig {
            request_id: uuid::Uuid::new_v4().to_string(),
            target: target.into(),
            port: 443,
            tls: true,
            allow_lan: lan,
            proxy: None,
        }
    }
    #[test]
    fn strict_inputs() {
        for t in [
            "--help",
            "x;id",
            "a.*",
            "a/24",
            "fe80::1%en0",
            "https://u:p@example.com",
            "https://example.com?q=x",
            "a..com",
            "127.1",
            "0x7f000001",
        ] {
            let c = normalize(config(t, false), None);
            assert!(
                c.is_err() || c.as_ref().unwrap().target.parse::<IpAddr>().is_ok(),
                "{t}"
            );
            if let Ok(c) = c {
                assert!(
                    validate_addresses(
                        &c,
                        vec![SocketAddr::new(c.target.parse().unwrap(), 443)],
                        &[]
                    )
                    .is_err()
                );
            }
        }
        assert!(
            normalize(config("https://EXAMPLE.com/path", false), None)
                .unwrap()
                .target
                == "example.com"
        );
        assert!(normalize(config("example.com", false), Some(443)).is_err());
        let mut through_proxy = config("127.0.0.1", false);
        through_proxy.proxy = Some(crate::proxy::ProxyConfig {
            url: "http://127.0.0.1:8080".into(),
        });
        assert!(normalize(through_proxy, None).is_err());
        let mut normalized = config("example.com", false);
        normalized.proxy = Some(crate::proxy::ProxyConfig {
            url: "http://LOCALHOST:8080/".into(),
        });
        let normalized = normalize(normalized, None).unwrap();
        assert_eq!(normalized.proxy.unwrap().url, "http://localhost:8080");
        assert!(
            serde_json::from_str::<DiagnosticConfig>(
                r#"{"request_id":"x","target":"a","port":443,"tls":true,"allow_loopback":true}"#
            )
            .is_err()
        );
    }
    #[test]
    fn scopes_and_all_answers() {
        let c = config("example.com", true);
        for ip in [
            "127.0.0.1",
            "::1",
            "169.254.169.254",
            "100.100.100.200",
            "168.63.129.16",
            "192.0.0.192",
            "fd00:ec2::254",
            "0.0.0.0",
            "224.0.0.1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
        ] {
            assert!(
                validate_addresses(&c, vec![SocketAddr::new(ip.parse().unwrap(), 443)], &[])
                    .is_err(),
                "{ip}"
            );
        }
        for ip in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "fd12::1",
            "8.8.8.8",
        ] {
            assert!(
                validate_addresses(&c, vec![SocketAddr::new(ip.parse().unwrap(), 443)], &[])
                    .is_ok(),
                "{ip}"
            );
        }
        let mut public = c.clone();
        public.allow_lan = false;
        assert!(
            validate_addresses(
                &public,
                vec![
                    "8.8.8.8:443".parse().unwrap(),
                    "10.0.0.1:443".parse().unwrap()
                ],
                &[]
            )
            .is_err()
        );
        let owned = "127.0.0.1:443".parse().unwrap();
        assert!(validate_addresses(&public, vec![owned], &[owned]).is_ok());
        assert!(
            validate_addresses(
                &public,
                vec!["127.0.0.1:444".parse().unwrap()],
                &["127.0.0.1:444".parse().unwrap()]
            )
            .is_err()
        );
        let pinned = validate_addresses(
            &public,
            vec![
                "8.8.8.8:443".parse().unwrap(),
                "1.1.1.1:443".parse().unwrap(),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(pinned[0].ip().to_string(), "8.8.8.8");
    }
}
