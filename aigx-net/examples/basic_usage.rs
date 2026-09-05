//! AIGX 网络层基础使用示例

use aigx_net::NetworkLayer;
use aigx_net::{AccountPool, ConnectionPool, SessionPool};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("🚀 AIGX Network Layer 基础使用示例\n");

    // 1. 创建网络层实例
    let network = NetworkLayer::new();
    println!("✅ 创建网络层实例");

    // 2. 初始化网络层
    println!("🔧 正在初始化网络层...");
    network.initialize().await?;
    println!("✅ 网络层初始化完成");

    // 3. 获取各个池
    let account_pool = network.account_pool();
    let connection_pool = network.connection_pool();
    let session_pool = network.session_pool();

    // 4. 查看账号池状态
    println!("\n📊 账号池状态:");
    let pool_status = account_pool.status();
    println!("  - 总账号数: {}", pool_status.total_accounts);
    println!("  - 可用账号: {}", pool_status.available_accounts);
    println!("  - 使用中: {}", pool_status.busy_accounts);

    // 5. 查看连接池状态
    println!("\n🔗 连接池状态:");
    let conn_status = connection_pool.status();
    println!("  - 总连接数: {}", conn_status.total_connections);
    println!("  - 活跃连接: {}", conn_status.active_connections);
    println!("  - 空闲连接: {}", conn_status.idle_connections);

    // 6. 使用会话池
    println!("\n🔄 使用会话池:");
    let session = session_pool.acquire_session().await?;
    println!("  - 获取会话: {}", session.id());

    // 7. 健康检查
    println!("\n🏥 执行健康检查...");
    controller.health_check().await?;
    println!("✅ 健康检查完成");

    // 8. 获取网络层状态
    println!("\n📈 网络层运行时指标:");
    println!("  - 账号池: {:?}", account_pool.status());
    println!("  - 连接池: {:?}", connection_pool.status());
    println!("  - 会话池: {:?}", session_pool.status());

    println!("\n🎉 AIGX 网络层示例运行完成！");
    Ok(())
}