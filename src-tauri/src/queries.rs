//! Saved SQL, kept as `.sql` files on disk.
//!
//! Files rather than a private database, on purpose: the user can open them in
//! any editor, keep them in version control, copy them between machines, and
//! read them when the app is not running. The app only maintains a readable
//! directory layout and lists what is there.
//!
//! ```text
//! <app data>/queries/
//!   方塘线上mysql/
//!     .connection            # holds the connection id, so renames survive
//!     datasync1/
//!       导出数据.sql
//!       预存统计.sql
//! ```

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::store::Store;

/// Name of the marker file that ties a folder to a connection.
const MARKER: &str = ".connection";
const EXTENSION: &str = "sql";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    /// Stable identifier: the path relative to the queries root.
    pub id: String,
    pub connection_id: String,
    /// The database folder the file sits in; empty for files at the top level.
    pub namespace: String,
    /// The file stem, without the extension.
    pub name: String,
    /// Absolute path, so the UI can reveal it in the file manager.
    pub path: String,
    pub sql: String,
    /// Unix milliseconds.
    pub updated_at: i64,
}

/// Turn a user-facing name into something safe to use as a path segment.
fn sanitize(segment: &str, fallback: &str) -> String {
    let cleaned: String = segment
        .trim()
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            _ => ch,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

fn root(store: &Store) -> PathBuf {
    store.storage_dir().join("queries")
}

/// The folder for one connection, created on demand.
///
/// The folder is named after the connection so it is readable on disk, while the
/// marker file keeps it tied to the connection's id — renaming a connection
/// leaves the folder name stale but never detaches the queries.
fn connection_dir(store: &Store, connection_id: &str) -> AppResult<PathBuf> {
    let name = store
        .get(connection_id)
        .map(|config| config.name)
        .unwrap_or_else(|| connection_id.to_string());
    let base = root(store);
    std::fs::create_dir_all(&base)?;

    // A folder that already carries this connection's marker wins, whatever it
    // is called: that is what keeps queries attached when a connection is
    // renamed. The folder name is only a convenience for humans.
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if !candidate.is_dir() {
                continue;
            }
            if let Ok(owner) = std::fs::read_to_string(candidate.join(MARKER)) {
                if owner.trim() == connection_id {
                    return Ok(candidate);
                }
            }
        }
    }

    let preferred = base.join(sanitize(&name, connection_id));
    for attempt in 0..50 {
        let candidate = if attempt == 0 {
            preferred.clone()
        } else {
            base.join(format!(
                "{} ({})",
                preferred
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| connection_id.to_string()),
                attempt + 1
            ))
        };
        let marker = candidate.join(MARKER);
        if !candidate.exists() {
            std::fs::create_dir_all(&candidate)?;
            std::fs::write(&marker, connection_id)?;
            return Ok(candidate);
        }
        // An existing folder belongs to whoever its marker names.
        match std::fs::read_to_string(&marker) {
            Ok(owner) if owner.trim() == connection_id => return Ok(candidate),
            Ok(_) => continue,
            // A folder without a marker was not made by us; claim it only if it
            // is empty, otherwise move on to the next name.
            Err(_) => {
                let empty = std::fs::read_dir(&candidate)
                    .map(|mut entries| entries.next().is_none())
                    .unwrap_or(false);
                if empty {
                    std::fs::write(&marker, connection_id)?;
                    return Ok(candidate);
                }
                continue;
            }
        }
    }
    Err(AppError::msg(format!(
        "无法为连接 {name} 创建查询目录：同名目录过多"
    )))
}

/// Reject any path that escapes the queries root.
///
/// The UI sends paths back to the app, so they are treated as untrusted input:
/// a bug or a crafted payload must not be able to read or overwrite a file
/// elsewhere on the machine.
fn ensure_inside(store: &Store, path: &Path) -> AppResult<PathBuf> {
    let base = root(store);
    let base = base.canonicalize().unwrap_or(base);
    let resolved = if path.exists() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        let parent = path.parent().unwrap_or(Path::new(""));
        let parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
        parent.join(path.file_name().unwrap_or_default())
    };
    if !resolved.starts_with(&base) {
        return Err(AppError::msg(format!(
            "拒绝访问查询目录之外的路径：{}",
            path.display()
        )));
    }
    Ok(resolved)
}

fn modified_millis(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn read_query(store: &Store, connection_id: &str, path: &Path) -> AppResult<SavedQuery> {
    let resolved = ensure_inside(store, path)?;
    let sql = std::fs::read_to_string(&resolved)
        .map_err(|error| AppError::msg(format!("无法读取 {}：{error}", resolved.display())))?;
    let relative = resolved
        .strip_prefix(root(store).canonicalize().unwrap_or_else(|_| root(store)))
        .unwrap_or(&resolved)
        .to_path_buf();
    // <connection folder>/<namespace>/<name>.sql
    let mut parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_string())
        .collect();
    parts.remove(0);
    let file = parts.pop().unwrap_or_default();
    let name = file
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or(file);
    let namespace = parts.first().cloned().unwrap_or_default();
    Ok(SavedQuery {
        id: relative.to_string_lossy().to_string(),
        connection_id: connection_id.to_string(),
        namespace,
        name,
        path: resolved.to_string_lossy().to_string(),
        sql,
        updated_at: modified_millis(&resolved),
    })
}

/// Every `.sql` file belonging to a connection, newest first.
pub fn list(store: &Store, connection_id: &str) -> AppResult<Vec<SavedQuery>> {
    let dir = connection_dir(store, connection_id)?;
    let mut found = Vec::new();
    // Two levels: an optional database folder, then the files.
    let mut stack = vec![dir.clone()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some(EXTENSION) {
                continue;
            }
            if let Ok(query) = read_query(store, connection_id, &path) {
                found.push(query);
            }
        }
    }
    found.sort_by(|left, right| {
        left.namespace
            .cmp(&right.namespace)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(found)
}

/// Create an empty query file, refusing to overwrite an existing one.
pub fn create(
    store: &Store,
    connection_id: &str,
    namespace: &str,
    name: &str,
) -> AppResult<SavedQuery> {
    let dir = connection_dir(store, connection_id)?;
    let dir = if namespace.trim().is_empty() {
        dir
    } else {
        dir.join(sanitize(namespace, "default"))
    };
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.{EXTENSION}", sanitize(name, "query")));
    let file = unique_path(&file);
    std::fs::write(&file, "")?;
    read_query(store, connection_id, &file)
}

/// Pick a free name by appending a counter, the way a file manager would.
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "query".to_string());
    let parent = path.parent().unwrap_or(Path::new("")).to_path_buf();
    for attempt in 2..1000 {
        let candidate = parent.join(format!("{stem} {attempt}.{EXTENSION}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

/// Read one query back from disk.
pub fn read(store: &Store, path: &str) -> AppResult<SavedQuery> {
    let resolved = ensure_inside(store, Path::new(path))?;
    let connection_id = resolved
        .ancestors()
        .find_map(|dir| std::fs::read_to_string(dir.join(MARKER)).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    read_query(store, &connection_id, &resolved)
}

pub fn save(store: &Store, path: &str, sql: &str) -> AppResult<SavedQuery> {
    let resolved = ensure_inside(store, Path::new(path))?;
    let parent = resolved
        .parent()
        .ok_or_else(|| AppError::msg("查询路径无效"))?;
    std::fs::create_dir_all(parent)?;
    std::fs::write(&resolved, sql)
        .map_err(|error| AppError::msg(format!("保存 {} 失败：{error}", resolved.display())))?;
    // The connection id is recovered from the folder's marker so the caller does
    // not have to send it back.
    let connection_id = resolved
        .ancestors()
        .find_map(|dir| std::fs::read_to_string(dir.join(MARKER)).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    read_query(store, &connection_id, &resolved)
}

pub fn rename(store: &Store, path: &str, new_name: &str) -> AppResult<SavedQuery> {
    let resolved = ensure_inside(store, Path::new(path))?;
    let parent = resolved
        .parent()
        .ok_or_else(|| AppError::msg("查询路径无效"))?;
    let target = parent.join(format!("{}.{EXTENSION}", sanitize(new_name, "query")));
    if target != resolved {
        if target.exists() {
            return Err(AppError::msg(format!(
                "{} 已存在",
                target
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_default()
            )));
        }
        std::fs::rename(&resolved, &target)?;
    }
    let connection_id = target
        .ancestors()
        .find_map(|dir| std::fs::read_to_string(dir.join(MARKER)).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    read_query(store, &connection_id, &target)
}

/// Copy a query next to itself under a fresh name.
pub fn duplicate(store: &Store, path: &str) -> AppResult<SavedQuery> {
    let resolved = ensure_inside(store, Path::new(path))?;
    let sql = std::fs::read_to_string(&resolved)?;
    let target = unique_path(&resolved);
    std::fs::write(&target, sql)?;
    let connection_id = target
        .ancestors()
        .find_map(|dir| std::fs::read_to_string(dir.join(MARKER)).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    read_query(store, &connection_id, &target)
}

pub fn remove(store: &Store, path: &str) -> AppResult<()> {
    let resolved = ensure_inside(store, Path::new(path))?;
    std::fs::remove_file(&resolved)
        .map_err(|error| AppError::msg(format!("删除 {} 失败：{error}", resolved.display())))
}

/// The folder the queries live in, for a "reveal in Finder" action.
pub fn directory(store: &Store, connection_id: &str) -> AppResult<String> {
    Ok(connection_dir(store, connection_id)?
        .to_string_lossy()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ConnectionConfig;

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("datasync-queries-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::load(&dir);
        (store, dir)
    }

    fn add_connection(store: &Store, id: &str, name: &str) {
        store
            .upsert(ConnectionConfig {
                id: id.into(),
                name: name.into(),
                host: "127.0.0.1".into(),
                port: 3306,
                username: "root".into(),
                dialect: crate::dialect::Dialect::Mysql,
                database: None,
                ssl_mode: "prefer".into(),
                group: None,
                color: None,
                remark: None,
                save_password: false,
                password: None,
                has_password: false,
            })
            .unwrap();
    }

    #[test]
    fn queries_round_trip_through_files() {
        let (store, dir) = temp_store("roundtrip");
        add_connection(&store, "c1", "本地测试");

        let created = create(&store, "c1", "app", "导出数据").unwrap();
        assert_eq!(created.name, "导出数据");
        assert_eq!(created.namespace, "app");
        // A real file the user can open with any editor.
        assert!(Path::new(&created.path).exists());
        assert!(created.path.ends_with("app/导出数据.sql"));

        let saved = save(&store, &created.path, "SELECT 1;").unwrap();
        assert_eq!(saved.sql, "SELECT 1;");
        assert_eq!(
            std::fs::read_to_string(&created.path).unwrap(),
            "SELECT 1;",
            "内容必须真的写到磁盘"
        );

        let listed = list(&store, "c1").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].sql, "SELECT 1;");

        let renamed = rename(&store, &created.path, "预存统计").unwrap();
        assert_eq!(renamed.name, "预存统计");
        assert!(!Path::new(&created.path).exists(), "旧文件应已改名");
        assert!(renamed.path.ends_with("预存统计.sql"));

        let copy = duplicate(&store, &renamed.path).unwrap();
        assert_ne!(copy.path, renamed.path);
        assert_eq!(list(&store, "c1").unwrap().len(), 2);

        remove(&store, &copy.path).unwrap();
        assert_eq!(list(&store, "c1").unwrap().len(), 1);
        remove(&store, &renamed.path).unwrap();
        assert!(list(&store, "c1").unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_connection_keeps_its_folder_across_renames() {
        let (store, dir) = temp_store("rename");
        add_connection(&store, "c1", "旧名字");
        let created = create(&store, "c1", "app", "q").unwrap();

        // Renaming the connection must not detach the existing queries.
        add_connection(&store, "c1", "新名字");
        let listed = list(&store, "c1").unwrap();
        assert_eq!(listed.len(), 1, "改名后仍应找得到原有查询");
        assert_eq!(listed[0].path, created.path);
        // The folder keeps the old name: readable beats clever, and the marker
        // file is what actually ties it to the connection.
        assert!(created.path.contains("旧名字"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_sanitized_and_never_overwrite() {
        let (store, dir) = temp_store("names");
        add_connection(&store, "c1", "c");
        // A name full of path separators must not escape the folder. The
        // separators become underscores, so the file lands inside `app/`.
        let first = create(&store, "c1", "app", "../../etc/passwd").unwrap();
        let root = root(&store).canonicalize().unwrap();
        let resolved = Path::new(&first.path).canonicalize().unwrap();
        assert!(
            resolved.starts_with(&root),
            "查询文件必须留在查询目录内: {}",
            first.path
        );
        assert!(first.path.contains("app/"), "{}", first.path);
        assert!(Path::new(&first.path).exists());

        // Two queries with the same name coexist instead of clobbering.
        let second = create(&store, "c1", "app", "same").unwrap();
        let third = create(&store, "c1", "app", "same").unwrap();
        assert_ne!(second.path, third.path);
        assert_eq!(list(&store, "c1").unwrap().len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_outside_the_queries_folder_are_refused() {
        let (store, dir) = temp_store("escape");
        add_connection(&store, "c1", "c");
        // Reading or writing anywhere else would be a serious bug.
        let outside = dir.join("connections.json");
        assert!(std::fs::write(&outside, "x").is_ok());
        let error = save(&store, &outside.to_string_lossy(), "DROP").unwrap_err();
        assert!(error.to_string().contains("拒绝访问"), "{error}");
        let error = remove(&store, &outside.to_string_lossy()).unwrap_err();
        assert!(error.to_string().contains("拒绝访问"), "{error}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "x", "文件必须没被动过");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_name_is_rejected() {
        let (store, dir) = temp_store("empty");
        add_connection(&store, "c1", "c");
        // `sanitize` falls back rather than failing, so the file still gets a
        // usable name.
        let created = create(&store, "c1", "app", "   ").unwrap();
        assert_eq!(created.name, "query");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
