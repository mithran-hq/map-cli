use clap::{Args, Parser, Subcommand};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "map", version, about = "Thin MAP client for Aegis.app")]
struct Cli {
    #[arg(long, global = true)]
    login_state: Option<PathBuf>,

    #[arg(long, global = true)]
    endpoint: Option<String>,

    #[arg(long, global = true)]
    token: Option<String>,

    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Login(LoginCommand),
    Whoami,
    Doctor,
    Init(InitArgs),
    Validate(DeployTarget),
    Deploy(DeployArgs),
    Status(IdArgs),
    Watch(WatchArgs),
    Logs(IdArgs),
    Evidence(IdArgs),
    Rollback(RollbackArgs),
    Version,
}

#[derive(Args)]
struct LoginCommand {
    #[command(subcommand)]
    command: LoginSubcommand,
}

#[derive(Subcommand)]
enum LoginSubcommand {
    Save(LoginSaveArgs),
    PrintToken(PrintTokenArgs),
}

#[derive(Args)]
struct LoginSaveArgs {
    #[arg(long)]
    map_control_endpoint: String,

    #[arg(long)]
    jason_controller_endpoint: Option<String>,

    #[arg(long)]
    access_token: String,

    #[arg(long, default_value = "map-control")]
    audience: String,

    #[arg(long = "scope", value_delimiter = ',')]
    scopes: Vec<String>,

    #[arg(long)]
    expires_at: Option<String>,

    #[arg(long)]
    email: Option<String>,

    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
struct PrintTokenArgs {
    #[arg(long)]
    audience: String,
}

#[derive(Args, Serialize)]
struct InitArgs {
    #[arg(long, default_value = "mithran.yaml")]
    manifest: PathBuf,
}

#[derive(Args, Serialize)]
struct DeployTarget {
    #[arg(long)]
    repo: String,

    #[arg(long)]
    env: Option<String>,

    #[arg(long = "ref", conflicts_with = "sha")]
    ref_name: Option<String>,

    #[arg(long)]
    sha: Option<String>,
}

#[derive(Args, Serialize)]
struct DeployArgs {
    #[command(flatten)]
    target: DeployTarget,

    #[arg(long)]
    evidence_ref: Option<String>,
}

#[derive(Args)]
struct IdArgs {
    id: String,
}

#[derive(Args)]
struct WatchArgs {
    id: String,

    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,

    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,
}

#[derive(Args, Serialize)]
struct RollbackArgs {
    id: String,

    #[arg(long)]
    evidence_ref: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct LoginState {
    map_control_endpoint: String,
    jason_controller_endpoint: Option<String>,
    access_token: String,
    expires_at: Option<String>,
    audience: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    principal: Option<Principal>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Principal {
    email: Option<String>,
    name: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(cli) {
        eprintln!("map: {}", redact(&error));
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match &cli.command {
        Command::Login(LoginCommand {
            command: LoginSubcommand::Save(args),
        }) => {
            let state = LoginState {
                map_control_endpoint: args.map_control_endpoint.clone(),
                jason_controller_endpoint: args.jason_controller_endpoint.clone(),
                access_token: args.access_token.clone(),
                expires_at: args.expires_at.clone(),
                audience: Some(args.audience.clone()),
                scopes: args.scopes.clone(),
                principal: Some(Principal {
                    email: args.email.clone(),
                    name: args.name.clone(),
                }),
            };
            let path = login_state_path(cli.login_state.as_ref())?;
            write_login_state(&path, &state)?;
            print_json_or_text(cli.json, json!({ "ok": true, "path": path }), "login saved")
        }
        Command::Login(LoginCommand {
            command: LoginSubcommand::PrintToken(args),
        }) => {
            let state = resolve_state(&cli)?;
            if !audience_allowed(&state, &args.audience) {
                return Err(format!(
                    "login state is not valid for audience `{}`; run `map login`",
                    args.audience
                ));
            }
            println!("{}", state.access_token);
            Ok(())
        }
        Command::Whoami => {
            let state = resolve_state(&cli)?;
            let principal = state.principal.unwrap_or(Principal {
                email: None,
                name: None,
            });
            print_json_or_text(
                cli.json,
                json!({
                    "ok": true,
                    "endpoint": state.map_control_endpoint,
                    "email": principal.email,
                    "name": principal.name,
                    "audience": state.audience,
                    "scopes": state.scopes,
                }),
                "logged in",
            )
        }
        Command::Doctor => {
            let state = resolve_state(&cli);
            let payload = match state {
                Ok(state) => json!({
                    "ok": true,
                    "schema_version": "map.doctor.v1",
                    "endpoint": state.map_control_endpoint,
                    "jason_controller_endpoint": state.jason_controller_endpoint,
                    "has_token": true,
                }),
                Err(error) => json!({
                    "ok": false,
                    "schema_version": "map.doctor.v1",
                    "error": redact(&error),
                }),
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
            } else if payload["ok"].as_bool() == Some(true) {
                println!("MAP client is configured");
            } else {
                println!("MAP client is not configured; run `map login`");
            }
            Ok(())
        }
        Command::Init(args) => {
            if args.manifest.exists() {
                return Err(format!("{} already exists", args.manifest.display()));
            }
            fs::write(
                &args.manifest,
                "schema_version: mithran.map.v1\nname: example\n",
            )
            .map_err(|error| format!("write {}: {error}", args.manifest.display()))?;
            print_json_or_text(
                cli.json,
                json!({ "ok": true, "manifest": args.manifest }),
                "created mithran.yaml",
            )
        }
        Command::Validate(target) => {
            validate_target(target)?;
            print_json_or_text(cli.json, json!({ "ok": true }), "target is valid")
        }
        Command::Deploy(args) => {
            validate_target(&args.target)?;
            post(
                &cli,
                "/v1/map-control/deploy/request",
                json!({
                    "repository_ref": args.target.repo,
                    "app_env": args.target.env,
                    "requested_ref": args.target.ref_name,
                    "source_sha": args.target.sha,
                    "authority_evidence_ref": args.evidence_ref,
                }),
            )
        }
        Command::Status(args) => get(
            &cli,
            "/v1/map-control/deploy/status",
            &[("deployment_ref", args.id.as_str())],
        ),
        Command::Watch(args) => watch(&cli, args),
        Command::Logs(_) => Err(
            "the live control plane exposes no deploy logs route; use `map status` or `map evidence`"
                .to_string(),
        ),
        Command::Evidence(args) => get(
            &cli,
            "/v1/map-control/deploy/evidence",
            &[("deployment_ref", args.id.as_str())],
        ),
        Command::Rollback(args) => post(
            &cli,
            "/v1/map-control/deploy/rollback",
            json!({
                "deployment_ref": args.id,
                "authority_evidence_ref": args.evidence_ref,
            }),
        ),
        Command::Version => print_json_or_text(
            cli.json,
            json!({ "name": "map-cli", "binary": "map", "version": VERSION }),
            VERSION,
        ),
    }
}

fn resolve_state(cli: &Cli) -> Result<LoginState, String> {
    if let (Some(endpoint), Some(token)) = (&cli.endpoint, &cli.token) {
        return Ok(LoginState {
            map_control_endpoint: endpoint.clone(),
            jason_controller_endpoint: None,
            access_token: token.clone(),
            expires_at: None,
            audience: Some("map-control".to_string()),
            scopes: vec![],
            principal: None,
        });
    }
    let path = login_state_path(cli.login_state.as_ref())?;
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "read login state {}: {error}; run `map login`",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn login_state_path(override_path: Option<&PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = override_path {
        return Ok(path.clone());
    }
    if let Ok(path) = env::var("MITHRAN_LOGIN_STATE") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = env::var("AEGIS_LOGIN_STATE") {
        return Ok(PathBuf::from(path));
    }
    let config_home = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map_err(|_| "HOME or XDG_CONFIG_HOME is required".to_string())?;
    Ok(config_home.join("mithran").join("login.json"))
}

fn write_login_state(path: &PathBuf, state: &LoginState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(state).expect("login state serializes");
    fs::write(path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

fn audience_allowed(state: &LoginState, audience: &str) -> bool {
    state.audience.as_deref() == Some(audience)
        || state.scopes.iter().any(|scope| scope == "map:*")
        || state
            .scopes
            .iter()
            .any(|scope| scope == &format!("audience:{audience}"))
}

fn validate_target(target: &DeployTarget) -> Result<(), String> {
    if target.ref_name.is_none() && target.sha.is_none() {
        return Err("deploy target requires --ref-name or --sha".to_string());
    }
    if let Some(sha) = &target.sha {
        if sha.len() != 40 || !sha.chars().all(|char| char.is_ascii_hexdigit()) {
            return Err("--sha must be a 40-character Git SHA".to_string());
        }
    }
    Ok(())
}

fn client(cli: &Cli) -> Result<(Client, LoginState), String> {
    let state = resolve_state(cli)?;
    let client = Client::builder()
        .build()
        .map_err(|error| format!("build HTTP client: {error}"))?;
    Ok((client, state))
}

fn get(cli: &Cli, path: &str, query: &[(&str, &str)]) -> Result<(), String> {
    let (client, state) = client(cli)?;
    let response = client
        .get(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            path
        ))
        .query(query)
        .bearer_auth(&state.access_token)
        .send()
        .map_err(|error| format!("MAP request failed: {error}"))?;
    print_response(cli.json, response)
}

fn post(cli: &Cli, path: &str, body: Value) -> Result<(), String> {
    let (client, state) = client(cli)?;
    let response = client
        .post(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            path
        ))
        .bearer_auth(&state.access_token)
        .json(&body)
        .send()
        .map_err(|error| format!("MAP request failed: {error}"))?;
    print_response(cli.json, response)
}

fn watch(cli: &Cli, args: &WatchArgs) -> Result<(), String> {
    let mut elapsed = 0;
    loop {
        let (client, state) = client(cli)?;
        let response = client
            .get(format!(
                "{}/v1/map-control/deploy/status",
                state.map_control_endpoint.trim_end_matches('/'),
            ))
            .query(&[("deployment_ref", args.id.as_str())])
            .bearer_auth(&state.access_token)
            .send()
            .map_err(|error| format!("MAP watch failed: {error}"))?;
        let value: Value = response
            .json()
            .map_err(|error| format!("read MAP watch response: {error}"))?;
        let phase = value
            .get("deployment")
            .and_then(|deployment| deployment.get("status"))
            .and_then(|status| status.get("status"))
            .and_then(|phase| phase.as_str())
            .unwrap_or("unknown");
        if cli.json {
            println!("{}", serde_json::to_string(&value).unwrap());
        } else {
            println!("{phase}");
        }
        if matches!(phase, "Succeeded" | "Failed" | "Superseded") {
            return Ok(());
        }
        if elapsed >= args.timeout_seconds {
            return Err("watch timed out".to_string());
        }
        thread::sleep(Duration::from_secs(args.interval_seconds));
        elapsed += args.interval_seconds;
    }
}

fn print_response(json_output: bool, response: reqwest::blocking::Response) -> Result<(), String> {
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("read MAP response: {error}"))?;
    if status != StatusCode::OK && status != StatusCode::CREATED && status != StatusCode::ACCEPTED {
        return Err(format!("MAP returned {status}: {}", redact(&text)));
    }
    if json_output {
        println!("{text}");
    } else {
        println!("ok");
    }
    Ok(())
}

fn print_json_or_text(json_output: bool, payload: Value, text: &str) -> Result<(), String> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("{text}");
    }
    Ok(())
}

fn redact(text: &str) -> String {
    let mut redacted = text.to_string();
    for marker in ["access_token", "Authorization", "Bearer"] {
        if redacted.contains(marker) {
            redacted = redacted.replace(marker, "[REDACTED]");
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_git_sha() {
        let target = DeployTarget {
            repo: "mithran-hq/demo".to_string(),
            env: None,
            ref_name: None,
            sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        };
        assert!(validate_target(&target).is_ok());
    }

    #[test]
    fn rejects_missing_ref_and_sha() {
        let target = DeployTarget {
            repo: "mithran-hq/demo".to_string(),
            env: None,
            ref_name: None,
            sha: None,
        };
        assert!(validate_target(&target).is_err());
    }

    #[test]
    fn allows_audience_by_scope() {
        let state = LoginState {
            map_control_endpoint: "https://map.example".to_string(),
            jason_controller_endpoint: None,
            access_token: "secret".to_string(),
            expires_at: None,
            audience: Some("map-control".to_string()),
            scopes: vec!["audience:jason-controller".to_string()],
            principal: None,
        };
        assert!(audience_allowed(&state, "jason-controller"));
    }
}
