/// 确保默认管理员账户存在
/// 邮箱: admin@gmail.com
/// 密码: admin123456
fn ensure_default_admin(user_store: &UserStore) {
    const DEFAULT_ADMIN_EMAIL: &str = "admin@gmail.com";
    const DEFAULT_ADMIN_PASSWORD: &str = "admin123456";

    // 删除旧的管理员账户（如果有）
    if let Some(old_admin) = user_store.get_by_email(DEFAULT_ADMIN_EMAIL) {
        let _ = user_store.delete(&old_admin.id);
        tracing::info!("Removed old admin account: {}", DEFAULT_ADMIN_EMAIL);
    }

    // 创建新的管理员账户
    match user_store.create_with_username(
        DEFAULT_ADMIN_EMAIL,
        "admin",
        DEFAULT_ADMIN_PASSWORD,
        Role::Admin,
        0,
    ) {
        Ok(_) => {
            tracing::success!(
                "Default admin account created: {} (password: {})",
                DEFAULT_ADMIN_EMAIL,
                DEFAULT_ADMIN_PASSWORD
            );
        }
        Err(e) => {
            tracing::error!("Failed to create default admin account: {}", e);
        }
    }
}