use serde::{Deserialize, Serialize};

use crate::dialect::Dialect;
use crate::schema::model::{
    ColumnInfo, CustomType, ForeignKeyInfo, IndexInfo, TableStructure, ViewStructure,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// Present in the source, absent in the target -> create.
    New,
    /// Present in the target, absent in the source -> drop.
    Missing,
    /// Present on both sides but different -> alter.
    Changed,
    /// Identical, nothing to do.
    Identical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub field: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDiff {
    pub name: String,
    pub kind: ChangeKind,
    pub source: Option<ColumnInfo>,
    pub target: Option<ColumnInfo>,
    pub differences: Vec<FieldChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDiff {
    pub name: String,
    pub kind: ChangeKind,
    pub source: Option<IndexInfo>,
    pub target: Option<IndexInfo>,
    pub differences: Vec<FieldChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyDiff {
    pub name: String,
    pub kind: ChangeKind,
    pub source: Option<ForeignKeyInfo>,
    pub target: Option<ForeignKeyInfo>,
    pub differences: Vec<FieldChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryKeyDiff {
    pub kind: ChangeKind,
    pub source: Vec<String>,
    pub target: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDiff {
    /// Name on the source side.
    pub name: String,
    /// Name on the target side (`None` when the table must be created).
    pub target_name: Option<String>,
    pub kind: ChangeKind,
    pub source_comment: Option<String>,
    pub target_comment: Option<String>,
    pub columns: Vec<ColumnDiff>,
    pub primary_key: PrimaryKeyDiff,
    pub indexes: Vec<IndexDiff>,
    pub foreign_keys: Vec<ForeignKeyDiff>,
}

impl TableDiff {
    pub fn has_changes(&self) -> bool {
        !matches!(self.kind, ChangeKind::Identical)
    }

}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewDiff {
    pub name: String,
    pub kind: ChangeKind,
    pub source: Option<ViewStructure>,
    pub target: Option<ViewStructure>,
    pub differences: Vec<FieldChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDiff {
    pub name: String,
    pub kind: ChangeKind,
    pub source: Option<CustomType>,
    pub target: Option<CustomType>,
    pub differences: Vec<FieldChange>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub new_tables: usize,
    pub missing_tables: usize,
    pub changed_tables: usize,
    pub identical_tables: usize,
    pub new_columns: usize,
    pub missing_columns: usize,
    pub changed_columns: usize,
    pub index_changes: usize,
    pub foreign_key_changes: usize,
    pub primary_key_changes: usize,
    pub new_types: usize,
    pub missing_types: usize,
    pub changed_types: usize,
    pub new_views: usize,
    pub missing_views: usize,
    pub changed_views: usize,
    pub identical_views: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDiff {
    pub source_namespace: String,
    pub target_namespace: String,
    pub source_dialect: Dialect,
    pub target_dialect: Dialect,
    pub tables: Vec<TableDiff>,
    pub views: Vec<ViewDiff>,
    pub types: Vec<TypeDiff>,
    pub summary: DiffSummary,
}

/// Everything the comparison needs beyond the two object lists.
#[derive(Debug, Clone, Copy)]
pub struct CompareContext<'a> {
    pub source_dialect: Dialect,
    pub target_dialect: Dialect,
    /// Namespaces let type and default references that merely differ by
    /// namespace (`src.mood` vs `dst.mood`) compare as equal.
    pub source_namespace: &'a str,
    pub target_namespace: &'a str,
    pub options: CompareOptions,
}

impl<'a> CompareContext<'a> {
    pub fn new(
        source_dialect: Dialect,
        target_dialect: Dialect,
        source_namespace: &'a str,
        target_namespace: &'a str,
        options: CompareOptions,
    ) -> CompareContext<'a> {
        CompareContext {
            source_dialect,
            target_dialect,
            source_namespace,
            target_namespace,
            options,
        }
    }

    /// Remove any `namespace.` qualifier from a type or expression reference.
    ///
    /// A PostgreSQL enum column reports its type as `src.order_state` and its
    /// default as `'new'::src.order_state`, while the target reports the same
    /// things against `dst`. The namespaces necessarily differ, so they are
    /// stripped before comparing; the quotes are dropped first so that quoted
    /// forms (`"src".order_state`) are handled too.
    fn localize(&self, value: &str, namespace: &str) -> String {
        let unquoted: String = value.chars().filter(|c| *c != '"' && *c != '`').collect();
        let trimmed = unquoted.trim();
        if namespace.is_empty() {
            return trimmed.to_string();
        }
        let chars: Vec<char> = trimmed.chars().collect();
        let needle: Vec<char> = namespace.to_lowercase().chars().collect();
        let mut out = String::with_capacity(trimmed.len());
        let mut i = 0;
        while i < chars.len() {
            let fits = i + needle.len() < chars.len()
                && chars[i..i + needle.len()]
                    .iter()
                    .map(|c| c.to_ascii_lowercase())
                    .eq(needle.iter().copied())
                && chars[i + needle.len()] == '.';
            let boundary = i == 0 || {
                let previous = chars[i - 1];
                !(previous.is_alphanumeric() || previous == '_')
            };
            if fits && boundary {
                i += needle.len() + 1;
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompareOptions {
    pub ignore_comments: bool,
    pub ignore_case: bool,
    pub compare_charset: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            ignore_comments: true,
            ignore_case: false,
            compare_charset: false,
        }
    }
}

/// Compare source (desired) against target (current) and describe what would
/// have to change in the target to match the source.
/// Full comparison including views.
#[allow(clippy::too_many_arguments)]
/// Full comparison: tables, views and user-defined types.
#[allow(clippy::too_many_arguments)]
pub fn compare_everything(
    source: &[TableStructure],
    target: &[TableStructure],
    source_views: &[ViewStructure],
    target_views: &[ViewStructure],
    source_types: &[CustomType],
    target_types: &[CustomType],
    source_dialect: Dialect,
    target_dialect: Dialect,
    source_namespace: &str,
    target_namespace: &str,
    options: CompareOptions,
) -> SchemaDiff {
    let context = CompareContext::new(
        source_dialect,
        target_dialect,
        source_namespace,
        target_namespace,
        options,
    );
    let key_of = |name: &str| {
        if options.ignore_case {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    };

    let target_map: std::collections::HashMap<String, &TableStructure> = target
        .iter()
        .map(|t| (key_of(&t.name), t))
        .collect();

    let mut tables: Vec<TableDiff> = Vec::new();
    let mut used_targets: Vec<String> = Vec::new();

    for src in source {
        let key = key_of(&src.name);
        match target_map.get(&key) {
            Some(tgt) => {
                used_targets.push(key);
                tables.push(diff_table(Some(src), Some(tgt), context));
            }
            None => tables.push(diff_table(Some(src), None, context)),
        }
    }

    for tgt in target {
        let key = key_of(&tgt.name);
        if !used_targets.contains(&key) {
            tables.push(diff_table(None, Some(tgt), context));
        }
    }

    tables.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let mut summary = DiffSummary::default();
    for table in &tables {
        match table.kind {
            ChangeKind::New => summary.new_tables += 1,
            ChangeKind::Missing => summary.missing_tables += 1,
            ChangeKind::Changed => summary.changed_tables += 1,
            ChangeKind::Identical => summary.identical_tables += 1,
        }
        for column in &table.columns {
            match column.kind {
                ChangeKind::New => summary.new_columns += 1,
                ChangeKind::Missing => summary.missing_columns += 1,
                ChangeKind::Changed => summary.changed_columns += 1,
                ChangeKind::Identical => {}
            }
        }
        summary.index_changes += table
            .indexes
            .iter()
            .filter(|i| i.kind != ChangeKind::Identical)
            .count();
        summary.foreign_key_changes += table
            .foreign_keys
            .iter()
            .filter(|f| f.kind != ChangeKind::Identical)
            .count();
        if table.primary_key.kind != ChangeKind::Identical {
            summary.primary_key_changes += 1;
        }
    }

    let views = diff_views(source_views, target_views);
    for view in &views {
        match view.kind {
            ChangeKind::New => summary.new_views += 1,
            ChangeKind::Missing => summary.missing_views += 1,
            ChangeKind::Changed => summary.changed_views += 1,
            ChangeKind::Identical => summary.identical_views += 1,
        }
    }

    let types = diff_types(source_types, target_types);
    for entry in &types {
        match entry.kind {
            ChangeKind::New => summary.new_types += 1,
            ChangeKind::Missing => summary.missing_types += 1,
            ChangeKind::Changed => summary.changed_types += 1,
            ChangeKind::Identical => {}
        }
    }

    SchemaDiff {
        source_namespace: source_namespace.to_string(),
        target_namespace: target_namespace.to_string(),
        source_dialect,
        target_dialect,
        tables,
        views,
        types,
        summary,
    }
}

fn diff_types(source: &[CustomType], target: &[CustomType]) -> Vec<TypeDiff> {
    let mut out = Vec::new();
    for entry in source {
        match target.iter().find(|t| t.name == entry.name) {
            Some(existing) => {
                let mut differences = Vec::new();
                if entry.kind != existing.kind {
                    differences.push(FieldChange {
                        field: "类型种类".into(),
                        source: Some(entry.kind.clone()),
                        target: Some(existing.kind.clone()),
                    });
                } else if entry.kind == "enum" {
                    let added: Vec<String> = entry
                        .labels
                        .iter()
                        .filter(|label| !existing.labels.contains(label))
                        .cloned()
                        .collect();
                    let removed: Vec<String> = existing
                        .labels
                        .iter()
                        .filter(|label| !entry.labels.contains(label))
                        .cloned()
                        .collect();
                    let reordered = added.is_empty()
                        && removed.is_empty()
                        && entry.labels != existing.labels;
                    if !added.is_empty() || !removed.is_empty() || reordered {
                        differences.push(FieldChange {
                            field: "枚举值".into(),
                            source: Some(entry.labels.join(", ")),
                            target: Some(existing.labels.join(", ")),
                        });
                    }
                } else {
                    if entry.base_type != existing.base_type {
                        differences.push(FieldChange {
                            field: "基础类型".into(),
                            source: entry.base_type.clone(),
                            target: existing.base_type.clone(),
                        });
                    }
                    if entry.domain_nullable != existing.domain_nullable {
                        differences.push(FieldChange {
                            field: "允许为空".into(),
                            source: Some(
                                if entry.domain_nullable { "YES" } else { "NO" }.into(),
                            ),
                            target: Some(
                                if existing.domain_nullable { "YES" } else { "NO" }.into(),
                            ),
                        });
                    }
                    if normalize_default(entry.domain_default.as_deref().unwrap_or(""))
                        != normalize_default(existing.domain_default.as_deref().unwrap_or(""))
                    {
                        differences.push(FieldChange {
                            field: "默认值".into(),
                            source: entry.domain_default.clone(),
                            target: existing.domain_default.clone(),
                        });
                    }
                    let mut source_constraints: Vec<String> = entry
                        .constraints
                        .iter()
                        .map(|c| normalize_type_text(c))
                        .collect();
                    let mut target_constraints: Vec<String> = existing
                        .constraints
                        .iter()
                        .map(|c| normalize_type_text(c))
                        .collect();
                    source_constraints.sort();
                    target_constraints.sort();
                    if source_constraints != target_constraints {
                        differences.push(FieldChange {
                            field: "约束".into(),
                            source: Some(entry.constraints.join("; ")),
                            target: Some(existing.constraints.join("; ")),
                        });
                    }
                }
                out.push(TypeDiff {
                    name: entry.name.clone(),
                    kind: if differences.is_empty() {
                        ChangeKind::Identical
                    } else {
                        ChangeKind::Changed
                    },
                    source: Some(entry.clone()),
                    target: Some(existing.clone()),
                    differences,
                });
            }
            None => out.push(TypeDiff {
                name: entry.name.clone(),
                kind: ChangeKind::New,
                source: Some(entry.clone()),
                target: None,
                differences: Vec::new(),
            }),
        }
    }
    for existing in target {
        if !source.iter().any(|entry| entry.name == existing.name) {
            out.push(TypeDiff {
                name: existing.name.clone(),
                kind: ChangeKind::Missing,
                source: None,
                target: Some(existing.clone()),
                differences: Vec::new(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn diff_views(source: &[ViewStructure], target: &[ViewStructure]) -> Vec<ViewDiff> {
    let mut out = Vec::new();
    for view in source {
        match target.iter().find(|t| t.name == view.name) {
            Some(existing) => {
                let mut differences = Vec::new();
                if view.kind != existing.kind {
                    differences.push(FieldChange {
                        field: "类型".into(),
                        source: Some(view.kind.clone()),
                        target: Some(existing.kind.clone()),
                    });
                }
                if view.normalized_definition() != existing.normalized_definition() {
                    differences.push(FieldChange {
                        field: "定义".into(),
                        source: Some(view.definition.clone()),
                        target: Some(existing.definition.clone()),
                    });
                }
                if view.comment.as_deref().unwrap_or("")
                    != existing.comment.as_deref().unwrap_or("")
                {
                    differences.push(FieldChange {
                        field: "注释".into(),
                        source: view.comment.clone(),
                        target: existing.comment.clone(),
                    });
                }
                out.push(ViewDiff {
                    name: view.name.clone(),
                    kind: if differences.is_empty() {
                        ChangeKind::Identical
                    } else {
                        ChangeKind::Changed
                    },
                    source: Some(view.clone()),
                    target: Some(existing.clone()),
                    differences,
                });
            }
            None => out.push(ViewDiff {
                name: view.name.clone(),
                kind: ChangeKind::New,
                source: Some(view.clone()),
                target: None,
                differences: Vec::new(),
            }),
        }
    }
    for existing in target {
        if !source.iter().any(|v| v.name == existing.name) {
            out.push(ViewDiff {
                name: existing.name.clone(),
                kind: ChangeKind::Missing,
                source: None,
                target: Some(existing.clone()),
                differences: Vec::new(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn diff_table(
    source: Option<&TableStructure>,
    target: Option<&TableStructure>,
    context: CompareContext<'_>,
) -> TableDiff {
    let options = context.options;
    let name = source
        .map(|t| t.name.clone())
        .or_else(|| target.map(|t| t.name.clone()))
        .unwrap_or_default();

    let kind = match (source, target) {
        (Some(_), None) => ChangeKind::New,
        (None, Some(_)) => ChangeKind::Missing,
        _ => ChangeKind::Changed,
    };

    let mut diff = TableDiff {
        name,
        target_name: target.map(|t| t.name.clone()),
        kind,
        source_comment: source.and_then(|t| t.comment.clone()),
        target_comment: target.and_then(|t| t.comment.clone()),
        columns: Vec::new(),
        primary_key: PrimaryKeyDiff {
            kind: ChangeKind::Identical,
            source: Vec::new(),
            target: Vec::new(),
        },
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    };

    // Wholesale creation needs no per-object detail.
    if kind == ChangeKind::New {
        return diff;
    }
    // A table that is about to be dropped still has to have its inbound foreign
    // keys released, and those constraints only exist on the target. They are
    // reported as `Missing` so the planner can drop them first.
    if kind == ChangeKind::Missing {
        if let Some(existing) = target {
            for fk in &existing.foreign_keys {
                diff.foreign_keys.push(ForeignKeyDiff {
                    name: fk.name.clone(),
                    kind: ChangeKind::Missing,
                    source: None,
                    target: Some(fk.clone()),
                    differences: Vec::new(),
                });
            }
        }
        return diff;
    }

    let (src, tgt) = match (source, target) {
        (Some(s), Some(t)) => (s, t),
        _ => return diff,
    };

    // --- columns -----------------------------------------------------------
    for column in &src.columns {
        match tgt.column(&column.name) {
            Some(existing) => {
                let differences = column_differences(column, existing, context);
                diff.columns.push(ColumnDiff {
                    name: column.name.clone(),
                    kind: if differences.is_empty() {
                        ChangeKind::Identical
                    } else {
                        ChangeKind::Changed
                    },
                    source: Some(column.clone()),
                    target: Some(existing.clone()),
                    differences,
                });
            }
            None => diff.columns.push(ColumnDiff {
                name: column.name.clone(),
                kind: ChangeKind::New,
                source: Some(column.clone()),
                target: None,
                differences: Vec::new(),
            }),
        }
    }
    for column in &tgt.columns {
        if src.column(&column.name).is_none() {
            diff.columns.push(ColumnDiff {
                name: column.name.clone(),
                kind: ChangeKind::Missing,
                source: None,
                target: Some(column.clone()),
                differences: Vec::new(),
            });
        }
    }

    // --- primary key -------------------------------------------------------
    if src.primary_key != tgt.primary_key {
        diff.primary_key = PrimaryKeyDiff {
            kind: if tgt.primary_key.is_empty() {
                ChangeKind::New
            } else if src.primary_key.is_empty() {
                ChangeKind::Missing
            } else {
                ChangeKind::Changed
            },
            source: src.primary_key.clone(),
            target: tgt.primary_key.clone(),
        };
    }

    // --- indexes -----------------------------------------------------------
    for index in &src.indexes {
        match tgt.index(&index.name) {
            Some(existing) => {
                let differences = index_differences(index, existing);
                diff.indexes.push(IndexDiff {
                    name: index.name.clone(),
                    kind: if differences.is_empty() {
                        ChangeKind::Identical
                    } else {
                        ChangeKind::Changed
                    },
                    source: Some(index.clone()),
                    target: Some(existing.clone()),
                    differences,
                });
            }
            None => diff.indexes.push(IndexDiff {
                name: index.name.clone(),
                kind: ChangeKind::New,
                source: Some(index.clone()),
                target: None,
                differences: Vec::new(),
            }),
        }
    }
    for index in &tgt.indexes {
        if src.index(&index.name).is_none() {
            diff.indexes.push(IndexDiff {
                name: index.name.clone(),
                kind: ChangeKind::Missing,
                source: None,
                target: Some(index.clone()),
                differences: Vec::new(),
            });
        }
    }

    // --- foreign keys ------------------------------------------------------
    for fk in &src.foreign_keys {
        match tgt.foreign_key(&fk.name) {
            Some(existing) => {
                let differences = fk_differences(fk, existing);
                diff.foreign_keys.push(ForeignKeyDiff {
                    name: fk.name.clone(),
                    kind: if differences.is_empty() {
                        ChangeKind::Identical
                    } else {
                        ChangeKind::Changed
                    },
                    source: Some(fk.clone()),
                    target: Some(existing.clone()),
                    differences,
                });
            }
            None => diff.foreign_keys.push(ForeignKeyDiff {
                name: fk.name.clone(),
                kind: ChangeKind::New,
                source: Some(fk.clone()),
                target: None,
                differences: Vec::new(),
            }),
        }
    }
    for fk in &tgt.foreign_keys {
        if src.foreign_key(&fk.name).is_none() {
            diff.foreign_keys.push(ForeignKeyDiff {
                name: fk.name.clone(),
                kind: ChangeKind::Missing,
                source: None,
                target: Some(fk.clone()),
                differences: Vec::new(),
            });
        }
    }

    let comment_changed = !options.ignore_comments
        && src.comment.as_deref().unwrap_or("") != tgt.comment.as_deref().unwrap_or("");
    let structure_clean = diff.columns.iter().all(|c| c.kind == ChangeKind::Identical)
        && diff.primary_key.kind == ChangeKind::Identical
        && diff.indexes.iter().all(|i| i.kind == ChangeKind::Identical)
        && diff.foreign_keys.iter().all(|f| f.kind == ChangeKind::Identical)
        && !comment_changed;
    if structure_clean {
        diff.kind = ChangeKind::Identical;
    }

    diff
}

fn column_differences(
    source: &ColumnInfo,
    target: &ColumnInfo,
    context: CompareContext<'_>,
) -> Vec<FieldChange> {
    let options = context.options;
    let source_dialect = context.source_dialect;
    let target_dialect = context.target_dialect;
    let mut out = Vec::new();

    // Both endpoints are the same engine, so the declared types are directly
    // comparable once display widths, whitespace and the (necessarily
    // different) namespace of a user-defined type are normalised away.
    let source_type = context.localize(&source.canonical_type(), context.source_namespace);
    let target_type = context.localize(&target.canonical_type(), context.target_namespace);
    if source_type != target_type {
        out.push(FieldChange {
            field: "类型".into(),
            source: Some(source.full_type.clone()),
            target: Some(target.full_type.clone()),
        });
    }

    if source.nullable != target.nullable {
        out.push(FieldChange {
            field: "允许为空".into(),
            source: Some(if source.nullable { "YES" } else { "NO" }.into()),
            target: Some(if target.nullable { "YES" } else { "NO" }.into()),
        });
    }

    let source_default = context.localize(
        &source.default.clone().unwrap_or_default(),
        context.source_namespace,
    );
    let target_default = context.localize(
        &target.default.clone().unwrap_or_default(),
        context.target_namespace,
    );
    if !defaults_match(source, target, &source_default, &target_default) {
        out.push(FieldChange {
            field: "默认值".into(),
            source: Some(if source_default.is_empty() {
                "(无)".into()
            } else {
                source_default
            }),
            target: Some(if target_default.is_empty() {
                "(无)".into()
            } else {
                target_default
            }),
        });
    }

    if source.is_auto_increment != target.is_auto_increment {
        out.push(FieldChange {
            field: "自增".into(),
            source: Some(if source.is_auto_increment { "是" } else { "否" }.into()),
            target: Some(if target.is_auto_increment { "是" } else { "否" }.into()),
        });
    }

    if source.is_generated != target.is_generated {
        out.push(FieldChange {
            field: "生成列".into(),
            source: Some(if source.is_generated { "是" } else { "否" }.into()),
            target: Some(if target.is_generated { "是" } else { "否" }.into()),
        });
    } else if source.is_generated {
        let source_expression = normalize_generation(source.generation_expression.as_deref());
        let target_expression = normalize_generation(target.generation_expression.as_deref());
        if source_expression != target_expression {
            out.push(FieldChange {
                field: "生成表达式".into(),
                source: source.generation_expression.clone(),
                target: target.generation_expression.clone(),
            });
        }
    }

    if !options.ignore_comments
        && source.comment.as_deref().unwrap_or("") != target.comment.as_deref().unwrap_or("")
    {
        out.push(FieldChange {
            field: "注释".into(),
            source: source.comment.clone(),
            target: target.comment.clone(),
        });
    }

    if options.compare_charset
        && source_dialect == target_dialect
        && (source.charset != target.charset || source.collation != target.collation)
    {
        out.push(FieldChange {
            field: "字符集".into(),
            source: source
                .collation
                .clone()
                .or_else(|| source.charset.clone()),
            target: target
                .collation
                .clone()
                .or_else(|| target.charset.clone()),
        });
    }

    out
}

fn index_differences(source: &IndexInfo, target: &IndexInfo) -> Vec<FieldChange> {
    let mut out = Vec::new();
    if source.columns != target.columns {
        out.push(FieldChange {
            field: "字段".into(),
            source: Some(source.columns.join(", ")),
            target: Some(target.columns.join(", ")),
        });
    }
    if source.is_unique != target.is_unique {
        out.push(FieldChange {
            field: "唯一".into(),
            source: Some(if source.is_unique { "是" } else { "否" }.into()),
            target: Some(if target.is_unique { "是" } else { "否" }.into()),
        });
    }
    out
}

fn fk_differences(source: &ForeignKeyInfo, target: &ForeignKeyInfo) -> Vec<FieldChange> {
    let mut out = Vec::new();
    if source.columns != target.columns {
        out.push(FieldChange {
            field: "字段".into(),
            source: Some(source.columns.join(", ")),
            target: Some(target.columns.join(", ")),
        });
    }
    if source.ref_table != target.ref_table || source.ref_columns != target.ref_columns {
        out.push(FieldChange {
            field: "引用".into(),
            source: Some(format!("{}({})", source.ref_table, source.ref_columns.join(", "))),
            target: Some(format!("{}({})", target.ref_table, target.ref_columns.join(", "))),
        });
    }
    if source.on_delete != target.on_delete {
        out.push(FieldChange {
            field: "ON DELETE".into(),
            source: Some(source.on_delete.clone()),
            target: Some(target.on_delete.clone()),
        });
    }
    if source.on_update != target.on_update {
        out.push(FieldChange {
            field: "ON UPDATE".into(),
            source: Some(source.on_update.clone()),
            target: Some(target.on_update.clone()),
        });
    }
    out
}

/// Compare generation expressions loosely. Engines differ in identifier
/// quoting, whitespace, redundant parentheses and cast annotations: the same
/// `amount * 2` comes back as ``(`amount` * 2)`` from MySQL and as
/// `(amount * (2)::numeric)` from PostgreSQL.
/// Whitespace/quote insensitive text used when comparing constraint bodies.
fn normalize_type_text(value: &str) -> String {
    value
        .to_lowercase()
        .replace('"', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_generation(value: Option<&str>) -> String {
    let Some(raw) = value else {
        return String::new();
    };
    let mut text = raw.to_lowercase().replace('`', "").replace('"', "");
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for _ in 0..16 {
        let next = strip_single_token_parens(&strip_outer_parens(&text));
        if next == text {
            break;
        }
        text = next;
    }
    text
}

/// Remove the parentheses of a fully wrapped expression, e.g. `(a * b)` -> `a * b`.
fn strip_outer_parens(text: &str) -> String {
    let trimmed = text.trim();
    if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
        return trimmed.to_string();
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return trimmed.to_string();
                }
            }
            _ => {}
        }
    }
    if depth == 0 {
        inner.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Remove parentheses that wrap a single token, e.g. `a * (2)` -> `a * 2`.
fn strip_single_token_parens(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '(' {
            let mut depth = 0i32;
            let mut close = None;
            for (offset, ch) in chars[i..].iter().enumerate() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(i + offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = close {
                let inner: String = chars[i + 1..end].iter().collect();
                if is_single_token(&inner) {
                    out.push_str(inner.trim());
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_single_token(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        return true;
    }
    trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn normalize_default(value: &str) -> String {
    let v = value.trim();
    let trimmed = v.trim_start_matches('(').trim_end_matches(')');
    let lower = trimmed.to_lowercase();
    if lower.is_empty() || lower == "null" {
        return String::new();
    }
    // `CURRENT_TIMESTAMP`, `CURRENT_TIMESTAMP(6)`, `now()` and `localtimestamp`
    // all mean "the current time" on some engine.
    let head = lower.split('(').next().unwrap_or("").trim();
    if matches!(head, "current_timestamp" | "now" | "localtimestamp" | "localtime") {
        return "current_timestamp".to_string();
    }
    lower
}

fn is_boolean_family(column: &ColumnInfo) -> bool {
    matches!(
        column.data_type.to_ascii_uppercase().as_str(),
        "BOOLEAN" | "BOOL" | "TINYINT"
    )
}

fn boolean_literal(value: &str) -> Option<bool> {
    let lower = value.trim().trim_matches('\'').to_lowercase();
    match lower.as_str() {
        "1" | "true" | "b'1'" | "t" | "yes" => Some(true),
        "0" | "false" | "b'0'" | "f" | "no" => Some(false),
        _ => None,
    }
}

/// Defaults are compared semantically: MySQL spells a boolean default as `1`
/// where PostgreSQL writes `true`, and both must be treated as the same default.
fn defaults_match(
    source: &ColumnInfo,
    target: &ColumnInfo,
    source_default: &str,
    target_default: &str,
) -> bool {
    if source_default == target_default {
        return true;
    }
    if is_boolean_family(source) || is_boolean_family(target) {
        if let (Some(a), Some(b)) = (
            boolean_literal(source_default),
            boolean_literal(target_default),
        ) {
            return a == b;
        }
    }
    normalize_default(source_default) == normalize_default(target_default)
}
