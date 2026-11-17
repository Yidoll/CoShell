//! Saved command definitions. This module never executes command text.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
const KEY: &str = "commands.catalog.v1";
const MAX_COMMANDS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedCommand {
    pub id: String,
    pub name: String,
    pub content: String,
    pub category: String,
    pub version: u64,
}

fn read(db: &Connection) -> Result<Vec<SavedCommand>, String> {
    let value: Option<String> = db
        .query_row("SELECT value FROM app_settings WHERE key=?1", [KEY], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|_| "读取命令失败")?;
    let result: Vec<SavedCommand> = match value {
        None => Vec::new(),
        Some(v) => serde_json::from_str(&v).map_err(|_| "命令数据无效")?,
    };
    if result.len() > MAX_COMMANDS {
        return Err("命令数量超出限制".into());
    }
    Ok(result)
}
pub fn list(db: &Connection) -> Result<Vec<SavedCommand>, String> {
    let mut result = read(db)?;
    result.sort_by(|a, b| (&a.category, &a.name, &a.id).cmp(&(&b.category, &b.name, &b.id)));
    Ok(result)
}
fn write(db: &Connection, items: &[SavedCommand]) -> Result<(), String> {
    let value = serde_json::to_string(items).map_err(|_| "命令数据无效")?;
    db.execute("INSERT INTO app_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![KEY,value]).map_err(|_|"保存命令失败")?;
    Ok(())
}
pub fn save(db: &mut Connection, mut command: SavedCommand) -> Result<SavedCommand, String> {
    command.name = command.name.trim().to_owned();
    command.category = command.category.trim().to_owned();
    if command.id.is_empty()
        || command.id.len() > 128
        || command.name.is_empty()
        || command.name.len() > 256
        || command.category.len() > 256
        || command.content.trim().is_empty()
        || command.content.len() > 64 * 1024
        || command
            .content
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("命令名称或内容无效".into());
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "命令存储正忙")?;
    let mut items = read(&tx)?;
    if items
        .iter()
        .any(|c| c.id != command.id && c.name == command.name && c.category == command.category)
    {
        return Err("该分类中已有同名命令".into());
    }
    if let Some(index) = items.iter().position(|c| c.id == command.id) {
        if command.version != items[index].version || command.version >= 9_007_199_254_740_991 {
            return Err("命令已变化，请刷新".into());
        }
        command.version += 1;
        items[index] = command.clone();
    } else {
        if command.version != 0 || items.len() >= MAX_COMMANDS {
            return Err("命令版本或数量无效".into());
        }
        items.push(command.clone());
    }
    write(&tx, &items)?;
    tx.commit().map_err(|_| "命令保存结果未知，请刷新确认")?;
    Ok(command)
}
pub fn delete(db: &mut Connection, id: &str, version: u64) -> Result<bool, String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "命令存储正忙")?;
    let mut items = read(&tx)?;
    let Some(index) = items.iter().position(|c| c.id == id) else {
        return Ok(false);
    };
    if items[index].version != version {
        return Err("命令已变化，请刷新".into());
    }
    items.remove(index);
    write(&tx, &items)?;
    tx.commit().map_err(|_| "删除结果未知，请刷新确认")?;
    Ok(true)
}
