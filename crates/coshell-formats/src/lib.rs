use coshell_types::ComponentStatus;

pub fn component_status() -> ComponentStatus {
    ComponentStatus::ready("formats", "format encoders boundary initialized")
}

pub fn encode_text_rows(rows: &[Vec<String>]) -> String {
    rows.iter()
        .map(|row| row.join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}
