//! Command execution.

use crate::Commands;
use colored::Colorize;
use rstmdb_client::Client;
use serde_json::Value;

/// Executes a command and returns the formatted output.
pub async fn execute(client: &Client, cmd: Commands) -> Result<String, Box<dyn std::error::Error>> {
    match cmd {
        Commands::Repl => unreachable!(),

        Commands::Ping => {
            client.ping().await?;
            Ok("PONG".green().to_string())
        }

        Commands::Info => {
            let info = client.info().await?;
            Ok(format_json(&info))
        }

        Commands::PutMachine {
            name,
            version,
            definition,
        } => {
            let def_json = parse_json_arg(&definition)?;
            let result = client.put_machine(&name, version, def_json).await?;

            if result.created {
                Ok(format!(
                    "{} machine {} v{} (checksum: {})",
                    "Created".green(),
                    name.cyan(),
                    version,
                    result.stored_checksum
                ))
            } else {
                Ok(format!(
                    "{} machine {} v{} (checksum: {})",
                    "Already exists".yellow(),
                    name.cyan(),
                    version,
                    result.stored_checksum
                ))
            }
        }

        Commands::GetMachine { name, version } => {
            let result = client.get_machine(&name, version).await?;
            Ok(format!(
                "{}\n{}",
                format!("Machine {} v{}", name.cyan(), version).bold(),
                format_json(&result.definition)
            ))
        }

        Commands::ListMachines => {
            let result = client.list_machines().await?;
            let items = result["items"].as_array();

            if let Some(items) = items {
                if items.is_empty() {
                    return Ok("No machines registered".yellow().to_string());
                }

                let mut output = String::new();
                for item in items {
                    let name = item["machine"].as_str().unwrap_or("?");
                    let versions = item["versions"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(|x| x.as_u64())
                                .map(|x| x.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    output.push_str(&format!("  {} [versions: {}]\n", name.cyan(), versions));
                }
                Ok(output)
            } else {
                Ok(format_json(&result))
            }
        }

        Commands::CreateInstance {
            machine,
            version,
            id,
            ctx,
        } => {
            let initial_ctx = ctx.map(|c| parse_json_arg(&c)).transpose()?;
            let result = client
                .create_instance(&machine, version, id.as_deref(), initial_ctx, None)
                .await?;

            Ok(format!(
                "{} instance {}\n  Machine: {} v{}\n  State: {}\n  WAL offset: {}",
                "Created".green(),
                result.instance_id.cyan(),
                machine,
                version,
                result.state.yellow(),
                result.wal_offset
            ))
        }

        Commands::GetInstance { id } => {
            let result = client.get_instance(&id).await?;
            Ok(format!(
                "{}\n  Machine: {} v{}\n  State: {}\n  Context: {}\n  Last WAL offset: {}",
                format!("Instance {}", id.cyan()).bold(),
                result.machine,
                result.version,
                result.state.yellow(),
                serde_json::to_string_pretty(&result.ctx)?,
                result.last_wal_offset
            ))
        }

        Commands::ApplyEvent {
            instance,
            event,
            payload,
            expected_state,
        } => {
            let payload_json = payload.map(|p| parse_json_arg(&p)).transpose()?;
            let result = client
                .apply_event(
                    &instance,
                    &event,
                    payload_json,
                    expected_state.as_deref(),
                    None,
                )
                .await?;

            Ok(format!(
                "{} event {} on {}\n  {} → {}\n  WAL offset: {}",
                "Applied".green(),
                event.cyan(),
                instance,
                result.from_state,
                result.to_state.yellow(),
                result.wal_offset
            ))
        }

        Commands::DeleteInstance { id } => {
            client.delete_instance(&id, None).await?;
            Ok(format!("{} instance {}", "Deleted".green(), id.cyan()))
        }

        Commands::WalRead { from, limit } => {
            let result = client.wal_read(from, limit).await?;
            let records = result["records"].as_array();

            if let Some(records) = records {
                if records.is_empty() {
                    return Ok("No WAL entries".yellow().to_string());
                }

                let mut output = String::new();
                for record in records {
                    let seq = record["sequence"].as_u64().unwrap_or(0);
                    let offset = record["offset"].as_u64().unwrap_or(0);
                    let entry = &record["entry"];
                    let entry_type = entry["type"].as_str().unwrap_or("unknown");

                    output.push_str(&format!(
                        "[{:>6}] {:>12} | {}\n",
                        seq.to_string().cyan(),
                        offset,
                        entry_type.yellow()
                    ));

                    // Show relevant details based on type
                    match entry_type {
                        "create_instance" => {
                            output.push_str(&format!(
                                "         Instance: {}, Machine: {} v{}\n",
                                entry["instance_id"].as_str().unwrap_or("?"),
                                entry["machine"].as_str().unwrap_or("?"),
                                entry["version"].as_u64().unwrap_or(0)
                            ));
                        }
                        "apply_event" => {
                            output.push_str(&format!(
                                "         Instance: {}, Event: {}, {} → {}\n",
                                entry["instance_id"].as_str().unwrap_or("?"),
                                entry["event"].as_str().unwrap_or("?"),
                                entry["from_state"].as_str().unwrap_or("?"),
                                entry["to_state"].as_str().unwrap_or("?")
                            ));
                        }
                        _ => {}
                    }
                }

                if let Some(next) = result["next_offset"].as_u64() {
                    output.push_str(&format!("\n{}: {}", "Next offset".dimmed(), next));
                }

                Ok(output)
            } else {
                Ok(format_json(&result))
            }
        }

        Commands::Compact { force } => {
            let result = client.compact(force).await?;

            let snapshots_created = result["snapshots_created"].as_u64().unwrap_or(0);
            let segments_deleted = result["segments_deleted"].as_u64().unwrap_or(0);
            let bytes = result["bytes_reclaimed"].as_u64().unwrap_or(0);
            let total_snapshots = result["total_snapshots"].as_u64().unwrap_or(0);
            let wal_segments = result["wal_segments"].as_u64().unwrap_or(0);

            Ok(format!(
                "{}\n  Snapshots created: {}\n  WAL segments deleted: {}\n  Bytes reclaimed: {}\n  ---\n  Total snapshots: {}\n  WAL segments remaining: {}",
                "Compaction complete".green(),
                snapshots_created,
                segments_deleted,
                format_bytes(bytes),
                total_snapshots,
                wal_segments
            ))
        }

        // HashToken is handled directly in main.rs (no server connection needed)
        Commands::HashToken { .. } => unreachable!(),

        // Watch commands are handled directly in main.rs (they stream events)
        Commands::WatchInstance { .. } => unreachable!(),
        Commands::WatchAll { .. } => unreachable!(),

        Commands::Unwatch { subscription_id } => {
            let result = client.unwatch(&subscription_id).await?;
            if result.removed {
                Ok(format!(
                    "{} subscription {}",
                    "Removed".green(),
                    subscription_id.cyan()
                ))
            } else {
                Ok(format!(
                    "{}: subscription {} not found",
                    "Warning".yellow(),
                    subscription_id
                ))
            }
        }
    }
}

/// Formats bytes as human-readable string.
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Parses a JSON argument (either inline JSON or @file.json).
fn parse_json_arg(arg: &str) -> Result<Value, Box<dyn std::error::Error>> {
    if let Some(path) = arg.strip_prefix('@') {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    } else {
        Ok(serde_json::from_str(arg)?)
    }
}

/// Formats JSON for display.
fn format_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
