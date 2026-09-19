use crate::article_index::ArticleIndex;
use anyhow::{bail, Result};
use serde_json::json;

use super::output::{print_value, resolve};

pub fn cmd_add(url: String, json_output: bool) -> Result<()> {
    let mut index = ArticleIndex::open_default()?;
    let subscription = index.add_subscription_by_url(&url)?;
    print_value(&json!(subscription), &resolve(json_output))
}

pub fn cmd_list(include_disabled: bool, json_output: bool) -> Result<()> {
    let index = ArticleIndex::open_default()?;
    let subscriptions: Vec<_> = index
        .list_subscriptions()?
        .into_iter()
        .filter(|item| include_disabled || item.enabled)
        .collect();
    print_value(&json!(subscriptions), &resolve(json_output))
}

pub fn cmd_remove(account_id: i64, json_output: bool) -> Result<()> {
    let index = ArticleIndex::open_default()?;
    if !index.remove_subscription(account_id)? {
        bail!("未找到启用中的订阅 ID: {account_id}");
    }
    print_value(
        &json!({ "id": account_id, "enabled": false }),
        &resolve(json_output),
    )
}
