use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::model::{DirectoryView, GroupView, GroupWithTasks, Snapshot, TaskCounts, TaskView};

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

#[derive(sqlx::FromRow)]
struct DirectoryRow {
    id: i64,
    name: String,
    explicit_deadline: Option<NaiveDate>,
    effective_deadline: Option<NaiveDate>,
    completed_task_count: i64,
}

impl Store {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(database_url)
            .await
            .context("failed to connect to PostgreSQL")?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .context("failed to apply database migrations")?;
        Ok(())
    }

    async fn notify(&self, event: &str) -> Result<()> {
        sqlx::query("SELECT pg_notify('actum_changed', $1)")
            .bind(event)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn directory_id(&self, path: &str) -> Result<Option<i64>> {
        let components = path_components(path);
        if components.is_empty() {
            return Ok(None);
        }

        let mut parent_id = None;
        for component in components {
            parent_id = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM directories WHERE parent_id IS NOT DISTINCT FROM $1 AND name = $2",
            )
            .bind(parent_id)
            .bind(&component)
            .fetch_optional(&self.pool)
            .await?;

            if parent_id.is_none() {
                return Ok(None);
            }
        }
        Ok(parent_id)
    }

    pub async fn ensure_directory(&self, path: &str, deadline: Option<NaiveDate>) -> Result<i64> {
        let components = path_components(path);
        if components.is_empty() {
            bail!("the virtual root directory cannot be created or updated");
        }

        let mut transaction = self.pool.begin().await?;
        let mut parent_id = None;
        for (index, component) in components.iter().enumerate() {
            let existing = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM directories WHERE parent_id IS NOT DISTINCT FROM $1 AND name = $2",
            )
            .bind(parent_id)
            .bind(component)
            .fetch_optional(&mut *transaction)
            .await?;

            let id = match existing {
                Some(id) => id,
                None => {
                    sqlx::query_scalar::<_, i64>(
                        "INSERT INTO directories (parent_id, name, deadline) VALUES ($1, $2, $3) RETURNING id",
                    )
                    .bind(parent_id)
                    .bind(component)
                    .bind(if index + 1 == components.len() {
                        deadline
                    } else {
                        None
                    })
                    .fetch_one(&mut *transaction)
                    .await?
                }
            };
            parent_id = Some(id);
        }

        let directory_id = parent_id.expect("non-empty directory path");
        if let Some(deadline) = deadline {
            sqlx::query("UPDATE directories SET deadline = $1 WHERE id = $2")
                .bind(deadline)
                .bind(directory_id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        self.notify("directory").await?;
        Ok(directory_id)
    }

    pub async fn create_group(
        &self,
        directory_path: &str,
        name: &str,
        deadline: Option<NaiveDate>,
        links: &[String],
    ) -> Result<GroupView> {
        let directory_id = self.require_directory(directory_path).await?;
        let group_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO task_groups (directory_id, name, deadline, links)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (directory_id, name) DO UPDATE SET
                deadline = COALESCE(EXCLUDED.deadline, task_groups.deadline),
                links = CASE WHEN cardinality(EXCLUDED.links) = 0 THEN task_groups.links ELSE EXCLUDED.links END
            RETURNING id
            "#,
        )
        .bind(directory_id)
        .bind(name)
        .bind(deadline)
        .bind(links)
        .fetch_one(&self.pool)
        .await?;
        self.notify("group").await?;
        self.group_by_id(group_id).await
    }

    pub async fn create_task(
        &self,
        directory_path: &str,
        group_name: Option<&str>,
        title: &str,
        deadline: Option<NaiveDate>,
        links: &[String],
    ) -> Result<TaskView> {
        let directory_id = self.require_directory(directory_path).await?;
        let group_id = match group_name {
            Some(name) => Some(self.require_group(directory_id, name).await?),
            None => None,
        };

        let task_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO tasks (directory_id, group_id, title, deadline, links)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (directory_id, title) DO UPDATE SET
                group_id = EXCLUDED.group_id,
                deadline = COALESCE(EXCLUDED.deadline, tasks.deadline),
                links = CASE WHEN cardinality(EXCLUDED.links) = 0 THEN tasks.links ELSE EXCLUDED.links END
            RETURNING id
            "#,
        )
        .bind(directory_id)
        .bind(group_id)
        .bind(title)
        .bind(deadline)
        .bind(links)
        .fetch_one(&self.pool)
        .await?;
        self.notify("task").await?;
        self.task_by_id(task_id).await
    }

    pub async fn complete_task(&self, task_id: i64, note: Option<&str>) -> Result<TaskView> {
        let updated = sqlx::query_scalar::<_, i64>(
            r#"
            UPDATE tasks
            SET status = 'completed', completed_at = now(), completion_note = $2
            WHERE id = $1
            RETURNING id
            "#,
        )
        .bind(task_id)
        .bind(note)
        .fetch_optional(&self.pool)
        .await?;

        if updated.is_none() {
            bail!("task #{task_id} does not exist");
        }
        self.notify("task").await?;
        self.task_by_id(task_id).await
    }

    pub async fn delete_task(&self, task_id: i64) -> Result<TaskView> {
        let task = self.task_by_id(task_id).await?;
        let deleted = sqlx::query_scalar::<_, i64>("DELETE FROM tasks WHERE id = $1 RETURNING id")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await?;

        if deleted.is_none() {
            bail!("task #{task_id} does not exist");
        }
        self.notify("task").await?;
        Ok(task)
    }

    pub async fn move_task(
        &self,
        task_id: i64,
        directory_path: &str,
        group_name: Option<&str>,
    ) -> Result<TaskView> {
        let directory_id = self.require_directory(directory_path).await?;
        let group_id = match group_name {
            Some(name) => Some(self.require_group(directory_id, name).await?),
            None => None,
        };
        let updated = sqlx::query_scalar::<_, i64>(
            "UPDATE tasks SET directory_id = $2, group_id = $3 WHERE id = $1 RETURNING id",
        )
        .bind(task_id)
        .bind(directory_id)
        .bind(group_id)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            bail!("task #{task_id} does not exist");
        }
        self.notify("task").await?;
        self.task_by_id(task_id).await
    }

    pub async fn set_task_deadline(&self, task_id: i64, deadline: NaiveDate) -> Result<TaskView> {
        let updated = sqlx::query_scalar::<_, i64>(
            "UPDATE tasks SET deadline = $2 WHERE id = $1 RETURNING id",
        )
        .bind(task_id)
        .bind(deadline)
        .fetch_optional(&self.pool)
        .await?;

        if updated.is_none() {
            bail!("task #{task_id} does not exist");
        }
        self.notify("task").await?;
        self.task_by_id(task_id).await
    }

    pub async fn snapshot(&self, path: &str, include_completed: bool) -> Result<Snapshot> {
        let normalized_path = normalize_path(path);
        let include_completed = include_completed || is_finished_archive(&normalized_path);
        let directory_id = self.directory_id(&normalized_path).await?;
        if normalized_path != "/" && directory_id.is_none() {
            bail!("directory {normalized_path} does not exist");
        }

        let directory_rows = sqlx::query_as::<_, DirectoryRow>(
            r#"
            SELECT child.id,
                   child.name,
                   child.deadline AS explicit_deadline,
                   COALESCE(task_rollup.deadline, inherited.deadline) AS effective_deadline,
                   task_rollup.completed_task_count
            FROM directories child
            LEFT JOIN LATERAL (
                WITH RECURSIVE ancestry AS (
                    SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                    FROM directories d WHERE d.id = child.id
                    UNION ALL
                    SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                    FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                )
                SELECT deadline
                FROM ancestry
                WHERE deadline IS NOT NULL
                ORDER BY depth
                LIMIT 1
            ) inherited ON true
            LEFT JOIN LATERAL (
                WITH RECURSIVE descendants AS (
                    SELECT child.id
                    UNION ALL
                    SELECT descendant.id
                    FROM directories descendant
                    JOIN descendants ON descendant.parent_id = descendants.id
                )
                SELECT MIN(COALESCE(
                    task.deadline,
                    task_group.deadline,
                    (WITH RECURSIVE task_ancestry AS (
                        SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                        FROM directories d WHERE d.id = task.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.deadline, task_ancestry.depth + 1
                        FROM directories parent
                        JOIN task_ancestry ON parent.id = task_ancestry.parent_id
                    )
                    SELECT deadline
                    FROM task_ancestry
                    WHERE deadline IS NOT NULL
                    ORDER BY depth
                    LIMIT 1)
                )) FILTER (WHERE task.status = 'open') AS deadline,
                COUNT(*) FILTER (WHERE task.status = 'completed') AS completed_task_count
                FROM descendants
                JOIN tasks task ON task.directory_id = descendants.id
                LEFT JOIN task_groups task_group ON task_group.id = task.group_id
            ) task_rollup ON true
            WHERE child.parent_id IS NOT DISTINCT FROM $1
            ORDER BY effective_deadline NULLS LAST, child.name
            "#,
        )
        .bind(directory_id)
        .fetch_all(&self.pool)
        .await?;

        let directories = directory_rows
            .into_iter()
            .map(|row| DirectoryView {
                id: row.id,
                path: join_path(&normalized_path, &row.name),
                name: row.name,
                explicit_deadline: row.explicit_deadline,
                effective_deadline: row.effective_deadline,
                completed_task_count: row.completed_task_count,
            })
            .collect();

        let Some(directory_id) = directory_id else {
            return Ok(Snapshot {
                directory_id: None,
                path: normalized_path,
                directories,
                groups: Vec::new(),
                ungrouped_tasks: Vec::new(),
            });
        };

        let group_views = self.groups_for_directory(directory_id).await?;
        let tasks = self
            .tasks_for_directory(directory_id, include_completed)
            .await?;
        let groups = group_views
            .into_iter()
            .map(|group| GroupWithTasks {
                tasks: tasks
                    .iter()
                    .filter(|task| task.group_id == Some(group.id))
                    .cloned()
                    .collect(),
                group,
            })
            .collect();
        let ungrouped_tasks = tasks
            .into_iter()
            .filter(|task| task.group_id.is_none())
            .collect();

        Ok(Snapshot {
            directory_id: Some(directory_id),
            path: normalized_path,
            directories,
            groups,
            ungrouped_tasks,
        })
    }

    pub async fn task_counts(&self, path: &str, today: NaiveDate) -> Result<TaskCounts> {
        let normalized_path = normalize_path(path);
        let directory_id = self.directory_id(&normalized_path).await?;
        if normalized_path != "/" && directory_id.is_none() {
            bail!("directory {normalized_path} does not exist");
        }

        sqlx::query_as(
            r#"
            WITH RECURSIVE descendants AS (
                SELECT id
                FROM directories
                WHERE CASE
                    WHEN $1::BIGINT IS NULL THEN parent_id IS NULL
                    ELSE id = $1
                END
                UNION ALL
                SELECT child.id
                FROM directories child
                JOIN descendants ON child.parent_id = descendants.id
            ), resolved_tasks AS (
                SELECT task.status,
                       COALESCE(task.deadline, task_group.deadline, inherited.deadline)
                           AS effective_deadline
                FROM descendants
                JOIN tasks task ON task.directory_id = descendants.id
                LEFT JOIN task_groups task_group ON task_group.id = task.group_id
                LEFT JOIN LATERAL (
                    WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                        FROM directories d WHERE d.id = task.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                        FROM directories parent
                        JOIN ancestry ON parent.id = ancestry.parent_id
                    )
                    SELECT deadline
                    FROM ancestry
                    WHERE deadline IS NOT NULL
                    ORDER BY depth
                    LIMIT 1
                ) inherited ON true
            )
            SELECT COUNT(*) FILTER (
                       WHERE status = 'open' AND effective_deadline <= $2
                   ) AS today,
                   COUNT(*) FILTER (WHERE status = 'open') AS backlog,
                   COUNT(*) FILTER (WHERE status = 'completed') AS completed
            FROM resolved_tasks
            "#,
        )
        .bind(directory_id)
        .bind(today)
        .fetch_one(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn completed_tasks(&self, path: &str) -> Result<Vec<TaskView>> {
        let normalized_path = normalize_path(path);
        let directory_id = self.directory_id(&normalized_path).await?;
        if normalized_path != "/" && directory_id.is_none() {
            bail!("directory {normalized_path} does not exist");
        }

        sqlx::query_as(
            r#"
            WITH RECURSIVE descendants AS (
                SELECT id
                FROM directories
                WHERE CASE
                    WHEN $1::BIGINT IS NULL THEN parent_id IS NULL
                    ELSE id = $1
                END
                UNION ALL
                SELECT child.id
                FROM directories child
                JOIN descendants ON child.parent_id = descendants.id
            )
            SELECT t.id, t.directory_id,
                   (WITH RECURSIVE path AS (
                      SELECT d.id, d.parent_id, d.name, 0 AS depth
                      FROM directories d WHERE d.id = t.directory_id
                      UNION ALL
                      SELECT parent.id, parent.parent_id, parent.name, path.depth + 1
                      FROM directories parent JOIN path ON parent.id = path.parent_id
                    ) SELECT '/' || string_agg(name, '/' ORDER BY depth DESC) FROM path)
                      AS directory_path,
                   t.group_id, g.name AS group_name, t.title,
                   t.deadline AS explicit_deadline,
                   COALESCE(
                     t.deadline,
                     g.deadline,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                        FROM directories d WHERE d.id = t.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT deadline FROM ancestry WHERE deadline IS NOT NULL ORDER BY depth LIMIT 1)
                   ) AS effective_deadline,
                   t.status, t.completed_at, t.completion_note, t.links
            FROM descendants
            JOIN tasks t ON t.directory_id = descendants.id
            LEFT JOIN task_groups g ON g.id = t.group_id
            WHERE t.status = 'completed'
            ORDER BY t.completed_at DESC NULLS LAST, t.id DESC
            "#,
        )
        .bind(directory_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn initialize(&self) -> Result<()> {
        self.ensure_directory("/projects", None).await?;
        Ok(())
    }

    async fn require_directory(&self, path: &str) -> Result<i64> {
        self.directory_id(path)
            .await?
            .with_context(|| format!("directory {} does not exist", normalize_path(path)))
    }

    async fn require_group(&self, directory_id: i64, name: &str) -> Result<i64> {
        sqlx::query_scalar("SELECT id FROM task_groups WHERE directory_id = $1 AND name = $2")
            .bind(directory_id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await?
            .with_context(|| format!("group {name} does not exist in the target directory"))
    }

    async fn group_by_id(&self, group_id: i64) -> Result<GroupView> {
        sqlx::query_as(
            r#"
            SELECT g.id, g.name, g.deadline AS explicit_deadline,
                   COALESCE(
                     (SELECT MIN(COALESCE(
                        task.deadline,
                        g.deadline,
                        (WITH RECURSIVE ancestry AS (
                            SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                            FROM directories d WHERE d.id = g.directory_id
                            UNION ALL
                            SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                            FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                        ) SELECT deadline FROM ancestry WHERE deadline IS NOT NULL ORDER BY depth LIMIT 1)
                      ))
                      FROM tasks task
                      WHERE task.group_id = g.id AND task.status = 'open'),
                     g.deadline,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                        FROM directories d WHERE d.id = g.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT deadline FROM ancestry WHERE deadline IS NOT NULL ORDER BY depth LIMIT 1)
                   ) AS effective_deadline,
                   g.links
            FROM task_groups g
            WHERE g.id = $1
            "#,
        )
        .bind(group_id)
        .fetch_one(&self.pool)
        .await
        .map_err(Into::into)
    }

    async fn task_by_id(&self, task_id: i64) -> Result<TaskView> {
        sqlx::query_as(
            r#"
            SELECT t.id, t.directory_id,
                   (WITH RECURSIVE path AS (
                      SELECT d.id, d.parent_id, d.name, 0 AS depth
                      FROM directories d WHERE d.id = t.directory_id
                      UNION ALL
                      SELECT parent.id, parent.parent_id, parent.name, path.depth + 1
                      FROM directories parent JOIN path ON parent.id = path.parent_id
                    ) SELECT '/' || string_agg(name, '/' ORDER BY depth DESC) FROM path)
                      AS directory_path,
                   t.group_id, g.name AS group_name, t.title,
                   t.deadline AS explicit_deadline,
                   COALESCE(
                     t.deadline,
                     g.deadline,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                        FROM directories d WHERE d.id = t.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT deadline FROM ancestry WHERE deadline IS NOT NULL ORDER BY depth LIMIT 1)
                   ) AS effective_deadline,
                   t.status, t.completed_at, t.completion_note, t.links
            FROM tasks t
            LEFT JOIN task_groups g ON g.id = t.group_id
            WHERE t.id = $1
            "#,
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?
        .with_context(|| format!("task #{task_id} does not exist"))
    }

    async fn groups_for_directory(&self, directory_id: i64) -> Result<Vec<GroupView>> {
        sqlx::query_as(
            r#"
            SELECT g.id, g.name, g.deadline AS explicit_deadline,
                   COALESCE(
                     (SELECT MIN(COALESCE(
                        task.deadline,
                        g.deadline,
                        (WITH RECURSIVE ancestry AS (
                            SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                            FROM directories d WHERE d.id = g.directory_id
                            UNION ALL
                            SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                            FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                        ) SELECT deadline FROM ancestry WHERE deadline IS NOT NULL ORDER BY depth LIMIT 1)
                      ))
                      FROM tasks task
                      WHERE task.group_id = g.id AND task.status = 'open'),
                     g.deadline,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                        FROM directories d WHERE d.id = g.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT deadline FROM ancestry WHERE deadline IS NOT NULL ORDER BY depth LIMIT 1)
                   ) AS effective_deadline,
                   g.links
            FROM task_groups g
            WHERE g.directory_id = $1
            ORDER BY effective_deadline NULLS LAST, g.position, g.name
            "#,
        )
        .bind(directory_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    async fn tasks_for_directory(
        &self,
        directory_id: i64,
        include_completed: bool,
    ) -> Result<Vec<TaskView>> {
        sqlx::query_as(
            r#"
            SELECT t.id, t.directory_id,
                   (WITH RECURSIVE path AS (
                      SELECT d.id, d.parent_id, d.name, 0 AS depth
                      FROM directories d WHERE d.id = t.directory_id
                      UNION ALL
                      SELECT parent.id, parent.parent_id, parent.name, path.depth + 1
                      FROM directories parent JOIN path ON parent.id = path.parent_id
                    ) SELECT '/' || string_agg(name, '/' ORDER BY depth DESC) FROM path)
                      AS directory_path,
                   t.group_id, g.name AS group_name, t.title,
                   t.deadline AS explicit_deadline,
                   COALESCE(
                     t.deadline,
                     g.deadline,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.deadline, 0 AS depth
                        FROM directories d WHERE d.id = t.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.deadline, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT deadline FROM ancestry WHERE deadline IS NOT NULL ORDER BY depth LIMIT 1)
                   ) AS effective_deadline,
                   t.status, t.completed_at, t.completion_note, t.links
            FROM tasks t
            LEFT JOIN task_groups g ON g.id = t.group_id
            WHERE t.directory_id = $1 AND ($2 OR t.status = 'open')
            ORDER BY effective_deadline NULLS LAST, COALESCE(g.position, 2147483647), t.position, t.id
            "#,
        )
        .bind(directory_id)
        .bind(include_completed)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }
}

pub fn normalize_path(path: &str) -> String {
    let components = path_components(path);
    if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    }
}

pub fn is_finished_archive(path: &str) -> bool {
    path_components(path)
        .iter()
        .any(|component| component.eq_ignore_ascii_case("finished"))
}

fn path_components(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .map(ToOwned::to_owned)
        .collect()
}

fn join_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize_path("projects//example/"), "/projects/example");
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn joins_root_and_nested_paths() {
        assert_eq!(join_path("/", "projects"), "/projects");
        assert_eq!(join_path("/projects", "example"), "/projects/example");
    }

    #[test]
    fn recognizes_finished_archive_paths() {
        assert!(is_finished_archive("/projects/oxia/finished"));
        assert!(is_finished_archive("/projects/finished/oxia"));
        assert!(!is_finished_archive("/projects/unfinished"));
    }
}
