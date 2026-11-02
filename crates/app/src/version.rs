//! Версия сборки.

/// `APP_VERSION` на этапе сборки (Makefile и Dockerfile передают
/// `git describe --tags --always --dirty`), иначе `dev`.
pub const VERSION: &str = match option_env!("APP_VERSION") {
    Some(version) => version,
    None => "dev",
};
