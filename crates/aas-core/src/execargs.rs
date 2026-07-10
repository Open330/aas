//! `exec`/`e` argument parsing. Mirrors asx `exec-args.ts` exactly.
//!
//! Cross-provider runs consume the context options (`-s/-i/--share/--isolate/--keep-context`)
//! before the agent args; same-provider runs pass everything through to the native binary.
//! `--` forces all remaining tokens through to the agent.

use crate::share::{resolve_share_selection, ShareOpts, ShareSelection};

#[derive(Debug, Clone)]
pub struct ExecArgs {
    pub forward_args: Vec<String>,
    pub bypass: bool,
    pub debug: bool,
    pub keep_context: bool,
    pub share: ShareSelection,
}

fn share_count(o: &ShareOpts) -> usize {
    [o.isolated, o.shared, o.share.is_some(), o.isolate.is_some()]
        .iter()
        .filter(|b| **b)
        .count()
}

fn guard_one(o: &ShareOpts) -> anyhow::Result<()> {
    if share_count(o) >= 1 {
        anyhow::bail!("Use only one of --isolated / --shared / --share / --isolate.");
    }
    Ok(())
}

fn need_value(args: &[String], i: usize, flag: &str) -> anyhow::Result<String> {
    match args.get(i + 1) {
        Some(v) if v != "--" && !v.starts_with('-') => Ok(v.clone()),
        _ => anyhow::bail!("{flag} requires categories. Example: {flag} sessions,skills"),
    }
}

pub fn parse_exec_args(
    args: &[String],
    is_cross: bool,
    agent_provider: Option<&str>,
) -> anyhow::Result<ExecArgs> {
    let mut forward: Vec<String> = Vec::new();
    let mut o = ShareOpts::default();
    let mut bypass = false;
    let mut debug = false;
    let mut keep_context = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            forward.extend(args[i + 1..].iter().cloned());
            break;
        }
        if arg == "-b" || arg == "--bypass" {
            bypass = true;
            i += 1;
            continue;
        }
        if arg == "-d" || arg == "--debug" {
            debug = true;
            i += 1;
            continue;
        }

        if is_cross {
            let mut consumed_extra = false;
            let mut matched = true;
            if arg == "-i" || arg == "--isolated" {
                guard_one(&o)?;
                o.isolated = true;
            } else if arg == "-s" || arg == "--shared" {
                guard_one(&o)?;
                o.shared = true;
            } else if arg == "--share" {
                let v = need_value(args, i, "--share")?;
                guard_one(&o)?;
                o.share = Some(v);
                consumed_extra = true;
            } else if let Some(v) = arg.strip_prefix("--share=") {
                if v.is_empty() {
                    anyhow::bail!("--share requires categories. Example: --share sessions,skills");
                }
                guard_one(&o)?;
                o.share = Some(v.to_string());
            } else if arg == "--isolate" {
                let v = need_value(args, i, "--isolate")?;
                guard_one(&o)?;
                o.isolate = Some(v);
                consumed_extra = true;
            } else if let Some(v) = arg.strip_prefix("--isolate=") {
                if v.is_empty() {
                    anyhow::bail!("--isolate requires categories. Example: --isolate settings");
                }
                guard_one(&o)?;
                o.isolate = Some(v.to_string());
            } else if arg == "--keep-context" {
                keep_context = true;
            } else {
                matched = false;
            }
            if matched {
                i += if consumed_extra { 2 } else { 1 };
                continue;
            }
        }

        forward.push(args[i].clone());
        i += 1;
    }

    let provider = if is_cross { agent_provider } else { None };
    let share = resolve_share_selection(&o, provider)?;
    Ok(ExecArgs {
        forward_args: forward,
        bypass,
        debug,
        keep_context,
        share,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn same_provider_passes_share_flags_through() {
        let r = parse_exec_args(&v(&["-s", "foo", "--isolate", "x"]), false, None).unwrap();
        assert_eq!(r.forward_args, v(&["-s", "foo", "--isolate", "x"]));
        assert!(!r.share.provided);
    }

    #[test]
    fn cross_consumes_context_opts() {
        let r = parse_exec_args(&v(&["-i", "hello"]), true, Some("claude")).unwrap();
        assert_eq!(r.share.value, Some(vec![]));
        assert_eq!(r.forward_args, v(&["hello"]));

        let r = parse_exec_args(
            &v(&["--share", "sessions,skills", "do", "it"]),
            true,
            Some("claude"),
        )
        .unwrap();
        assert_eq!(
            r.share.value,
            Some(vec!["sessions".into(), "skills".into()])
        );
        assert_eq!(r.forward_args, v(&["do", "it"]));

        let r = parse_exec_args(&v(&["--keep-context", "run"]), true, Some("codex")).unwrap();
        assert!(r.keep_context);
        assert_eq!(r.forward_args, v(&["run"]));
    }

    #[test]
    fn double_dash_passthrough() {
        let r = parse_exec_args(
            &v(&["-b", "--", "-s", "-i", "--share"]),
            true,
            Some("claude"),
        )
        .unwrap();
        assert!(r.bypass);
        assert_eq!(r.forward_args, v(&["-s", "-i", "--share"]));
        assert!(!r.share.provided);
    }

    #[test]
    fn bypass_and_debug() {
        let r = parse_exec_args(&v(&["-b", "-d", "cmd"]), false, None).unwrap();
        assert!(r.bypass && r.debug);
        assert_eq!(r.forward_args, v(&["cmd"]));
    }

    #[test]
    fn only_one_share_flag() {
        assert!(parse_exec_args(&v(&["-i", "-s"]), true, Some("claude")).is_err());
        assert!(parse_exec_args(&v(&["--share", "--"]), true, Some("claude")).is_err());
    }
}
