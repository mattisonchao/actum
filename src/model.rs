use anyhow::Context;
use chrono::NaiveDate;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DirectoryView {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub explicit_deadline: Option<NaiveDate>,
    pub effective_deadline: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GroupView {
    pub id: i64,
    pub name: String,
    pub explicit_deadline: Option<NaiveDate>,
    pub effective_deadline: Option<NaiveDate>,
    pub links: Vec<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TaskView {
    pub id: i64,
    pub directory_id: i64,
    pub group_id: Option<i64>,
    pub group_name: Option<String>,
    pub title: String,
    pub explicit_deadline: Option<NaiveDate>,
    pub effective_deadline: Option<NaiveDate>,
    pub status: String,
    pub links: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupWithTasks {
    #[serde(flatten)]
    pub group: GroupView,
    pub tasks: Vec<TaskView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub directory_id: Option<i64>,
    pub path: String,
    pub directories: Vec<DirectoryView>,
    pub groups: Vec<GroupWithTasks>,
    pub ungrouped_tasks: Vec<TaskView>,
}

pub fn parse_deadline(value: Option<&str>) -> anyhow::Result<Option<NaiveDate>> {
    value
        .map(|value| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .with_context(|| "deadline must use YYYY-MM-DD")
        })
        .transpose()
}

pub fn format_deadline(value: Option<NaiveDate>) -> String {
    value
        .map(|deadline| format!("DDL {deadline}"))
        .unwrap_or_else(|| "DDL —".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_deadlines() {
        assert_eq!(
            parse_deadline(Some("2028-02-29")).unwrap(),
            NaiveDate::from_ymd_opt(2028, 2, 29)
        );
        assert_eq!(parse_deadline(None).unwrap(), None);
    }

    #[test]
    fn rejects_invalid_deadlines() {
        assert!(parse_deadline(Some("2027-02-29")).is_err());
        assert!(parse_deadline(Some("tomorrow")).is_err());
    }
}
