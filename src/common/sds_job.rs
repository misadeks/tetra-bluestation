use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Trace metadata carried alongside an SDS request so we can track per-part status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdsTraceMeta {
    pub job_id: u64,
    pub part_index: usize, // 0-based
    pub part_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SdsPartStatus {
    Queued,
    SubmittedToStack,
    CmceEncoded,
    LcmcSubmitted,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdsPartInfo {
    pub part_index: usize, // 0-based
    pub part_total: usize,
    pub status: SdsPartStatus,
    pub mr: u8,
    pub payload_bits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdsJob {
    pub job_id: u64,
    pub created_ms: u64,
    pub dst_type: String,
    pub dst: u32,
    pub calling: u32,
    pub format: String,
    pub tcs: String,
    pub text_preview: String,
    pub parts: Vec<SdsPartInfo>,
}

#[derive(Default)]
struct Tracker {
    // keep a small rolling buffer to avoid unbounded growth
    jobs: VecDeque<SdsJob>,
}

const MAX_JOBS: usize = 50;

static TRACKER: OnceLock<Mutex<Tracker>> = OnceLock::new();

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn tracker() -> &'static Mutex<Tracker> {
    TRACKER.get_or_init(|| Mutex::new(Tracker::default()))
}

pub fn new_job(mut job: SdsJob) -> u64 {
    // Ensure id and timestamp
    if job.job_id == 0 {
        job.job_id = now_ms();
    }
    if job.created_ms == 0 {
        job.created_ms = now_ms();
    }

    let mut g = tracker().lock().unwrap();
    g.jobs.push_front(job);
    while g.jobs.len() > MAX_JOBS {
        g.jobs.pop_back();
    }
    g.jobs.front().map(|j| j.job_id).unwrap_or(0)
}

pub fn list_jobs() -> Vec<SdsJob> {
    tracker().lock().unwrap().jobs.iter().cloned().collect()
}

pub fn get_job(job_id: u64) -> Option<SdsJob> {
    tracker().lock().unwrap().jobs.iter().find(|j| j.job_id == job_id).cloned()
}

pub fn update_part_status(job_id: u64, part_index: usize, status: SdsPartStatus) {
    let mut g = tracker().lock().unwrap();
    if let Some(job) = g.jobs.iter_mut().find(|j| j.job_id == job_id) {
        if let Some(p) = job.parts.iter_mut().find(|p| p.part_index == part_index) {
            // Keep first error if already error
            if p.status != SdsPartStatus::Error {
                p.status = status;
            }
        }
    }
}

pub fn set_part_error(job_id: u64, part_index: usize, error: String) {
    let mut g = tracker().lock().unwrap();
    if let Some(job) = g.jobs.iter_mut().find(|j| j.job_id == job_id) {
        if let Some(p) = job.parts.iter_mut().find(|p| p.part_index == part_index) {
            p.status = SdsPartStatus::Error;
            p.error = Some(error);
        }
    }
}
