use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// A compared-and-ready-to-run data sync plan. Cached so that "预览" and
/// "执行" operate on exactly the same statement set.
pub struct DataPlan {
    pub insert: Vec<String>,
    pub update: Vec<String>,
    pub delete: Vec<String>,
    pub created_at: Instant,
}

impl DataPlan {
    pub fn select(&self, insert: bool, update: bool, delete: bool) -> Vec<String> {
        let mut out = Vec::new();
        if delete {
            out.extend(self.delete.iter().cloned());
        }
        if update {
            out.extend(self.update.iter().cloned());
        }
        if insert {
            out.extend(self.insert.iter().cloned());
        }
        out
    }

}

#[derive(Default)]
pub struct PlanCache {
    plans: Mutex<HashMap<String, DataPlan>>,
}

impl PlanCache {
    pub fn put(&self, plan: DataPlan) -> String {
        let id = Uuid::new_v4().to_string();
        if let Ok(mut guard) = self.plans.lock() {
            guard.retain(|_, p| p.created_at.elapsed() < Duration::from_secs(3600));
            guard.insert(id.clone(), plan);
        }
        id
    }

    pub fn with<F, T>(&self, id: &str, f: F) -> Option<T>
    where
        F: FnOnce(&DataPlan) -> T,
    {
        let guard = self.plans.lock().ok()?;
        guard.get(id).map(f)
    }

    pub fn remove(&self, id: &str) {
        if let Ok(mut guard) = self.plans.lock() {
            guard.remove(id);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.plans.lock() {
            guard.clear();
        }
    }
}
