//! Показ X11-окна по PID.
//!
//! Нужна для трея: окно могло остаться на другом рабочем столе или быть
//! свёрнутым. Winit отправляет `_NET_ACTIVE_WINDOW` с `CURRENT_TIME`, и
//! GNOME такие запросы от приложения часто игнорирует, показывая вместо
//! этого уведомление «окно ожидает». Здесь окно переносится на текущий
//! рабочий стол, при необходимости разворачивается через unmap/map и
//! поднимается без запроса фокуса; состояние «требует внимания» снимается.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, EventMask, PropMode, StackMode,
};
use x11rb::wrapper::ConnectionExt as _;

/// Находит окно процесса и делает его видимым на текущем рабочем столе.
///
/// Возвращает `Ok(false)`, если окно ещё не появилось в `_NET_CLIENT_LIST`
/// (тогда вызывающий может повторить попытку).
pub(crate) fn activate(pid: u32) -> Result<bool, String> {
    let (connection, screen_number) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = connection.setup().roots[screen_number].root;
    let atom = |name: &str| -> Result<u32, String> {
        connection
            .intern_atom(false, name.as_bytes())
            .map_err(|error| error.to_string())?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|error| error.to_string())
    };
    let client_list = atom("_NET_CLIENT_LIST")?;
    let pid_atom = atom("_NET_WM_PID")?;
    let desktop_atom = atom("_NET_WM_DESKTOP")?;
    let current_desktop_atom = atom("_NET_CURRENT_DESKTOP")?;
    let wm_state_atom = atom("_NET_WM_STATE")?;
    let hidden_atom = atom("_NET_WM_STATE_HIDDEN")?;
    let demands_atom = atom("_NET_WM_STATE_DEMANDS_ATTENTION")?;

    let windows = connection
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 1024)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .value32()
        .ok_or_else(|| "пустой список окон X11".to_string())?
        .collect::<Vec<_>>();
    let current_desktop = connection
        .get_property(false, root, current_desktop_atom, AtomEnum::CARDINAL, 0, 1)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .value32()
        .and_then(|mut values| values.next())
        .unwrap_or(0);

    for window in windows {
        let owner = connection
            .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?
            .value32()
            .and_then(|mut values| values.next());
        if owner != Some(pid) {
            continue;
        }
        let hidden = connection
            .get_property(false, window, wm_state_atom, AtomEnum::ATOM, 0, 64)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?
            .value32()
            .is_some_and(|values| values.into_iter().any(|atom| atom == hidden_atom));
        if hidden {
            // Winit не разворачивает окно: `_NET_ACTIVE_WINDOW` с
            // CURRENT_TIME GNOME игнорирует, а WM_STATE остаётся Iconic.
            // Unmap/map заставляет WM заново управлять окном как Normal.
            connection
                .unmap_window(window)
                .map_err(|error| error.to_string())?;
            connection.flush().map_err(|error| error.to_string())?;
            std::thread::sleep(Duration::from_millis(120));
            connection
                .map_window(window)
                .map_err(|error| error.to_string())?;
            connection.flush().map_err(|error| error.to_string())?;
        }
        // Переносим на текущий рабочий стол: иначе окно остаётся на старом
        // столе, а пользователь видит только уведомление «окно ожидает».
        connection
            .change_property32(
                PropMode::REPLACE,
                window,
                desktop_atom,
                AtomEnum::CARDINAL,
                &[current_desktop],
            )
            .map_err(|error| error.to_string())?;
        // Снимаем «требует внимания», иначе GNOME держит уведомление.
        let event = ClientMessageEvent::new(32, window, wm_state_atom, [0, demands_atom, 0, 2, 0]);
        connection
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .map_err(|error| error.to_string())?;
        connection
            .configure_window(
                window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )
            .map_err(|error| error.to_string())?;
        connection.flush().map_err(|error| error.to_string())?;
        return Ok(true);
    }
    Ok(false)
}
