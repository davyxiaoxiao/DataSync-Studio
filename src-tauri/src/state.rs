use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::data::plan::PlanCache;
use crate::db::Registry;
use crate::schema::diff::SchemaDiff;
use crate::schema::model::TableStructure;
use crate::store::Store;

/// Everything captured by a structure comparison so that DDL preview and
/// execution use the exact same snapshot.
pub struct SchemaCompare {
    pub diff: SchemaDiff,
    pub source_structures: HashMap<String, TableStructure>,
    pub target_connection_id: String,
    pub target_namespace: String,
    pub created_at: Instant,
}

#[derive(Default)]
pub struct CompareCache {
    entries: Mutex<HashMap<String, SchemaCompare>>,
}

impl CompareCache {
    pub fn put(&self, compare: SchemaCompare) -> String {
        let id = Uuid::new_v4().to_string();
        if let Ok(mut guard) = self.entries.lock() {
            guard.retain(|_, c| c.created_at.elapsed() < Duration::from_secs(3600));
            guard.insert(id.clone(), compare);
        }
        id
    }

    pub fn with<F, T>(&self, id: &str, f: F) -> Option<T>
    where
        F: FnOnce(&SchemaCompare) -> T,
    {
        let guard = self.entries.lock().ok()?;
        guard.get(id).map(f)
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.clear();
        }
    }
}

pub struct AppState {
    pub store: Store,
    pub registry: Registry,
    pub compares: CompareCache,
    pub plans: PlanCache,
}

impl AppState {
    pub fn new(store: Store) -> AppState {
        AppState {
            store,
            registry: Registry::default(),
            compares: CompareCache::default(),
            plans: PlanCache::default(),
        }
    }
}
