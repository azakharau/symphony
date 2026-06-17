use libsql::{Row, params};

use crate::state::{
    SelfDefectOccurrenceRecord, SelfDefectRecord, SelfDefectRelationMode, SelfDefectResolutionState,
};

use super::{SqliteStore, StorageError, collect_rows, optional_row};

impl SqliteStore {
    pub async fn record_self_defect_occurrence(
        &self,
        occurrence: &SelfDefectOccurrenceRecord,
    ) -> Result<SelfDefectRecord, StorageError> {
        if let Some(existing) = self
            .open_self_defect_by_fingerprint(&occurrence.fingerprint)
            .await?
        {
            self.conn
                .execute(
                    r#"
                    UPDATE self_defect_registry
                    SET defect_kind = ?2,
                        category = ?3,
                        severity = ?4,
                        source_project_id = ?5,
                        source_issue_id = ?6,
                        source_issue_identifier = ?7,
                        source_session_id = ?8,
                        source_process_id = ?9,
                        occurrence_count = occurrence_count + 1,
                        last_seen_at = CURRENT_TIMESTAMP,
                        latest_evidence_summary = ?10,
                        relation_mode = ?11
                    WHERE registry_id = ?1
                    "#,
                    params![
                        existing.registry_id.as_str(),
                        occurrence.defect_kind.as_str(),
                        occurrence.category.as_str(),
                        occurrence.severity.as_str(),
                        occurrence.source_project_id.as_str(),
                        occurrence.source_issue_id.as_str(),
                        occurrence.source_issue_identifier.as_str(),
                        occurrence.source_session_id.as_deref(),
                        occurrence.source_process_id,
                        bounded_summary(&occurrence.latest_evidence_summary).as_str(),
                        occurrence.relation_mode.as_str(),
                    ],
                )
                .await?;
            return self
                .self_defect_by_registry_id(&existing.registry_id)
                .await?
                .ok_or_else(|| StorageError::Invariant("updated self-defect row missing".into()));
        }

        let registry_id = format!("{}:{}", occurrence.fingerprint, occurrence.managed_issue_id);
        self.conn
            .execute(
                r#"
                INSERT INTO self_defect_registry (
                    registry_id,
                    fingerprint,
                    defect_kind,
                    category,
                    severity,
                    initial_routing_decision,
                    source_project_id,
                    source_issue_id,
                    source_issue_identifier,
                    source_session_id,
                    source_process_id,
                    managed_issue_id,
                    managed_issue_identifier,
                    occurrence_count,
                    latest_evidence_summary,
                    resolution_state,
                    relation_mode
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, 'open', ?15)
                "#,
                params![
                    registry_id.as_str(),
                    occurrence.fingerprint.as_str(),
                    occurrence.defect_kind.as_str(),
                    occurrence.category.as_str(),
                    occurrence.severity.as_str(),
                    occurrence.initial_routing_decision.as_str(),
                    occurrence.source_project_id.as_str(),
                    occurrence.source_issue_id.as_str(),
                    occurrence.source_issue_identifier.as_str(),
                    occurrence.source_session_id.as_deref(),
                    occurrence.source_process_id,
                    occurrence.managed_issue_id.as_str(),
                    occurrence.managed_issue_identifier.as_str(),
                    bounded_summary(&occurrence.latest_evidence_summary).as_str(),
                    occurrence.relation_mode.as_str(),
                ],
            )
            .await?;
        self.self_defect_by_registry_id(&registry_id)
            .await?
            .ok_or_else(|| StorageError::Invariant("inserted self-defect row missing".into()))
    }

    pub async fn open_self_defect_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> Result<Option<SelfDefectRecord>, StorageError> {
        let sql = self_defect_select_sql("WHERE fingerprint = ?1 AND resolution_state = 'open'");
        let mut rows = self.conn.query(&sql, params![fingerprint]).await?;
        optional_row(&mut rows, self_defect_from_row).await
    }

    pub async fn self_defects_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> Result<Vec<SelfDefectRecord>, StorageError> {
        let sql = self_defect_select_sql("WHERE fingerprint = ?1 ORDER BY first_seen_at ASC");
        let mut rows = self.conn.query(&sql, params![fingerprint]).await?;
        collect_rows(&mut rows, self_defect_from_row).await
    }

    pub async fn mark_self_defect_managed_issue_resolved(
        &self,
        managed_issue_id: &str,
        resolution: SelfDefectResolutionState,
    ) -> Result<u64, StorageError> {
        if resolution == SelfDefectResolutionState::Open {
            return Ok(0);
        }
        Ok(self
            .conn
            .execute(
                r#"
                UPDATE self_defect_registry
                SET resolution_state = ?2,
                    last_seen_at = CURRENT_TIMESTAMP
                WHERE managed_issue_id = ?1 AND resolution_state = 'open'
                "#,
                params![managed_issue_id, resolution.as_str()],
            )
            .await?)
    }

    async fn self_defect_by_registry_id(
        &self,
        registry_id: &str,
    ) -> Result<Option<SelfDefectRecord>, StorageError> {
        let sql = self_defect_select_sql("WHERE registry_id = ?1");
        let mut rows = self.conn.query(&sql, params![registry_id]).await?;
        optional_row(&mut rows, self_defect_from_row).await
    }
}

fn self_defect_select_sql(clause: &str) -> String {
    format!(
        r#"
        SELECT registry_id,
               fingerprint,
               defect_kind,
               category,
               severity,
               initial_routing_decision,
               source_project_id,
               source_issue_id,
               source_issue_identifier,
               source_session_id,
               source_process_id,
               managed_issue_id,
               managed_issue_identifier,
               occurrence_count,
               first_seen_at,
               last_seen_at,
               latest_evidence_summary,
               resolution_state,
               relation_mode
        FROM self_defect_registry
        {clause}
        "#
    )
}

fn self_defect_from_row(row: &Row) -> Result<SelfDefectRecord, StorageError> {
    let resolution_state: String = row.get(17)?;
    let relation_mode: String = row.get(18)?;
    let source_process_id = row
        .get::<Option<i64>>(10)?
        .and_then(|value| u32::try_from(value).ok());

    Ok(SelfDefectRecord {
        registry_id: row.get(0)?,
        fingerprint: row.get(1)?,
        defect_kind: row.get(2)?,
        category: row.get(3)?,
        severity: row.get(4)?,
        initial_routing_decision: row.get(5)?,
        source_project_id: row.get(6)?,
        source_issue_id: row.get(7)?,
        source_issue_identifier: row.get(8)?,
        source_session_id: row.get(9)?,
        source_process_id,
        managed_issue_id: row.get(11)?,
        managed_issue_identifier: row.get(12)?,
        occurrence_count: row.get::<i64>(13)?.max(0) as u32,
        first_seen_at: row.get(14)?,
        last_seen_at: row.get(15)?,
        latest_evidence_summary: row.get(16)?,
        resolution_state: parse_resolution_state(&resolution_state)?,
        relation_mode: parse_relation_mode(&relation_mode)?,
    })
}

fn parse_resolution_state(input: &str) -> Result<SelfDefectResolutionState, StorageError> {
    match input {
        "open" => Ok(SelfDefectResolutionState::Open),
        "done" => Ok(SelfDefectResolutionState::Done),
        "canceled" => Ok(SelfDefectResolutionState::Canceled),
        other => Err(StorageError::Invariant(format!(
            "unknown self-defect resolution state `{other}`"
        ))),
    }
}

fn parse_relation_mode(input: &str) -> Result<SelfDefectRelationMode, StorageError> {
    match input {
        "blocking" => Ok(SelfDefectRelationMode::Blocking),
        "related_only" => Ok(SelfDefectRelationMode::RelatedOnly),
        other => Err(StorageError::Invariant(format!(
            "unknown self-defect relation mode `{other}`"
        ))),
    }
}

fn bounded_summary(input: &str) -> String {
    const MAX_BYTES: usize = 2048;
    let trimmed = input.trim();
    if trimmed.len() <= MAX_BYTES {
        return trimmed.to_string();
    }

    let mut end = MAX_BYTES;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::state::{SelfDefectRelationMode, SelfDefectResolutionState};

    use super::*;

    #[tokio::test]
    async fn self_defect_migration_records_and_dedupes_same_fingerprint() {
        let store = test_store().await;

        let first = store
            .record_self_defect_occurrence(&occurrence(
                "fingerprint-a",
                "source-a",
                "SYM-1",
                "managed-a",
                "SYM-DEFECT-1",
            ))
            .await
            .expect("first occurrence");
        let second = store
            .record_self_defect_occurrence(&occurrence(
                "fingerprint-a",
                "source-b",
                "SYM-2",
                "managed-a",
                "SYM-DEFECT-1",
            ))
            .await
            .expect("second occurrence");

        assert_eq!(first.occurrence_count, 1);
        assert_eq!(second.registry_id, first.registry_id);
        assert_eq!(second.occurrence_count, 2);
        assert_eq!(second.source_issue_id, "source-b");
        assert_eq!(second.relation_mode, SelfDefectRelationMode::Blocking);
        assert_eq!(
            store
                .self_defects_by_fingerprint("fingerprint-a")
                .await
                .expect("query")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn resolved_self_defect_reopened_occurrence_gets_fresh_active_row() {
        let store = test_store().await;
        let first = store
            .record_self_defect_occurrence(&occurrence(
                "fingerprint-b",
                "source-a",
                "SYM-1",
                "managed-old",
                "SYM-DEFECT-OLD",
            ))
            .await
            .expect("first occurrence");

        let changed = store
            .mark_self_defect_managed_issue_resolved("managed-old", SelfDefectResolutionState::Done)
            .await
            .expect("resolve old");
        assert_eq!(changed, 1);
        assert!(
            store
                .open_self_defect_by_fingerprint("fingerprint-b")
                .await
                .expect("open lookup")
                .is_none()
        );

        let reopened = store
            .record_self_defect_occurrence(&occurrence(
                "fingerprint-b",
                "source-c",
                "SYM-3",
                "managed-new",
                "SYM-DEFECT-NEW",
            ))
            .await
            .expect("reopened occurrence");

        assert_ne!(reopened.registry_id, first.registry_id);
        assert_eq!(reopened.occurrence_count, 1);
        assert_eq!(reopened.resolution_state, SelfDefectResolutionState::Open);
        assert_eq!(
            store
                .self_defects_by_fingerprint("fingerprint-b")
                .await
                .expect("query")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn cleanup_retains_unresolved_self_defect_rows() {
        let store = test_store().await;
        store
            .record_self_defect_occurrence(&occurrence(
                "fingerprint-c",
                "source-a",
                "SYM-1",
                "managed-open",
                "SYM-DEFECT-OPEN",
            ))
            .await
            .expect("open occurrence");

        let report = store
            .cleanup_runtime_state(Duration::from_secs(0))
            .await
            .expect("cleanup");

        assert_eq!(report.self_defects_deleted, 0);
        assert!(
            store
                .open_self_defect_by_fingerprint("fingerprint-c")
                .await
                .expect("open lookup")
                .is_some()
        );
    }

    async fn test_store() -> SqliteStore {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runtime.sqlite3");
        let store = SqliteStore::open(path).await.expect("open");
        store.migrate().await.expect("migrate");
        std::mem::forget(dir);
        store
    }

    fn occurrence(
        fingerprint: &str,
        source_issue_id: &str,
        source_identifier: &str,
        managed_issue_id: &str,
        managed_identifier: &str,
    ) -> SelfDefectOccurrenceRecord {
        SelfDefectOccurrenceRecord {
            fingerprint: fingerprint.into(),
            defect_kind: "malformed_handoff".into(),
            category: "runtime".into(),
            severity: "blocking".into(),
            initial_routing_decision: "managed_self_defect".into(),
            source_project_id: "symphony".into(),
            source_issue_id: source_issue_id.into(),
            source_issue_identifier: source_identifier.into(),
            source_session_id: Some("oc-session".into()),
            source_process_id: Some(42),
            managed_issue_id: managed_issue_id.into(),
            managed_issue_identifier: managed_identifier.into(),
            latest_evidence_summary: "bounded evidence summary".into(),
            relation_mode: SelfDefectRelationMode::Blocking,
        }
    }
}
