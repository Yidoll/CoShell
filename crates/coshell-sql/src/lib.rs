use coshell_types::ComponentStatus;

pub fn component_status() -> ComponentStatus {
    ComponentStatus::ready("sql", "SQL parsing and dialect boundary initialized")
}

/// Read-only classification is intentionally conservative until a dialect parser is installed.
pub fn is_read_only_statement(sql: &str) -> bool {
    matches!(
        sql.trim_start()
            .split_whitespace()
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("select" | "show" | "describe" | "explain")
    )
}

#[cfg(test)]
mod tests {
    use super::is_read_only_statement;

    #[test]
    fn classifies_obvious_read_only_statements() {
        assert!(is_read_only_statement("  SELECT 1"));
        assert!(!is_read_only_statement("delete from jobs"));
        assert!(!is_read_only_statement(
            "with deleted as (delete from jobs returning *) select * from deleted"
        ));
    }
}
