use sqlx::postgres::PgPool;
use sqlx::Row;

#[derive(Debug, Clone)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub wallet_address: String,
    pub icon: String,
    pub icon_class: String,
    pub description: String,
    pub org: String,
    pub tags: Vec<String>,
    pub services: Vec<String>,
    pub reward_rate_fat: f64,
    pub status: String,
    pub health_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPool::connect(database_url).await
}

fn parse_agent_row(r: sqlx::postgres::PgRow) -> AgentRow {
    let rate_str: String = r.get("reward_rate_str");
    AgentRow {
        id: r.get("id"),
        name: r.get("name"),
        agent_type: r.get("agent_type"),
        wallet_address: r.get("wallet_address"),
        icon: r.get("icon"),
        icon_class: r.get("icon_class"),
        description: r.get("description"),
        org: r.get("org"),
        tags: r.get("tags"),
        services: r.get("services"),
        reward_rate_fat: rate_str.parse::<f64>().unwrap_or(0.0),
        status: r.get("status"),
        health_url: r.get("health_url"),
        created_at: r.get("created_at"),
    }
}

const AGENT_QUERY: &str =
    "SELECT id, name, agent_type, wallet_address, icon, icon_class, description, org,
            tags, services, reward_rate_fat::text AS reward_rate_str, status, health_url, created_at
     FROM ai_agents";

pub async fn list_agents(pool: &PgPool) -> Result<Vec<AgentRow>, sqlx::Error> {
    let rows = sqlx::query(&format!("{} ORDER BY created_at ASC", AGENT_QUERY))
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(parse_agent_row).collect())
}

pub async fn get_agent(pool: &PgPool, id: &str) -> Result<Option<AgentRow>, sqlx::Error> {
    let row = sqlx::query(&format!("{} WHERE id = $1", AGENT_QUERY))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(parse_agent_row))
}

pub async fn get_agent_by_wallet(
    pool: &PgPool,
    wallet: &str,
) -> Result<Option<AgentRow>, sqlx::Error> {
    let row = sqlx::query(&format!(
        "{} WHERE LOWER(wallet_address) = LOWER($1)",
        AGENT_QUERY
    ))
    .bind(wallet)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(parse_agent_row))
}
