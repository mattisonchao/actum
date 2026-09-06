use serde::Serialize;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DirectoryView {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub explicit_priority: Option<i16>,
    pub effective_priority: i16,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GroupView {
    pub id: i64,
    pub name: String,
    pub explicit_priority: Option<i16>,
    pub effective_priority: i16,
    pub links: Vec<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TaskView {
    pub id: i64,
    pub directory_id: i64,
    pub group_id: Option<i64>,
    pub group_name: Option<String>,
    pub title: String,
    pub explicit_priority: Option<i16>,
    pub effective_priority: i16,
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

pub fn parse_priority(value: Option<&str>) -> anyhow::Result<Option<i16>> {
    value
        .map(|value| match value.to_ascii_uppercase().as_str() {
            "P1" | "1" => Ok(1),
            "P2" | "2" => Ok(2),
            "P3" | "3" => Ok(3),
            _ => anyhow::bail!("priority must be P1, P2, or P3"),
        })
        .transpose()
}

pub fn format_priority(value: i16) -> &'static str {
    match value {
        1 => "P1",
        2 => "P2",
        _ => "P3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_priorities_case_insensitively() {
        assert_eq!(parse_priority(Some("p1")).unwrap(), Some(1));
        assert_eq!(parse_priority(Some("P2")).unwrap(), Some(2));
        assert_eq!(parse_priority(Some("3")).unwrap(), Some(3));
        assert_eq!(parse_priority(None).unwrap(), None);
    }

    #[test]
    fn rejects_unknown_priorities() {
        assert!(parse_priority(Some("urgent")).is_err());
    }
}
