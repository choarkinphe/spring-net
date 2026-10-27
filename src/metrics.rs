use crate::model::{Run, RunConfig, Sample, Target, TargetStats};

// Fixed 1 ms buckets for bounded 1–30 s requests; the final bucket is overflow.
struct Histogram {
    buckets: Vec<u64>,
    count: u64,
}
impl Histogram {
    fn new() -> Self {
        Self {
            buckets: vec![0; 60_001],
            count: 0,
        }
    }
    fn record(&mut self, ms: f64) {
        let index = (ms.ceil() as usize).min(60_000);
        self.buckets[index] += 1;
        self.count += 1;
    }
    fn quantile(&self, q: f64) -> f64 {
        let rank = (self.count as f64 * q).ceil() as u64;
        let mut sum = 0;
        for (i, count) in self.buckets.iter().enumerate() {
            sum += count;
            if sum >= rank {
                return i as f64;
            }
        }
        0.0
    }
}

pub struct Metrics {
    pub run: Run,
    hist: Histogram,
    last_completed: u64,
    last_elapsed: f64,
}
#[derive(Default)]
pub struct Outcome {
    pub status: Option<u16>,
    pub error: Option<String>,
    pub cancelled: bool,
    pub latency_ms: f64,
    pub bytes: u64,
    pub truncated: bool,
    pub location: Option<String>,
    pub retry_after_secs: Option<u64>,
    pub tcp_success: bool,
}
impl Metrics {
    pub fn new(config: RunConfig, targets: &[Target]) -> Self {
        let run = Run {
            id: uuid::Uuid::new_v4().to_string(),
            status: "running".into(),
            mode: config.mode.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
            elapsed_secs: 0.0,
            config,
            reason: String::new(),
            attempted: 0,
            completed: 0,
            responses: 0,
            good_status: 0,
            errors: 0,
            cancelled: 0,
            active: 0,
            peak_active: 0,
            connected: 0,
            peak_connected: 0,
            bytes: 0,
            p50_ms: None,
            p95_ms: None,
            rps: 0.0,
            series: vec![],
            sequence: 0,
            targets: targets
                .iter()
                .map(|t| TargetStats {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    url: t.url.clone(),
                    ..Default::default()
                })
                .collect(),
        };
        Self {
            run,
            hist: Histogram::new(),
            last_completed: 0,
            last_elapsed: 0.0,
        }
    }
    pub fn start(&mut self, i: usize) {
        self.run.attempted += 1;
        self.run.active += 1;
        self.run.peak_active = self.run.peak_active.max(self.run.active);
        let t = &mut self.run.targets[i];
        t.attempted += 1;
        t.active += 1;
    }
    pub fn connected(&mut self, i: usize, delta: bool) {
        if delta {
            self.run.connected += 1;
            self.run.targets[i].connected += 1;
        } else {
            self.run.connected = self.run.connected.saturating_sub(1);
            self.run.targets[i].connected = self.run.targets[i].connected.saturating_sub(1);
        }
        self.run.peak_connected = self.run.peak_connected.max(self.run.connected);
    }
    pub fn finish(&mut self, i: usize, o: Outcome) {
        let r = &mut self.run;
        let t = &mut r.targets[i];
        r.active = r.active.saturating_sub(1);
        t.active = t.active.saturating_sub(1);
        if o.cancelled {
            r.cancelled += 1;
            t.cancelled += 1;
            return;
        }
        r.completed += 1;
        t.completed += 1;
        if let Some(s) = o.status {
            r.responses += 1;
            t.responses += 1;
            if (200..400).contains(&s) {
                r.good_status += 1;
                t.good_status += 1;
            }
        }
        if o.error.is_some() {
            r.errors += 1;
            t.errors += 1;
        }
        t.last_status = o.status;
        t.last_error = o.error;
        t.last_latency_ms = Some(o.latency_ms);
        t.location = o.location;
        r.bytes += o.bytes;
        t.bytes += o.bytes;
        t.truncated += u64::from(o.truncated);
        if o.status.is_some() || o.tcp_success {
            self.hist.record(o.latency_ms);
            r.p50_ms = Some(self.hist.quantile(0.5));
            r.p95_ms = Some(self.hist.quantile(0.95));
        }
    }
    pub fn sample(&mut self, elapsed: f64) {
        self.run.elapsed_secs = elapsed;
        let dt = elapsed - self.last_elapsed;
        if dt >= 0.001 {
            self.run.rps = (self.run.completed - self.last_completed) as f64 / dt;
        }
        self.last_completed = self.run.completed;
        self.last_elapsed = elapsed;
        self.run.sequence += 1;
        self.run.series.push(Sample {
            elapsed_secs: elapsed,
            rps: self.run.rps,
            p95_ms: self.run.p95_ms,
            active: self.run.active,
            connected: self.run.connected,
        });
        if self.run.series.len() > 600 {
            self.run.series.remove(0);
        }
    }
}
pub fn csv(run: &Run) -> String {
    fn cell(v: &str) -> String {
        let trim = v.trim_start();
        let prefix = if trim.starts_with(['=', '+', '-', '@']) || v.starts_with(['\t', '\r', '\n'])
        {
            "'"
        } else {
            ""
        };
        format!("\"{prefix}{}\"", v.replace('"', "\"\""))
    }
    let mut result = "\u{feff}run_id,mode,status,reason,name,url,attempted,completed,responses,http_2xx_3xx,errors,cancelled,last_status,last_latency_ms,bytes,truncated\r\n".to_owned();
    for t in &run.targets {
        let fields = [
            run.id.clone(),
            run.mode.clone(),
            run.status.clone(),
            run.reason.clone(),
            t.name.clone(),
            t.url.clone(),
            t.attempted.to_string(),
            t.completed.to_string(),
            t.responses.to_string(),
            t.good_status.to_string(),
            t.errors.to_string(),
            t.cancelled.to_string(),
            t.last_status.map(|v| v.to_string()).unwrap_or_default(),
            t.last_latency_ms
                .map(|v| format!("{v:.2}"))
                .unwrap_or_default(),
            t.bytes.to_string(),
            t.truncated.to_string(),
        ];
        result.push_str(&fields.iter().map(|s| cell(s)).collect::<Vec<_>>().join(","));
        result.push_str("\r\n");
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn m() -> Metrics {
        Metrics::new(
            RunConfig {
                mode: "http".into(),
                list_ids: vec![],
                target_ids: vec![],
                authorized_ids: vec![],
                concurrency: 1,
                rps: 1,
                duration_secs: 1,
                timeout_secs: 1,
                ramp_secs: 0,
                fresh_connections: false,
                proxy: None,
            },
            &[Target {
                id: "x".into(),
                name: "=evil".into(),
                url: "http://example.com/".into(),
                region: String::new(),
                source: String::new(),
                checked_at: String::new(),
                probe_only: true,
            }],
        )
    }
    #[test]
    fn http_errors_not_network_failures() {
        let mut m = m();
        m.start(0);
        m.finish(
            0,
            Outcome {
                status: Some(403),
                latency_ms: 10.0,
                ..Default::default()
            },
        );
        assert_eq!(m.run.responses, 1);
        assert_eq!(m.run.errors, 0);
        assert_eq!(m.run.good_status, 0);
        assert_eq!(m.run.active, 0);
        assert!(csv(&m.run).contains("'=evil"));
    }
    #[test]
    fn cancelled_accounting() {
        let mut m = m();
        m.start(0);
        m.finish(
            0,
            Outcome {
                cancelled: true,
                ..Default::default()
            },
        );
        assert_eq!(m.run.attempted, m.run.completed + m.run.cancelled);
        assert_eq!(m.run.active, 0);
    }
}
