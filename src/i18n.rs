//! Interface languages. Each one is a `Strings` value, so a string missing
//! from a language is a compile error, not a blank label at runtime.
//! To add a language: a `Lang` variant, a `Strings` constant, and a line in
//! `code`, `strings` and `from_code`.
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ru];

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::En => &EN,
            Lang::Ru => &RU,
        }
    }

    /// "ru", "ru-RU", "ru_KZ.UTF-8" -> Ru.
    fn from_code(code: &str) -> Option<Lang> {
        match code.get(..2)?.to_ascii_lowercase().as_str() {
            "en" => Some(Lang::En),
            "ru" => Some(Lang::Ru),
            _ => None,
        }
    }

    /// The saved choice, else the system language, else English.
    pub fn load() -> Lang {
        let saved = file().and_then(|path| std::fs::read_to_string(path).ok());
        saved.as_deref().and_then(Lang::from_code).or_else(|| Lang::from_code(&sys_locale::get_locale()?)).unwrap_or(Lang::En)
    }

    /// Best effort: an unsaved choice only means the app starts in the system language.
    pub fn save(self) {
        if let Some(path) = file() {
            let _ = path.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(path, self.code());
        }
    }
}

fn file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("bkn-proxy").join("lang"))
}

/// Puts `value` into a template's `{}`.
pub fn fill(template: &str, value: &str) -> String {
    template.replacen("{}", value, 1)
}

pub struct Strings {
    /// The language's own name, as shown in the language switch.
    pub name: &'static str,

    // Sign in
    pub login_hint: &'static str,
    pub password: &'static str,
    pub code: &'static str,
    pub sign_in: &'static str,
    pub sign_in_hint: &'static str,
    pub waiting_browser: &'static str,
    /// Shown in the browser tab after sign-in.
    pub signed_in_browser: &'static str,
    pub open_window: &'static str,
    pub quit: &'static str,
    pub passkeys: &'static str,
    pub passkeys_hint: &'static str,
    pub manage_in_browser: &'static str,

    // Main screen
    pub settings: &'static str,
    pub connected: &'static str,
    pub connecting: &'static str,
    pub off: &'static str,
    pub direct: &'static str,
    /// `{}` is how long the connection has been up, e.g. 12:34.
    pub for_time: &'static str,
    pub download: &'static str,
    pub upload: &'static str,
    /// `{}` is an amount of data.
    pub in_total: &'static str,
    pub last_minute: &'static str,
    /// `{}` is a speed.
    pub peak: &'static str,
    /// `{}` is an amount of data.
    pub per_second: &'static str,
    /// Bytes, then each next thousand.
    pub units: [&'static str; 5],
    pub disconnect: &'static str,
    pub proxies: &'static str,
    pub none_yet: &'static str,
    pub connect: &'static str,
    pub rename: &'static str,
    pub rename_placeholder: &'static str,
    pub rename_hint: &'static str,
    pub save: &'static str,
    pub cancel: &'static str,
    /// `{}` is a version number.
    pub update_ready: &'static str,
    pub install: &'static str,

    // Settings
    pub back: &'static str,
    pub account: &'static str,
    pub signed_in_as: &'static str,
    pub sign_out: &'static str,
    pub two_factor: &'static str,
    pub two_factor_on: &'static str,
    pub two_factor_off: &'static str,
    pub turn_off_hint: &'static str,
    pub turn_off: &'static str,
    pub set_up_hint: &'static str,
    pub copy_key: &'static str,
    pub copy_link: &'static str,
    pub turn_on: &'static str,
    pub set_up: &'static str,
    pub current_password: &'static str,
    pub new_password: &'static str,
    pub change_password: &'static str,
    pub password_changed: &'static str,
    pub language: &'static str,

    // Problems
    pub proxy_revoked: &'static str,
    /// `{}` is the system's reason.
    pub session_not_saved: &'static str,
    pub tunnel_failed: &'static str,
    pub invalid_credentials: &'static str,
    pub totp_required: &'static str,
    pub invalid_totp: &'static str,
    pub locked: &'static str,
    pub disabled: &'static str,
    pub weak_password: &'static str,
    pub name_too_long: &'static str,
    pub offline: &'static str,
    pub elevation_denied: &'static str,
    pub helper_not_started: &'static str,
    pub helper_exited: &'static str,
}

impl Strings {
    /// Text for a machine code from the API ("invalid_totp") or the tunnel
    /// ("helper_exited"). Anything else, such as an operating system error,
    /// is shown as it came.
    pub fn message<'a>(&'a self, code: &'a str) -> &'a str {
        match code {
            "invalid_credentials" => self.invalid_credentials,
            "totp_required" => self.totp_required,
            "invalid_totp" => self.invalid_totp,
            "locked" => self.locked,
            "disabled" => self.disabled,
            "weak_password" => self.weak_password,
            "name_too_long" => self.name_too_long,
            "offline" => self.offline,
            "elevation_denied" => self.elevation_denied,
            "helper_not_started" => self.helper_not_started,
            "helper_exited" => self.helper_exited,
            other => other,
        }
    }
}

const EN: Strings = Strings {
    name: "English",

    login_hint: "Use the account your admin created.",
    password: "Password",
    code: "6-digit code",
    sign_in: "Sign in with the browser",
    sign_in_hint: "Your browser opens the sign-in page. Password, 2FA code and passkeys all work there.",
    waiting_browser: "Finish signing in in your browser…",
    signed_in_browser: "You are signed in. Go back to bkn-proxy; this tab can be closed.",
    open_window: "Open bkn-proxy",
    quit: "Quit",
    passkeys: "Passkeys",
    passkeys_hint: "Sign in with a fingerprint, your face or a security key instead of a password.",
    manage_in_browser: "Manage in browser",

    settings: "Settings",
    connected: "Connected",
    connecting: "Connecting",
    off: "Off",
    direct: "Traffic goes out directly",
    for_time: "for {}",
    download: "Download",
    upload: "Upload",
    in_total: "{} in total",
    last_minute: "Last minute",
    peak: "peak {}",
    per_second: "{}/s",
    units: ["B", "KB", "MB", "GB", "TB"],
    disconnect: "Disconnect",
    proxies: "Proxies",
    none_yet: "None yet. Ask your admin for access.",
    connect: "Connect",
    rename: "Rename",
    rename_placeholder: "Your name for this connection",
    rename_hint: "Only you see this name. Leave it empty to go back to the original.",
    save: "Save",
    cancel: "Cancel",
    update_ready: "Version {} is downloaded",
    install: "Install",

    back: "Back",
    account: "Account",
    signed_in_as: "Signed in as",
    sign_out: "Sign out",
    two_factor: "Two-factor authentication",
    two_factor_on: "On",
    two_factor_off: "Off",
    turn_off_hint: "Enter a code from your authenticator app to turn it off.",
    turn_off: "Turn off",
    set_up_hint: "Add this key to your authenticator app, then enter the code it shows.",
    copy_key: "Copy key",
    copy_link: "Copy setup link",
    turn_on: "Turn on",
    set_up: "Set up",
    current_password: "Current password",
    new_password: "New password (min 10 characters)",
    change_password: "Change password",
    password_changed: "Password changed. Your other devices were signed out.",
    language: "Language",

    proxy_revoked: "The proxy you were using is no longer available",
    session_not_saved: "Signed in, but the session can't be saved: {}",
    tunnel_failed: "Could not start the tunnel",
    invalid_credentials: "Wrong email or password",
    totp_required: "Enter the 6-digit code from your authenticator app",
    invalid_totp: "Wrong code",
    locked: "Too many attempts. Try again in 15 minutes",
    disabled: "This account is disabled",
    weak_password: "Password must be at least 10 characters",
    name_too_long: "The name can be 60 characters at most",
    offline: "Can't reach the server",
    elevation_denied: "Administrator access was not granted",
    helper_not_started: "The tunnel helper did not start",
    helper_exited: "The tunnel helper exited",
};

const RU: Strings = Strings {
    name: "Русский",

    login_hint: "Используйте учётную запись, которую создал администратор.",
    password: "Пароль",
    code: "6-значный код",
    sign_in: "Войти через браузер",
    sign_in_hint: "Откроется страница входа в браузере. Там работают пароль, код 2FA и ключи доступа.",
    waiting_browser: "Завершите вход в браузере…",
    signed_in_browser: "Вход выполнен. Вернитесь в bkn-proxy — эту вкладку можно закрыть.",
    open_window: "Открыть bkn-proxy",
    quit: "Выйти из программы",
    passkeys: "Ключи доступа",
    passkeys_hint: "Вход по отпечатку, лицу или аппаратному ключу вместо пароля.",
    manage_in_browser: "Настроить в браузере",

    settings: "Настройки",
    connected: "Подключено",
    connecting: "Соединение",
    off: "Выключено",
    direct: "Трафик идёт напрямую",
    for_time: "уже {}",
    download: "Загрузка",
    upload: "Отдача",
    in_total: "всего {}",
    last_minute: "Последняя минута",
    peak: "пик {}",
    per_second: "{}/с",
    units: ["Б", "КБ", "МБ", "ГБ", "ТБ"],
    disconnect: "Отключить",
    proxies: "Прокси",
    none_yet: "Пока пусто. Попросите администратора выдать доступ.",
    connect: "Подключить",
    rename: "Изменить",
    rename_placeholder: "Своё название подключения",
    rename_hint: "Это название видите только вы. Оставьте поле пустым, чтобы вернуть исходное.",
    save: "Сохранить",
    cancel: "Отмена",
    update_ready: "Версия {} загружена",
    install: "Установить",

    back: "Назад",
    account: "Аккаунт",
    signed_in_as: "Вы вошли как",
    sign_out: "Выйти",
    two_factor: "Двухфакторная защита",
    two_factor_on: "Вкл.",
    two_factor_off: "Выкл.",
    turn_off_hint: "Чтобы выключить, введите код из приложения-аутентификатора.",
    turn_off: "Выключить",
    set_up_hint: "Добавьте этот ключ в приложение-аутентификатор и введите код, который оно покажет.",
    copy_key: "Скопировать ключ",
    copy_link: "Скопировать ссылку",
    turn_on: "Включить",
    set_up: "Настроить",
    current_password: "Текущий пароль",
    new_password: "Новый пароль (от 10 символов)",
    change_password: "Сменить пароль",
    password_changed: "Пароль изменён. На других устройствах выполнен выход.",
    language: "Язык",

    proxy_revoked: "Прокси, которым вы пользовались, больше недоступен",
    session_not_saved: "Вход выполнен, но сохранить сессию не удалось: {}",
    tunnel_failed: "Не удалось запустить туннель",
    invalid_credentials: "Неверная почта или пароль",
    totp_required: "Введите 6-значный код из приложения-аутентификатора",
    invalid_totp: "Неверный код",
    locked: "Слишком много попыток. Попробуйте через 15 минут",
    disabled: "Учётная запись отключена",
    weak_password: "Пароль должен быть не короче 10 символов",
    name_too_long: "Название не длиннее 60 символов",
    offline: "Нет связи с сервером",
    elevation_denied: "Права администратора не получены",
    helper_not_started: "Помощник туннеля не запустился",
    helper_exited: "Помощник туннеля завершил работу",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(Lang::from_code("ru_KZ.UTF-8"), Some(Lang::Ru));
        assert_eq!(Lang::from_code("en-US"), Some(Lang::En));
        assert_eq!(Lang::from_code("kk-KZ"), None);
        assert_eq!(Lang::from_code(""), None);

        // The compiler checks every string exists; it can't check that a
        // template kept its placeholder.
        for lang in Lang::ALL {
            let s = lang.strings();
            for template in [s.for_time, s.in_total, s.peak, s.per_second, s.session_not_saved, s.update_ready] {
                assert!(template.contains("{}"), "{lang:?}: {template:?} lost its {{}}");
            }
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(fill(RU.in_total, "84 МБ"), "всего 84 МБ");
        assert_eq!(RU.message("invalid_totp"), "Неверный код");
        assert_eq!(RU.message("Operation not permitted"), "Operation not permitted");
    }
}
