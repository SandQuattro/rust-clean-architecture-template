//! Разбор длительностей в формате Go: последовательность `<целое><единица>`.

use std::time::Duration;

/// Разбирает строку вида `1m30s`; единицы: `h`, `m`, `s`, `ms`. Ошибка — текст причины:
/// вызывающий добавляет имя переменной окружения.
pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    if s.is_empty() {
        return Err("empty duration".to_owned());
    }
    let mut rest = s;
    let mut total = Duration::ZERO;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(format!("invalid duration {s:?}: expected a number"));
        }
        let (num, tail) = rest.split_at(digits);
        let value: u64 = num
            .parse()
            .map_err(|_| format!("invalid duration {s:?}: number out of range"))?;
        // "ms" проверяется раньше "m", иначе миллисекунды читались бы как минуты.
        let (unit_ms, tail) = if let Some(t) = tail.strip_prefix("ms") {
            (1, t)
        } else if let Some(t) = tail.strip_prefix('h') {
            (3_600_000, t)
        } else if let Some(t) = tail.strip_prefix('m') {
            (60_000, t)
        } else if let Some(t) = tail.strip_prefix('s') {
            (1_000, t)
        } else {
            return Err(format!("invalid duration {s:?}: missing or unknown unit"));
        };
        let part = value
            .checked_mul(unit_ms)
            .map(Duration::from_millis)
            .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
        total = total
            .checked_add(part)
            .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
        rest = tail;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_go_style() {
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("1m30s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        for bad in ["", "10", "1x"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} должно быть ошибкой");
        }
    }
}
