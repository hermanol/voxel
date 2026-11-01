// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Runs commands on deployment nodes: bounded SSH to sleds, whose host-LAN
//! addresses Falcon discovers once, and Falcon serial execution on routers.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use futures::future::BoxFuture;
use tokio::sync::{Mutex, oneshot, watch};
use voxel_config::VoxelConfig;

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
    fn from_config(config: &VoxelConfig) -> Self {
        let mut resolved = BTreeMap::new();
        if config.external.isolated() {
            resolved.extend(config.static_external_ips());
        }
        Self { resolved, in_flight: BTreeMap::new() }
    }

    fn address_for_probe(&self, host: &str) -> Option<String> {
        self.resolved.get(host).cloned()
    }
}

pub(crate) struct FalconExecutor {
    topo: Arc<crate::topo::Topo>,
    ips: Arc<Mutex<IpCache>>,
    router_commands: Arc<Mutex<BTreeSet<String>>>,
    serial_tasks: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    timeout: Duration,
}

impl FalconExecutor {
    pub(crate) fn new(
        topo: crate::topo::Topo,
        config: &VoxelConfig,
        timeout: Duration,
    ) -> Self {
        Self {
            topo: Arc::new(topo),
            ips: Arc::new(Mutex::new(IpCache::from_config(config))),
            router_commands: Default::default(),
            serial_tasks: Default::default(),
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

    async fn sled_ip(&self, host: &str) -> anyhow::Result<String> {
        let mut cache = self.ips.lock().await;
        if let Some(ip) = cache.address_for_probe(host) {
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
                NodeTarget::Router { name } => {
                    self.router_exec(name, command).await
                }
            }
        })
    }
}
