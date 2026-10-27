use super::model::DiagnosticStage;
use serde::Serialize;
use serde_json::json;
use std::{net::IpAddr, time::Duration};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::network_process::OUTPUT_LIMIT;
#[derive(Debug, Serialize)]
pub(crate) struct Hop {
    ttl: u8,
    addresses: Vec<String>,
    rtt_ms: Vec<f64>,
    stars: usize,
    raw: String,
}

pub(crate) fn parse(raw: &str, target: IpAddr) -> (Vec<Hop>, bool, bool) {
    let mut hops = Vec::new();
    let mut incomplete = false;
    let mut reached = false;
    for line in raw.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        let Some(ttl) = words
            .first()
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| (1..=20).contains(v))
        else {
            continue;
        };
        if hops.len() >= 20 {
            incomplete = true;
            break;
        }
        let mut hop = Hop {
            ttl,
            addresses: vec![],
            rtt_ms: vec![],
            stars: 0,
            raw: line.to_owned(),
        };
        for (i, word) in words.iter().enumerate().skip(1) {
            if let Ok(ip) = word.trim_matches(['(', ')']).parse::<IpAddr>() {
                if !hop.addresses.contains(&ip.to_string()) {
                    hop.addresses.push(ip.to_string());
                }
            } else if *word == "*" {
                hop.stars += 1;
            } else if let Some(number) = word.strip_suffix("ms") {
                if let Ok(v) = number.trim_start_matches('<').parse::<f64>() {
                    hop.rtt_ms.push(v);
                }
            } else if words.get(i + 1) == Some(&"ms")
                && let Ok(v) = word.trim_start_matches('<').parse::<f64>()
            {
                hop.rtt_ms.push(v);
            }
        }
        if hop.addresses.is_empty() && hop.stars == 0 {
            incomplete = true;
        }
        let unreachable = words.iter().any(|w| w.starts_with('!'));
        if unreachable {
            incomplete = true;
        }
        // Require a measured destination reply, not merely the address echoed
        // in an unreachable/unknown-format line.
        reached |=
            !unreachable && !hop.rtt_ms.is_empty() && hop.addresses.contains(&target.to_string());
        hops.push(hop);
    }
    if hops.is_empty() {
        incomplete = true;
    }
    (hops, incomplete, reached)
}

#[cfg(target_os = "windows")]
pub(crate) fn system_command(ip: IpAddr) -> Option<(Command, &'static str)> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    let mut path = vec![0_u16; 32768];
    let len = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) } as usize;
    if len == 0 || len >= path.len() {
        return None;
    }
    use std::os::windows::ffi::OsStringExt;
    let base = std::ffi::OsString::from_wide(&path[..len]);
    let base = std::path::PathBuf::from(base);
    let mut command = Command::new(base.join("tracert.exe"));
    command
        .current_dir(&base)
        .env_clear()
        .env("SystemRoot", base.parent()?)
        .creation_flags(0x08000000);
    command.args([
        "/d",
        "/h",
        "20",
        "/w",
        "1000",
        if ip.is_ipv4() { "/4" } else { "/6" },
    ]);
    Some((
        command,
        "Windows tracert ICMP; 3 probes/hop; max 20 hops/60 probes",
    ))
}
#[cfg(target_os = "macos")]
pub(crate) fn system_command(ip: IpAddr) -> Option<(Command, &'static str)> {
    let path = if ip.is_ipv4() {
        "/usr/sbin/traceroute"
    } else {
        "/usr/sbin/traceroute6"
    };
    let mut command = Command::new(path);
    command.current_dir("/").env_clear().env("LC_ALL", "C");
    command.args(["-n", "-m", "20", "-q", "1", "-w", "1"]);
    Some((
        command,
        "macOS traceroute UDP; 1 probe/hop; max 20 hops/20 probes",
    ))
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn system_command(_ip: IpAddr) -> Option<(Command, &'static str)> {
    None
}

pub(super) async fn trace(ip: IpAddr, cancel: &CancellationToken) -> DiagnosticStage {
    let Some((mut command, method)) = system_command(ip) else {
        return DiagnosticStage {
            status: "unsupported".into(),
            observations: json!({"outcome":"unsupported","method":"no verified trusted route tool on this platform"}),
            ..Default::default()
        };
    };
    command.arg(ip.to_string());
    supervise(command, method, ip, cancel, Duration::from_secs(30)).await
}

async fn supervise(
    command: Command,
    method: &str,
    ip: IpAddr,
    cancel: &CancellationToken,
    budget: Duration,
) -> DiagnosticStage {
    let result = crate::network_process::supervise(command, cancel, budget, None).await;
    let raw_stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    let raw_stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    let mut outcome = match result.outcome {
        "completed" => "incomplete",
        "unsupported" => {
            return DiagnosticStage {
                status: "unsupported".into(),
                elapsed_ms: Some(result.elapsed_ms),
                error: result.error,
                observations: json!({"method":method,"outcome":"process_error"}),
            };
        }
        other => other,
    };
    let (hops, parse_incomplete, reached) = parse(&raw_stdout, ip);
    if outcome == "incomplete" {
        let lower = format!("{raw_stdout}\n{raw_stderr}").to_lowercase();
        if lower.contains("permission denied")
            || lower.contains("operation not permitted")
            || lower.contains("must be root")
        {
            outcome = "permission_denied";
        } else if !result.success {
            outcome = "process_error";
        } else if reached {
            outcome = "reached";
        }
    }
    let status = match outcome {
        "timeout" => "timeout",
        "cancelled" => "cancelled",
        "process_error" | "permission_denied" | "truncated" => "error",
        _ => "success",
    };
    DiagnosticStage {
        status: status.into(),
        elapsed_ms: Some(result.elapsed_ms),
        error: result.error,
        observations: json!({"method":method,"raw_stdout":raw_stdout,"raw_stderr":raw_stderr,"hops":hops,"parse_incomplete":parse_incomplete,"reached":reached,"truncated":outcome=="truncated","exit_code":result.exit_code,"outcome":outcome}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_parser() {
        let ip = "8.8.8.8".parse().unwrap();
        let (hops, incomplete, reached) = parse(
            "traceroute header\n 1 10.0.0.1 1.23 ms\n 2 *\n 3 8.8.8.8 2.5 ms",
            ip,
        );
        assert_eq!(hops.len(), 3);
        assert!(!incomplete);
        assert!(reached);
        let (hops, _, reached) = parse(" 1 <1 ms 2 ms 3 ms 8.8.8.8\n追踪完成", ip);
        assert_eq!(hops[0].rtt_ms, vec![1.0, 2.0, 3.0]);
        assert!(reached);
        assert!(parse("未知语言", ip).1);
        assert!(parse(" 1 unknown text", ip).1);
        assert!(!parse(" 1 8.8.8.8 1 ms !H", ip).2);
        assert!(!parse(" 1 8.8.8.8 1 ms !X", ip).2);
        assert_eq!(
            parse(
                &(1..=30).map(|i| format!(" {i} *\n")).collect::<String>(),
                ip
            )
            .0
            .len(),
            20
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_timeout_and_output_reap() {
        let ip = "127.0.0.1".parse().unwrap();
        let token = CancellationToken::new();
        let mut command = Command::new("/bin/sleep");
        command.arg("10");
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            supervise(command, "fixture", ip, &task_token, Duration::from_secs(20)).await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
        assert_eq!(task.await.unwrap().status, "cancelled");
        let mut command = Command::new("/bin/sleep");
        command.arg("10");
        assert_eq!(
            supervise(
                command,
                "fixture",
                ip,
                &CancellationToken::new(),
                Duration::from_millis(20)
            )
            .await
            .status,
            "timeout"
        );
        let command = Command::new("/usr/bin/yes");
        let stage = supervise(
            command,
            "fixture",
            ip,
            &CancellationToken::new(),
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(stage.observations["outcome"], "truncated");
        assert_eq!(
            stage.observations["raw_stdout"].as_str().unwrap().len(),
            OUTPUT_LIMIT
        );
    }
}
