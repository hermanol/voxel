// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Periodic telemetry collection. Probes sleds and routers over Falcon
//! serial and SSH for addresses, service health and RSS progress, and
//! publishes the samples as [`AppEvent`]s on a bounded schedule.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use futures::future::BoxFuture;
use futures::{StreamExt, stream};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use voxel_config::VoxelConfig;

use crate::tui::event::AppEvent;
use crate::tui::reconcile::{parse_rss_observation, rss_probe_command};
use crate::tui::telemetry::{
    HealthDiagnostic, RackId, ResourceId, ResourceKind, parse_chrony_tracking,
    parse_failed_services, parse_ipadm_addresses, parse_linux_ip_addresses,
    parse_service_state, parse_zone_diagnostics, resource_descriptors,
};

const IPADM: &str = "ipadm show-addr -p -o addrobj,addr";
const LINUX_ADDRESSES: &str = "ip -o addr show";
const SLED_AGENT_STATE: &str = "svcs -H -o state svc:/oxide/sled-agent:default";
const MAINTENANCE_SERVICES: &str =
    "svcs -H -o state,fmri -a | awk '$1 == \"maintenance\" { print $2 }'";

async fn ssh_capture_bounded(
    ip: &str,
    remote: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    let askpass = crate::util::temp_dir().join("voxel-tui-empty-askpass.sh");
    if !askpass.exists() {
        std::fs::write(&askpass, "#!/bin/sh\necho\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &askpass,
                std::fs::Permissions::from_mode(0o700),
            )?;
        }
    }
    let mut command = tokio::process::Command::new("ssh");
    command
        .kill_on_drop(true)
        .env("SSH_ASKPASS", &askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .stdin(Stdio::null())
        .args(crate::net::EPHEMERAL_HOST_OPTS)
        .args([
            "-o",
            "PreferredAuthentications=password",
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "NumberOfPasswordPrompts=1",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
        ])
        .arg(format!("root@{ip}"))
        .arg(remote);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| anyhow!("SSH command on {ip} timed out"))??;
    if !output.status.success() {
        return Err(anyhow!(
            "SSH command on {ip} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(Into::into)
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum NodeTarget {
    Sled { name: String },
    SwitchZone { host: String, zone: String },
    Router { name: String },
}

pub trait NodeExecutor: Send + Sync + 'static {
    fn execute<'a>(
        &'a self,
        target: &'a NodeTarget,
        command: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<String>>;
}

/// Production execution boundary: Falcon is used only for identity and bounded,
/// one-time host-LAN discovery. Recurring illumos probes use bounded SSH.
#[derive(Default)]
struct IpCache {
    resolved: BTreeMap<String, String>,
    in_flight:
        BTreeMap<String, watch::Receiver<Option<Result<String, String>>>>,
}

impl IpCache {
    fn from_config_and_addresses(
        config: &VoxelConfig,
        mut resolved: BTreeMap<String, String>,
    ) -> Self {
        if config.external.isolated() {
            resolved.extend(config.static_external_ips());
        }
        Self { resolved, in_flight: BTreeMap::new() }
    }

    fn address_for_probe(
        &self,
        host: &str,
        resumed: bool,
    ) -> anyhow::Result<Option<String>> {
        match self.resolved.get(host) {
            Some(ip) => Ok(Some(ip.clone())),
            None if resumed => Err(anyhow!(
                "telemetry for {host} is unavailable after resuming without a saved address"
            )),
            None => Ok(None),
        }
    }
}

pub(crate) struct FalconExecutor {
    topo: Arc<crate::topo::Topo>,
    ips: Arc<Mutex<IpCache>>,
    router_commands: Arc<Mutex<BTreeSet<String>>>,
    serial_tasks: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    resumed: bool,
    timeout: Duration,
}

impl FalconExecutor {
    pub(crate) fn new(
        topo: crate::topo::Topo,
        config: &VoxelConfig,
        addresses: BTreeMap<String, String>,
        resumed: bool,
        timeout: Duration,
    ) -> Self {
        Self {
            topo: Arc::new(topo),
            ips: Arc::new(Mutex::new(IpCache::from_config_and_addresses(
                config, addresses,
            ))),
            router_commands: Default::default(),
            serial_tasks: Default::default(),
            resumed,
            timeout,
        }
    }

    fn track_serial_task(&self, task: tokio::task::JoinHandle<()>) {
        // Registration must not yield after spawning: otherwise cancellation
        // could drop the JoinHandle and leave untracked Falcon serial work.
        let mut tasks = self.serial_tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    /// Wait for Falcon serial workers that reached their caller's timeout.
    /// Service supervisors call this before admitting mutating work so a timed
    /// out read-only probe cannot outlive telemetry or reconciliation ownership.
    pub(crate) async fn drain_serial_tasks(&self) {
        loop {
            let tasks = {
                let mut tracked = self.serial_tasks.lock().unwrap();
                if tracked.is_empty() {
                    break;
                }
                std::mem::take(&mut *tracked)
            };
            for task in tasks {
                let _ = task.await;
            }
        }
    }

    pub(crate) async fn resolved_addresses(&self) -> BTreeMap<String, String> {
        self.ips.lock().await.resolved.clone()
    }

    async fn sled_ip(&self, host: &str) -> anyhow::Result<String> {
        let mut cache = self.ips.lock().await;
        if let Some(ip) = cache.address_for_probe(host, self.resumed)? {
            return Ok(ip);
        }
        let node = self
            .topo
            .node_ref(host)
            .ok_or_else(|| anyhow!("unknown Falcon sled {host}"))?;
        let mut result = if let Some(in_flight) = cache.in_flight.get(host) {
            in_flight.clone()
        } else {
            let (sender, receiver) = watch::channel(None);
            cache.in_flight.insert(host.into(), receiver.clone());
            let topo = self.topo.clone();
            let ips = self.ips.clone();
            let host = host.to_owned();
            let task = tokio::spawn(async move {
                let result =
                    crate::net::node_external_ip(&topo.runner, node, false)
                        .await
                        .map_err(|error| error.to_string());
                let mut cache = ips.lock().await;
                cache.in_flight.remove(&host);
                if let Ok(ip) = &result {
                    cache.resolved.insert(host, ip.clone());
                }
                let _ = sender.send(Some(result));
            });
            self.track_serial_task(task);
            receiver
        };
        drop(cache);

        let wait = async {
            loop {
                if let Some(result) = result.borrow().clone() {
                    return result.map_err(|message| anyhow!(message));
                }
                result
                    .changed()
                    .await
                    .map_err(|_| anyhow!("host-LAN discovery task stopped"))?;
            }
        };
        tokio::time::timeout(self.timeout, wait)
            .await
            .map_err(|_| anyhow!("host-LAN discovery for {host} timed out"))?
    }

    async fn router_exec(
        &self,
        name: &str,
        command: &str,
    ) -> anyhow::Result<String> {
        let node = self
            .topo
            .node_ref(name)
            .ok_or_else(|| anyhow!("unknown Falcon router {name}"))?;
        {
            let mut commands = self.router_commands.lock().await;
            if !commands.insert(name.into()) {
                return Err(anyhow!(
                    "a previous router command on {name} is still running"
                ));
            }
        }
        let (sender, receiver) = oneshot::channel();
        let topo = self.topo.clone();
        let commands = self.router_commands.clone();
        let name = name.to_owned();
        let task_name = name.clone();
        let command = command.to_owned();
        let task = tokio::spawn(async move {
            let result = topo
                .runner
                .exec(node, &command)
                .await
                .map_err(|error| error.to_string());
            commands.lock().await.remove(&task_name);
            let _ = sender.send(result);
        });
        self.track_serial_task(task);
        tokio::time::timeout(self.timeout, receiver)
            .await
            .map_err(|_| anyhow!("router command on {name} timed out"))?
            .map_err(|_| anyhow!("router command task on {name} stopped"))?
            .map_err(|error| anyhow!("router command on {name}: {error}"))
    }
}

impl NodeExecutor for FalconExecutor {
    fn execute<'a>(
        &'a self,
        target: &'a NodeTarget,
        command: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async move {
            match target {
                NodeTarget::Sled { name } => {
                    let ip = self.sled_ip(name).await?;
                    ssh_capture_bounded(&ip, command, self.timeout).await
                }
                NodeTarget::SwitchZone { host, zone: _ } => {
                    let ip = self.sled_ip(host).await?;
                    ssh_capture_bounded(
                        &ip,
                        &crate::net::zlogin(command),
                        self.timeout,
                    )
                    .await
                }
                NodeTarget::Router { name } => {
                    self.router_exec(name, command).await
                }
            }
        })
    }
}

#[derive(Clone, Debug)]
pub struct ResourceTarget {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub target: NodeTarget,
}

#[derive(Clone, Debug)]
pub struct RssTarget {
    pub rack: RackId,
    pub target: NodeTarget,
    pub bootstrap_addr: String,
}

#[derive(Clone, Debug)]
pub struct CollectorTargets {
    pub resources: Vec<ResourceTarget>,
    pub rss: Vec<RssTarget>,
}

impl CollectorTargets {
    pub fn from_config(config: &VoxelConfig) -> Self {
        let resources = resource_descriptors(config)
            .into_iter()
            .map(|descriptor| ResourceTarget {
                id: descriptor.id,
                kind: descriptor.kind,
                target: match descriptor.kind {
                    ResourceKind::Sled => {
                        NodeTarget::Sled { name: descriptor.name }
                    }
                    ResourceKind::SwitchZone => NodeTarget::SwitchZone {
                        host: descriptor
                            .host
                            .expect("switch descriptor has host"),
                        zone: "oxz_switch".into(),
                    },
                    ResourceKind::Router => {
                        NodeTarget::Router { name: descriptor.name }
                    }
                },
            })
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        let rss = config
            .sleds()
            .into_iter()
            .filter(|sled| sled.rss && seen.insert(sled.rack))
            .map(|sled| RssTarget {
                rack: RackId(sled.rack),
                target: NodeTarget::Sled { name: sled.name.clone() },
                bootstrap_addr: sled.bootstrap_addr(),
            })
            .collect();
        Self { resources, rss }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SchedulerConfig {
    pub health_interval: Duration,
    pub concurrency: usize,
}

impl SchedulerConfig {
    pub fn new(
        health_interval: Duration,
        concurrency: usize,
    ) -> anyhow::Result<Self> {
        if health_interval.is_zero() || concurrency == 0 {
            return Err(anyhow!(
                "collector intervals and concurrency must be nonzero"
            ));
        }
        Ok(Self { health_interval, concurrency })
    }
}

pub struct Collector<E> {
    executor: Arc<E>,
    targets: CollectorTargets,
}

impl<E: NodeExecutor> Collector<E> {
    pub fn new(executor: Arc<E>, config: &VoxelConfig) -> Self {
        Self { executor, targets: CollectorTargets::from_config(config) }
    }

    async fn call(
        &self,
        cancel: &CancellationToken,
        target: &NodeTarget,
        command: &str,
    ) -> anyhow::Result<String> {
        if cancel.is_cancelled() {
            return Err(anyhow!("cancelled"));
        }
        if matches!(target, NodeTarget::Router { .. }) {
            // Falcon serial exec is bounded but not cancellation-safe on drop.
            // Finish this safe-boundary call, then observe cancellation before
            // publishing or beginning another command.
            let result = self.executor.execute(target, command).await;
            if cancel.is_cancelled() {
                Err(anyhow!("cancelled"))
            } else {
                result
            }
        } else {
            tokio::select! { biased;
                _ = cancel.cancelled() => Err(anyhow!("cancelled")),
                result = self.executor.execute(target, command) => result,
            }
        }
    }

    async fn send(
        sender: &mpsc::Sender<AppEvent>,
        cancel: &CancellationToken,
        event: AppEvent,
    ) -> bool {
        tokio::select! { biased;
            _ = cancel.cancelled() => false,
            result = sender.send(event) => result.is_ok(),
        }
    }

    async fn collect_health_with_limit(
        &self,
        sender: &mpsc::Sender<AppEvent>,
        cancel: &CancellationToken,
        concurrency: usize,
    ) -> bool {
        let at = Instant::now();
        let resource_jobs = self.targets.resources.clone().into_iter().map(
            |resource| async move {
                self.health_events(&resource, at, cancel).await
            },
        );
        let mut events = stream::iter(resource_jobs)
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let rss_jobs =
            self.targets.rss.clone().into_iter().map(|rss| async move {
                self.rss_event(&rss, at, cancel).await
            });
        events.extend(
            stream::iter(rss_jobs)
                .buffer_unordered(concurrency)
                .collect::<Vec<_>>()
                .await,
        );
        for event in events {
            if !Self::send(sender, cancel, event).await {
                return false;
            }
        }
        true
    }

    async fn health_events(
        &self,
        resource: &ResourceTarget,
        at: Instant,
        cancel: &CancellationToken,
    ) -> Vec<AppEvent> {
        let addresses = match resource.kind {
            ResourceKind::Router => self
                .call(cancel, &resource.target, LINUX_ADDRESSES)
                .await
                .map(|s| parse_linux_ip_addresses(&s)),
            _ => self
                .call(cancel, &resource.target, IPADM)
                .await
                .map(|s| parse_ipadm_addresses(&s)),
        };
        let mut events = vec![match addresses {
            Ok(addresses) => {
                AppEvent::Addresses { id: resource.id.clone(), at, addresses }
            }
            Err(error) => AppEvent::AddressesFailed {
                id: resource.id.clone(),
                at,
                message: error.to_string(),
            },
        }];
        if resource.kind == ResourceKind::Sled {
            let diagnostic = self.sled_health(resource, cancel).await;
            events.push(match diagnostic {
                Ok(diagnostic) => {
                    AppEvent::Health { id: resource.id.clone(), at, diagnostic }
                }
                Err(error) => AppEvent::HealthFailed {
                    id: resource.id.clone(),
                    at,
                    message: error.to_string(),
                },
            });
        }
        events
    }

    async fn rss_event(
        &self,
        rss: &RssTarget,
        at: Instant,
        cancel: &CancellationToken,
    ) -> AppEvent {
        let command = rss_probe_command(&rss.bootstrap_addr);
        match self.call(cancel, &rss.target, &command).await {
            Ok(body) if !body.trim().is_empty() => AppEvent::Rss {
                rack: rss.rack,
                at,
                observation: parse_rss_observation(&body),
            },
            Ok(_) => AppEvent::RssFailed {
                rack: rss.rack,
                at,
                message: "empty RSS response".into(),
            },
            Err(error) => AppEvent::RssFailed {
                rack: rss.rack,
                at,
                message: error.to_string(),
            },
        }
    }

    async fn sled_health(
        &self,
        resource: &ResourceTarget,
        cancel: &CancellationToken,
    ) -> anyhow::Result<HealthDiagnostic> {
        let service =
            self.call(cancel, &resource.target, SLED_AGENT_STATE).await?;
        let sled_agent = parse_service_state(&service)
            .ok_or_else(|| anyhow!("invalid sled-agent service state"))?;
        let failed_services = parse_failed_services(
            &self.call(cancel, &resource.target, MAINTENANCE_SERVICES).await?,
        );
        let zones = parse_zone_diagnostics(
            &self
                .call(
                    cancel,
                    &resource.target,
                    "zoneadm list -cp | cut -d: -f2",
                )
                .await?,
        );
        let ntp_zone = zones
            .zones
            .iter()
            .find(|zone| zone.starts_with("oxz_ntp_"))
            .ok_or_else(|| anyhow!("NTP zone not found"))?;
        let ntp = parse_chrony_tracking(
            &self
                .call(
                    cancel,
                    &resource.target,
                    &format!(
                        "zlogin {ntp_zone} chronyc -n tracking 2>/dev/null"
                    ),
                )
                .await?,
        );
        if ntp.synchronized.is_none() {
            return Err(anyhow!("invalid NTP tracking state"));
        }
        Ok(HealthDiagnostic {
            sled_agent: Some(sled_agent),
            failed_services,
            ntp,
            zones,
            notes: vec![],
        })
    }

    pub(crate) async fn run_gated(
        self: Arc<Self>,
        config: SchedulerConfig,
        sender: mpsc::Sender<AppEvent>,
        cancel: CancellationToken,
        gate: Arc<tokio::sync::Semaphore>,
    ) {
        let mut health_due = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep_until(health_due) => {},
            }
            let permit = tokio::select! {
                _ = cancel.cancelled() => break,
                permit = gate.clone().acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => break,
                },
            };
            let keep_running = self
                .collect_health_with_limit(&sender, &cancel, config.concurrency)
                .await;
            health_due = tokio::time::Instant::now() + config.health_interval;
            drop(permit);
            if !keep_running {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use crate::tui::reconcile::ObservedDeploymentState;
    use crate::tui::telemetry::HealthState;
    use crate::tui::test_support::config;
    use crate::tui::ui::monitor::resource_health_state;
    use indoc::indoc;

    /// How long a step may take before it counts as hung. Only a failing
    /// test waits this long.
    const HANG: Duration = Duration::from_secs(5);

    const ZONEADM: &str = "zoneadm list -cp | cut -d: -f2";
    const NTP_TRACKING: &str =
        "zlogin oxz_ntp_deadbeef chronyc -n tracking 2>/dev/null";
    const RSS_PROBE: &str = "curl -s --max-time 5 ";

    /// What a healthy node answers.
    fn response(command: &str) -> String {
        match command {
            SLED_AGENT_STATE => "online\n".into(),
            ZONEADM => "global\noxz_ntp_deadbeef\n".into(),
            NTP_TRACKING => "Stratum : 3\nLeap status : Normal\n".into(),
            _ if command.starts_with(RSS_PROBE) => {
                r#"{"status":"initializing","step":"sled_init"}"#.into()
            }
            // No addresses, and no service in maintenance.
            _ => String::new(),
        }
    }

    /// What a scripted command does instead of answering with `response`.
    #[derive(Clone, Copy)]
    enum Reply {
        Output(&'static str),
        Fail(&'static str),
        /// Wait until `FakeExecutor::release` is cancelled.
        Held,
    }

    /// The node it applies to (any if `None`), a command prefix, and the reply.
    type Rule = (Option<&'static str>, &'static str, Reply);

    /// A node executor that answers from `response`, except where a rule
    /// scripts another reply, and records every command it is given.
    #[derive(Default)]
    struct FakeExecutor {
        rules: Vec<Rule>,
        commands: std::sync::Mutex<Vec<String>>,
        release: CancellationToken,
    }

    impl FakeExecutor {
        fn commands(&self) -> Vec<String> {
            self.commands.lock().unwrap().clone()
        }
    }

    fn fake(rules: Vec<Rule>) -> Arc<FakeExecutor> {
        Arc::new(FakeExecutor { rules, ..Default::default() })
    }

    impl NodeExecutor for FakeExecutor {
        fn execute<'a>(
            &'a self,
            target: &'a NodeTarget,
            command: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<String>> {
            self.commands.lock().unwrap().push(command.to_owned());
            let node = match target {
                NodeTarget::Sled { name } | NodeTarget::Router { name } => name,
                NodeTarget::SwitchZone { host, .. } => host,
            };
            let reply = self
                .rules
                .iter()
                .find(|(on, prefix, _)| {
                    on.is_none_or(|on| on == node)
                        && command.starts_with(prefix)
                })
                .map(|(_, _, reply)| *reply);
            Box::pin(async move {
                match reply {
                    Some(Reply::Output(output)) => return Ok(output.into()),
                    Some(Reply::Fail(message)) => return Err(anyhow!(message)),
                    Some(Reply::Held) => self.release.cancelled().await,
                    None => {}
                }
                Ok(response(command))
            })
        }
    }

    async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(HANG, async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}: not reached within {HANG:?}"));
    }

    /// A collector over explicit targets.
    fn collector(
        executor: Arc<FakeExecutor>,
        resources: Vec<ResourceTarget>,
    ) -> Collector<FakeExecutor> {
        Collector {
            executor,
            targets: CollectorTargets { resources, rss: vec![] },
        }
    }

    /// The fleet's resources become typed probe targets with the right
    /// address source.
    #[test]
    fn every_node_is_probed_at_its_address() {
        let fleet = config(indoc! {r#"
            [topology]
            racks = 2
            sleds = 3
            routers = ["ce", "cr1"]
        "#});

        let targets = CollectorTargets::from_config(&fleet);
        let sled = |name: &str| NodeTarget::Sled { name: name.into() };
        let switch = |host: &str| NodeTarget::SwitchZone {
            host: host.into(),
            zone: "oxz_switch".into(),
        };
        let router = |name: &str| NodeTarget::Router { name: name.into() };
        assert_eq!(
            targets
                .rss
                .iter()
                .map(|rss| (rss.rack, rss.target.clone()))
                .collect::<Vec<_>>(),
            [(RackId(0), sled("g0")), (RackId(1), sled("g3"))]
        );
        // Order is not part of the contract, so compare sorted by name.
        let mut probed: Vec<_> = targets
            .resources
            .iter()
            .map(|resource| resource.target.clone())
            .collect();
        probed.sort_by_key(|target| format!("{target:?}"));
        let mut expected_targets = vec![
            sled("g0"),
            switch("g0"),
            sled("g1"),
            sled("g2"),
            switch("g2"),
            sled("g3"),
            switch("g3"),
            sled("g4"),
            sled("g5"),
            switch("g5"),
            router("ce"),
            router("cr1"),
        ];
        expected_targets.sort_by_key(|target| format!("{target:?}"));
        assert_eq!(probed, expected_targets);

        let isolated = config(indoc! {r#"
            [topology]
            racks = 1
            sleds = 3
            routers = ["ce"]
            [external]
            mode = "isolated"
        "#});
        let default = VoxelConfig::default();
        let addresses = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(host, ip)| ((*host).to_owned(), (*ip).to_owned()))
                .collect::<BTreeMap<_, _>>()
        };
        let none = BTreeMap::new;
        let saved = addresses(&[("g0", "192.168.1.184")]);
        // (case, config, saved addresses, resumed, addresses the cache
        // holds, and g0's probe address or the error it gives)
        let cases = [
            (
                "isolated",
                &isolated,
                none(),
                false,
                addresses(&[
                    ("g0", "172.30.199.10"),
                    ("g1", "172.30.199.11"),
                    ("g2", "172.30.199.12"),
                    ("ce", "172.30.199.13"),
                ]),
                Ok(Some("172.30.199.10".to_owned())),
            ),
            ("default", &default, none(), false, none(), Ok(None)),
            (
                "resumed",
                &default,
                saved.clone(),
                true,
                saved,
                Ok(Some("192.168.1.184".to_owned())),
            ),
            (
                "resumed without a saved address",
                &default,
                none(),
                true,
                none(),
                Err("telemetry for g0 is unavailable after resuming without a saved address".to_owned()),
            ),
        ];
        for (case, config, saved, resumed, holds, g0) in cases {
            let cache = IpCache::from_config_and_addresses(config, saved);
            let probe = cache
                .address_for_probe("g0", resumed)
                .map_err(|error| error.to_string());
            assert_eq!((cache.resolved, probe), (holds, g0), "{case}");
        }
    }

    /// After a health round, every resource and rack has been observed and
    /// the monitor shows it: rack 0 is healthy, while rack 1 has a service in
    /// maintenance and an unreachable RSS sled, neither of which hides rack
    /// 0's observations.
    #[tokio::test]
    async fn health_round_reaches_the_operator_for_every_resource_and_rack() {
        let config = config(indoc! {r#"
            [topology]
            racks = 2
            sleds = 2
            routers = ["ce"]
        "#});
        let broken = Reply::Output("svc:/oxide/broken:default\n");
        let fake = fake(vec![
            (Some("g2"), MAINTENANCE_SERVICES, broken),
            (Some("g3"), MAINTENANCE_SERVICES, broken),
            (Some("g2"), RSS_PROBE, Reply::Fail("unreachable")),
        ]);
        let collector = Collector::new(fake, &config);
        let (sender, mut receiver) = mpsc::channel(64);
        let cancel = CancellationToken::new();

        assert!(collector.collect_health_with_limit(&sender, &cancel, 2).await);
        let events: Vec<_> =
            std::iter::from_fn(|| receiver.try_recv().ok()).collect();

        let resources = resource_descriptors(&config);
        let addressed: BTreeSet<_> = events
            .iter()
            .filter_map(|event| match event {
                AppEvent::Addresses { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            addressed,
            resources.iter().map(|resource| resource.id.clone()).collect(),
            "resources whose addresses are published"
        );
        let rss: BTreeMap<_, _> = events
            .iter()
            .filter_map(|event| match event {
                AppEvent::Rss { rack, .. } => Some((*rack, None)),
                AppEvent::RssFailed { rack, message, .. } => {
                    Some((*rack, Some(message.as_str())))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            rss,
            BTreeMap::from([
                (RackId(0), None),
                (RackId(1), Some("unreachable"))
            ]),
            "RSS failures by rack"
        );

        let mut app = App::new(resources.clone(), 8);
        app.deployment.observed = ObservedDeploymentState::Running;
        for event in events {
            app.update(event);
        }
        app.update(AppEvent::Tick { now: Instant::now() });
        let rack1_sleds = [
            ResourceId::rack(RackId(1), ResourceKind::Sled, "g2"),
            ResourceId::rack(RackId(1), ResourceKind::Sled, "g3"),
        ];
        let shown: Vec<_> = resources
            .iter()
            .map(|resource| {
                (resource.id.clone(), resource_health_state(&app, &resource.id))
            })
            .collect();
        let expected: Vec<_> = resources
            .iter()
            .map(|resource| {
                let state = if rack1_sleds.contains(&resource.id) {
                    HealthState::Degraded
                } else {
                    HealthState::Healthy
                };
                (resource.id.clone(), state)
            })
            .collect();
        assert_eq!(shown, expected, "operator view");
    }

    /// Wherever a round is blocked, cancelling it ends the round promptly
    /// without publishing anything further, except that a Falcon serial call
    /// is not safe to drop, so the round waits for it first. A round also
    /// stops once the UI stops receiving.
    #[tokio::test]
    async fn collection_stops_cleanly_when_cancelled_or_the_ui_closes() {
        let sled = || ResourceTarget {
            id: ResourceId::rack(RackId(0), ResourceKind::Sled, "g0"),
            kind: ResourceKind::Sled,
            target: NodeTarget::Sled { name: "g0".into() },
        };
        let router = || ResourceTarget {
            id: ResourceId::fleet(ResourceKind::Router, "ce"),
            kind: ResourceKind::Router,
            target: NodeTarget::Router { name: "ce".into() },
        };
        // (case, resources, the command held until release, and whether the
        // event channel starts full)
        let rows = [
            ("sending into a full event channel", vec![sled()], None, true),
            ("in-flight SSH probe", vec![sled()], Some(IPADM), false),
            (
                "in-flight router call",
                vec![router()],
                Some(LINUX_ADDRESSES),
                false,
            ),
            (
                "in-flight router call with a full event channel",
                vec![sled(), router()],
                Some(LINUX_ADDRESSES),
                true,
            ),
        ];
        for (case, resources, held, queued_event) in rows {
            let fake = fake(
                held.map(|command| (None, command, Reply::Held))
                    .into_iter()
                    .collect(),
            );
            let limit = resources.len();
            let collector = collector(fake.clone(), resources);
            let (sender, mut receiver) = mpsc::channel(1);
            if queued_event {
                sender
                    .send(AppEvent::Tick { now: Instant::now() })
                    .await
                    .unwrap();
            }
            let cancel = CancellationToken::new();
            let task = tokio::spawn({
                let cancel = cancel.clone();
                async move {
                    collector
                        .collect_health_with_limit(&sender, &cancel, limit)
                        .await
                }
            });
            // Unheld, the round blocks sending the sample its last command
            // produced.
            let blocked_on = held.unwrap_or(NTP_TRACKING);
            wait_until(case, || {
                fake.commands().iter().any(|c| c == blocked_on)
            })
            .await;

            cancel.cancel();
            if held == Some(LINUX_ADDRESSES) {
                tokio::task::yield_now().await;
                assert!(
                    !task.is_finished(),
                    "{case}: returned before the router call finished"
                );
                fake.release.cancel();
            }

            assert!(
                matches!(tokio::time::timeout(HANG, task).await, Ok(Ok(false))),
                "{case}: the round did not stop and report cancellation"
            );
            assert_eq!(
                std::iter::from_fn(|| receiver.try_recv().ok()).count(),
                usize::from(queued_event),
                "{case}: events left in the channel"
            );
        }

        let collector = collector(fake(vec![]), vec![sled()]);
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        let cancel = CancellationToken::new();
        let round = collector.collect_health_with_limit(&sender, &cancel, 1);
        assert!(
            matches!(tokio::time::timeout(HANG, round).await, Ok(false)),
            "UI closed: the round did not stop"
        );
    }

    /// The scheduler keeps its concurrency limit and stops when cancelled.
    #[tokio::test]
    async fn scheduler_keeps_its_limit_and_stops_when_cancelled() {
        let config = config(indoc! {r#"
            [topology]
            racks = 2
            sleds = 2
            routers = ["ce"]
        "#});
        let held = fake(vec![
            (None, IPADM, Reply::Held),
            (None, LINUX_ADDRESSES, Reply::Held),
        ]);
        let collector = Arc::new(Collector::new(held.clone(), &config));
        let hour = Duration::from_secs(3_600);
        let (sender, _receiver) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(collector.run_gated(
            SchedulerConfig::new(hour, 2).unwrap(),
            sender,
            cancel.clone(),
            Arc::new(tokio::sync::Semaphore::new(1)),
        ));
        wait_until("two health probes running", || held.commands().len() >= 2)
            .await;
        let commands = held.commands();
        assert!(
            commands.len() == 2
                && commands
                    .iter()
                    .all(|c| *c == IPADM || *c == LINUX_ADDRESSES),
            "probes beyond the limit: {commands:?}"
        );
        // A held router call finishes before the round observes cancellation.
        cancel.cancel();
        held.release.cancel();
        assert!(
            tokio::time::timeout(HANG, task).await.is_ok_and(|r| r.is_ok()),
            "the scheduler did not stop when cancelled"
        );
    }
}
