//! Прокачка сообщений Windows в фоновом сервисе.
//!
//! `RegisterHotKey` и трей доставляют события через оконные сообщения.
//! У сервиса нет `winit`-цикла, поэтому сообщения нужно забирать вручную:
//! иначе глобальные клавиши регистрируются, но обработчик не вызывается.

use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

/// Обрабатывает все накопившиеся сообщения потока.
pub fn pump_messages() {
    // SAFETY: MSG — обычная структура Win32; цикл завершается, когда
    // очередь пуста. Указатель на окно не нужен: забираем сообщения всех
    // окон текущего потока.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

use std::marker::PhantomData;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// Эксклюзивный признак единственного фонового сервиса.
///
/// Два одновременно запущенных сервиса заняли бы разные TCP-порты и
/// перезаписали endpoint-файл: один из них остался бы сиротой и мог
/// продолжать запись. Именованный mutex закрывает эту гонку.
#[derive(Debug)]
pub struct DaemonLock {
    handle: *mut std::ffi::c_void,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for DaemonLock {
    /// Освобождает именованный mutex.
    fn drop(&mut self) {
        // SAFETY: handle получен из CreateMutexW и ещё не закрыт.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Пытается захватить единственный экземпляр сервиса.
///
/// `None` означает, что сервис уже запущен в этой сессии.
pub fn daemon_lock() -> Result<Option<DaemonLock>, String> {
    let name: Vec<u16> = "Local\\ScreenRecorderDaemon-v4\0".encode_utf16().collect();
    // SAFETY: имя — валидная UTF-16 строка с завершающим нулём.
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 1, name.as_ptr()) };
    if handle.is_null() {
        return Err(format!(
            "не удалось создать mutex сервиса: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: GetLastError читает код последнего вызова потока.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: дескриптор больше не нужен.
        unsafe {
            CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(DaemonLock {
        handle,
        _not_send: PhantomData,
    }))
}
