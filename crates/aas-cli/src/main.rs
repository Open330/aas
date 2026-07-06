//! `aas` CLI entry point. Command surface mirrors asx (see `docs/PARITY_SPEC.md` §A).
//! P1 wires `list` (no live usage yet) and `import`; the rest are declared and stubbed so the
//! command tree and help are stable while P2–P4 fill in behavior.

use aas_core::naming::{is_known_provider, normalize_provider, KNOWN_PROVIDERS};
use aas_core::share::describe_share;
use aas_core::store::AccountStore;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "aas", version, about = "Agent Account Switcher — multi-account switcher for LLM coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List accounts per provider (or all). `-u` live usage (P2), `-d` dump credentials (P2).
    #[command(visible_alias = "ls")]
    List {
        provider: Option<String>,
        #[arg(short, long)]
        usage: bool,
        #[arg(short, long)]
        debug: bool,
    },
    /// Show asx-tracked active account(s).
    Status { provider: Option<String> },
    /// Adopt / inspect existing asx state.
    Import,
    // --- declared for P2+; not yet implemented ---
    /// Snapshot the live credential as a system profile. (P2)
    Load { provider: Option<String>, name: Option<String> },
    /// Login and store a new isolated profile. (P2)
    Login { provider: Option<String>, name: Option<String> },
    /// Switch the active credential. (P2)
    #[command(visible_alias = "s")]
    Switch { provider: String, name: Option<String> },
    /// Rename an account. (P2)
    Rename { from: String, to: String },
    /// Remove a stored account. (P2)
    #[command(visible_alias = "rm")]
    Remove { args: Vec<String> },
    /// Show/change profile sharing. (P2)
    Sharing { name: String },
    /// Refresh (rotate) a stored credential. (P2)
    Refresh { provider: String, name: Option<String> },
    /// Run the native CLI under a profile. (P3)
    #[command(visible_alias = "e")]
    Exec { name: String, rest: Vec<String> },
    /// Standalone cross-provider proxy. (P4)
    Proxy { name: String, frontend: String },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List { provider, usage, debug } => cmd_list(provider.as_deref(), usage, debug),
        Command::Status { provider } => cmd_status(provider.as_deref()),
        Command::Import => cmd_import(),
        other => {
            eprintln!("`{}` is not implemented yet in this build.", cmd_name(&other));
            std::process::exit(2);
        }
    }
}

fn cmd_name(c: &Command) -> &'static str {
    match c {
        Command::Load { .. } => "load",
        Command::Login { .. } => "login",
        Command::Switch { .. } => "switch",
        Command::Rename { .. } => "rename",
        Command::Remove { .. } => "remove",
        Command::Sharing { .. } => "sharing",
        Command::Refresh { .. } => "refresh",
        Command::Exec { .. } => "exec",
        Command::Proxy { .. } => "proxy",
        _ => "command",
    }
}

fn cmd_list(provider: Option<&str>, usage: bool, _debug: bool) -> anyhow::Result<()> {
    let store = AccountStore::open_default();

    // Resolve which providers to show + optional single-account filter.
    let (provs, single_name): (Vec<String>, Option<String>) = match provider {
        Some(p) if is_known_provider(p) => (vec![normalize_provider(p).unwrap()], None),
        Some(p) => match store.get_by_name(p)? {
            Some(acct) => (vec![acct.provider.clone()], Some(p.to_string())),
            None => {
                eprintln!("Unknown provider or name: {p}");
                std::process::exit(1);
            }
        },
        None => (KNOWN_PROVIDERS.iter().map(|s| s.to_string()).collect(), None),
    };

    for p in &provs {
        let accts: Vec<_> = store
            .list(Some(p))
            .into_iter()
            .filter(|a| single_name.as_ref().map_or(true, |n| &a.name == n))
            .collect();

        if accts.is_empty() {
            // Hide empty providers unless one was named explicitly (parity fix from asx).
            if provider.is_some() && single_name.is_none() {
                println!("{p}:");
                println!("  (none)");
            }
            continue;
        }

        let active = store.get_active(p);
        println!("{p}:");
        for a in &accts {
            let star = if active.as_deref() == Some(a.name.as_str()) { "*" } else { " " };
            let email = a.email.as_deref().map(|e| format!(" <{e}>")).unwrap_or_default();
            let label = match &a.label {
                Some(l) if l != &a.name => format!(" ({l})"),
                _ => String::new(),
            };
            let can_share = a.profile_type != Some(aas_core::model::ProfileType::System)
                && matches!(aas_core::naming::normalize_provider_key(p).as_str(), "claude" | "codex" | "grok");
            let share = if can_share {
                format!(" [{}]", describe_share(a.share.as_deref(), Some(p)))
            } else {
                String::new()
            };
            println!(" {star} {}{email}{label}{share}", a.name);
        }
    }

    if usage {
        eprintln!("(-u live usage lands in P2 — provider HTTP not wired yet)");
    }
    Ok(())
}

fn cmd_status(provider: Option<&str>) -> anyhow::Result<()> {
    let store = AccountStore::open_default();
    let provs: Vec<String> = match provider {
        Some(p) => vec![p.to_string()],
        None => KNOWN_PROVIDERS.iter().map(|s| s.to_string()).collect(),
    };
    for p in provs {
        match store.get_active(&p) {
            Some(name) => println!("{p}: {name}"),
            None => println!("{p}: (none)"),
        }
    }
    Ok(())
}

fn cmd_import() -> anyhow::Result<()> {
    let report = aas_import::inspect()?;
    println!("asx state at {}", aas_core::platform::asx_config_dir().display());
    println!("  accounts:          {}", report.accounts);
    println!("  with profile home: {}", report.with_profile_home);
    if !report.missing_credential.is_empty() {
        println!("  missing credential: {}", report.missing_credential.join(", "));
    }
    println!("aas reads these directly — no migration needed.");
    Ok(())
}
