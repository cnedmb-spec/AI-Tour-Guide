//! 运行指标。对齐原 `/api/stats` 的口径，便于重写后逐项对照。

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Metrics {
    requests: AtomicU64,
    llm_requests: AtomicU64,
    tts_requests: AtomicU64,
    llm_failures: AtomicU64,
}

impl Metrics {
    pub fn on_request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn on_llm(&self) {
        self.llm_requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn on_tts(&self) {
        self.tts_requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn on_llm_failure(&self) {
        self.llm_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            llm_requests: self.llm_requests.load(Ordering::Relaxed),
            tts_requests: self.tts_requests.load(Ordering::Relaxed),
            llm_failures: self.llm_failures.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct MetricsSnapshot {
    pub requests: u64,
    pub llm_requests: u64,
    pub tts_requests: u64,
    pub llm_failures: u64,
}
