//! Доменные ошибки. Тексты первых восьми вариантов совпадают с Go-шаблоном.

/// Непрозрачный источник служебной ошибки; наружу (клиенту) не отдаётся.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("user not found")]
    UserNotFound,
    #[error("source account not found")]
    SourceAccountNotFound,
    #[error("destination account not found")]
    DestAccountNotFound,
    #[error("user name must be a non-empty valid UTF-8 string")]
    InvalidUserName,
    #[error("page and size must be greater than zero")]
    InvalidPagination,
    #[error("transfer amount must be positive")]
    NonPositiveAmount,
    #[error("cannot transfer to the same account")]
    SameAccount,
    #[error("insufficient funds")]
    InsufficientFunds,
    /// Временная перегрузка (например, таймаут ожидания соединения из пула).
    #[error("{context}")]
    Unavailable {
        context: &'static str,
        source: BoxError,
    },
    /// Непредвиденный сбой инфраструктуры.
    #[error("{context}")]
    Internal {
        context: &'static str,
        source: BoxError,
    },
}

impl Error {
    pub fn internal(context: &'static str, source: impl Into<BoxError>) -> Self {
        Self::Internal {
            context,
            source: source.into(),
        }
    }

    pub fn unavailable(context: &'static str, source: impl Into<BoxError>) -> Self {
        Self::Unavailable {
            context,
            source: source.into(),
        }
    }
}
