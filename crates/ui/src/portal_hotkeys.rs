//! Глобальные горячие клавиши через XDG Desktop Portal.
//!
//! На Wayland X11-захват клавиш работает только когда фокус у XWayland-окна.
//! Портал `org.freedesktop.portal.GlobalShortcuts` регистрирует сочетания на
//! уровне compositor'а, поэтому они срабатывают и в нативных приложениях.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ashpd::desktop::CreateSessionOptions;
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::{WindowIdentifier, WindowIdentifierType};
use futures_lite::StreamExt;

use crate::TrayCommand;

/// Событие портала для фонового сервиса.
pub(crate) enum PortalHotkeyEvent {
    /// Портал вернул фактические сочетания: (id, описание).
    Bound(Vec<(String, String)>),
    /// Пользователь нажал сочетание.
    Activated(TrayCommand),
    /// Портал недоступен; нужен fallback на X11.
    Failed(String),
}

/// Описание привязываемых сочетаний.
const SHORTCUTS: &[(&str, &str, &str, TrayCommand)] = &[
    (
        "fullscreen",
        "Запись всего экрана",
        "CTRL+SHIFT+1",
        TrayCommand::FullScreen,
    ),
    (
        "area",
        "Запись области экрана",
        "CTRL+SHIFT+2",
        TrayCommand::Area,
    ),
    (
        "program",
        "Запись программы",
        "CTRL+SHIFT+3",
        TrayCommand::Program,
    ),
    (
        "audio_only",
        "Только аудио",
        "CTRL+SHIFT+4",
        TrayCommand::AudioOnly,
    ),
    (
        "stop",
        "Остановить запись",
        "CTRL+SHIFT+5",
        TrayCommand::Stop,
    ),
];

/// Запускает поток портала и возвращает канал событий.
///
/// `parent` — X11-окно UI: портал использует его как родителя диалога
/// настройки. Ждём его недолго, чтобы запуск не блокировался.
pub(crate) fn spawn(parent: Receiver<u64>) -> Result<Receiver<PortalHotkeyEvent>, String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let parent = parent.recv_timeout(Duration::from_secs(3)).ok();
        if let Err(error) = futures_lite::future::block_on(run(sender.clone(), parent)) {
            let _ = sender.send(PortalHotkeyEvent::Failed(error));
        }
    });
    Ok(receiver)
}

/// Создаёт сессию, привязывает сочетания и слушает активации.
async fn run(sender: Sender<PortalHotkeyEvent>, parent: Option<u64>) -> Result<(), String> {
    let portal = GlobalShortcuts::new()
        .await
        .map_err(|error| format!("портал GlobalShortcuts: {error}"))?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|error| format!("сессия портала: {error}"))?;
    let shortcuts = SHORTCUTS
        .iter()
        .map(|(id, description, trigger, _)| {
            NewShortcut::new(*id, *description).preferred_trigger(*trigger)
        })
        .collect::<Vec<_>>();
    let identifier = parent.map(|xid| WindowIdentifier::from(WindowIdentifierType::X11(xid as _)));
    let request = portal
        .bind_shortcuts(
            &session,
            &shortcuts,
            identifier.as_ref(),
            BindShortcutsOptions::default(),
        )
        .await
        .map_err(|error| format!("привязка клавиш: {error}"))?;
    let response = request
        .response()
        .map_err(|error| format!("ответ портала: {error}"))?;
    let bound = response
        .shortcuts()
        .iter()
        .map(|shortcut| {
            (
                shortcut.id().to_string(),
                shortcut.trigger_description().to_string(),
            )
        })
        .collect::<Vec<_>>();
    let _ = sender.send(PortalHotkeyEvent::Bound(bound));

    let mut activations = portal
        .receive_activated()
        .await
        .map_err(|error| format!("сигнал активации: {error}"))?;
    while let Some(activated) = activations.next().await {
        let Some((_, _, _, command)) = SHORTCUTS
            .iter()
            .find(|(id, _, _, _)| *id == activated.shortcut_id())
        else {
            continue;
        };
        let _ = sender.send(PortalHotkeyEvent::Activated(*command));
    }
    Ok(())
}
