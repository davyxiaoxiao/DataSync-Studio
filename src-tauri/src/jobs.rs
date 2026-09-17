//! Cancellation flags and progress reporting for long-running operations.
//!
//! A job is identified by an id chosen by the frontend. The frontend registers
//! an event listener once and filters updates by that id, which keeps the IPC
//! surface to a single event channel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::error::{AppError, AppResult};

pub const PROGRESS_EVENT: &str = "sync://progress";

/// Minimum interval between two progress events. Progress callbacks fire on
/// every row or statement; emitting each one would flood the webview.
const EMIT_INTERVAL: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressUpdate {
    pub job_id: String,
    /// Machine readable stage key.
    pub stage: String,
    /// Human readable stage label.
    pub label: String,
    pub processed: u64,
    /// `None` means "unknown length", rendered as an indeterminate bar.
    pub total: Option<u64>,
    pub detail: String,
    pub finished: bool,
    pub cancelled: bool,
}

#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl JobRegistry {
    pub fn begin(&self, job_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut guard) = self.jobs.lock() {
            // Bound the map: a stale entry only exists if a job never finished.
            if guard.len() > 32 {
                guard.clear();
            }
            guard.insert(job_id.to_string(), flag.clone());
        }
        flag
    }

    /// Returns true when a live job with that id was flagged for cancellation.
    pub fn cancel(&self, job_id: &str) -> bool {
        match self.jobs.lock().ok().and_then(|guard| guard.get(job_id).cloned()) {
            Some(flag) => {
                flag.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    pub fn finish(&self, job_id: &str) {
        if let Ok(mut guard) = self.jobs.lock() {
            guard.remove(job_id);
        }
    }
}

/// Progress sink handed to the core routines. When no `AppHandle` is available
/// (unit and integration tests) every call is a cheap no-op.
pub struct ProgressReporter {
    job_id: String,
    app: Option<AppHandle>,
    cancelled: Arc<AtomicBool>,
    last_emit: Mutex<Instant>,
}

impl ProgressReporter {
    pub fn new(
        job_id: String,
        app: Option<AppHandle>,
        cancelled: Arc<AtomicBool>,
    ) -> ProgressReporter {
        ProgressReporter {
            job_id,
            app,
            cancelled,
            last_emit: Mutex::new(Instant::now() - EMIT_INTERVAL),
        }
    }

    /// A reporter that never emits and is never cancelled, for tests.
    pub fn inert() -> ProgressReporter {
        ProgressReporter::new(String::new(), None, Arc::new(AtomicBool::new(false)))
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Abort the current operation when the user asked to cancel.
    pub fn check_cancelled(&self) -> AppResult<()> {
        if self.is_cancelled() {
            return Err(AppError::msg("操作已取消"));
        }
        Ok(())
    }

    /// Report progress, throttled to at most one event per `EMIT_INTERVAL`.
    pub fn tick(
        &self,
        stage: &str,
        label: &str,
        processed: u64,
        total: Option<u64>,
        detail: impl Into<String>,
    ) {
        if self.app.is_none() {
            return;
        }
        let should_emit = match self.last_emit.lock() {
            Ok(mut last) => {
                if last.elapsed() >= EMIT_INTERVAL {
                    *last = Instant::now();
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        };
        if should_emit {
            self.emit(stage, label, processed, total, detail.into(), false);
        }
    }

    /// Report regardless of the throttle, used for stage changes and the end.
    pub fn force(
        &self,
        stage: &str,
        label: &str,
        processed: u64,
        total: Option<u64>,
        detail: impl Into<String>,
    ) {
        if self.app.is_none() {
            return;
        }
        if let Ok(mut last) = self.last_emit.lock() {
            *last = Instant::now();
        }
        self.emit(stage, label, processed, total, detail.into(), false);
    }

    pub fn finish(&self, stage: &str, label: &str, detail: impl Into<String>) {
        if self.app.is_none() {
            return;
        }
        self.emit(stage, label, 0, None, detail.into(), true);
    }

    fn emit(
        &self,
        stage: &str,
        label: &str,
        processed: u64,
        total: Option<u64>,
        detail: String,
        finished: bool,
    ) {
        let Some(app) = &self.app else {
            return;
        };
        let payload = ProgressUpdate {
            job_id: self.job_id.clone(),
            stage: stage.to_string(),
            label: label.to_string(),
            processed,
            total,
            detail,
            finished,
            cancelled: self.is_cancelled(),
        };
        if let Err(err) = app.emit(PROGRESS_EVENT, payload) {
            eprintln!("[datasync-studio] 进度事件发送失败: {err}");
        }
    }
}
