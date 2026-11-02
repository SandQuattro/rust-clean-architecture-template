//! Конфигурация: файл + переменные окружения поверх.
//!
//! Имена переменных окружения совпадают с Go-шаблоном. Модуль не зависит от
//! остальных модулей `app`: `database` и `telemetry` получают простые параметры.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::duration::parse_duration;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {}", .0.display())]
    FileNotFound(PathBuf),
    #[error("required config value {env} is not set")]
    MissingRequired { env: &'static str },
    #[error("invalid value for {env}: {reason}")]
    Invalid { env: &'static str, reason: String },
    #[error("config source error: {0}")]
    Source(#[from] ::config::ConfigError),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub app: AppConfig,
    pub http: HttpConfig,
    pub db: DbConfig,
    pub log: LogConfig,
    pub tracing: TracingConfig,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub name: String,
    pub environment: String,
    pub debug: bool,
}

impl AppConfig {
    pub fn is_prod(&self) -> bool {
        self.environment == "prod"
    }

    pub fn is_dev(&self) -> bool {
        self.environment == "dev"
    }
}

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub port: u16,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub max_concurrency: usize,
}

#[derive(Clone)]
pub struct DbConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub name: String,
    pub sslmode: String,
    pub migrations_dir: PathBuf,
    pub pool_max: u32,
    pub pool_min: u32,
    pub connect_timeout: Duration,
    pub health_check_period: Duration,
    pub acquire_timeout: Duration,
}

/// Вывод без пароля: `debug!(?cfg)` не должен раскрывать секрет.
impl fmt::Debug for DbConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Деструктуризация: новое поле без строки здесь не скомпилируется.
        let Self {
            host,
            port,
            user,
            password: _,
            name,
            sslmode,
            migrations_dir,
            pool_max,
            pool_min,
            connect_timeout,
            health_check_period,
            acquire_timeout,
        } = self;
        f.debug_struct("DbConfig")
            .field("host", host)
            .field("port", port)
            .field("user", user)
            .field("password", &format_args!("<redacted>"))
            .field("name", name)
            .field("sslmode", sslmode)
            .field("migrations_dir", migrations_dir)
            .field("pool_max", pool_max)
            .field("pool_min", pool_min)
            .field("connect_timeout", connect_timeout)
            .field("health_check_period", health_check_period)
            .field("acquire_timeout", acquire_timeout)
            .finish()
    }
}

impl DbConfig {
    /// Единственный источник строки подключения (пул и мигратор). Пользователь и пароль
    /// кодируются: сгенерированный секрет может содержать `@ / ? # % :`.
    pub fn dsn(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            encode_userinfo(&self.user),
            encode_userinfo(&self.password),
            self.host,
            self.port,
            self.name,
            self.sslmode
        )
    }
}

/// Percent-encoding части userinfo URL (RFC 3986): кодируется всё, кроме unreserved-символов
/// (буквы, цифры, `-._~`), в том числе `:` и `@`, отделяющие пароль и хост.
fn encode_userinfo(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            // Запись в `String` не возвращает ошибку.
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Debug, Clone)]
pub struct LogConfig {
    pub level: tracing::Level,
}

#[derive(Debug, Clone)]
pub struct TracingConfig {
    pub url: String,
}

/// Соответствие «переменная окружения → ключ конфигурации».
/// Секция файла для логов называется `logger` (как в Go), поле — `Config.log`.
const ENV_KEYS: &[(&str, &str)] = &[
    ("APP_NAME", "app.name"),
    ("ENV_NAME", "app.environment"),
    ("DEBUG", "app.debug"),
    ("HTTP_PORT", "http.port"),
    ("HTTP_READ_TIMEOUT", "http.read_timeout"),
    ("HTTP_WRITE_TIMEOUT", "http.write_timeout"),
    ("HTTP_IDLE_TIMEOUT", "http.idle_timeout"),
    ("HTTP_REQUEST_TIMEOUT", "http.request_timeout"),
    ("HTTP_SHUTDOWN_TIMEOUT", "http.shutdown_timeout"),
    ("HTTP_MAX_CONCURRENCY", "http.max_concurrency"),
    ("DB_HOST", "db.host"),
    ("DB_PORT", "db.port"),
    ("DB_USER", "db.user"),
    ("DB_PASSWORD", "db.password"),
    ("DB_NAME", "db.name"),
    ("DB_SSLMODE", "db.sslmode"),
    ("MIGRATIONS_DIR", "db.migrations_dir"),
    ("PG_POOL_MAX", "db.pool_max"),
    ("PG_POOL_MIN", "db.pool_min"),
    ("PG_POOL_CONN_TIMEOUT", "db.connect_timeout"),
    ("PG_POOL_HEALTHCHECK", "db.health_check_period"),
    ("PG_POOL_ACQUIRE_TIMEOUT", "db.acquire_timeout"),
    ("LOG_LEVEL", "logger.level"),
    ("TRACING_URL", "tracing.url"),
];

/// Умолчания: как в Go; у параметров без умолчания — нулевые значения Go.
const DEFAULTS: &[(&str, &str)] = &[
    ("app.name", ""),
    ("app.environment", "dev"),
    ("app.debug", "false"),
    ("http.port", "8000"),
    ("http.read_timeout", "10s"),
    ("http.write_timeout", "10s"),
    ("http.idle_timeout", "60s"),
    ("http.request_timeout", "30s"),
    ("http.shutdown_timeout", "10s"),
    ("http.max_concurrency", "256"),
    ("db.host", ""),
    ("db.port", "0"),
    ("db.user", ""),
    ("db.name", ""),
    ("db.sslmode", "disable"),
    ("db.migrations_dir", "migrations"),
    ("db.pool_min", "1"),
    ("db.connect_timeout", "5"),
    ("db.health_check_period", "1"),
    ("db.acquire_timeout", "2s"),
    ("logger.level", "INFO"),
    ("tracing.url", ""),
];

/// Читает конфигурацию из `std::env::vars()` и текущей директории.
pub fn load() -> Result<Config, ConfigError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().map_err(|e| ConfigError::Invalid {
        env: "CONFIG_PATH",
        reason: format!("cannot determine current directory: {e}"),
    })?;
    load_from(&config_path(&env, &cwd), &env)
}

/// Порядок поиска файла: `CONFIG_PATH`; `config/config.toml`, `config/config.json`
/// от рабочей директории; те же файлы в корне репозитория относительно крейта.
/// Если ничего не найдено, возвращается первый кандидат (ошибка будет понятной).
pub fn config_path(env: &HashMap<String, String>, cwd: &Path) -> PathBuf {
    if let Some(path) = env.get("CONFIG_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let candidates = [
        cwd.join("config/config.toml"),
        cwd.join("config/config.json"),
        crate_root.join("config.toml"),
        crate_root.join("config.json"),
    ];
    candidates
        .iter()
        .find(|p| p.is_file())
        .unwrap_or(&candidates[0])
        .clone()
}

/// Чистая функция загрузки: файл + умолчания + переопределения из `env`.
pub fn load_from(file: &Path, env: &HashMap<String, String>) -> Result<Config, ConfigError> {
    if !file.is_file() {
        return Err(ConfigError::FileNotFound(file.to_path_buf()));
    }
    let mut builder = ::config::Config::builder();
    for (key, value) in DEFAULTS {
        builder = builder.set_default(*key, *value)?;
    }
    builder = builder.add_source(::config::File::from(file).required(true));
    for (name, key) in ENV_KEYS {
        if let Some(value) = env.get(*name) {
            builder = builder.set_override(*key, value.as_str())?;
        }
    }
    let raw = Reader(builder.build()?);

    let (host, host_port) = split_host(raw.string("DB_HOST")?)?;
    let db = DbConfig {
        host,
        port: match host_port {
            Some(p) => p,
            None => raw.parse("DB_PORT")?,
        },
        user: raw.string("DB_USER")?,
        password: raw.required("DB_PASSWORD")?,
        name: raw.string("DB_NAME")?,
        sslmode: ssl_mode(raw.string("DB_SSLMODE")?)?,
        migrations_dir: PathBuf::from(raw.string("MIGRATIONS_DIR")?),
        pool_max: pool_max(parse_value("PG_POOL_MAX", &raw.required("PG_POOL_MAX")?)?)?,
        pool_min: raw.parse("PG_POOL_MIN")?,
        connect_timeout: non_zero(
            "PG_POOL_CONN_TIMEOUT",
            Duration::from_secs(raw.parse("PG_POOL_CONN_TIMEOUT")?),
        )?,
        // Ноль сделал бы `idle_timeout` пула нулевым: чистка пула sqlx крутилась бы без пауз.
        health_check_period: non_zero(
            "PG_POOL_HEALTHCHECK",
            minutes("PG_POOL_HEALTHCHECK", raw.parse("PG_POOL_HEALTHCHECK")?)?,
        )?,
        acquire_timeout: raw.timeout("PG_POOL_ACQUIRE_TIMEOUT")?,
    };
    Ok(Config {
        app: AppConfig {
            name: raw.string("APP_NAME")?,
            environment: raw.string("ENV_NAME")?,
            debug: parse_bool(&raw.string("DEBUG")?).ok_or_else(|| ConfigError::Invalid {
                env: "DEBUG",
                reason: "expected a boolean".to_owned(),
            })?,
        },
        http: HttpConfig {
            port: raw.parse("HTTP_PORT")?,
            read_timeout: raw.timeout("HTTP_READ_TIMEOUT")?,
            write_timeout: raw.timeout("HTTP_WRITE_TIMEOUT")?,
            idle_timeout: raw.timeout("HTTP_IDLE_TIMEOUT")?,
            request_timeout: raw.timeout("HTTP_REQUEST_TIMEOUT")?,
            shutdown_timeout: raw.timeout("HTTP_SHUTDOWN_TIMEOUT")?,
            max_concurrency: concurrency_limit(raw.parse("HTTP_MAX_CONCURRENCY")?)?,
        },
        db,
        log: LogConfig {
            level: raw.parse("LOG_LEVEL")?,
        },
        tracing: TracingConfig {
            url: raw.string("TRACING_URL")?,
        },
    })
}

/// Проверяет `HTTP_MAX_CONCURRENCY`: при 0 API отклонял бы все запросы, значение больше
/// `Semaphore::MAX_PERMITS` роняет `GlobalConcurrencyLimitLayer` паникой.
fn concurrency_limit(value: usize) -> Result<usize, ConfigError> {
    if (1..=Semaphore::MAX_PERMITS).contains(&value) {
        Ok(value)
    } else {
        Err(ConfigError::Invalid {
            env: "HTTP_MAX_CONCURRENCY",
            reason: format!("{value} is out of range 1..={}", Semaphore::MAX_PERMITS),
        })
    }
}

/// Проверяет `DB_SSLMODE`: допустимы все шесть режимов libpq. `require` шифрует без проверки
/// сертификата, `verify-ca` и `verify-full` проверяют его по системному хранилищу
/// корневых сертификатов (фича sqlx `tls-rustls-ring-native-roots`). Регистр не важен, как в sqlx.
fn ssl_mode(value: String) -> Result<String, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "disable" | "allow" | "prefer" | "require" | "verify-ca" | "verify-full" => Ok(value),
        _ => Err(ConfigError::Invalid {
            env: "DB_SSLMODE",
            reason: format!(
                "{value:?}: expected disable, allow, prefer, require, verify-ca or verify-full"
            ),
        }),
    }
}

/// Длительность должна быть больше нуля. В Go 0 означает «без ограничения», здесь каждое
/// ожидание ограничено, а нулевой предел молча сделал бы сервис неработоспособным
/// (например, `HTTP_READ_TIMEOUT=0s` закрывал бы каждое соединение сразу).
fn non_zero(env: &'static str, value: Duration) -> Result<Duration, ConfigError> {
    if value.is_zero() {
        Err(ConfigError::Invalid {
            env,
            reason: "duration must be greater than zero".to_owned(),
        })
    } else {
        Ok(value)
    }
}

/// Целое число минут, как в Go; переполнение при переводе в секунды — ошибка конфигурации.
fn minutes(env: &'static str, value: u64) -> Result<Duration, ConfigError> {
    value
        .checked_mul(60)
        .map(Duration::from_secs)
        .ok_or_else(|| ConfigError::Invalid {
            env,
            reason: format!("{value} minutes is out of range"),
        })
}

/// `PG_POOL_MAX` не меньше 1, как `MaxConns` в pgxpool: пул без соединений не обслужил бы
/// ни одного запроса, а ошибка выглядела бы как недоступная БД.
fn pool_max(value: u32) -> Result<u32, ConfigError> {
    if value == 0 {
        Err(ConfigError::Invalid {
            env: "PG_POOL_MAX",
            reason: "must be at least 1".to_owned(),
        })
    } else {
        Ok(value)
    }
}

/// Булево значение в формате Go `strconv.ParseBool`.
fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Аналог Go `net.SplitHostPort` для `DB_HOST`; без двоеточия порт не задан.
fn split_host(value: String) -> Result<(String, Option<u16>), ConfigError> {
    let invalid = |reason: String| ConfigError::Invalid {
        env: "DB_HOST",
        reason,
    };
    if !value.contains(':') {
        return Ok((value, None));
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| invalid(format!("missing ']' in address {value:?}")))?;
        let port = tail
            .strip_prefix(':')
            .ok_or_else(|| invalid(format!("missing port in address {value:?}")))?;
        (host, port)
    } else {
        let (host, port) = value.split_once(':').unwrap_or((&value, ""));
        if port.contains(':') {
            return Err(invalid(format!("too many colons in address {value:?}")));
        }
        (host, port)
    };
    let port = port
        .parse::<u16>()
        .map_err(|e| invalid(format!("invalid port {port:?}: {e}")))?;
    Ok((host.to_owned(), Some(port)))
}

/// Доступ к значениям собранного `config::Config` по имени переменной окружения.
struct Reader(::config::Config);

fn parse_value<T: FromStr>(env: &'static str, value: &str) -> Result<T, ConfigError>
where
    T::Err: std::fmt::Display,
{
    value
        .trim()
        .parse()
        .map_err(|e: T::Err| ConfigError::Invalid {
            env,
            reason: format!("{value:?}: {e}"),
        })
}

impl Reader {
    fn key(env: &'static str) -> &'static str {
        ENV_KEYS
            .iter()
            .find(|(name, _)| *name == env)
            .map_or("", |(_, key)| key)
    }

    fn lookup(&self, env: &'static str) -> Result<Option<String>, ConfigError> {
        match self.0.get_string(Self::key(env)) {
            Ok(v) => Ok(Some(v)),
            Err(::config::ConfigError::NotFound(_)) => Ok(None),
            Err(e) => Err(ConfigError::Invalid {
                env,
                reason: e.to_string(),
            }),
        }
    }

    fn string(&self, env: &'static str) -> Result<String, ConfigError> {
        Ok(self.lookup(env)?.unwrap_or_default())
    }

    fn required(&self, env: &'static str) -> Result<String, ConfigError> {
        match self.lookup(env)? {
            Some(value) if !value.is_empty() => Ok(value),
            _ => Err(ConfigError::MissingRequired { env }),
        }
    }

    fn parse<T: FromStr>(&self, env: &'static str) -> Result<T, ConfigError>
    where
        T::Err: std::fmt::Display,
    {
        parse_value(env, &self.string(env)?)
    }

    fn timeout(&self, env: &'static str) -> Result<Duration, ConfigError> {
        let value = self.string(env)?;
        let parsed =
            parse_duration(&value).map_err(|reason| ConfigError::Invalid { env, reason })?;
        non_zero(env, parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::ConnectOptions;
    use sqlx::postgres::PgConnectOptions;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn json_file() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/config.json")
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// Пароль БД для тестов создаётся при запуске: значений паролей в git нет.
    /// Состоит из букв и цифр, поэтому в строке подключения не кодируется.
    fn generated_password() -> String {
        format!("pw{}", std::process::id())
    }

    /// Окружение `pairs` с обязательным `DB_PASSWORD` из [`generated_password`].
    fn env_with_password(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        let mut e = env(pairs);
        e.insert("DB_PASSWORD".to_owned(), generated_password());
        e
    }

    fn unique_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("app-cfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_file_defaults() {
        let cfg = load_from(&json_file(), &env_with_password(&[])).unwrap();
        assert_eq!(cfg.app.name, "clean_architecture_template");
        assert_eq!(cfg.app.environment, "prod");
        assert_eq!(cfg.http.port, 8000);
        assert_eq!(cfg.http.request_timeout, Duration::from_secs(30));
        assert_eq!(cfg.http.shutdown_timeout, Duration::from_secs(10));
        assert_eq!(cfg.http.read_timeout, Duration::from_secs(10));
        assert_eq!(cfg.http.write_timeout, Duration::from_secs(10));
        assert_eq!(cfg.http.idle_timeout, Duration::from_secs(60));
        assert_eq!(cfg.db.host, "postgres");
        assert_eq!(cfg.db.pool_max, 2);
        assert_eq!(cfg.db.pool_min, 1);
        assert_eq!(cfg.db.connect_timeout, Duration::from_secs(2));
        assert_eq!(cfg.db.acquire_timeout, Duration::from_secs(2));
        assert_eq!(cfg.db.migrations_dir, PathBuf::from("migrations"));
        assert_eq!(cfg.log.level, tracing::Level::DEBUG);
        assert_eq!(cfg.tracing.url, "jaeger:4317");
    }

    #[test]
    fn env_overrides_file() {
        let e = env_with_password(&[
            ("HTTP_PORT", "9100"),
            ("PG_POOL_MAX", "10"),
            ("HTTP_REQUEST_TIMEOUT", "5s"),
            ("PG_POOL_ACQUIRE_TIMEOUT", "250ms"),
            ("HTTP_MAX_CONCURRENCY", "64"),
            ("ENV_NAME", "dev"),
        ]);
        let cfg = load_from(&json_file(), &e).unwrap();
        assert_eq!(cfg.http.port, 9100);
        assert_eq!(cfg.db.pool_max, 10);
        assert_eq!(cfg.http.request_timeout, Duration::from_secs(5));
        assert_eq!(cfg.db.acquire_timeout, Duration::from_millis(250));
        assert_eq!(cfg.http.max_concurrency, 64);
        assert_eq!(cfg.app.environment, "dev");
        assert!(cfg.app.is_dev());
        assert!(!cfg.app.is_prod());
    }

    #[test]
    fn max_concurrency_must_fit_semaphore() {
        let max = Semaphore::MAX_PERMITS.to_string();
        let too_big = (Semaphore::MAX_PERMITS + 1).to_string();
        for (value, valid) in [("0", false), ("1", true), (&max, true), (&too_big, false)] {
            let e = env_with_password(&[("HTTP_MAX_CONCURRENCY", value)]);
            let res = load_from(&json_file(), &e);
            if valid {
                assert_eq!(res.unwrap().http.max_concurrency.to_string(), value);
            } else {
                assert!(
                    matches!(
                        res,
                        Err(ConfigError::Invalid {
                            env: "HTTP_MAX_CONCURRENCY",
                            ..
                        })
                    ),
                    "{value}: {res:?}"
                );
            }
        }
    }

    #[test]
    fn zero_timeouts_are_rejected() {
        let cases = [
            ("HTTP_READ_TIMEOUT", "0s"),
            ("HTTP_WRITE_TIMEOUT", "0ms"),
            ("HTTP_IDLE_TIMEOUT", "0s"),
            ("HTTP_REQUEST_TIMEOUT", "0s"),
            ("HTTP_SHUTDOWN_TIMEOUT", "0m"),
            ("PG_POOL_ACQUIRE_TIMEOUT", "0s"),
            // Целое число секунд, как в Go.
            ("PG_POOL_CONN_TIMEOUT", "0"),
        ];
        for (name, value) in cases {
            let e = env_with_password(&[(name, value)]);
            let res = load_from(&json_file(), &e);
            assert!(
                matches!(&res, Err(ConfigError::Invalid { env, .. }) if *env == name),
                "{name}={value}: {res:?}"
            );
        }
    }

    #[test]
    fn all_libpq_ssl_modes_are_accepted() {
        for value in [
            "disable",
            "allow",
            "prefer",
            "require",
            "verify-ca",
            "verify-full",
            "REQUIRE",
        ] {
            let e = env_with_password(&[("DB_SSLMODE", value)]);
            let cfg = load_from(&json_file(), &e).unwrap();
            assert_eq!(cfg.db.sslmode, value);
        }
    }

    #[test]
    fn unknown_ssl_mode_is_rejected() {
        let e = env_with_password(&[("DB_SSLMODE", "bogus")]);
        let res = load_from(&json_file(), &e);
        assert!(
            matches!(
                &res,
                Err(ConfigError::Invalid {
                    env: "DB_SSLMODE",
                    ..
                })
            ),
            "{res:?}"
        );
    }

    #[test]
    fn invalid_pool_settings_are_rejected() {
        let overflow = u64::MAX.to_string();
        let cases = [
            // Период 0 крутил бы чистку пула sqlx в цикле без пауз.
            ("PG_POOL_HEALTHCHECK", "0"),
            // Минуты переводятся в секунды умножением на 60.
            ("PG_POOL_HEALTHCHECK", overflow.as_str()),
            ("PG_POOL_MAX", "0"),
        ];
        for (name, value) in cases {
            let e = env_with_password(&[(name, value)]);
            let res = load_from(&json_file(), &e);
            assert!(
                matches!(&res, Err(ConfigError::Invalid { env, .. }) if *env == name),
                "{name}={value}: {res:?}"
            );
        }
    }

    #[test]
    fn debug_output_redacts_password() {
        let cfg = load_from(&json_file(), &env_with_password(&[])).unwrap();

        let out = format!("{cfg:?}");

        assert!(!out.contains(&generated_password()), "{out}");
        assert!(out.contains("password: <redacted>"), "{out}");
        assert!(out.contains(r#"host: "postgres""#), "{out}");
    }

    #[test]
    fn missing_password_is_error() {
        let err = load_from(&json_file(), &env(&[])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::MissingRequired { env: "DB_PASSWORD" }
        ));
        assert!(err.to_string().contains("DB_PASSWORD"));
    }

    #[test]
    fn missing_pool_max_is_error() {
        let dir = unique_dir("pool-max");
        let file = dir.join("config.toml");
        std::fs::write(&file, "[db]\nhost = \"h\"\n").unwrap();
        let res = load_from(&file, &env_with_password(&[]));
        let _ = std::fs::remove_dir_all(&dir);
        let err = res.unwrap_err();
        assert!(matches!(
            err,
            ConfigError::MissingRequired { env: "PG_POOL_MAX" }
        ));
        assert!(err.to_string().contains("PG_POOL_MAX"));
    }

    #[test]
    fn db_host_with_port_is_split() {
        let e = env_with_password(&[("DB_HOST", "db.local:6543")]);
        let cfg = load_from(&json_file(), &e).unwrap();
        assert_eq!(cfg.db.host, "db.local");
        assert_eq!(cfg.db.port, 6543);
    }

    #[test]
    fn db_host_with_invalid_port_is_error() {
        let e = env_with_password(&[("DB_HOST", "db.local:abc")]);
        let err = load_from(&json_file(), &e).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { env: "DB_HOST", .. }));
    }

    #[test]
    fn invalid_env_value_names_variable() {
        let e = env_with_password(&[("HTTP_READ_TIMEOUT", "10")]);
        let err = load_from(&json_file(), &e).unwrap_err();
        assert!(err.to_string().contains("HTTP_READ_TIMEOUT"));
    }

    #[test]
    fn missing_file_is_error() {
        let err = load_from(Path::new("/nonexistent/config.toml"), &env(&[])).unwrap_err();
        assert!(matches!(err, ConfigError::FileNotFound(_)));
    }

    #[test]
    fn loads_toml_file() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/config.toml");
        let cfg = load_from(&file, &env_with_password(&[])).unwrap();
        assert_eq!(cfg.app.name, "template");
        assert_eq!(cfg.db.connect_timeout, Duration::from_secs(1));
        assert_eq!(cfg.db.health_check_period, Duration::from_secs(60));
    }

    #[test]
    fn dsn_format() {
        let cfg = load_from(&json_file(), &env_with_password(&[])).unwrap();
        assert_eq!(
            cfg.db.dsn(),
            format!(
                "postgres://postgres:{}@postgres:5432/demo?sslmode=disable",
                generated_password()
            )
        );
    }

    #[test]
    fn dsn_percent_encodes_credentials() {
        // К паролю добавлены зарезервированные символы URL: `%41` без кодирования
        // превратился бы в `A`, остальные ломают разбор URL.
        let mut e = env(&[("DB_USER", "us:er@")]);
        e.insert(
            "DB_PASSWORD".to_owned(),
            format!("{}@/?#%41:", generated_password()),
        );
        let db = load_from(&json_file(), &e).unwrap().db;
        let encoded = format!("{}%40%2F%3F%23%2541%3A", generated_password());

        assert_eq!(
            db.dsn(),
            format!("postgres://us%3Aer%40:{encoded}@postgres:5432/demo?sslmode=disable")
        );
        let options = PgConnectOptions::from_str(&db.dsn()).unwrap();
        assert_eq!(options.get_username(), "us:er@");
        assert_eq!(options.get_host(), "postgres");
        assert_eq!(options.get_database(), Some("demo"));
        // sqlx декодирует пароль в исходный: `to_url_lossy` кодирует его заново (все символы,
        // кроме букв и цифр). Имя пользователя `to_url_lossy` не кодирует, поэтому здесь оно простое.
        let plain_user = DbConfig {
            user: "u".to_owned(),
            ..db
        };
        let url = PgConnectOptions::from_str(&plain_user.dsn())
            .unwrap()
            .to_url_lossy();
        assert_eq!(url.password(), Some(encoded.as_str()));
    }

    #[test]
    fn config_path_prefers_env_then_cwd() {
        let dir = unique_dir("path");
        let cwd = dir.as_path();
        // CONFIG_PATH побеждает всегда.
        assert_eq!(
            config_path(&env(&[("CONFIG_PATH", "/x/y.toml")]), cwd),
            PathBuf::from("/x/y.toml")
        );
        // Только json в cwd.
        std::fs::create_dir_all(cwd.join("config")).unwrap();
        std::fs::write(cwd.join("config/config.json"), "{}").unwrap();
        let only_json = config_path(&env(&[]), cwd);
        // toml в cwd приоритетнее json.
        std::fs::write(cwd.join("config/config.toml"), "").unwrap();
        let with_toml = config_path(&env(&[]), cwd);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(only_json, cwd.join("config/config.json"));
        assert_eq!(with_toml, cwd.join("config/config.toml"));
    }
}
