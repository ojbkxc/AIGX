use anyhow::{anyhow, Result};
use chrono::Datelike;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

use crate::account::CfAccount;

/// 查询 CF 账号的用量汇总
pub async fn query_usage_summary(account: &CfAccount, client: &Client) -> Result<UsageSummary> {
    let now = chrono::Utc::now();

    // 今天 00:00 UTC
    let today_start = format!(
        "{:04}-{:02}-{:02}T00:00:00Z",
        now.year(),
        now.month(),
        now.day()
    );

    // 月初 00:00 UTC
    let month_start = format!(
        "{:04}-{:02}-{:02}T00:00:00Z",
        now.year(),
        now.month(),
        1
    );

    // 查询今天的数据
    let today_groups = query_graphql(&account.account_id, &account.api_token, &today_start, client).await?;

    // 查询本月的数据（如果月初不是今天）
    let month_groups = if today_start == month_start {
        today_groups.clone()
    } else {
        query_graphql(&account.account_id, &account.api_token, &month_start, client).await?
    };

    let today_neurons: u64 = today_groups.iter().map(|g| g.sum.total_neurons).sum();
    let today_requests: u64 = today_groups.iter().map(|g| g.count as u64).sum();

    let month_neurons: u64 = month_groups.iter().map(|g| g.sum.total_neurons).sum();
    let month_requests: u64 = month_groups.iter().map(|g| g.count as u64).sum();

    Ok(UsageSummary {
        neurons: month_neurons,
        requests: month_requests,
        today_neurons,
        today_requests,
    })
}

/// 查询历史用量（按天汇总）
#[allow(dead_code)]
pub async fn query_usage_history(account: &CfAccount, client: &Client) -> Result<Vec<DailyUsage>> {
    let seven_days_ago = chrono::Utc::now() - chrono::Duration::days(7);
    let start = format!(
        "{:04}-{:02}-{:02}T00:00:00Z",
        seven_days_ago.year(),
        seven_days_ago.month(),
        seven_days_ago.day()
    );

    let groups = query_graphql(&account.account_id, &account.api_token, &start, client).await?;

    // 初始化过去7天的数据
    let mut daily_map: HashMap<String, (u64, u64)> = HashMap::new();
    for i in 0..7 {
        let day = chrono::Utc::now() - chrono::Duration::days(i);
        let date_str = format!("{:04}-{:02}-{:02}", day.year(), day.month(), day.day());
        daily_map.entry(date_str).or_insert((0, 0));
    }

    // 聚合 GraphQL 返回的数据
    for group in &groups {
        let date_str = &group.dimensions.date;
        let entry = daily_map.entry(date_str.clone()).or_insert((0, 0));
        entry.0 += group.sum.total_neurons;
        entry.1 += group.count as u64;
    }

    // 排序输出
    let mut history: Vec<DailyUsage> = daily_map
        .into_iter()
        .map(|(date, (neurons, requests))| DailyUsage {
            date,
            neurons,
            requests,
        })
        .collect();
    history.sort_by(|a, b| a.date.cmp(&b.date));

    Ok(history)
}

/// 执行 GraphQL 查询
async fn query_graphql(
    account_id: &str,
    api_token: &str,
    start_datetime: &str,
    client: &Client,
) -> Result<Vec<GraphQLGroup>> {
    let query = r#"
        query GetAIUsage($accountId: String!, $start: String!) {
            viewer {
                accounts(filter: { accountTag: $accountId }) {
                    aiInferenceAdaptiveGroups(
                        filter: { datetime_geq: $start }
                        limit: 1000
                    ) {
                        count
                        sum {
                            totalNeurons
                        }
                        dimensions {
                            date
                            modelId
                        }
                    }
                }
            }
        }
    "#;

    let body = json!({
        "query": query,
        "variables": {
            "accountId": account_id,
            "start": start_datetime
        }
    });

    let response = client
        .post("https://api.cloudflare.com/client/v4/graphql")
        .bearer_auth(api_token)
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| anyhow!("GraphQL request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        return Err(anyhow!("GraphQL API error {status}: {body_text}"));
    }

    let result: GraphQLResponse = response
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse GraphQL response: {e}"))?;

    if let Some(errors) = result.errors {
        if !errors.is_empty() {
            return Err(anyhow!("GraphQL error: {}", errors[0].message));
        }
    }

    let groups = result
        .data
        .viewer
        .accounts
        .into_iter()
        .next()
        .map(|a| a.ai_inference_adaptive_groups)
        .unwrap_or_default();

    Ok(groups)
}

// ── GraphQL 响应类型 ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQLResponse {
    data: GraphQLData,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQLData {
    viewer: GraphQLViewer,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQLViewer {
    accounts: Vec<GraphQLAccount>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQLAccount {
    ai_inference_adaptive_groups: Vec<GraphQLGroup>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GraphQLGroup {
    count: i64,
    sum: GraphQLSum,
    dimensions: GraphQLDimensions,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GraphQLSum {
    total_neurons: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GraphQLDimensions {
    date: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQLError {
    message: String,
}

// ── 公开类型 ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageSummary {
    pub neurons: u64,
    pub requests: u64,
    pub today_neurons: u64,
    pub today_requests: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DailyUsage {
    pub date: String,
    pub neurons: u64,
    pub requests: u64,
}