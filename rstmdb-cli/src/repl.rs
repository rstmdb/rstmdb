//! Interactive REPL.

use colored::Colorize;
use rstmdb_client::Client;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{Config, Editor};
use serde_json::Value;
use std::net::SocketAddr;

const HELP_TEXT: &str = r#"
Available commands:
  help                          Show this help
  reconnect                     Reconnect to the server
  ping                          Ping the server
  info                          Get server info

  put-machine <name> <ver> <def>  Register a machine definition
  get-machine <name> <ver>        Get a machine definition
  list-machines                   List all machines

  create <machine> <ver> [id]     Create an instance
  get <instance_id>               Get instance state
  list-instances [machine] [state] List instances
  delete <instance_id>            Delete an instance

  apply <instance_id> <event> [payload]  Apply an event

  wal [from] [limit]              Read WAL entries
  wal-stats                       Show WAL statistics

  quit, exit                      Exit the REPL
"#;

pub async fn run(
    client: Client,
    addr: SocketAddr,
    _token: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "rstmdb CLI".bold().cyan());
    println!("Connecting to {}...", addr);

    // Connect
    client.connect().await?;
    println!("{}", "Connected!".green());

    // Spawn read loop
    let conn = client.connection();
    tokio::spawn(async move {
        let _ = conn.read_loop().await;
    });

    // Give read_loop a chance to start
    tokio::task::yield_now().await;

    // Create readline editor
    let config = Config::builder()
        .history_ignore_space(true)
        .auto_add_history(true)
        .build();
    let mut rl: Editor<(), DefaultHistory> = Editor::with_config(config)?;

    // Load history
    let history_path = std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".rstmdb_history"))
        .unwrap_or_else(|_| ".rstmdb_history".into());
    let _ = rl.load_history(&history_path);

    println!("Type 'help' for available commands.\n");

    loop {
        let prompt = format!("{} ", "rstmdb>".cyan());
        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                match execute_repl_command(&client, line).await {
                    Ok(CommandResult::Output(output)) => {
                        if !output.is_empty() {
                            println!("{}\n", output);
                        }
                    }
                    Ok(CommandResult::Exit) => break,
                    Ok(CommandResult::Reconnect) => {
                        println!("Closing existing connection...");
                        // Close existing connection first
                        let _ = client.close().await;

                        // Give time for read_loop to exit
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                        println!("Reconnecting to {}...", addr);
                        // Add timeout to prevent hanging
                        match tokio::time::timeout(
                            tokio::time::Duration::from_secs(10),
                            client.connect(),
                        )
                        .await
                        {
                            Ok(Ok(_)) => {
                                // Respawn read loop
                                let conn = client.connection();
                                tokio::spawn(async move {
                                    let _ = conn.read_loop().await;
                                });
                                tokio::task::yield_now().await;
                                println!("{}\n", "Reconnected!".green());
                            }
                            Ok(Err(e)) => {
                                println!("{}: {}\n", "Reconnect failed".red(), e);
                            }
                            Err(_) => {
                                println!("{}\n", "Reconnect timed out (10s)".red());
                            }
                        }
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        // Check if it's a connection error
                        if err_str.contains("not connected")
                            || err_str.contains("connection")
                            || err_str.contains("channel closed")
                        {
                            println!(
                                "{}: {}\n{}\n",
                                "Error".red(),
                                e,
                                "Hint: Use 'reconnect' command to reconnect to the server."
                                    .yellow()
                            );
                        } else {
                            println!("{}: {}\n", "Error".red(), e);
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("^D");
                break;
            }
            Err(err) => {
                println!("{}: {:?}", "Error".red(), err);
                break;
            }
        }
    }

    // Save history
    let _ = rl.save_history(&history_path);

    // Disconnect
    let _ = client.close().await;
    println!("{}", "Disconnected.".dimmed());

    Ok(())
}

/// Command result indicating what action to take
enum CommandResult {
    /// Output to display
    Output(String),
    /// Exit the REPL
    Exit,
    /// Reconnect to the server
    Reconnect,
}

async fn execute_repl_command(
    client: &Client,
    line: &str,
) -> Result<CommandResult, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(CommandResult::Output(String::new()));
    }

    let cmd = parts[0].to_lowercase();
    let args = &parts[1..];

    match cmd.as_str() {
        "help" | "?" => Ok(CommandResult::Output(HELP_TEXT.to_string())),

        "quit" | "exit" | "q" => Ok(CommandResult::Exit),

        "reconnect" | "rc" => Ok(CommandResult::Reconnect),

        "ping" => {
            client.ping().await?;
            Ok(CommandResult::Output("PONG".green().to_string()))
        }

        "info" => {
            let info = client.info().await?;
            Ok(CommandResult::Output(format_json(&info)))
        }

        "put-machine" | "pm" => {
            if args.len() < 3 {
                return Ok(CommandResult::Output(
                    "Usage: put-machine <name> <version> <definition_json>".to_string(),
                ));
            }
            let name = args[0];
            let version: u32 = args[1].parse()?;
            let def_str = args[2..].join(" ");
            let definition: Value = serde_json::from_str(&def_str)?;

            let result = client.put_machine(name, version, definition).await?;
            Ok(CommandResult::Output(format!(
                "{} {} v{} (checksum: {})",
                if result.created {
                    "Created".green()
                } else {
                    "Exists".yellow()
                },
                name.cyan(),
                version,
                result.stored_checksum
            )))
        }

        "get-machine" | "gm" => {
            if args.len() < 2 {
                return Ok(CommandResult::Output(
                    "Usage: get-machine <name> <version>".to_string(),
                ));
            }
            let name = args[0];
            let version: u32 = args[1].parse()?;
            let result = client.get_machine(name, version).await?;
            Ok(CommandResult::Output(format_json(&result.definition)))
        }

        "list-machines" | "lm" => {
            let result = client.list_machines().await?;
            if let Some(items) = result["items"].as_array() {
                if items.is_empty() {
                    return Ok(CommandResult::Output("No machines".yellow().to_string()));
                }
                let mut output = String::new();
                for item in items {
                    let name = item["machine"].as_str().unwrap_or("?");
                    let versions: Vec<u64> = item["versions"]
                        .as_array()
                        .map(|v| v.iter().filter_map(|x| x.as_u64()).collect())
                        .unwrap_or_default();

                    // Get latest version to show states/transitions count
                    if let Some(&latest) = versions.iter().max() {
                        if let Ok(machine) = client.get_machine(name, latest as u32).await {
                            let def = &machine.definition;
                            let states = def["states"].as_array().map(|a| a.len()).unwrap_or(0);
                            let transitions =
                                def["transitions"].as_array().map(|a| a.len()).unwrap_or(0);

                            output.push_str(&format!(
                                "  {} v{} ({} states, {} transitions)\n",
                                name.cyan(),
                                latest,
                                states.to_string().yellow(),
                                transitions.to_string().yellow()
                            ));
                        } else {
                            output.push_str(&format!(
                                "  {} [versions: {:?}]\n",
                                name.cyan(),
                                versions
                            ));
                        }
                    } else {
                        output.push_str(&format!("  {} [no versions]\n", name.cyan()));
                    }
                }
                Ok(CommandResult::Output(output))
            } else {
                Ok(CommandResult::Output(format_json(&result)))
            }
        }

        "create" | "c" => {
            if args.len() < 2 {
                return Ok(CommandResult::Output(
                    "Usage: create <machine> <version> [instance_id] [ctx_json]".to_string(),
                ));
            }
            let machine = args[0];
            let version: u32 = args[1].parse()?;
            let instance_id = args.get(2).copied();
            let ctx = args.get(3).map(|s| serde_json::from_str(s)).transpose()?;

            let result = client
                .create_instance(machine, version, instance_id, ctx, None)
                .await?;
            Ok(CommandResult::Output(format!(
                "{} {} (state: {}, offset: {})",
                "Created".green(),
                result.instance_id.cyan(),
                result.state.yellow(),
                result.wal_offset
            )))
        }

        "get" | "g" => {
            if args.is_empty() {
                return Ok(CommandResult::Output(
                    "Usage: get <instance_id>".to_string(),
                ));
            }
            let result = client.get_instance(args[0]).await?;
            Ok(CommandResult::Output(format!(
                "{} {} v{}\n  State: {}\n  Context: {}",
                result.machine.cyan(),
                result.version,
                "",
                result.state.yellow(),
                serde_json::to_string_pretty(&result.ctx)?
            )))
        }

        "list-instances" | "li" => {
            let machine = args.first().copied();
            let state = args.get(1).copied();

            let result = client
                .list_instances(machine, state, Some(50), None)
                .await?;

            if result.instances.is_empty() {
                return Ok(CommandResult::Output("No instances".yellow().to_string()));
            }

            let mut output = format!(
                "{} ({} total{})\n",
                "Instances".bold().cyan(),
                result.total,
                if result.has_more {
                    ", showing first 50"
                } else {
                    ""
                }
            );

            for inst in &result.instances {
                output.push_str(&format!(
                    "  {} ({} v{}) state={}\n",
                    inst.id.cyan(),
                    inst.machine,
                    inst.version,
                    inst.state.yellow()
                ));
            }

            Ok(CommandResult::Output(output))
        }

        "delete" | "d" => {
            if args.is_empty() {
                return Ok(CommandResult::Output(
                    "Usage: delete <instance_id>".to_string(),
                ));
            }
            client.delete_instance(args[0], None).await?;
            Ok(CommandResult::Output(format!(
                "{} {}",
                "Deleted".green(),
                args[0].cyan()
            )))
        }

        "apply" | "a" => {
            if args.len() < 2 {
                return Ok(CommandResult::Output(
                    "Usage: apply <instance_id> <event> [payload_json]".to_string(),
                ));
            }
            let instance_id = args[0];
            let event = args[1];
            let payload = args.get(2).map(|s| serde_json::from_str(s)).transpose()?;

            let result = client
                .apply_event(instance_id, event, payload, None, None)
                .await?;
            Ok(CommandResult::Output(format!(
                "{} {} → {} (offset: {})",
                event.cyan(),
                result.from_state,
                result.to_state.yellow(),
                result.wal_offset
            )))
        }

        "wal" => {
            let from = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let limit = args.get(1).and_then(|s| s.parse().ok());

            let result = client.wal_read(from, limit).await?;
            if let Some(records) = result["records"].as_array() {
                if records.is_empty() {
                    return Ok(CommandResult::Output("No entries".yellow().to_string()));
                }
                let mut output = String::new();
                for record in records {
                    let seq = record["sequence"].as_u64().unwrap_or(0);
                    let entry = &record["entry"];
                    let entry_type = entry["type"].as_str().unwrap_or("?");
                    output.push_str(&format!(
                        "[{}] {}\n",
                        seq.to_string().cyan(),
                        entry_type.yellow()
                    ));
                }
                Ok(CommandResult::Output(output))
            } else {
                Ok(CommandResult::Output(format_json(&result)))
            }
        }

        "wal-stats" | "ws" => {
            let result = client.wal_stats().await?;
            let entry_count = result["entry_count"].as_u64().unwrap_or(0);
            let segment_count = result["segment_count"].as_u64().unwrap_or(0);
            let total_size = result["total_size_bytes"].as_u64().unwrap_or(0);
            let latest_offset = result["latest_offset"].as_u64();

            let io_stats = &result["io_stats"];
            let bytes_written = io_stats["bytes_written"].as_u64().unwrap_or(0);
            let bytes_read = io_stats["bytes_read"].as_u64().unwrap_or(0);
            let writes = io_stats["writes"].as_u64().unwrap_or(0);
            let reads = io_stats["reads"].as_u64().unwrap_or(0);
            let fsyncs = io_stats["fsyncs"].as_u64().unwrap_or(0);

            let size_str = if total_size >= 1024 * 1024 {
                format!("{:.2} MB", total_size as f64 / (1024.0 * 1024.0))
            } else if total_size >= 1024 {
                format!("{:.2} KB", total_size as f64 / 1024.0)
            } else {
                format!("{} B", total_size)
            };

            let mut output = format!(
                "{}\n  Entries: {}\n  Segments: {}\n  Size: {}\n",
                "WAL Statistics".bold().cyan(),
                entry_count.to_string().yellow(),
                segment_count,
                size_str
            );

            if let Some(offset) = latest_offset {
                output.push_str(&format!("  Latest offset: {}\n", offset));
            }

            output.push_str(&format!(
                "\n{}\n  Bytes written: {}\n  Bytes read: {}\n  Writes: {}\n  Reads: {}\n  Fsyncs: {}",
                "I/O Statistics".bold().cyan(),
                bytes_written,
                bytes_read,
                writes,
                reads,
                fsyncs
            ));

            Ok(CommandResult::Output(output))
        }

        _ => Ok(CommandResult::Output(format!(
            "Unknown command: {}. Type 'help' for help.",
            cmd
        ))),
    }
}

fn format_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help_text_contains_reconnect() {
        assert!(HELP_TEXT.contains("reconnect"));
        assert!(HELP_TEXT.contains("Reconnect to the server"));
    }

    #[test]
    fn test_help_text_contains_all_commands() {
        let expected_commands = [
            "help",
            "reconnect",
            "ping",
            "info",
            "put-machine",
            "get-machine",
            "list-machines",
            "create",
            "get",
            "list-instances",
            "delete",
            "apply",
            "wal",
            "wal-stats",
            "quit",
            "exit",
        ];

        for cmd in expected_commands {
            assert!(
                HELP_TEXT.contains(cmd),
                "Help text should contain command: {}",
                cmd
            );
        }
    }

    #[test]
    fn test_command_result_variants() {
        // Test that all CommandResult variants can be created
        let output = CommandResult::Output("test".to_string());
        let exit = CommandResult::Exit;
        let reconnect = CommandResult::Reconnect;

        // Verify they are distinct by matching
        match output {
            CommandResult::Output(s) => assert_eq!(s, "test"),
            _ => panic!("Expected Output variant"),
        }

        match exit {
            CommandResult::Exit => {}
            _ => panic!("Expected Exit variant"),
        }

        match reconnect {
            CommandResult::Reconnect => {}
            _ => panic!("Expected Reconnect variant"),
        }
    }

    #[test]
    fn test_format_json() {
        use serde_json::json;

        let value = json!({"key": "value"});
        let formatted = format_json(&value);
        assert!(formatted.contains("key"));
        assert!(formatted.contains("value"));
    }
}
