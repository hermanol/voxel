// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Telemetry domain types and parsers.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use voxel_config::VoxelConfig;

pub const SLED_CUBBIES: u8 = 32;
pub const SWITCH_SLOTS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RackId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceKind {
    Sled,
    SwitchZone,
    Router,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceScope {
    Fleet,
    Rack(RackId),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId {
    pub scope: ResourceScope,
    pub kind: ResourceKind,
    pub name: String,
}

impl ResourceId {
    pub fn rack(
        rack: RackId,
        kind: ResourceKind,
        name: impl Into<String>,
    ) -> Self {
        Self { scope: ResourceScope::Rack(rack), kind, name: name.into() }
    }

    pub fn fleet(kind: ResourceKind, name: impl Into<String>) -> Self {
        Self { scope: ResourceScope::Fleet, kind, name: name.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDescriptor {
    pub id: ResourceId,
    pub rack: Option<RackId>,
    pub kind: ResourceKind,
    pub name: String,
    /// Hosting sled for a switch zone.
    pub host: Option<String>,
    /// Physical position in the rack elevation: a sled's cubby or a switch
    /// zone's switch slot. None for routers and for anything a real rack has
    /// no room for.
    pub slot: Option<u8>,
}

pub fn resource_descriptors(config: &VoxelConfig) -> Vec<ResourceDescriptor> {
    let sleds = config.sleds();
    let mut result = Vec::new();
    let mut switch_slots = BTreeMap::<usize, usize>::new();
    for sled in sleds {
        let rack = RackId(sled.rack);
        result.push(ResourceDescriptor {
            id: ResourceId::rack(rack, ResourceKind::Sled, &sled.name),
            rack: Some(rack),
            kind: ResourceKind::Sled,
            name: sled.name.clone(),
            host: None,
            // The SP location MGS reports is the global sled index, so a
            // second rack's sleds sit where the control plane says they do.
            slot: u8::try_from(sled.index)
                .ok()
                .filter(|cubby| *cubby < SLED_CUBBIES),
        });
        if sled.scrimlet {
            let slot = switch_slots.entry(sled.rack).or_default();
            let name = format!("switch{slot}");
            let switch_slot =
                u8::try_from(*slot).ok().filter(|slot| *slot < SWITCH_SLOTS);
            *slot += 1;
            result.push(ResourceDescriptor {
                // The rack-local slot is presentation/targeting data; the hosting
                // scrimlet's configured name remains stable if slots are reordered.
                id: ResourceId::rack(
                    rack,
                    ResourceKind::SwitchZone,
                    &sled.name,
                ),
                rack: Some(rack),
                kind: ResourceKind::SwitchZone,
                name,
                host: Some(sled.name),
                slot: switch_slot,
            });
        }
    }
    for name in &config.topology.routers {
        result.push(ResourceDescriptor {
            id: ResourceId::fleet(ResourceKind::Router, name),
            rack: None,
            kind: ResourceKind::Router,
            name: name.clone(),
            host: None,
            slot: None,
        });
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    result
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeAddresses {
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
}

pub fn parse_ipadm_addresses(output: &str) -> NodeAddresses {
    let mut result = NodeAddresses::default();
    for line in output.lines() {
        let Some((object, raw)) = line.split_once(':') else {
            continue;
        };
        if object.starts_with("lo0") || object.ends_with("/ll") {
            continue;
        }
        let address = raw.replace("\\:", ":");
        let target = if address.contains('.') {
            if object.starts_with("vioif") && !result.ipv4.contains(&address) {
                result.ipv4.insert(0, address);
                continue;
            }
            &mut result.ipv4
        } else if address.contains(':') && !address.starts_with("fe80") {
            if (object.contains("sled") || object.contains("underlay"))
                && !result.ipv6.contains(&address)
            {
                result.ipv6.insert(0, address);
                continue;
            }
            &mut result.ipv6
        } else {
            continue;
        };
        if !target.contains(&address) {
            target.push(address);
        }
    }
    result
}

pub fn parse_linux_ip_addresses(output: &str) -> NodeAddresses {
    let mut result = NodeAddresses::default();
    for line in output.lines() {
        if line.contains(": lo ")
            || line.contains("docker")
            || line.contains("veth")
        {
            continue;
        }
        let (marker, target, ipv6) = if line.contains(" inet6 ") {
            (" inet6 ", &mut result.ipv6, true)
        } else if line.contains(" inet ") {
            (" inet ", &mut result.ipv4, false)
        } else {
            continue;
        };
        let Some(raw) = line
            .split_once(marker)
            .and_then(|(_, rest)| rest.split_whitespace().next())
        else {
            continue;
        };
        let valid_family = raw
            .split('/')
            .next()
            .and_then(|address| address.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_ipv6() == ipv6);
        if !valid_family {
            continue;
        }
        if raw.starts_with("127.")
            || raw.starts_with("fe80")
            || raw.starts_with("::1")
        {
            continue;
        }
        let address = raw.to_owned();
        if !target.contains(&address) {
            target.push(address);
        }
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Online,
    Offline,
    Disabled,
    Maintenance,
    Degraded,
}

pub fn parse_service_state(output: &str) -> Option<ServiceState> {
    match output.trim() {
        "online" => Some(ServiceState::Online),
        "offline" => Some(ServiceState::Offline),
        "disabled" => Some(ServiceState::Disabled),
        "maintenance" => Some(ServiceState::Maintenance),
        "degraded" => Some(ServiceState::Degraded),
        _ => None,
    }
}

pub fn parse_failed_services(output: &str) -> Vec<String> {
    let mut services: Vec<_> = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("svc:/"))
        .filter_map(|service| {
            service.rsplit_once(':').map(|(name, _)| name.to_owned())
        })
        .collect();
    services.sort();
    services.dedup();
    services
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneDiagnostic {
    pub zones: Vec<String>,
}

pub fn parse_zone_diagnostics(output: &str) -> ZoneDiagnostic {
    let mut zones: Vec<_> = output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
        .map(str::to_owned)
        .collect();
    zones.sort();
    zones.dedup();
    ZoneDiagnostic { zones }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NtpDiagnostic {
    pub stratum: Option<u32>,
    pub synchronized: Option<bool>,
}

pub fn parse_chrony_tracking(output: &str) -> NtpDiagnostic {
    let mut result = NtpDiagnostic::default();
    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "stratum" => result.stratum = value.trim().parse().ok(),
            "leap status" => {
                let value = value.trim().to_ascii_lowercase();
                result.synchronized = Some(
                    !(value.contains("not synchron")
                        || value.contains("unsynchron")),
                );
            }
            _ => {}
        }
    }
    if result.synchronized.is_none() {
        result.synchronized = result.stratum.map(|s| s > 0 && s < 16);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Checking,
    Healthy,
    Degraded,
    Failed,
    Unknown,
    Stale,
    Unavailable,
    Stopped,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthDiagnostic {
    pub sled_agent: Option<ServiceState>,
    pub failed_services: Vec<String>,
    pub ntp: NtpDiagnostic,
    pub zones: ZoneDiagnostic,
    pub notes: Vec<String>,
}

impl HealthDiagnostic {
    pub fn state(&self) -> HealthState {
        if matches!(
            self.sled_agent,
            Some(ServiceState::Offline | ServiceState::Maintenance)
        ) {
            return HealthState::Failed;
        }
        if !self.failed_services.is_empty() {
            return HealthState::Degraded;
        }
        if self.sled_agent.is_none() && self.ntp.synchronized.is_none() {
            return HealthState::Unknown;
        }
        if self.sled_agent != Some(ServiceState::Online)
            || self.ntp.synchronized != Some(true)
        {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthContext {
    Checking,
    Active,
    Stopped,
}

pub fn derive_health_state(
    context: HealthContext,
    diagnostic: Option<&HealthDiagnostic>,
    freshness: Freshness,
) -> HealthState {
    match context {
        HealthContext::Checking => HealthState::Checking,
        HealthContext::Stopped => HealthState::Stopped,
        HealthContext::Active => match freshness {
            Freshness::Stale => HealthState::Stale,
            Freshness::Unavailable => HealthState::Unavailable,
            Freshness::Fresh => diagnostic
                .map(HealthDiagnostic::state)
                .unwrap_or(HealthState::Unknown),
        },
    }
}

#[derive(Debug, Clone)]
pub struct TimedValue<T> {
    pub captured_at: Instant,
    pub value: T,
}
#[derive(Debug, Clone)]
pub struct CollectionError {
    pub attempted_at: Instant,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LatestSample<T> {
    pub good: Option<TimedValue<T>>,
    pub last_attempt: Option<Instant>,
    pub latest_error: Option<CollectionError>,
}

impl<T> Default for LatestSample<T> {
    fn default() -> Self {
        Self { good: None, last_attempt: None, latest_error: None }
    }
}

impl<T> LatestSample<T> {
    pub fn record_success(&mut self, captured_at: Instant, value: T) {
        self.last_attempt = Some(captured_at);
        self.good = Some(TimedValue { captured_at, value });
        self.latest_error = None;
    }
    pub fn record_error(
        &mut self,
        attempted_at: Instant,
        message: impl Into<String>,
    ) {
        self.last_attempt = Some(attempted_at);
        self.latest_error =
            Some(CollectionError { attempted_at, message: message.into() });
    }
    /// Boundary semantics: age equal to `stale_after` remains fresh and age equal
    /// to `unavailable_after` remains stale. Threshold order errors degrade safely.
    pub fn freshness(
        &self,
        now: Instant,
        stale_after: Duration,
        unavailable_after: Duration,
    ) -> Freshness {
        let Some(good) = &self.good else {
            return Freshness::Unavailable;
        };
        let age =
            now.checked_duration_since(good.captured_at).unwrap_or_default();
        if age <= stale_after {
            Freshness::Fresh
        } else if age <= unavailable_after.max(stale_after) {
            Freshness::Stale
        } else {
            Freshness::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{config, router, sled, switch};
    use indoc::indoc;
    use std::time::{Duration, Instant};

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&s| s.to_owned()).collect()
    }

    fn addresses(ipv4: &[&str], ipv6: &[&str]) -> NodeAddresses {
        NodeAddresses { ipv4: strings(ipv4), ipv6: strings(ipv6) }
    }

    fn ntp(stratum: Option<u32>, synchronized: bool) -> NtpDiagnostic {
        NtpDiagnostic { stratum, synchronized: Some(synchronized) }
    }

    #[test]
    fn host_command_output_parses() {
        assert_eq!(
            parse_ipadm_addresses(
                "net0/v4:10.0.0.1/24\n\
                 vioif3/v4:192.168.68.122/22\n\
                 net0/v6:2001\\:db8\\:\\:2/64\n\
                 underlay0/sled6:fd00\\:1122\\:3344\\:101\\:\\:1/64\n\
                 vioif1/ll:fe80\\:\\:1/10\n\
                 malformed\n"
            ),
            addresses(
                &["192.168.68.122/22", "10.0.0.1/24"],
                &["fd00:1122:3344:101::1/64", "2001:db8::2/64"],
            ),
            "ipadm puts host LAN and underlay first and skips link-local"
        );
        assert_eq!(
            parse_linux_ip_addresses(
                "5: enp0s11    inet 192.168.68.108/22 scope global enp0s11\n\
                 5: enp0s11    inet6 2001:db8::1/64 scope global\n\
                 6: docker0    inet 172.17.0.1/16 scope global docker0\n\
                 7: enp0s12    inet not-an-address scope global\n"
            ),
            addresses(&["192.168.68.108/22"], &["2001:db8::1/64"]),
            "linux addresses skip bridges and invalid addresses"
        );
        assert_eq!(
            parse_zone_diagnostics(
                "global\noxz_switch\noxz_switch\noxz_ntp_deadbeef0\nbad zone\n"
            ),
            ZoneDiagnostic {
                zones: strings(&["global", "oxz_ntp_deadbeef0", "oxz_switch"]),
            },
            "zone list is sorted, deduplicated and skips malformed names"
        );
        assert_eq!(
            parse_service_state(" online\n"),
            Some(ServiceState::Online),
            "service state is trimmed"
        );
        assert_eq!(
            parse_service_state("garbage"),
            None,
            "service state rejects unknown states"
        );
        assert_eq!(
            parse_failed_services(
                "svc:/oxide/sled-agent:default\n\
                 noise\n\
                 svc:/network/dns/client:default\n"
            ),
            strings(&["network/dns/client", "oxide/sled-agent"]),
            "failed services are sorted FMRI names without noise"
        );
        assert_eq!(
            parse_chrony_tracking(
                "Reference ID : A29FC87B\nStratum : 4\nLeap status : Normal\n"
            ),
            ntp(Some(4), true),
            "chrony synchronised"
        );
        assert_eq!(
            parse_chrony_tracking(
                "Stratum : 0\nLeap status : Not synchronised"
            ),
            ntp(Some(0), false),
            "chrony not synchronised"
        );
        assert_eq!(
            parse_chrony_tracking(
                "Stratum : nope\nmalformed\nLeap status : Unsynchronised"
            ),
            ntp(None, false),
            "chrony malformed stratum"
        );
    }

    #[test]
    fn descriptors_cover_every_rack_and_router() {
        // Discovery lists every rack's sleds, numbered across the fleet, with
        // two switch zones on each rack's scrimlets, and fleet-wide routers.
        let cfg = config(indoc! {r#"
            [topology]
            racks = 2
            sleds = 3
            routers = ["ce", "cr1"]
        "#});
        assert_eq!(
            resource_descriptors(&cfg),
            vec![
                router("ce"),
                router("cr1"),
                sled(0, 0),
                sled(0, 1),
                sled(0, 2),
                switch(0, 0, "g0"),
                switch(0, 1, "g2"),
                sled(1, 3),
                sled(1, 4),
                sled(1, 5),
                switch(1, 0, "g3"),
                switch(1, 1, "g5"),
            ]
        );

        // Switch slots follow sled order, not the order scrimlets are listed
        // in.
        let reordered = config(indoc! {r#"
            [topology]
            racks = 1
            sleds = 3
            scrimlets = ["g2", "g0"]
            routers = []
        "#});
        assert_eq!(
            resource_descriptors(&reordered),
            vec![
                sled(0, 0),
                sled(0, 1),
                sled(0, 2),
                switch(0, 0, "g0"),
                switch(0, 1, "g2"),
            ]
        );
    }

    #[test]
    fn health_state_follows_freshness_and_diagnostics() {
        // The latest good sample survives errors, and freshness is measured
        // from it, with both boundaries inclusive.
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let fresh_for = Duration::from_secs(10);
        let available_for = Duration::from_secs(30);
        let mut sample = LatestSample::default();
        assert_eq!(
            sample.freshness(start, fresh_for, available_for),
            Freshness::Unavailable
        );
        sample.record_success(start, 7u32);
        sample.record_error(at(5), "timeout");
        assert_eq!(sample.latest_error.as_ref().unwrap().message, "timeout");
        for (age, freshness) in [
            (10, Freshness::Fresh),
            (11, Freshness::Stale),
            (30, Freshness::Stale),
            (31, Freshness::Unavailable),
        ] {
            assert_eq!(
                sample.freshness(at(age), fresh_for, available_for),
                freshness,
                "{age} s after the good sample"
            );
        }
        sample.record_success(at(32), 8);
        assert_eq!(sample.good.as_ref().map(|g| g.value), Some(8));
        assert!(sample.latest_error.is_none());

        // Health follows the context, then freshness, then the diagnostic.
        // A healthy diagnostic sees only the global zone: the zone count is
        // not evidence of health.
        let healthy = HealthDiagnostic {
            sled_agent: Some(ServiceState::Online),
            ntp: NtpDiagnostic { stratum: None, synchronized: Some(true) },
            zones: ZoneDiagnostic { zones: vec!["global".into()] },
            ..Default::default()
        };
        // A failed service degrades even when nothing else is known.
        let failed_service = HealthDiagnostic {
            failed_services: vec!["oxide/foo".into()],
            ..Default::default()
        };
        let sled_agent_maintenance = HealthDiagnostic {
            sled_agent: Some(ServiceState::Maintenance),
            ..healthy.clone()
        };

        use Freshness::{Fresh, Stale, Unavailable};
        use HealthContext::{Active, Checking, Stopped};
        // (context, diagnostic, freshness, state)
        let cases = [
            (Checking, Some(&failed_service), Stale, HealthState::Checking),
            (Active, Some(&healthy), Fresh, HealthState::Healthy),
            (Active, Some(&failed_service), Fresh, HealthState::Degraded),
            (Active, Some(&sled_agent_maintenance), Fresh, HealthState::Failed),
            (Active, Some(&healthy), Stale, HealthState::Stale),
            (Active, Some(&healthy), Unavailable, HealthState::Unavailable),
            (Active, None, Fresh, HealthState::Unknown),
            (Stopped, Some(&failed_service), Stale, HealthState::Stopped),
        ];
        for (context, diagnostic, freshness, state) in cases {
            assert_eq!(
                derive_health_state(context, diagnostic, freshness),
                state,
                "{context:?}, {:?}, {freshness:?}",
                diagnostic.map(HealthDiagnostic::state),
            );
        }
    }
}
