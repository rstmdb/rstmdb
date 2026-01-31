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
  ping                          Ping the server
  info                          Get server info

  put-machine <name> <ver> <def>  Register a machine definition
  get-machine <name> <ver>        Get a machine definition
  list-machines                   List all machines

  create <machine> <ver> [id]     Create an instance
  get <instance_id>               Get instance state
  delete <instance_id>            Delete an instance

  apply <instance_id> <event> [payload]  Apply an event

  wal [from] [limit]              Read WAL entries

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
                    Ok(Some(output)) => println!("{}\n", output),
                    Ok(None) => break, // Exit command
                    Err(e) => println!("{}: {}\n", "Error".red(), e),
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

async fn execute_repl_command(
    client: &Client,
    line: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(Some(String::new()));
    }

    let cmd = parts[0].to_lowercase();
    let args = &parts[1..];

    match cmd.as_str() {
        "help" | "?" => Ok(Some(HELP_TEXT.to_string())),

        "quit" | "exit" | "q" => Ok(None),

        "ping" => {
            client.ping().await?;
            Ok(Some("PONG".green().to_string()))
        }

        "info" => {
            let info = client.info().await?;
            Ok(Some(format_json(&info)))
        }

        "put-machine" | "pm" => {
            if args.len() < 3 {
                return Ok(Some(
                    "Usage: put-machine <name> <version> <definition_json>".to_string(),
                ));
            }
            let name = args[0];
            let version: u32 = args[1].parse()?;
            let def_str = args[2..].join(" ");
            let definition: Value = serde_json::from_str(&def_str)?;

            let result = client.put_machine(name, version, definition).await?;
            Ok(Some(format!(
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
                return Ok(Some("Usage: get-machine <name> <version>".to_string()));
            }
            let name = args[0];
            let version: u32 = args[1].parse()?;
            let result = client.get_machine(name, version).await?;
            Ok(Some(format_json(&result.definition)))
        }

        "list-machines" | "lm" => {
            let result = client.list_machines().await?;
            if let Some(items) = result["items"].as_array() {
                if items.is_empty() {
                    return Ok(Some("No machines".yellow().to_string()));
                }
                let mut output = String::new();
                for item in items {
                    let name = item["machine"].as_str().unwrap_or("?");
                    let versions: Vec<_> = item["versions"]
                        .as_array()
                        .map(|v| v.iter().filter_map(|x| x.as_u64()).collect())
                        .unwrap_or_default();
                    output.push_str(&format!("  {} v{:?}\n", name.cyan(), versions));
                }
                Ok(Some(output))
            } else {
                Ok(Some(format_json(&result)))
            }
        }

        "create" | "c" => {
            if args.len() < 2 {
                return Ok(Some(
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
            Ok(Some(format!(
                "{} {} (state: {}, offset: {})",
                "Created".green(),
                result.instance_id.cyan(),
                result.state.yellow(),
                result.wal_offset
            )))
        }

        "get" | "g" => {
            if args.is_empty() {
                return Ok(Some("Usage: get <instance_id>".to_string()));
            }
            let result = client.get_instance(args[0]).await?;
            Ok(Some(format!(
                "{} {} v{}\n  State: {}\n  Context: {}",
                result.machine.cyan(),
                result.version,
                "",
                result.state.yellow(),
                serde_json::to_string_pretty(&result.ctx)?
            )))
        }

        "delete" | "d" => {
            if args.is_empty() {
                return Ok(Some("Usage: delete <instance_id>".to_string()));
            }
            client.delete_instance(args[0], None).await?;
            Ok(Some(format!("{} {}", "Deleted".green(), args[0].cyan())))
        }

        "apply" | "a" => {
            if args.len() < 2 {
                return Ok(Some(
                    "Usage: apply <instance_id> <event> [payload_json]".to_string(),
                ));
            }
            let instance_id = args[0];
            let event = args[1];
            let payload = args.get(2).map(|s| serde_json::from_str(s)).transpose()?;

            let result = client
                .apply_event(instance_id, event, payload, None, None)
                .await?;
            Ok(Some(format!(
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
                    return Ok(Some("No entries".yellow().to_string()));
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
                Ok(Some(output))
            } else {
                Ok(Some(format_json(&result)))
            }
        }

        _ => Ok(Some(format!(
            "Unknown command: {}. Type 'help' for help.",
            cmd
        ))),
    }
}

fn format_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
