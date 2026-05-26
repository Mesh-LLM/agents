use a2a::{A2AError, ListTasksRequest, ListTasksResponse, Task, TaskState};
use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;
use std::sync::Mutex;

pub struct PersistentTaskStore {
    connection: Mutex<Connection>,
}

impl PersistentTaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, A2AError> {
        let path = path.as_ref();
        ensure_parent_dir(path)?;
        let connection = Connection::open(path).map_err(sql_error("open task store"))?;
        configure_connection(&connection)?;
        migrate(&connection)?;
        recover_active_tasks(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
}

#[async_trait]
impl a2a_server::TaskStore for PersistentTaskStore {
    async fn create(&self, task: Task) -> Result<u64, A2AError> {
        let connection = self.lock_connection()?;
        insert_task(&connection, &task, 1).map_err(|error| {
            if is_constraint_violation(&error) {
                A2AError::internal("task already exists")
            } else {
                sql_error("create task")(error)
            }
        })?;
        Ok(1)
    }

    async fn update(&self, task: Task) -> Result<u64, A2AError> {
        let connection = self.lock_connection()?;
        let version = task_version(&connection, &task.id)?
            .ok_or_else(|| A2AError::task_not_found(&task.id))?
            + 1;
        update_task(&connection, &task, version)?;
        Ok(version)
    }

    async fn get(&self, task_id: &str) -> Result<Option<Task>, A2AError> {
        let connection = self.lock_connection()?;
        get_task(&connection, task_id)
    }

    async fn list(&self, req: &ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        let connection = self.lock_connection()?;
        let tasks = load_tasks(&connection)?;
        Ok(list_tasks(tasks.iter(), req))
    }
}

impl PersistentTaskStore {
    fn lock_connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, A2AError> {
        self.connection
            .lock()
            .map_err(|_| A2AError::internal("task store lock poisoned"))
    }
}

fn ensure_parent_dir(path: &Path) -> Result<(), A2AError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            A2AError::internal(format!("failed to create {}: {error}", parent.display()))
        })?;
    }
    Ok(())
}

fn configure_connection(connection: &Connection) -> Result<(), A2AError> {
    connection
        .execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            "#,
        )
        .map_err(sql_error("configure task store"))?;
    Ok(())
}

fn migrate(connection: &Connection) -> Result<(), A2AError> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY NOT NULL,
                context_id TEXT NOT NULL,
                state TEXT NOT NULL,
                status_timestamp TEXT,
                version INTEGER NOT NULL,
                task_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_context_id ON tasks(context_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state);
            "#,
        )
        .map_err(sql_error("migrate task store"))?;
    Ok(())
}

fn recover_active_tasks(connection: &Connection) -> Result<(), A2AError> {
    let records = load_task_records(connection)?;
    for mut record in records {
        if record.task.status.state.is_terminal() {
            continue;
        }
        record.task.status.state = TaskState::Failed;
        record.task.status.message = None;
        mark_interrupted_metadata(&mut record.task);
        update_task(connection, &record.task, record.version + 1)?;
    }
    Ok(())
}

fn insert_task(connection: &Connection, task: &Task, version: u64) -> rusqlite::Result<usize> {
    connection.execute(
        r#"
        INSERT INTO tasks (id, context_id, state, status_timestamp, version, task_json, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch())
        "#,
        task_params(task, version)?,
    )
}

fn update_task(connection: &Connection, task: &Task, version: u64) -> Result<(), A2AError> {
    let updated = connection
        .execute(
            r#"
            UPDATE tasks
            SET context_id = ?2,
                state = ?3,
                status_timestamp = ?4,
                version = ?5,
                task_json = ?6,
                updated_at = unixepoch()
            WHERE id = ?1
            "#,
            task_params(task, version).map_err(sql_error("encode task"))?,
        )
        .map_err(sql_error("update task"))?;
    if updated == 0 {
        return Err(A2AError::task_not_found(&task.id));
    }
    Ok(())
}

fn task_params(task: &Task, version: u64) -> rusqlite::Result<[Box<dyn rusqlite::ToSql>; 6]> {
    let state = serde_json::to_string(&task.status.state)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let task_json = serde_json::to_string(task)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let timestamp = task.status.timestamp.map(|value| value.to_rfc3339());
    Ok([
        Box::new(task.id.clone()),
        Box::new(task.context_id.clone()),
        Box::new(state),
        Box::new(timestamp),
        Box::new(version as i64),
        Box::new(task_json),
    ])
}

fn task_version(connection: &Connection, task_id: &str) -> Result<Option<u64>, A2AError> {
    connection
        .query_row(
            "SELECT version FROM tasks WHERE id = ?1",
            params![task_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|version| version.map(|value| value as u64))
        .map_err(sql_error("read task version"))
}

fn get_task(connection: &Connection, task_id: &str) -> Result<Option<Task>, A2AError> {
    connection
        .query_row(
            "SELECT task_json FROM tasks WHERE id = ?1",
            params![task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_error("read task"))?
        .map(|raw| decode_task(&raw))
        .transpose()
}

fn load_tasks(connection: &Connection) -> Result<Vec<Task>, A2AError> {
    Ok(load_task_records(connection)?
        .into_iter()
        .map(|record| record.task)
        .collect())
}

fn load_task_records(connection: &Connection) -> Result<Vec<TaskRecord>, A2AError> {
    let mut statement = connection
        .prepare("SELECT task_json, version FROM tasks ORDER BY id")
        .map_err(sql_error("prepare task list"))?;
    let rows = statement
        .query_map([], |row| {
            let task_json: String = row.get(0)?;
            let version: i64 = row.get(1)?;
            Ok((task_json, version as u64))
        })
        .map_err(sql_error("query task list"))?;

    let mut records = Vec::new();
    for row in rows {
        let (task_json, version) = row.map_err(sql_error("read task list row"))?;
        records.push(TaskRecord {
            task: decode_task(&task_json)?,
            version,
        });
    }
    Ok(records)
}

struct TaskRecord {
    task: Task,
    version: u64,
}

fn decode_task(raw: &str) -> Result<Task, A2AError> {
    serde_json::from_str(raw).map_err(|error| A2AError::internal(format!("decode task: {error}")))
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn sql_error(context: &'static str) -> impl FnOnce(rusqlite::Error) -> A2AError {
    move |error| A2AError::internal(format!("{context}: {error}"))
}

fn mark_interrupted_metadata(task: &mut Task) {
    let metadata = task.metadata.get_or_insert_with(Default::default);
    metadata.insert(
        "mesh_llm_recovery".to_string(),
        Value::Object(Map::from_iter([
            ("interrupted".to_string(), Value::Bool(true)),
            (
                "reason".to_string(),
                Value::String(
                    "mesh-llm restarted before this task reached a terminal state".into(),
                ),
            ),
        ])),
    );
}

fn list_tasks<'a>(
    tasks: impl Iterator<Item = &'a Task>,
    req: &ListTasksRequest,
) -> ListTasksResponse {
    let mut tasks: Vec<Task> = tasks
        .filter(|task| {
            req.context_id
                .as_ref()
                .is_none_or(|context_id| task.context_id == *context_id)
        })
        .filter(|task| {
            req.status
                .as_ref()
                .is_none_or(|status| task.status.state == *status)
        })
        .filter(|task| {
            req.status_timestamp_after
                .as_ref()
                .is_none_or(|after| task.status.timestamp.as_ref().is_none_or(|ts| ts > after))
        })
        .cloned()
        .collect();
    tasks.sort_by(|left, right| left.id.cmp(&right.id));

    let total_size = tasks.len();
    let page_size = req.page_size.filter(|size| *size > 0).unwrap_or(50) as usize;
    let start = req
        .page_token
        .as_deref()
        .and_then(|token| token.parse::<usize>().ok())
        .unwrap_or(0)
        .min(total_size);
    let end = (start + page_size).min(total_size);
    let next_page_token = if end < total_size {
        end.to_string()
    } else {
        String::new()
    };

    let tasks = tasks[start..end]
        .iter()
        .cloned()
        .map(|mut task| {
            apply_history_length(&mut task, req.history_length);
            if req.include_artifacts == Some(false) {
                task.artifacts = None;
            }
            task
        })
        .collect();

    ListTasksResponse {
        tasks,
        next_page_token,
        page_size: page_size as i32,
        total_size: total_size as i32,
    }
}

fn apply_history_length(task: &mut Task, history_length: Option<i32>) {
    let Some(history_length) = history_length else {
        return;
    };
    let Some(history) = &mut task.history else {
        return;
    };
    let history_length = history_length.max(0) as usize;
    if history_length == 0 {
        history.clear();
    } else if history.len() > history_length {
        *history = history[history.len() - history_length..].to_vec();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2a::{TaskState, TaskStatus};
    use a2a_server::TaskStore;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mesh-agents-a2a-task-store-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root.join("tasks.sqlite")
    }

    fn task(id: &str, context_id: &str, state: TaskState) -> Task {
        Task {
            id: id.to_string(),
            context_id: context_id.to_string(),
            status: TaskStatus {
                state,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: None,
            metadata: None,
        }
    }

    fn list_request() -> ListTasksRequest {
        ListTasksRequest {
            context_id: None,
            status: None,
            page_size: None,
            page_token: None,
            history_length: None,
            status_timestamp_after: None,
            include_artifacts: None,
            tenant: None,
        }
    }

    #[tokio::test]
    async fn persists_task_across_reopen() {
        let path = temp_path("persist");
        let store = PersistentTaskStore::open(&path).unwrap();
        store
            .create(task("task-1", "ctx-1", TaskState::Completed))
            .await
            .unwrap();

        let reopened = PersistentTaskStore::open(&path).unwrap();
        let restored = reopened.get("task-1").await.unwrap().unwrap();

        assert_eq!(restored.context_id, "ctx-1");
        assert_eq!(restored.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn marks_active_tasks_failed_on_reopen() {
        let path = temp_path("recover");
        let store = PersistentTaskStore::open(&path).unwrap();
        store
            .create(task("task-1", "ctx-1", TaskState::Working))
            .await
            .unwrap();

        let reopened = PersistentTaskStore::open(&path).unwrap();
        let restored = reopened.get("task-1").await.unwrap().unwrap();

        assert_eq!(restored.status.state, TaskState::Failed);
        assert_eq!(
            restored
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("mesh_llm_recovery"))
                .and_then(|value| value.get("interrupted")),
            Some(&Value::Bool(true))
        );
    }

    #[tokio::test]
    async fn lists_with_filters_and_pagination() {
        let path = temp_path("list");
        let store = PersistentTaskStore::open(&path).unwrap();
        store
            .create(task("a", "ctx-1", TaskState::Completed))
            .await
            .unwrap();
        store
            .create(task("b", "ctx-2", TaskState::Completed))
            .await
            .unwrap();

        let mut req = list_request();
        req.context_id = Some("ctx-1".to_string());
        req.page_size = Some(1);
        let page = store.list(&req).await.unwrap();

        assert_eq!(page.total_size, 1);
        assert_eq!(page.tasks[0].id, "a");
        assert_eq!(page.next_page_token, "");
    }
}
