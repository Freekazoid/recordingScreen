//! Показ X11-окна по PID и настройка его заголовка.
//!
//! Нужна для трея: окно могло остаться на другом рабочем столе, быть
//! свёрнутым или «выпасть» из управления WM после unmap/map. Winit
//! отправляет `_NET_ACTIVE_WINDOW` с `CURRENT_TIME`, и GNOME такие запросы
//! от приложения часто игнорирует, показывая вместо этого уведомление
//! «окно ожидает». Здесь окно находится по PID (в том числе вне
//! `_NET_CLIENT_LIST`), возвращается в Normal, переносится на текущий стол
//! и поднимается без запроса фокуса.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, EventMask, PropMode, StackMode,
};
use x11rb::wrapper::ConnectionExt as _;

/// Возвращает атом по имени.
fn atom(connection: &impl Connection, name: &str) -> Result<u32, String> {
    connection
        .intern_atom(false, name.as_bytes())
        .map_err(|error| error.to_string())?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| error.to_string())
}

/// Читает `_NET_WM_PID` окна.
fn window_pid(
    connection: &impl Connection,
    window: u32,
    pid_atom: u32,
) -> Result<Option<u32>, String> {
    Ok(connection
        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .value32()
        .and_then(|mut values| values.next()))
}

/// Находит окно процесса среди managed-окон или прямых детей root.
///
/// Второй путь нужен, если окно осталось в состоянии Withdrawn и пропало
/// из `_NET_CLIENT_LIST`.
fn find_window(connection: &impl Connection, root: u32, pid: u32) -> Result<Option<u32>, String> {
    let client_list = atom(connection, "_NET_CLIENT_LIST")?;
    let pid_atom = atom(connection, "_NET_WM_PID")?;
    let clients = connection
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 1024)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .value32()
        .map(|values| values.collect::<Vec<_>>())
        .unwrap_or_default();
    for window in clients {
        if window_pid(connection, window, pid_atom)? == Some(pid) {
            return Ok(Some(window));
        }
    }
    let tree = connection
        .query_tree(root)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?;
    for window in tree.children {
        if window_pid(connection, window, pid_atom)? == Some(pid) {
            return Ok(Some(window));
        }
    }
    Ok(None)
}

/// Возвращает окно в состояние Normal через unmap/map.
///
/// Mutter перечитывает Motif hints только при повторном map, а один map
/// после unmap иногда ещё застаёт окно в Withdrawn — поэтому проверяем
/// `WM_STATE` и повторяем map.
fn ensure_mapped(connection: &impl Connection, window: u32) -> Result<(), String> {
    let wm_state = atom(connection, "WM_STATE")?;
    connection
        .unmap_window(window)
        .map_err(|error| error.to_string())?;
    connection.flush().map_err(|error| error.to_string())?;
    std::thread::sleep(Duration::from_millis(250));
    for _ in 0..8 {
        connection
            .map_window(window)
            .map_err(|error| error.to_string())?;
        connection.flush().map_err(|error| error.to_string())?;
        std::thread::sleep(Duration::from_millis(120));
        let state = connection
            .get_property(false, window, wm_state, AtomEnum::ANY, 0, 2)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?
            .value32()
            .and_then(|mut values| values.next());
        if state == Some(1) {
            // NormalState
            return Ok(());
        }
    }
    Err("окно не вернулось в Normal после map".to_string())
}

/// Оставляет в заголовке X11-окна только «свернуть» и «закрыть».
///
/// Mutter учитывает Motif hints при повторном map: без `MWM_FUNC_MAXIMIZE`
/// кнопка maximize пропадает. Функции: resize|move|minimize|close.
pub(crate) fn disable_maximize(pid: u32) -> Result<bool, String> {
    let (connection, screen_number) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = connection.setup().roots[screen_number].root;
    let Some(window) = find_window(&connection, root, pid)? else {
        return Ok(false);
    };
    let motif_atom = atom(&connection, "_MOTIF_WM_HINTS")?;
    // flags=FUNCTIONS|DECORATIONS; без MAXIMIZE; decorations=all.
    connection
        .change_property32(
            PropMode::REPLACE,
            window,
            motif_atom,
            motif_atom,
            &[3, 46, 1, 0, 0],
        )
        .map_err(|error| error.to_string())?;
    connection.flush().map_err(|error| error.to_string())?;
    ensure_mapped(&connection, window)?;
    Ok(true)
}

/// Находит окно процесса и делает его видимым на текущем рабочем столе.
///
/// Возвращает `Ok(false)`, если окно ещё не появилось.
pub(crate) fn activate(pid: u32) -> Result<bool, String> {
    let (connection, screen_number) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = connection.setup().roots[screen_number].root;
    let Some(window) = find_window(&connection, root, pid)? else {
        return Ok(false);
    };
    let current_desktop_atom = atom(&connection, "_NET_CURRENT_DESKTOP")?;
    let desktop_atom = atom(&connection, "_NET_WM_DESKTOP")?;
    let wm_state_atom = atom(&connection, "_NET_WM_STATE")?;
    let hidden_atom = atom(&connection, "_NET_WM_STATE_HIDDEN")?;
    let demands_atom = atom(&connection, "_NET_WM_STATE_DEMANDS_ATTENTION")?;
    let current_desktop = connection
        .get_property(false, root, current_desktop_atom, AtomEnum::CARDINAL, 0, 1)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .value32()
        .and_then(|mut values| values.next())
        .unwrap_or(0);
    let hidden = connection
        .get_property(false, window, wm_state_atom, AtomEnum::ATOM, 0, 64)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .value32()
        .is_some_and(|values| values.into_iter().any(|atom| atom == hidden_atom));
    if hidden {
        ensure_mapped(&connection, window)?;
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
    Ok(true)
}

/// Возвращает X11-идентификатор окна процесса.
pub(crate) fn window_id(pid: u32) -> Result<Option<u64>, String> {
    let (connection, screen_number) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = connection.setup().roots[screen_number].root;
    Ok(find_window(&connection, root, pid)?.map(u64::from))
}
