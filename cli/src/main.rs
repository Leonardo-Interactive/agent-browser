mod color;
mod commands;
mod connection;
mod flags;
mod install;
mod native;
mod output;
#[cfg(test)]
mod test_utils;
mod upgrade;
mod validation;

use serde_json::json;
use std::env;
use std::fs;
use std::process::exit;

#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use commands::{gen_id, parse_command, ParseError};
use connection::{ensure_daemon, get_socket_dir, send_command, DaemonOptions};
use flags::{clean_args, parse_flags, Flags};
use install::run_install;
use output::{
    print_command_help, print_help, print_response_with_opts, print_version, OutputOptions,
};
use upgrade::run_upgrade;

fn serialize_json_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        r#"{"success":false,"error":"Failed to serialize JSON response"}"#.to_string()
    })
}

fn print_json_value(value: serde_json::Value) {
    println!("{}", serialize_json_value(&value));
}

fn print_json_error(message: impl AsRef<str>) {
    print_json_value(json!({
        "success": false,
        "error": message.as_ref(),
    }));
}

fn print_json_error_with_type(message: impl AsRef<str>, error_type: &str) {
    print_json_value(json!({
        "success": false,
        "error": message.as_ref(),
        "type": error_type,
    }));
}

struct ParsedProxy {
    server: String,
    username: Option<String>,
    password: Option<String>,
}

fn parse_proxy(proxy_str: &str) -> ParsedProxy {
    let Some(protocol_end) = proxy_str.find("://") else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };
    let protocol = &proxy_str[..protocol_end + 3];
    let rest = &proxy_str[protocol_end + 3..];

    let Some(at_pos) = rest.rfind('@') else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };

    let creds = &rest[..at_pos];
    let server_part = &rest[at_pos + 1..];
    let server = format!("{}{}", protocol, server_part);

    let (username, password) = match creds.find(':') {
        Some(colon_pos) => {
            let u = &creds[..colon_pos];
            let p = &creds[colon_pos + 1..];
            (
                if u.is_empty() {
                    None
                } else {
                    Some(u.to_string())
                },
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                },
            )
        }
        None => (
            if creds.is_empty() {
                None
            } else {
                Some(creds.to_string())
            },
            None,
        ),
    };

    ParsedProxy {
        server,
        username,
        password,
    }
}

fn run_session(args: &[String], session: &str, json_mode: bool) {
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        Some("list") => {
            let socket_dir = get_socket_dir();
            let mut sessions: Vec<String> = Vec::new();

            if let Ok(entries) = fs::read_dir(&socket_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Look for pid files in socket directory
                    if name.ends_with(".pid") {
                        let session_name = name.strip_suffix(".pid").unwrap_or("");
                        if !session_name.is_empty() {
                            // Check if session is actually running
                            let pid_path = socket_dir.join(&name);
                            if let Ok(pid_str) = fs::read_to_string(&pid_path) {
                                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                                    #[cfg(unix)]
                                    let running = unsafe {
                                        libc::kill(pid as i32, 0) == 0
                                            || std::io::Error::last_os_error().raw_os_error()
                                                != Some(libc::ESRCH)
                                    };
                                    #[cfg(windows)]
                                    let running = unsafe {
                                        let handle =
                                            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                                        if handle != 0 {
                                            CloseHandle(handle);
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if running {
                                        sessions.push(session_name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if json_mode {
                println!(
                    r#"{{"success":true,"data":{{"sessions":{}}}}}"#,
                    serde_json::to_string(&sessions).unwrap_or_default()
                );
            } else if sessions.is_empty() {
                println!("No active sessions");
            } else {
                println!("Active sessions:");
                for s in &sessions {
                    let marker = if s == session {
                        color::cyan("→")
                    } else {
                        " ".to_string()
                    };
                    println!("{} {}", marker, s);
                }
            }
        }
        None | Some(_) => {
            // Just show current session
            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": {
                        "session": session,
                    },
                }));
            } else {
                println!("{}", session);
            }
        }
    }
}

fn run_close_all(flags: &Flags) {
    let socket_dir = get_socket_dir();
    let mut sessions: Vec<String> = Vec::new();

    if let Ok(entries) = fs::read_dir(&socket_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(session_name) = name.strip_suffix(".pid") {
                if session_name.is_empty() {
                    continue;
                }
                let pid_path = socket_dir.join(&name);
                if let Ok(pid_str) = fs::read_to_string(&pid_path) {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        #[cfg(unix)]
                        let running = unsafe {
                            libc::kill(pid as i32, 0) == 0
                                || std::io::Error::last_os_error().raw_os_error()
                                    != Some(libc::ESRCH)
                        };
                        #[cfg(windows)]
                        let running = unsafe {
                            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                            if handle != 0 {
                                CloseHandle(handle);
                                true
                            } else {
                                false
                            }
                        };
                        if running {
                            sessions.push(session_name.to_string());
                        }
                    }
                }
            }
        }
    }

    if sessions.is_empty() {
        if flags.json {
            print_json_value(json!({
                "success": true,
                "data": { "closed": 0, "sessions": [] },
            }));
        } else {
            println!("No active sessions");
        }
        return;
    }

    let mut closed: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();

    for session in &sessions {
        let cmd = json!({ "id": gen_id(), "action": "close" });
        match send_command(cmd, session) {
            Ok(resp) if resp.success => closed.push(session.clone()),
            Ok(resp) => {
                let err = resp.error.unwrap_or_else(|| "Unknown error".to_string());
                failed.push((session.clone(), err));
            }
            Err(e) => failed.push((session.clone(), e.to_string())),
        }
    }

    if flags.json {
        print_json_value(json!({
            "success": failed.is_empty(),
            "data": {
                "closed": closed.len(),
                "sessions": closed,
                "failed": failed.iter().map(|(s, e)| json!({"session": s, "error": e})).collect::<Vec<_>>(),
            },
        }));
    } else {
        for s in &closed {
            println!("{} Closed session: {}", color::green("✓"), s);
        }
        for (s, e) in &failed {
            eprintln!("{} Failed to close {}: {}", color::error_indicator(), s, e);
        }
        if closed.is_empty() && !failed.is_empty() {
            exit(1);
        }
    }

    if !failed.is_empty() {
        exit(1);
    }
}

fn main() {
    // Rust ignores SIGPIPE by default, causing println! to panic on broken pipes.
    // Reset to SIG_DFL so the OS terminates the process cleanly instead.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Prevent MSYS/Git Bash path translation from mangling arguments
    #[cfg(windows)]
    {
        env::set_var("MSYS_NO_PATHCONV", "1");
        env::set_var("MSYS2_ARG_CONV_EXCL", "*");
    }

    // Native daemon mode: when AGENT_BROWSER_DAEMON is set, run as the daemon process
    if env::var("AGENT_BROWSER_DAEMON").is_ok() {
        // Ignore SIGPIPE so the daemon isn't killed when the parent drops
        // the piped stderr handle after confirming the daemon is ready.
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        let session = env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string());
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(native::daemon::run_daemon(&session));
        return;
    }

    let args: Vec<String> = env::args().skip(1).collect();
    let flags = parse_flags(&args);
    let clean = clean_args(&args);

    let has_help = args.iter().any(|a| a == "--help" || a == "-h");
    let has_version = args.iter().any(|a| a == "--version" || a == "-V");

    if has_help {
        if let Some(cmd) = clean.first() {
            if print_command_help(cmd) {
                return;
            }
        }
        print_help();
        return;
    }

    if has_version {
        print_version();
        return;
    }

    if clean.is_empty() {
        print_help();
        return;
    }

    // Handle install separately
    if clean.first().map(|s| s.as_str()) == Some("install") {
        let with_deps = args.iter().any(|a| a == "--with-deps" || a == "-d");
        run_install(with_deps);
        return;
    }

    // Handle upgrade separately
    if clean.first().map(|s| s.as_str()) == Some("upgrade") {
        run_upgrade();
        return;
    }

    // Handle session separately (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("session") {
        run_session(&clean, &flags.session, flags.json);
        return;
    }

    // Handle close --all: close all active sessions
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("close") | Some("quit") | Some("exit")
    ) && clean.iter().any(|a| a == "--all")
    {
        run_close_all(&flags);
        return;
    }

    let mut cmd = match parse_command(&clean, &flags) {
        Ok(c) => c,
        Err(e) => {
            if flags.json {
                let error_type = match &e {
                    ParseError::UnknownCommand { .. } => "unknown_command",
                    ParseError::UnknownSubcommand { .. } => "unknown_subcommand",
                    ParseError::MissingArguments { .. } => "missing_arguments",
                    ParseError::InvalidValue { .. } => "invalid_value",
                    ParseError::InvalidSessionName { .. } => "invalid_session_name",
                };
                print_json_error_with_type(e.format(), error_type);
            } else {
                eprintln!("{}", color::red(&e.format()));
            }
            exit(1);
        }
    };

    // Handle --password-stdin for auth save
    if cmd.get("action").and_then(|v| v.as_str()) == Some("auth_save") {
        if cmd.get("password").is_some() {
            eprintln!(
                "{} Passwords on the command line may be visible in process listings and shell history. Use --password-stdin instead.",
                color::warning_indicator()
            );
        }
        if cmd
            .get("passwordStdin")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let mut pass = String::new();
            if std::io::stdin().read_line(&mut pass).is_err() || pass.is_empty() {
                eprintln!(
                    "{} Failed to read password from stdin",
                    color::error_indicator()
                );
                exit(1);
            }
            let pass = pass.trim_end_matches('\n').trim_end_matches('\r');
            if pass.is_empty() {
                eprintln!("{} Password from stdin is empty", color::error_indicator());
                exit(1);
            }
            cmd["password"] = json!(pass);
            cmd.as_object_mut().unwrap().remove("passwordStdin");
        }
    }

    // Validate session name before starting daemon
    if let Some(ref name) = flags.session_name {
        if !validation::is_valid_session_name(name) {
            let msg = validation::session_name_error(name);
            if flags.json {
                print_json_error_with_type(msg, "invalid_session_name");
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Handle state management commands locally — these are pure file operations
    // that don't need a daemon, avoiding an unnecessary daemon startup that
    // would lack runtime config like session_name.
    if let Some(result) = native::state::dispatch_state_command(&cmd) {
        let action = cmd.get("action").and_then(|v| v.as_str());
        let resp = match result {
            Ok(data) => connection::Response {
                success: true,
                data: Some(data),
                error: None,
                warning: None,
            },
            Err(e) => connection::Response {
                success: false,
                data: None,
                error: Some(e),
                warning: None,
            },
        };
        let output_opts = OutputOptions::from_flags(&flags);
        output::print_response_with_opts(&resp, action, &output_opts);
        if !resp.success {
            exit(1);
        }
        return;
    }

    // Parse proxy URL to separate server from credentials for the daemon.
    let (proxy_server, proxy_username, proxy_password) = if let Some(ref proxy_str) = flags.proxy {
        let parsed = parse_proxy(proxy_str);
        (Some(parsed.server), parsed.username, parsed.password)
    } else {
        (None, None, None)
    };
    let daemon_opts = DaemonOptions {
        headed: flags.headed,
        debug: flags.debug,
        args: flags.args.as_deref(),
        user_agent: flags.user_agent.as_deref(),
        proxy: proxy_server.as_deref(),
        proxy_bypass: flags.proxy_bypass.as_deref(),
        proxy_username: proxy_username.as_deref(),
        proxy_password: proxy_password.as_deref(),
        ignore_https_errors: flags.ignore_https_errors,
        profile: flags.profile.as_deref(),
        state: flags.state.as_deref(),
        device: flags.device.as_deref(),
        session_name: flags.session_name.as_deref(),
        download_path: flags.download_path.as_deref(),
        allowed_domains: flags.allowed_domains.as_deref(),
        action_policy: flags.action_policy.as_deref(),
        confirm_actions: flags.confirm_actions.as_deref(),
        engine: flags.engine.as_deref(),
        idle_timeout: flags.idle_timeout.as_deref(),
        no_auto_dialog: flags.no_auto_dialog,
    };

    let daemon_result = match ensure_daemon(&flags.session, &daemon_opts) {
        Ok(result) => result,
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    };

    // Warn if launch-time options were explicitly passed via CLI but daemon was already running
    // Only warn about flags that were passed on the command line, not those set via environment
    // variables (since the daemon already uses the env vars when it starts).
    if daemon_result.already_running {
        let ignored_flags: Vec<&str> = [
            if flags.cli_profile {
                Some("--profile")
            } else {
                None
            },
            if flags.cli_state {
                Some("--state")
            } else {
                None
            },
            if flags.cli_args { Some("--args") } else { None },
            if flags.cli_user_agent {
                Some("--user-agent")
            } else {
                None
            },
            if flags.cli_proxy {
                Some("--proxy")
            } else {
                None
            },
            if flags.cli_proxy_bypass {
                Some("--proxy-bypass")
            } else {
                None
            },
            flags.ignore_https_errors.then_some("--ignore-https-errors"),
            flags.cli_download_path.then_some("--download-path"),
            flags.cli_headed.then_some("--headed"),
        ]
        .into_iter()
        .flatten()
        .collect();

        if !ignored_flags.is_empty() && !flags.json {
            eprintln!(
                "{} {} ignored: daemon already running. Use 'agent-browser close' first to restart with new options.",
                color::warning_indicator(),
                ignored_flags.join(", ")
            );
        }
    }

    // Launch headed browser or configure browser options
    if flags.headed
        || flags.cli_headed  // User explicitly set --headed (even if false)
        || flags.profile.is_some()
        || flags.state.is_some()
        || flags.proxy.is_some()
        || flags.args.is_some()
        || flags.user_agent.is_some()
        || flags.color_scheme.is_some()
        || flags.download_path.is_some()
        || flags.engine.is_some()
    {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "headless": !flags.headed
        });

        let cmd_obj = launch_cmd
            .as_object_mut()
            .expect("json! macro guarantees object type");

        // Add profile path if specified
        if let Some(ref profile_path) = flags.profile {
            cmd_obj.insert("profile".to_string(), json!(profile_path));
        }

        // Add state path if specified
        if let Some(ref state_path) = flags.state {
            cmd_obj.insert("storageState".to_string(), json!(state_path));
        }

        if let Some(ref proxy_str) = flags.proxy {
            let parsed = parse_proxy(proxy_str);
            let mut proxy_obj = json!({ "server": parsed.server });
            if let Some(ref username) = parsed.username {
                proxy_obj["username"] = json!(username);
            }
            if let Some(ref password) = parsed.password {
                proxy_obj["password"] = json!(password);
            }
            if let Some(ref bypass) = flags.proxy_bypass {
                proxy_obj["bypass"] = json!(bypass);
            }
            cmd_obj.insert("proxy".to_string(), proxy_obj);
        }

        if let Some(ref ua) = flags.user_agent {
            cmd_obj.insert("userAgent".to_string(), json!(ua));
        }

        if let Some(ref a) = flags.args {
            // Parse args (comma or newline separated)
            let args_vec: Vec<String> = a
                .split(&[',', '\n'][..])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            cmd_obj.insert("args".to_string(), json!(args_vec));
        }

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        if let Some(ref domains) = flags.allowed_domains {
            launch_cmd["allowedDomains"] = json!(domains);
        }

        if let Some(ref engine) = flags.engine {
            launch_cmd["engine"] = json!(engine);
        }

        match send_command(launch_cmd, &flags.session) {
            Ok(resp) if !resp.success => {
                // Launch command failed (e.g., invalid state file, profile error)
                let error_msg = resp
                    .error
                    .unwrap_or_else(|| "Browser launch failed".to_string());
                if flags.json {
                    print_json_error(error_msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), error_msg);
                }
                exit(1);
            }
            Err(e) => {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!(
                        "{} Could not configure browser: {}",
                        color::error_indicator(),
                        e
                    );
                }
                exit(1);
            }
            Ok(_) => {
                // Launch succeeded
            }
        }
    }

    // Handle batch command: read commands from stdin, execute sequentially
    if cmd.get("action").and_then(|v| v.as_str()) == Some("batch") {
        let bail = cmd.get("bail").and_then(|v| v.as_bool()).unwrap_or(false);
        run_batch(&flags, bail);
        return;
    }

    let output_opts = OutputOptions::from_flags(&flags);

    match send_command(cmd.clone(), &flags.session) {
        Ok(resp) => {
            let success = resp.success;
            // Handle interactive confirmation
            if flags.confirm_interactive {
                if let Some(data) = &resp.data {
                    if data
                        .get("confirmation_required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        let desc = data
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown action");
                        let category = data.get("category").and_then(|v| v.as_str()).unwrap_or("");
                        let cid = data
                            .get("confirmation_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        eprintln!("[agent-browser] Action requires confirmation:");
                        eprintln!("  {}: {}", category, desc);
                        eprint!("  Allow? [y/N]: ");

                        let mut input = String::new();
                        let approved = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                            std::io::stdin().read_line(&mut input).is_ok()
                                && matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
                        } else {
                            false
                        };

                        let confirm_cmd = if approved {
                            json!({ "id": gen_id(), "action": "confirm", "confirmationId": cid })
                        } else {
                            json!({ "id": gen_id(), "action": "deny", "confirmationId": cid })
                        };

                        match send_command(confirm_cmd, &flags.session) {
                            Ok(r) => {
                                if !approved {
                                    eprintln!("{} Action denied", color::error_indicator());
                                    exit(1);
                                }
                                print_response_with_opts(&r, None, &output_opts);
                            }
                            Err(e) => {
                                eprintln!("{} {}", color::error_indicator(), e);
                                exit(1);
                            }
                        }
                        return;
                    }
                }
            }
            // Extract action for context-specific output handling
            let action = cmd.get("action").and_then(|v| v.as_str());
            print_response_with_opts(&resp, action, &output_opts);
            if !success {
                exit(1);
            }
        }
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }
}

fn run_batch(flags: &Flags, bail: bool) {
    use std::io::Read as _;

    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        if flags.json {
            print_json_error(format!("Failed to read stdin: {}", e));
        } else {
            eprintln!("{} Failed to read stdin: {}", color::error_indicator(), e);
        }
        exit(1);
    }

    let commands: Vec<Vec<String>> = match serde_json::from_str(&input) {
        Ok(c) => c,
        Err(e) => {
            if flags.json {
                print_json_error(format!(
                    "Invalid JSON input: {}. Expected an array of string arrays, e.g. [[\"open\", \"https://example.com\"], [\"snapshot\"]]",
                    e
                ));
            } else {
                eprintln!(
                    "{} Invalid JSON input: {}. Expected an array of string arrays.",
                    color::error_indicator(),
                    e
                );
            }
            exit(1);
        }
    };

    if commands.is_empty() {
        if flags.json {
            println!("[]");
        }
        return;
    }

    let output_opts = OutputOptions::from_flags(flags);

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut had_error = false;

    for (i, cmd_args) in commands.iter().enumerate() {
        if cmd_args.is_empty() {
            continue;
        }

        let parsed = match parse_command(cmd_args, flags) {
            Ok(c) => c,
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.format(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!(
                        "{} Command {}: {}",
                        color::error_indicator(),
                        i + 1,
                        e.format()
                    );
                    if bail {
                        exit(1);
                    }
                }
                continue;
            }
        };

        let action = parsed
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match send_command(parsed, &flags.session) {
            Ok(resp) => {
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": resp.success,
                        "result": resp.data,
                        "error": resp.error,
                    }));
                } else {
                    if i > 0 {
                        println!();
                    }
                    print_response_with_opts(&resp, action.as_deref(), &output_opts);
                }
                if !resp.success {
                    had_error = true;
                    if bail {
                        if !flags.json {
                            exit(1);
                        }
                        break;
                    }
                }
            }
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.to_string(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!("{} Command {}: {}", color::error_indicator(), i + 1, e);
                    if bail {
                        exit(1);
                    }
                }
            }
        }
    }

    if flags.json {
        println!(
            "{}",
            serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string())
        );
    }

    if had_error {
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proxy_simple() {
        let result = parse_proxy("http://proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert!(result.username.is_none());
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_with_auth() {
        let result = parse_proxy("http://user:pass@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("pass"));
    }

    #[test]
    fn test_parse_proxy_username_only() {
        let result = parse_proxy("http://user@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_no_protocol() {
        let result = parse_proxy("proxy.com:8080");
        assert_eq!(result.server, "proxy.com:8080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5() {
        let result = parse_proxy("socks5://proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5_with_auth() {
        let result = parse_proxy("socks5://admin:secret@proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert_eq!(result.username.as_deref(), Some("admin"));
        assert_eq!(result.password.as_deref(), Some("secret"));
    }

    #[test]
    fn test_parse_proxy_complex_password() {
        let result = parse_proxy("http://user:p@ss:w0rd@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("p@ss:w0rd"));
    }

    #[test]
    fn test_serialize_json_value_escapes_control_characters() {
        let payload = serialize_json_value(&json!({
            "success": false,
            "error": "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m",
        }));

        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(
            parsed["error"],
            "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m"
        );
    }
}
