//! AIGX 网络层基础使用示例

use aigx_net::NetworkLayer;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("AIGX Network Layer basic usage example\n");

    // 1. 创建网络层实例
    let network = NetworkLayer::new();
    println!("[1/3] network layer instance created");

    // 2. 初始化网络层
    network.initialize().await?;
    println!("[2/3] network layer initialized");

    // 3. 查看池状态
    let account_status = network.account_pool().status();
    println!(
        "[3/3] account pool: total={} available={}",
        account_status.total_accounts, account_status.available_accounts
    );

    let session_status = network.session_pool().status();
    println!(
        "      session pool: total={} idle={}",
        session_status.total_sessions, session_status.idle_sessions
    );

    println!("\nAIGX network layer example completed!");
    Ok(())
}
