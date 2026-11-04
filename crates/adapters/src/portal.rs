//! Подключение к xdg-desktop-portal.
//!
//! У ashpd есть процесс-глобальное D-Bus-соединение, но приложение создаёт
//! отдельный Tokio runtime для каждого портального запроса. Соединение,
//! созданное в одном runtime, нельзя использовать в другом, поэтому каждый
//! запрос открывает своё соединение. На нём же регистрируется app_id, чтобы
//! разрешения ScreenCast сохранялись стабильно.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Идентификатор приложения для порталов.
pub(crate) const PORTAL_APP_ID: &str = "io.github.freekazoid.recordingscreen";

/// Признак того, что Registry-интерфейс портала доступен.
static REGISTRY_AVAILABLE: AtomicBool = AtomicBool::new(true);

/// Создаёт D-Bus-соединение и регистрирует на нём app_id.
///
/// Ошибка регистрации не критична: приложение продолжит работать, но
/// системный диалог доступа к экрану может появляться каждый раз.
pub(crate) async fn connect() -> Result<ashpd::zbus::Connection, String> {
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|error| format!("D-Bus: {error}"))?;
    if REGISTRY_AVAILABLE.load(Ordering::Relaxed) {
        let app_id = ashpd::AppID::try_from(PORTAL_APP_ID)
            .map_err(|error| format!("некорректный app_id: {error}"))?;
        let registered = tokio::time::timeout(
            Duration::from_secs(2),
            ashpd::register_host_app_with_connection(connection.clone(), app_id),
        )
        .await;
        if !matches!(registered, Ok(Ok(()))) {
            // Старый портал без Registry: не тратим 2 секунды на каждый запрос.
            REGISTRY_AVAILABLE.store(false, Ordering::Relaxed);
        }
    }
    Ok(connection)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Проверяет регистрацию app_id на отдельном соединении.
    #[test]
    #[ignore = "нужен живой xdg-desktop-portal"]
    fn host_app_registration_works() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let connection = ashpd::zbus::Connection::session()
                .await
                .expect("соединение с D-Bus");
            let app_id = ashpd::AppID::try_from(PORTAL_APP_ID).expect("корректный идентификатор");
            ashpd::register_host_app_with_connection(connection, app_id)
                .await
                .expect("регистрация приложения");
        });
    }

    /// Проверяет, что портал доступен из двух последовательных runtime.
    ///
    /// Тест создаёт и сразу закрывает пустые портальные сессии; диалог
    /// пользователю не показывается. Требует живой xdg-desktop-portal.
    #[test]
    #[ignore = "нужен живой xdg-desktop-portal"]
    fn portal_connection_works_across_runtimes() {
        for _ in 0..2 {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async {
                let connection = connect().await.expect("соединение с порталом");
                let proxy = ashpd::desktop::screencast::Screencast::with_connection(connection)
                    .await
                    .expect("Screencast");
                let session = tokio::time::timeout(
                    Duration::from_secs(3),
                    proxy.create_session(Default::default()),
                )
                .await
                .expect("таймаут CreateSession")
                .expect("CreateSession");
                let _ = session.close().await;
            });
            drop(runtime);
        }
    }
}
