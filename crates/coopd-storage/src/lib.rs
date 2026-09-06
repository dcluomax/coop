//! # coopd-storage
//!
//! Persistent storage for `coopd` using `redb`.
//!
//! Stores Hens, durable jobs and episodic memory.

#![warn(missing_docs)]

use std::path::Path;
use std::sync::Arc;

use coopd_core::{Hen, HenId, Job, JobQuery, MemoryEntry};
use redb::{Database, ReadableTable, TableDefinition};
use thiserror::Error;

/// Storage errors.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Underlying redb error.
    #[error("redb: {0}")]
    Redb(#[from] redb::Error),
    /// Database open/transaction error.
    #[error("db: {0}")]
    Db(String),
    /// Serialization failed.
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    /// Row not found.
    #[error("not found: {0}")]
    NotFound(String),
}

impl From<redb::DatabaseError> for StorageError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::TransactionError> for StorageError {
    fn from(e: redb::TransactionError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::TableError> for StorageError {
    fn from(e: redb::TableError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::StorageError> for StorageError {
    fn from(e: redb::StorageError) -> Self {
        Self::Db(e.to_string())
    }
}
impl From<redb::CommitError> for StorageError {
    fn from(e: redb::CommitError) -> Self {
        Self::Db(e.to_string())
    }
}

type Result<T> = std::result::Result<T, StorageError>;

const HENS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("hens_v1");
const JOBS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("jobs_v1");
const MEMORIES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("memories_v1");

/// NUL separator between a Hen id and an episode id in a memory key. Keeps all
/// of a Hen's episodes in a contiguous, chronologically-ordered key range
/// (episode ids are UUIDv7).
const MEM_SEP: char = '\u{0}';

/// Build the lo/hi bounds (inclusive lo, exclusive hi) for a Hen's episode
/// key range: `"<hen>\0"` ..= `"<hen>\u{1}"`.
fn mem_range_bounds(hen_id: &HenId) -> (String, String) {
    let h = hen_id.as_str();
    (format!("{h}{MEM_SEP}"), format!("{h}\u{1}"))
}

/// Storage handle (thread-safe, cloneable).
#[derive(Debug, Clone)]
pub struct Store {
    db: Arc<Database>,
}

impl Store {
    /// Open or create the database at `path`.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if the redb file cannot be created/opened
    /// (e.g. permission denied, corrupted file, locked by another process),
    /// or if the initial write transaction creating the hens/jobs tables
    /// fails to commit.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        let db = Database::create(path_ref).map_err(StorageError::from)?;
        // H1: redb file holds operational metadata (hen configs, job
        // history); confine to owner only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path_ref, std::fs::Permissions::from_mode(0o600));
        }
        // Ensure tables exist.
        let write = db.begin_write()?;
        {
            let _ = write.open_table(HENS_TABLE)?;
            let _ = write.open_table(JOBS_TABLE)?;
            let _ = write.open_table(MEMORIES_TABLE)?;
        }
        write.commit()?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Persist (insert or update) a Hen.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON encoding of `hen` fails or the redb
    /// write transaction cannot be committed.
    pub fn put_hen(&self, hen: &Hen) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(HENS_TABLE)?;
            let key = hen.id.as_str().to_string();
            let value = serde_json::to_vec(hen)?;
            table.insert(key.as_str(), value.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    /// Fetch a Hen by ID.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if no row exists for `id`, or a
    /// transport / deserialization error if the underlying redb read fails
    /// or the stored bytes are not valid JSON.
    pub fn get_hen(&self, id: &HenId) -> Result<Hen> {
        let read = self.db.begin_read()?;
        let table = read.open_table(HENS_TABLE)?;
        let val = table
            .get(id.as_str())?
            .ok_or_else(|| StorageError::NotFound(id.to_string()))?;
        let hen: Hen = serde_json::from_slice(val.value())?;
        Ok(hen)
    }

    /// List all Hens.
    ///
    /// # Errors
    ///
    /// Returns an error if a read transaction cannot be opened or if any
    /// row fails to deserialize.
    pub fn list_hens(&self) -> Result<Vec<Hen>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(HENS_TABLE)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_k, v) = entry?;
            let hen: Hen = serde_json::from_slice(v.value())?;
            out.push(hen);
        }
        Ok(out)
    }

    /// Delete a Hen by ID. Returns `Ok(true)` if removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the redb write transaction cannot be committed.
    pub fn delete_hen(&self, id: &HenId) -> Result<bool> {
        let write = self.db.begin_write()?;
        let removed = {
            let mut table = write.open_table(HENS_TABLE)?;
            table.remove(id.as_str())?.is_some()
        };
        write.commit()?;
        Ok(removed)
    }

    /// Persist (insert or update) a Job.
    ///
    /// # Errors
    ///
    /// Returns an error if `job` cannot be JSON-encoded or if the redb
    /// write transaction fails to commit.
    pub fn put_job(&self, job: &Job) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(JOBS_TABLE)?;
            let value = serde_json::to_vec(job)?;
            table.insert(job.id.as_str(), value.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    /// Persist a Hen and its job together, so execution state cannot diverge.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON encoding or the shared redb transaction fails.
    pub fn put_hen_and_job(&self, hen: &Hen, job: &Job) -> Result<()> {
        let hen_value = serde_json::to_vec(hen)?;
        let job_value = serde_json::to_vec(job)?;
        let write = self.db.begin_write()?;
        {
            let mut hens = write.open_table(HENS_TABLE)?;
            let mut jobs = write.open_table(JOBS_TABLE)?;
            hens.insert(hen.id.as_str(), hen_value.as_slice())?;
            jobs.insert(job.id.as_str(), job_value.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    /// Fetch a job by ID.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if no row exists for `id`, or a
    /// deserialization error if the stored bytes are corrupt.
    pub fn get_job(&self, id: &str) -> Result<Job> {
        let read = self.db.begin_read()?;
        let table = read.open_table(JOBS_TABLE)?;
        let val = table
            .get(id)?
            .ok_or_else(|| StorageError::NotFound(id.to_string()))?;
        Ok(serde_json::from_slice(val.value())?)
    }

    /// List all jobs, optionally filtering by Hen.
    ///
    /// # Errors
    ///
    /// Returns an error if a read transaction cannot be opened or if any
    /// row fails to deserialize.
    pub fn list_jobs(&self, hen_id: Option<&HenId>) -> Result<Vec<Job>> {
        self.query_jobs(&JobQuery {
            hen_id: hen_id.cloned(),
            ..JobQuery::default()
        })
    }

    /// Read a filtered history in UUIDv7 creation order, stopping at the limit.
    ///
    /// Unlike loading and sorting the entire history, a bounded recent page
    /// only materializes matching records from the requested end of the table.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or decoding a visited record fails.
    pub fn query_jobs(&self, query: &JobQuery) -> Result<Vec<Job>> {
        if query.limit == Some(0) {
            return Ok(Vec::new());
        }
        let read = self.db.begin_read()?;
        let table = read.open_table(JOBS_TABLE)?;
        let mut entries = table.iter()?;
        let search = query.search.as_ref().map(|s| s.to_lowercase());
        let mut skip = query.offset;
        let mut out = Vec::new();
        loop {
            let entry = if query.descending {
                entries.next_back()
            } else {
                entries.next()
            };
            let Some(entry) = entry else { break };
            let (_k, v) = entry?;
            let job: Job = serde_json::from_slice(v.value())?;
            if let Some(h) = &query.hen_id
                && &job.hen_id != h
            {
                continue;
            }
            if query.status.is_some_and(|status| job.status != status) {
                continue;
            }
            if let Some(search) = &search
                && ![
                    Some(job.id.as_str()),
                    Some(job.hen_id.as_str()),
                    Some(job.prompt.as_str()),
                    job.result.as_deref(),
                    job.error.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|text| text.to_lowercase().contains(search))
            {
                continue;
            }
            if skip > 0 {
                skip -= 1;
                continue;
            }
            out.push(job);
            if query.limit.is_some_and(|limit| out.len() >= limit) {
                break;
            }
        }
        Ok(out)
    }

    /// Persist an episodic [`MemoryEntry`].
    ///
    /// # Errors
    ///
    /// Returns an error if `entry` cannot be JSON-encoded or if the redb
    /// write transaction fails to commit.
    pub fn put_memory(&self, entry: &MemoryEntry) -> Result<()> {
        let key = format!("{}{MEM_SEP}{}", entry.hen_id.as_str(), entry.id);
        let value = serde_json::to_vec(entry)?;
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(MEMORIES_TABLE)?;
            table.insert(key.as_str(), value.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    /// List a Hen's episodic memories in chronological order (oldest first).
    /// When `limit` is `Some(n)`, only the `n` most recent episodes are
    /// returned (still oldest-first).
    ///
    /// # Errors
    ///
    /// Returns an error if a read transaction cannot be opened or if any
    /// stored episode fails to deserialize.
    pub fn list_memories(&self, hen_id: &HenId, limit: Option<usize>) -> Result<Vec<MemoryEntry>> {
        let (lo, hi) = mem_range_bounds(hen_id);
        let read = self.db.begin_read()?;
        let table = read.open_table(MEMORIES_TABLE)?;
        let mut out = Vec::new();
        for entry in table.range::<&str>(lo.as_str()..hi.as_str())? {
            let (_k, v) = entry?;
            let mem: MemoryEntry = serde_json::from_slice(v.value())?;
            out.push(mem);
        }
        if let Some(n) = limit
            && out.len() > n
        {
            out.drain(0..out.len() - n);
        }
        Ok(out)
    }

    /// Prune a Hen's episodes recorded strictly before `cutoff`. Returns the
    /// number of episodes removed.
    ///
    /// # Errors
    ///
    /// Returns an error if a transaction cannot be opened/committed or if a
    /// stored episode fails to deserialize.
    pub fn prune_memories(&self, hen_id: &HenId, cutoff: time::OffsetDateTime) -> Result<usize> {
        let (lo, hi) = mem_range_bounds(hen_id);
        let write = self.db.begin_write()?;
        let mut removed = 0usize;
        {
            let mut table = write.open_table(MEMORIES_TABLE)?;
            let mut stale: Vec<String> = Vec::new();
            for entry in table.range::<&str>(lo.as_str()..hi.as_str())? {
                let (k, v) = entry?;
                let mem: MemoryEntry = serde_json::from_slice(v.value())?;
                if mem.at < cutoff {
                    stale.push(k.value().to_string());
                }
            }
            for key in stale {
                if table.remove(key.as_str())?.is_some() {
                    removed += 1;
                }
            }
        }
        write.commit()?;
        Ok(removed)
    }

    /// Delete all of a Hen's episodic memories. Returns the number removed.
    ///
    /// # Errors
    ///
    /// Returns an error if a transaction cannot be opened/committed or if a
    /// stored key cannot be read.
    pub fn delete_memories(&self, hen_id: &HenId) -> Result<usize> {
        let (lo, hi) = mem_range_bounds(hen_id);
        let write = self.db.begin_write()?;
        let mut removed = 0usize;
        {
            let mut table = write.open_table(MEMORIES_TABLE)?;
            let keys: Vec<String> = {
                let mut ks = Vec::new();
                for entry in table.range::<&str>(lo.as_str()..hi.as_str())? {
                    let (k, _v) = entry?;
                    ks.push(k.value().to_string());
                }
                ks
            };
            for key in keys {
                if table.remove(key.as_str())?.is_some() {
                    removed += 1;
                }
            }
        }
        write.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coopd_core::{AgentManifest, CoopId};
    use tempfile::tempdir;

    fn make_hen(name: &str) -> Hen {
        let coop = CoopId::new("alice.coop").unwrap();
        let id = HenId::new(&coop, name).unwrap();
        let manifest = AgentManifest::minimal(name.to_string());
        Hen::new(id, manifest)
    }

    #[test]
    fn put_get_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let store = Store::open(&path).unwrap();
        let hen = make_hen("aria");
        store.put_hen(&hen).unwrap();
        let loaded = store.get_hen(&hen.id).unwrap();
        assert_eq!(loaded.id, hen.id);
        assert_eq!(loaded.state, hen.state);
    }

    #[test]
    fn list_and_delete() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("t.redb")).unwrap();
        store.put_hen(&make_hen("aria")).unwrap();
        store.put_hen(&make_hen("bolt")).unwrap();
        store.put_hen(&make_hen("coda")).unwrap();
        assert_eq!(store.list_hens().unwrap().len(), 3);
        let id = make_hen("bolt").id;
        assert!(store.delete_hen(&id).unwrap());
        assert_eq!(store.list_hens().unwrap().len(), 2);
        assert!(store.get_hen(&id).is_err());
    }

    #[test]
    fn job_query_filters_before_paging_and_preserves_legacy_order() {
        use coopd_core::JobStatus;
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("jobs.redb")).unwrap();
        let aria = make_hen("aria").id;
        let bolt = make_hen("bolt").id;
        for (id, hen, status, prompt) in [
            ("a", &aria, JobStatus::Failed, "First repair"),
            ("b", &bolt, JobStatus::Failed, "Other repair"),
            ("c", &aria, JobStatus::Done, "Completed repair"),
            ("d", &aria, JobStatus::Failed, "Last REPAIR"),
        ] {
            let mut job = Job::new(hen.clone(), prompt.into());
            job.id = id.into();
            job.status = status;
            store.put_job(&job).unwrap();
        }
        let all = store.list_jobs(None).unwrap();
        assert_eq!(
            all.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        let mut query = JobQuery {
            hen_id: Some(aria),
            status: Some(JobStatus::Failed),
            search: Some("REPAIR".into()),
            descending: true,
            limit: Some(1),
            offset: 1,
        };
        let page = store.query_jobs(&query).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "a");
        query.offset = usize::MAX;
        assert!(store.query_jobs(&query).unwrap().is_empty());
        query.offset = 0;
        query.search = Some("no match".into());
        assert!(store.query_jobs(&query).unwrap().is_empty());
    }

    #[test]
    fn job_search_includes_results_and_errors() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("search.redb")).unwrap();
        let mut job = Job::new(make_hen("aria").id, "hello".into());
        job.mark_failed("Vault is locked".into());
        store.put_job(&job).unwrap();
        let query = JobQuery {
            search: Some("VAULT".into()),
            ..JobQuery::default()
        };
        assert_eq!(store.query_jobs(&query).unwrap().len(), 1);
        job.mark_done("Vault is now unlocked".into());
        job.error = None;
        store.put_job(&job).unwrap();
        assert_eq!(store.query_jobs(&query).unwrap().len(), 1);
    }

    #[test]
    fn hen_and_job_execution_state_persist_together() {
        use coopd_core::{HenState, JobStatus};
        let dir = tempdir().unwrap();
        let path = dir.path().join("atomic.redb");
        let store = Store::open(&path).unwrap();
        let mut hen = make_hen("aria");
        hen.state = HenState::Working;
        let mut job = Job::new(hen.id.clone(), "work".into());
        job.mark_running();
        store.put_hen_and_job(&hen, &job).unwrap();
        drop(store);

        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.get_hen(&hen.id).unwrap().state, HenState::Working);
        assert_eq!(
            reopened.get_job(&job.id).unwrap().status,
            JobStatus::Running
        );
    }

    fn mem(
        hen: &HenId,
        id: &str,
        at: time::OffsetDateTime,
        outcome: coopd_core::MemoryOutcome,
    ) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            hen_id: hen.clone(),
            job_id: "j".to_string(),
            at,
            prompt: "p".to_string(),
            summary: "s".to_string(),
            turns: 1,
            outcome,
        }
    }

    #[test]
    fn memory_order_limit_and_isolation() {
        use coopd_core::MemoryOutcome::Done;
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("m.redb")).unwrap();
        let aria = make_hen("aria").id;
        let bolt = make_hen("bolt").id;
        let t0 = time::OffsetDateTime::now_utc();
        // Lexicographic ids a<b<c stand in for UUIDv7 chronological order.
        store.put_memory(&mem(&aria, "a", t0, Done)).unwrap();
        store.put_memory(&mem(&aria, "b", t0, Done)).unwrap();
        store.put_memory(&mem(&aria, "c", t0, Done)).unwrap();
        store.put_memory(&mem(&bolt, "z", t0, Done)).unwrap();

        let all = store.list_memories(&aria, None).unwrap();
        assert_eq!(
            all.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"],
            "oldest-first ordering"
        );
        let recent = store.list_memories(&aria, Some(2)).unwrap();
        assert_eq!(
            recent.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["b", "c"],
            "limit keeps the most-recent N, oldest-first"
        );
        // Per-hen isolation: bolt's episode never bleeds into aria's range.
        assert_eq!(store.list_memories(&bolt, None).unwrap().len(), 1);
    }

    #[test]
    fn memory_prune_and_delete() {
        use coopd_core::MemoryOutcome::Done;
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("p.redb")).unwrap();
        let aria = make_hen("aria").id;
        let now = time::OffsetDateTime::now_utc();
        let old = now - time::Duration::days(30);
        store.put_memory(&mem(&aria, "old", old, Done)).unwrap();
        store.put_memory(&mem(&aria, "new", now, Done)).unwrap();

        let cutoff = now - time::Duration::days(7);
        assert_eq!(store.prune_memories(&aria, cutoff).unwrap(), 1);
        let kept = store.list_memories(&aria, None).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "new");

        assert_eq!(store.delete_memories(&aria).unwrap(), 1);
        assert!(store.list_memories(&aria, None).unwrap().is_empty());
    }
}
