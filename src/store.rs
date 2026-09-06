use anyhow::{Context, Result, bail};
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::model::{DirectoryView, GroupView, GroupWithTasks, Snapshot, TaskView};

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

#[derive(sqlx::FromRow)]
struct DirectoryRow {
    id: i64,
    name: String,
    explicit_priority: Option<i16>,
    effective_priority: i16,
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

    pub async fn ensure_directory(&self, path: &str, priority: Option<i16>) -> Result<i64> {
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
                        "INSERT INTO directories (parent_id, name, priority) VALUES ($1, $2, $3) RETURNING id",
                    )
                    .bind(parent_id)
                    .bind(component)
                    .bind(if index + 1 == components.len() {
                        priority
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
        if let Some(priority) = priority {
            sqlx::query("UPDATE directories SET priority = $1 WHERE id = $2")
                .bind(priority)
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
        priority: Option<i16>,
        links: &[String],
    ) -> Result<GroupView> {
        let directory_id = self.require_directory(directory_path).await?;
        let group_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO task_groups (directory_id, name, priority, links)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (directory_id, name) DO UPDATE SET
                priority = COALESCE(EXCLUDED.priority, task_groups.priority),
                links = CASE WHEN cardinality(EXCLUDED.links) = 0 THEN task_groups.links ELSE EXCLUDED.links END
            RETURNING id
            "#,
        )
        .bind(directory_id)
        .bind(name)
        .bind(priority)
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
        priority: Option<i16>,
        links: &[String],
    ) -> Result<TaskView> {
        let directory_id = self.require_directory(directory_path).await?;
        let group_id = match group_name {
            Some(name) => Some(self.require_group(directory_id, name).await?),
            None => None,
        };

        let task_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO tasks (directory_id, group_id, title, priority, links)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (directory_id, title) DO UPDATE SET
                group_id = EXCLUDED.group_id,
                priority = COALESCE(EXCLUDED.priority, tasks.priority),
                links = CASE WHEN cardinality(EXCLUDED.links) = 0 THEN tasks.links ELSE EXCLUDED.links END
            RETURNING id
            "#,
        )
        .bind(directory_id)
        .bind(group_id)
        .bind(title)
        .bind(priority)
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

    pub async fn snapshot(&self, path: &str, include_completed: bool) -> Result<Snapshot> {
        let normalized_path = normalize_path(path);
        let directory_id = self.directory_id(&normalized_path).await?;
        if normalized_path != "/" && directory_id.is_none() {
            bail!("directory {normalized_path} does not exist");
        }

        let directory_rows = sqlx::query_as::<_, DirectoryRow>(
            r#"
            SELECT child.id,
                   child.name,
                   child.priority AS explicit_priority,
                   COALESCE(
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.priority, 0 AS depth
                        FROM directories d WHERE d.id = child.id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.priority, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT priority FROM ancestry WHERE priority IS NOT NULL ORDER BY depth LIMIT 1),
                     3
                   )::SMALLINT AS effective_priority
            FROM directories child
            WHERE child.parent_id IS NOT DISTINCT FROM $1
            ORDER BY effective_priority, child.name
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
                explicit_priority: row.explicit_priority,
                effective_priority: row.effective_priority,
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

    pub async fn seed(&self) -> Result<()> {
        self.ensure_directory("/projects/oxia", Some(1)).await?;
        self.ensure_directory("/projects/release-readiness", Some(1))
            .await?;
        self.ensure_directory("/projects/sql-workspace", Some(2))
            .await?;
        self.ensure_directory("/projects/dss-hackathon", Some(3))
            .await?;
        self.ensure_directory("/projects/integrations", Some(2))
            .await?;
        self.ensure_directory("/projects/docs", Some(3)).await?;

        self.create_group("/projects/oxia", "Subscriptions", Some(1), &[])
            .await?;
        self.create_task(
            "/projects/oxia",
            Some("Subscriptions"),
            "Renew long-lived Oxia Java subscriptions",
            Some(1),
            &["https://github.com/oxia-db/oxia-client-java/pull/372".into()],
        )
        .await?;

        self.create_group(
            "/projects/oxia",
            "Oxia 0.16.9 production verification",
            Some(2),
            &["https://github.com/oxia-db/oxia/pull/1286".into()],
        )
        .await?;
        self.create_task(
            "/projects/oxia",
            Some("Oxia 0.16.9 production verification"),
            "Roll out Oxia 0.16.9 and verify the APIKey backlog/readiness fix for Aegis Financial",
            Some(2),
            &[
                "https://github.com/streamnative/eng-support-tickets/issues/4925".into(),
                "https://github.com/oxia-db/oxia/pull/1286".into(),
            ],
        )
        .await?;
        self.create_task(
            "/projects/oxia",
            Some("Oxia 0.16.9 production verification"),
            "Verify the fix for slow OxiaNamespace creation at production scale",
            Some(2),
            &["https://github.com/oxia-db/oxia/pull/1286".into()],
        )
        .await?;

        self.create_group(
            "/projects/release-readiness",
            "Pulsar staging",
            Some(1),
            &[],
        )
        .await?;
        self.create_task(
            "/projects/release-readiness",
            Some("Pulsar staging"),
            "Fix the unified-rbac plugin build failure in the Pulsar 5.0.0-M1-SNAPSHOT staging release",
            Some(1),
            &["https://github.com/streamnative/streamnative-ci/actions/runs/32701047955/job/98060742997".into()],
        )
        .await?;

        self.create_group(
            "/projects/sql-workspace",
            "Public Preview",
            Some(3),
            &["https://github.com/streamnative/product-roadmap/issues/1901".into()],
        )
        .await?;
        for (title, priority, link) in [
            (
                "Provision a SQL Workspace environment for Kundan in production",
                2,
                None,
            ),
            ("SQLWorkspace: support a customized image", 2, None),
            (
                "SQLWorkspace: automatically load the RisingWave license",
                2,
                Some("https://github.com/streamnative/sn-operator/pull/1399"),
            ),
            (
                "Hot-reload native TLS certificates in SQL Gateway",
                2,
                Some("https://github.com/streamnative/sql-gateway/pull/45"),
            ),
            (
                "Next SQLWorkspace epic: public preview and native Lakestream catalog",
                3,
                Some("https://github.com/streamnative/snip/pull/132"),
            ),
            (
                "Support SQL workspace and catalog creation in Cloud CLI",
                3,
                Some("https://github.com/streamnative/cloud-cli/pull/267"),
            ),
        ] {
            let links = link.into_iter().map(String::from).collect::<Vec<_>>();
            self.create_task(
                "/projects/sql-workspace",
                Some("Public Preview"),
                title,
                Some(priority),
                &links,
            )
            .await?;
        }
        self.create_group(
            "/projects/sql-workspace",
            "Lakestream Catalog",
            Some(3),
            &["https://github.com/streamnative/product-roadmap/issues/1900".into()],
        )
        .await?;
        self.create_task(
            "/projects/sql-workspace",
            None,
            "Review SQL Workspace tracking sheet",
            Some(2),
            &["https://docs.google.com/spreadsheets/d/10upSTBaxPDnx4BrnZa_5k-N99R2XkZuetZZblknM4WY/edit?gid=0#gid=0".into()],
        )
        .await?;

        self.create_group("/projects/dss-hackathon", "Launch", Some(3), &[])
            .await?;
        self.create_task(
            "/projects/dss-hackathon",
            Some("Launch"),
            "Deploy a cluster for the DSS Hackathon",
            Some(2),
            &[],
        )
        .await?;

        self.create_task(
            "/projects/integrations",
            None,
            "Support Pulsar Schema Registry for RisingWave Avro sources",
            Some(2),
            &["https://github.com/risingwavelabs/risingwave/pull/26347".into()],
        )
        .await?;
        self.create_task(
            "/projects/docs",
            None,
            "Document the Kafka upstream-source syncing logic",
            Some(3),
            &[],
        )
        .await?;
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
            SELECT g.id, g.name, g.priority AS explicit_priority,
                   COALESCE(
                     g.priority,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.priority, 0 AS depth
                        FROM directories d WHERE d.id = g.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.priority, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT priority FROM ancestry WHERE priority IS NOT NULL ORDER BY depth LIMIT 1),
                     3
                   )::SMALLINT AS effective_priority,
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
            SELECT t.id, t.directory_id, t.group_id, g.name AS group_name, t.title,
                   t.priority AS explicit_priority,
                   COALESCE(
                     t.priority,
                     g.priority,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.priority, 0 AS depth
                        FROM directories d WHERE d.id = t.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.priority, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT priority FROM ancestry WHERE priority IS NOT NULL ORDER BY depth LIMIT 1),
                     3
                   )::SMALLINT AS effective_priority,
                   t.status, t.links
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
            SELECT g.id, g.name, g.priority AS explicit_priority,
                   COALESCE(
                     g.priority,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.priority, 0 AS depth
                        FROM directories d WHERE d.id = g.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.priority, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT priority FROM ancestry WHERE priority IS NOT NULL ORDER BY depth LIMIT 1),
                     3
                   )::SMALLINT AS effective_priority,
                   g.links
            FROM task_groups g
            WHERE g.directory_id = $1
            ORDER BY effective_priority, g.position, g.name
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
            SELECT t.id, t.directory_id, t.group_id, g.name AS group_name, t.title,
                   t.priority AS explicit_priority,
                   COALESCE(
                     t.priority,
                     g.priority,
                     (WITH RECURSIVE ancestry AS (
                        SELECT d.id, d.parent_id, d.priority, 0 AS depth
                        FROM directories d WHERE d.id = t.directory_id
                        UNION ALL
                        SELECT parent.id, parent.parent_id, parent.priority, ancestry.depth + 1
                        FROM directories parent JOIN ancestry ON parent.id = ancestry.parent_id
                      ) SELECT priority FROM ancestry WHERE priority IS NOT NULL ORDER BY depth LIMIT 1),
                     3
                   )::SMALLINT AS effective_priority,
                   t.status, t.links
            FROM tasks t
            LEFT JOIN task_groups g ON g.id = t.group_id
            WHERE t.directory_id = $1 AND ($2 OR t.status = 'open')
            ORDER BY effective_priority, COALESCE(g.position, 2147483647), t.position, t.id
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
        assert_eq!(normalize_path("projects//oxia/"), "/projects/oxia");
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn joins_root_and_nested_paths() {
        assert_eq!(join_path("/", "projects"), "/projects");
        assert_eq!(join_path("/projects", "oxia"), "/projects/oxia");
    }
}
