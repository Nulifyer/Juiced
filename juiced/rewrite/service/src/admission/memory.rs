use super::{Error, Priority};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Legacy,
    Protected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mode: Mode,
    pub capacity_bytes: Option<u64>,
    pub essential_headroom_bytes: u64,
    pub maximum_sample_age_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Legacy,
            capacity_bytes: None,
            essential_headroom_bytes: 0,
            maximum_sample_age_ms: 500,
        }
    }
}

impl Settings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), Error> {
        if !(1..=5_000).contains(&self.maximum_sample_age_ms)
            || self.capacity_bytes.is_some_and(|capacity| {
                capacity > i64::MAX as u64 || self.essential_headroom_bytes > capacity
            })
            || self.essential_headroom_bytes > i64::MAX as u64
        {
            return Err(Error::new(
                400,
                "invalid_memory_settings",
                "Use bounded host capacity, headroom within capacity, and a sample age from 1 to 5000 milliseconds.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub pool: &'static str,
    pub capacity_bytes: Option<u64>,
    pub essential_headroom_bytes: u64,
    pub sampled_available_bytes: Option<u64>,
    pub sample_age_ms: Option<u64>,
    pub reserved_bytes: u64,
    pub outstanding_bytes: u64,
    pub resident_bytes: u64,
    pub capacity_state: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Allocation {
    bytes: u64,
    resident: bool,
}

impl Allocation {
    pub(super) fn outstanding(bytes: u64) -> Self {
        Self {
            bytes,
            resident: false,
        }
    }
}

#[derive(Default)]
pub(super) struct Ledger {
    reserved: u64,
    outstanding: u64,
    sample: Option<(Option<u64>, Instant)>,
    minimum_observed_at: Option<Instant>,
}

impl Ledger {
    pub(super) fn publish(&mut self, available: Option<u64>, observed_at: Instant) {
        if self
            .minimum_observed_at
            .is_none_or(|minimum| observed_at > minimum)
            && self
                .sample
                .is_none_or(|(_, previous)| observed_at >= previous)
        {
            self.sample = Some((available, observed_at));
        }
    }

    pub(super) fn fresh(&self, settings: &Settings, now: Instant) -> bool {
        self.sample.is_some_and(|(_, at)| {
            now.checked_duration_since(at)
                .is_some_and(|age| age <= Duration::from_millis(settings.maximum_sample_age_ms))
        })
    }

    fn available(&self, settings: &Settings, now: Instant) -> Option<u64> {
        let (available, observed_at) = self.sample?;
        let age = now.checked_duration_since(observed_at)?;
        (age <= Duration::from_millis(settings.maximum_sample_age_ms))
            .then_some(available)
            .flatten()
    }

    pub(super) fn check(
        &self,
        settings: &Settings,
        bytes: u64,
        priority: Priority,
        now: Instant,
    ) -> Result<(), Error> {
        let unknown = || {
            Error::new(
                503,
                "unknown_host_capacity",
                "Protected execution requires known host capacity and a fresh complete memory observation.",
            )
        };
        if bytes == 0 || bytes > i64::MAX as u64 {
            return Err(Error::new(
                400,
                "invalid_memory_claim",
                "A host memory claim must be positive and bounded.",
            ));
        }
        let capacity = settings.capacity_bytes.ok_or_else(unknown)?;
        let headroom = if priority == Priority::Essential {
            0
        } else {
            settings.essential_headroom_bytes
        };
        let configured_limit = capacity.saturating_sub(headroom);
        if self
            .reserved
            .checked_add(bytes)
            .is_none_or(|next| next > configured_limit)
        {
            return Err(Error::new(
                503,
                "host_memory_exhausted",
                "The declared protected host capacity cannot admit this claim.",
            ));
        }
        let available = self.available(settings, now).ok_or_else(unknown)?;
        let observed_limit = available.saturating_sub(headroom);
        if self
            .outstanding
            .checked_add(bytes)
            .is_none_or(|next| next > observed_limit)
        {
            return Err(Error::new(
                503,
                "host_memory_exhausted",
                "The protected host pool cannot admit this memory claim while preserving its headroom.",
            ));
        }
        Ok(())
    }

    pub(super) fn reserve(
        &mut self,
        settings: &Settings,
        bytes: u64,
        priority: Priority,
        now: Instant,
    ) -> Result<Allocation, Error> {
        self.check(settings, bytes, priority, now)?;
        self.reserved += bytes;
        self.outstanding += bytes;
        Ok(Allocation {
            bytes,
            resident: false,
        })
    }

    pub(super) fn shrink_outstanding(
        &mut self,
        allocation: &mut Allocation,
        bytes: u64,
    ) -> Result<(), Error> {
        if allocation.resident || bytes == 0 || bytes > allocation.bytes {
            return Err(Error::new(
                409,
                "invalid_outstanding_reduction",
                "Only a positive unmaterialized claim can shrink within its current bound.",
            ));
        }
        let released = allocation.bytes - bytes;
        self.reserved -= released;
        self.outstanding -= released;
        allocation.bytes = bytes;
        Ok(())
    }

    pub(super) fn materialize(
        &mut self,
        allocation: &mut Allocation,
        resident_bytes: u64,
    ) -> Result<(), Error> {
        if allocation.resident || resident_bytes == 0 || resident_bytes > allocation.bytes {
            return Err(Error::new(
                409,
                "invalid_resident_transition",
                "Only an unsettled load peak can become its bounded positive resident claim once.",
            ));
        }
        self.reserved -= allocation.bytes - resident_bytes;
        self.outstanding -= allocation.bytes;
        allocation.bytes = resident_bytes;
        allocation.resident = true;
        // A cached pre-load sample does not yet include the materialized resident.
        self.sample = None;
        self.minimum_observed_at = Some(Instant::now());
        Ok(())
    }

    pub(super) fn release(&mut self, allocation: Allocation) {
        self.reserved -= allocation.bytes;
        if !allocation.resident {
            self.outstanding -= allocation.bytes;
        }
    }

    pub(super) fn snapshot(&self, settings: &Settings, now: Instant) -> Snapshot {
        let available = self.available(settings, now);
        Snapshot {
            pool: "host",
            capacity_bytes: settings.capacity_bytes,
            essential_headroom_bytes: settings.essential_headroom_bytes,
            sampled_available_bytes: available,
            sample_age_ms: self.sample.and_then(|(_, at)| {
                now.checked_duration_since(at)
                    .map(|age| age.as_millis().min(u64::MAX as u128) as u64)
            }),
            reserved_bytes: self.reserved,
            outstanding_bytes: self.outstanding,
            resident_bytes: self.reserved - self.outstanding,
            capacity_state: if settings.capacity_bytes == Some(0) {
                "exhausted"
            } else if settings.capacity_bytes.is_none() || available.is_none() {
                "unknown"
            } else if available == Some(0) {
                "exhausted"
            } else {
                "observed"
            },
        }
    }
}

#[cfg(test)]
mod tests;
