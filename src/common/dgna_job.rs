use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::saps::dgna::DgnaOp;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DgnaTargetStatus {
    Queued,
    SubmittedToStack,
    Acked,
    Rejected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DgnaTargetInfo {
    pub issi: u32,
    pub status: DgnaTargetStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DgnaJob {
    pub job_id: u64,
    pub created_ms: u64,
    pub op: DgnaOp,
    pub gssi: u32,
    pub detach_all_first: bool,
    pub lifetime: u8,
    pub class_of_usage: u8,
    pub ack_req: bool,
    pub targets: Vec<DgnaTargetInfo>,
}

#[derive(Default)]
struct Tracker {
    jobs: VecDeque<DgnaJob>,
    pending_by_issi: HashMap<u32, u64>,
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

pub fn new_job(mut job: DgnaJob) -> u64 {
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

pub fn list_jobs() -> Vec<DgnaJob> {
    tracker().lock().unwrap().jobs.iter().cloned().collect()
}

pub fn get_job(job_id: u64) -> Option<DgnaJob> {
    tracker()
        .lock()
        .unwrap()
        .jobs
        .iter()
        .find(|j| j.job_id == job_id)
        .cloned()
}

pub fn mark_submitted(job_id: u64, issi: u32, ack_req: bool) {
    let mut g = tracker().lock().unwrap();
    if let Some(job) = g.jobs.iter_mut().find(|j| j.job_id == job_id) {
        if let Some(t) = job.targets.iter_mut().find(|t| t.issi == issi) {
            if t.status != DgnaTargetStatus::Error {
                t.status = DgnaTargetStatus::SubmittedToStack;
            }
        }
        if ack_req {
            g.pending_by_issi.insert(issi, job_id);
        }
    }
}

pub fn set_error(job_id: u64, issi: u32, error: String) {
    let mut g = tracker().lock().unwrap();
    if let Some(job) = g.jobs.iter_mut().find(|j| j.job_id == job_id) {
        if let Some(t) = job.targets.iter_mut().find(|t| t.issi == issi) {
            t.status = DgnaTargetStatus::Error;
            t.error = Some(error);
        }
    }
    g.pending_by_issi.remove(&issi);
}

/// Peek the currently pending DGNA job id for this ISSI (if any) without clearing it.
pub fn peek_pending_job_id(issi: u32) -> Option<u64> {
    let g = tracker().lock().unwrap();
    g.pending_by_issi.get(&issi).copied()
}

/// Called when we receive U-ATTACH/DETACH GROUP IDENTITY ACK from a subscriber.
pub fn mark_ack_from_issi(issi: u32, ok: bool, reason: Option<String>) -> Option<u64> {
    let mut g = tracker().lock().unwrap();
    let Some(job_id) = g.pending_by_issi.remove(&issi) else { return None; };
    if let Some(job) = g.jobs.iter_mut().find(|j| j.job_id == job_id) {
        if let Some(t) = job.targets.iter_mut().find(|t| t.issi == issi) {
            if t.status != DgnaTargetStatus::Error {
                if ok {
                    t.status = DgnaTargetStatus::Acked;
                    t.error = None;
                } else {
                    t.status = DgnaTargetStatus::Rejected;
                    // Keep a short, actionable hint for the UI.
                    t.error = Some(reason.unwrap_or_else(|| {
                        "terminal rejected DGNA (check codeplug: DGNA/Dynamic Regrouping enable, allowlist/range, security settings)".to_string()
                    }));
                }
            }
        }
    }
    Some(job_id)
}

