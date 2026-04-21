//! High-throughput load generator for rstmdb.
//!
//! Uses `rstmdb-client` with persistent TCP connections + tokio async
//! concurrency to pump thousands of ops per second.
//!
//! Scenarios:
//! - 100k state machine definitions
//! - 1M+ total WAL entries (machines + instances + events)
//! - Replication stress (converge across replicas)
//! - Auto-compaction trigger (10k entries default threshold)
//!
//! Example:
//!   load-gen \
//!       --primary 127.0.0.1:7401 \
//!       --machines 100000 \
//!       --instances-per-machine 1 \
//!       --events-per-instance 9 \
//!       --workers 64
//!   (→ 100k machines + 100k instances + 900k events = 1.1M WAL entries)

use clap::Parser;
use rstmdb_client::{Client, ConnectionConfig};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "load-gen")]
#[command(about = "High-throughput load generator for rstmdb (replication & compaction stress)")]
struct Args {
    /// Primary server address.
    #[arg(long, default_value = "127.0.0.1:7401")]
    primary: SocketAddr,

    /// Bearer token (if server auth is enabled).
    #[arg(long)]
    token: Option<String>,

    /// Number of distinct state machine definitions to create.
    #[arg(long, default_value_t = 100)]
    machines: u64,

    /// Number of instances per machine.
    #[arg(long, default_value_t = 10)]
    instances_per_machine: u64,

    /// Number of events per instance.
    #[arg(long, default_value_t = 5)]
    events_per_instance: u64,

    /// Number of concurrent worker connections.
    #[arg(long, default_value_t = 32)]
    workers: u64,

    /// Skip the machines phase (assume machines already exist).
    #[arg(long, default_value_t = false)]
    skip_machines: bool,

    /// Skip the instances phase.
    #[arg(long, default_value_t = false)]
    skip_instances: bool,

    /// Skip the events phase.
    #[arg(long, default_value_t = false)]
    skip_events: bool,

    /// Reporting interval in seconds.
    #[arg(long, default_value_t = 5)]
    report_every: u64,

    /// Unique prefix for this run's IDs (keeps multiple runs isolated).
    #[arg(long, default_value = "")]
    prefix: String,

    /// Request timeout (seconds).
    #[arg(long, default_value_t = 30)]
    request_timeout_secs: u64,
}

struct Counters {
    ops_done: AtomicU64,
    ops_failed: AtomicU64,
}

fn machine_def() -> serde_json::Value {
    json!({
        "states": ["created", "active", "done"],
        "initial": "created",
        "transitions": [
            {"from": "created", "event": "START", "to": "active"},
            {"from": "active",  "event": "STEP",  "to": "active"},
            {"from": "active",  "event": "FINISH","to": "done"}
        ]
    })
}

/// Builds a client and runs `handler` for each item from `rx`.
async fn worker<T, F, Fut>(
    worker_id: u64,
    config: ConnectionConfig,
    counters: Arc<Counters>,
    mut rx: tokio::sync::mpsc::Receiver<T>,
    handler: F,
) where
    T: Send + 'static,
    F: Fn(Arc<Client>, T) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    // One persistent connection per worker.
    let client = Arc::new(Client::new(config));
    if let Err(e) = client.connect().await {
        eprintln!("[worker {}] connect failed: {}", worker_id, e);
        return;
    }

    // Spawn the response read loop — without this, request futures never
    // resolve because the client waits forever for a reply.
    let conn = client.connection();
    let read_handle = tokio::spawn(async move {
        let _ = conn.read_loop().await;
    });
    tokio::task::yield_now().await;

    while let Some(item) = rx.recv().await {
        match handler(client.clone(), item).await {
            Ok(()) => {
                counters.ops_done.fetch_add(1, Ordering::Relaxed);
            }
            Err(_e) => {
                counters.ops_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let _ = client.close().await;
    read_handle.abort();
}

fn build_client_config(args: &Args) -> ConnectionConfig {
    let mut cfg = ConnectionConfig::new(args.primary);
    cfg.request_timeout = Duration::from_secs(args.request_timeout_secs);
    cfg.client_name = Some("load-gen".to_string());
    if let Some(ref t) = args.token {
        cfg.auth_token = Some(t.clone());
    }
    cfg
}

/// Fans out `items` across `workers` via bounded channels, then drains.
/// `keyer` returns a stable key per item; items with the same key are pinned
/// to the same worker so ordered operations (e.g. START → STEP for the same
/// instance) aren't interleaved across workers.
async fn run_phase<T, F, Fut, K>(
    phase_name: &str,
    items: Vec<T>,
    args: &Args,
    handler: F,
    keyer: K,
) -> (u64, u64, f64)
where
    T: Send + 'static,
    F: Fn(Arc<Client>, T) -> Fut + Send + Sync + 'static + Clone,
    Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    K: Fn(&T) -> u64 + Send + Sync + 'static,
{
    let total = items.len() as u64;
    let counters = Arc::new(Counters {
        ops_done: AtomicU64::new(0),
        ops_failed: AtomicU64::new(0),
    });

    println!("\n[{}] starting — {} items, {} workers", phase_name, total, args.workers);
    let started = Instant::now();

    // Per-worker channels so workers can pull independently
    let mut txs = Vec::with_capacity(args.workers as usize);
    let mut worker_handles = Vec::with_capacity(args.workers as usize);
    let config = build_client_config(args);

    for worker_id in 0..args.workers {
        let (tx, rx) = tokio::sync::mpsc::channel::<T>(256);
        txs.push(tx);
        let counters_c = counters.clone();
        let config_c = config.clone();
        let handler_c = handler.clone();
        worker_handles.push(tokio::spawn(async move {
            worker(worker_id, config_c, counters_c, rx, handler_c).await;
        }));
    }

    // Dispatcher: round-robin items across workers
    let counters_r = counters.clone();
    let report_every = args.report_every;
    let reporter = tokio::spawn(async move {
        let start = Instant::now();
        let mut last_report = Instant::now();
        let mut last_done = 0u64;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let done = counters_r.ops_done.load(Ordering::Relaxed);
            let failed = counters_r.ops_failed.load(Ordering::Relaxed);
            if done + failed >= total {
                break;
            }
            if last_report.elapsed() >= Duration::from_secs(report_every) {
                let delta = done - last_done;
                let elapsed = start.elapsed().as_secs_f64();
                let inst_rate = delta as f64 / last_report.elapsed().as_secs_f64();
                let avg_rate = done as f64 / elapsed;
                let pct = (done + failed) as f64 * 100.0 / total as f64;
                println!(
                    "  [{:>5.1}s] {:>6.1}% | done={} failed={} | inst={:.0}/s avg={:.0}/s",
                    elapsed, pct, done, failed, inst_rate, avg_rate,
                );
                last_report = Instant::now();
                last_done = done;
            }
        }
    });

    let num_workers = args.workers;
    let dispatch = tokio::spawn(async move {
        for item in items {
            // Route by key so all items with the same key land on the same
            // worker (preserves per-key ordering while parallelising across
            // distinct keys).
            let key = keyer(&item);
            let worker_idx = (key % num_workers) as usize;
            if txs[worker_idx].send(item).await.is_err() {
                break;
            }
        }
        drop(txs);
    });

    dispatch.await.unwrap();
    for h in worker_handles {
        let _ = h.await;
    }
    let _ = reporter.await;

    let elapsed = started.elapsed().as_secs_f64();
    let done = counters.ops_done.load(Ordering::Relaxed);
    let failed = counters.ops_failed.load(Ordering::Relaxed);
    let rate = done as f64 / elapsed.max(0.001);

    println!(
        "[{}] complete: done={} failed={} in {:.2}s (~{:.0}/s)",
        phase_name, done, failed, elapsed, rate,
    );
    (done, failed, rate)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let prefix = if args.prefix.is_empty() {
        format!(
            "lg-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        )
    } else {
        args.prefix.clone()
    };

    println!("rstmdb load-gen");
    println!("  primary:             {}", args.primary);
    println!("  machines:            {}", args.machines);
    println!("  instances/machine:   {}", args.instances_per_machine);
    println!("  events/instance:     {}", args.events_per_instance);
    println!("  workers:             {}", args.workers);
    println!("  id prefix:           {}", prefix);

    let total_machines = args.machines;
    let total_instances = args.machines * args.instances_per_machine;
    let total_events = total_instances * args.events_per_instance;
    let total_ops = total_machines + total_instances + total_events;
    println!(
        "  total WAL entries:   {} (machines:{} + instances:{} + events:{})",
        total_ops, total_machines, total_instances, total_events
    );

    // Verify the primary is reachable before starting big workload
    let probe_cfg = build_client_config(&args);
    let probe = Client::new(probe_cfg);
    probe.connect().await?;
    let probe_conn = probe.connection();
    let probe_read_handle = tokio::spawn(async move {
        let _ = probe_conn.read_loop().await;
    });
    tokio::task::yield_now().await;
    probe.ping().await?;
    println!("\n✓ primary reachable");
    probe.close().await?;
    probe_read_handle.abort();

    let wall_start = Instant::now();
    let mut grand_done = 0u64;
    let mut grand_failed = 0u64;

    // -------- Phase 1: machines --------
    if !args.skip_machines && args.machines > 0 {
        let items: Vec<(u64, String)> = (0..args.machines)
            .map(|i| (i, format!("{}-m-{}", prefix, i)))
            .collect();
        let (done, failed, _) = run_phase(
            "phase 1: put_machine",
            items,
            &args,
            |client, (_i, name)| async move {
                let def = machine_def();
                client.put_machine(&name, 1, def).await.map_err(|e| e.to_string())?;
                Ok(())
            },
            |(i, _)| *i, // machine index as key; distinct keys → full parallelism
        )
        .await;
        grand_done += done;
        grand_failed += failed;
    }

    // -------- Phase 2: instances --------
    if !args.skip_instances && args.instances_per_machine > 0 {
        let mut items: Vec<(u64, u64, String, String)> = Vec::with_capacity(
            (args.machines * args.instances_per_machine) as usize,
        );
        for m in 0..args.machines {
            for i in 0..args.instances_per_machine {
                items.push((
                    m,
                    i,
                    format!("{}-m-{}", prefix, m),
                    format!("{}-m{}-i{}", prefix, m, i),
                ));
            }
        }
        let ipm = args.instances_per_machine;
        let (done, failed, _) = run_phase(
            "phase 2: create_instance",
            items,
            &args,
            |client, (_m, _i, machine, instance_id)| async move {
                client
                    .create_instance(&machine, 1, Some(&instance_id), None, None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            },
            move |(m, i, _, _)| m * ipm + *i, // each instance has a unique global index
        )
        .await;
        grand_done += done;
        grand_failed += failed;
    }

    // -------- Phase 3: events --------
    if !args.skip_events && args.events_per_instance > 0 {
        let mut items: Vec<(u64, u64, String, &'static str)> =
            Vec::with_capacity(total_events as usize);
        for m in 0..args.machines {
            for i in 0..args.instances_per_machine {
                let id = format!("{}-m{}-i{}", prefix, m, i);
                // First event: START (created → active), then N-1 × STEP.
                items.push((m, i, id.clone(), "START"));
                for _ in 1..args.events_per_instance {
                    items.push((m, i, id.clone(), "STEP"));
                }
            }
        }
        let ipm = args.instances_per_machine;
        let (done, failed, _) = run_phase(
            "phase 3: apply_event",
            items,
            &args,
            |client, (_m, _i, instance_id, event)| async move {
                client
                    .apply_event(&instance_id, event, None, None, None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            },
            // Route all events for the same instance to the same worker so
            // START → STEP → STEP stays in order per instance.
            move |(m, i, _, _)| m * ipm + *i,
        )
        .await;
        grand_done += done;
        grand_failed += failed;
    }

    // -------- Summary --------
    let elapsed = wall_start.elapsed().as_secs_f64();
    let total = grand_done + grand_failed;
    let rate = grand_done as f64 / elapsed.max(0.001);

    println!("\n===== Summary =====");
    println!("  total submitted:  {}", total);
    println!("  succeeded:        {}", grand_done);
    println!("  failed:           {}", grand_failed);
    println!("  wall time:        {:.2}s", elapsed);
    println!("  avg throughput:   {:.0} ops/s", rate);

    if grand_failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}
