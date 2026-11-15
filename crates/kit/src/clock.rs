//! Relógio injetável: os serviços recebem o horário de fora, para os testes
//! serem determinísticos.

use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};

use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime};

/// Fonte de horário.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Agora, em UTC.
    fn now(&self) -> OffsetDateTime;

    /// Agora, em segundos desde a época.
    fn epoch(&self) -> u64 {
        u64::try_from(self.now().unix_timestamp()).unwrap_or(0)
    }

    /// Agora, em RFC 3339 (`2026-11-15T14:02:11Z`).
    fn rfc3339(&self) -> String {
        rfc3339(self.now())
    }

    /// Hoje (UTC).
    fn today(&self) -> Date {
        self.now().date()
    }
}

/// Relógio do sistema.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Relógio fixo e ajustável, para testes.
#[derive(Debug)]
pub struct FixedClock(AtomicI64);

impl FixedClock {
    /// Começa no instante dado (segundos desde a época).
    #[must_use]
    pub fn at(epoch: i64) -> Self {
        Self(AtomicI64::new(epoch))
    }

    /// Avança o relógio.
    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.0.load(Ordering::SeqCst))
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }
}

/// Formata em RFC 3339, em UTC e sem frações.
#[must_use]
pub fn rfc3339(t: OffsetDateTime) -> String {
    t.replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Formata segundos desde a época em RFC 3339.
#[must_use]
pub fn rfc3339_epoch(epoch: u64) -> String {
    OffsetDateTime::from_unix_timestamp(i64::try_from(epoch).unwrap_or(0))
        .map(rfc3339)
        .unwrap_or_default()
}

/// Lê uma data `AAAA-MM-DD`.
#[must_use]
pub fn parse_date(value: &str) -> Option<Date> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(value, &format).ok()
}

/// Lê uma data e hora RFC 3339 e devolve segundos desde a época.
#[must_use]
pub fn parse_rfc3339(value: &str) -> Option<u64> {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|t| u64::try_from(t.unix_timestamp()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_formats() {
        let clock = FixedClock::at(1_763_215_331);
        assert_eq!(clock.rfc3339(), "2025-11-15T14:02:11Z");
        clock.advance(60);
        assert_eq!(clock.epoch(), 1_763_215_391);
        assert_eq!(parse_rfc3339("2025-11-15T14:02:11Z"), Some(1_763_215_331));
        assert!(parse_date("1990-02-30").is_none());
    }
}
