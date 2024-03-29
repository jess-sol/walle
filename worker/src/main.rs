use std::{
    collections::BTreeMap,
    fs::{read_dir, read_to_string},
    io,
    ops::Deref,
    process::{Command, ExitCode, ExitStatus},
};

use clap::{Parser, Subcommand};
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use snafu::{Backtrace, ResultExt, Snafu};
use tokio::runtime::Builder;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{prelude::*, EnvFilter};

use crate::retention::RetentionSpec;

mod retention;

type Result<T, E = Error> = ::std::result::Result<T, E>;

#[snafu::report]
fn main() -> Result<()> {
    let mut fmt_layer = tracing_subscriber::fmt::layer();
    fmt_layer.set_ansi(false);
    tracing_subscriber::registry().with(fmt_layer).with(EnvFilter::from_default_env()).init();

    let args = Cli::parse();

    let namespace = args.namespace.unwrap_or(
        read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
            .with_context(|_| ConfigIoSnafu { msg: "No namespace argument provided, and unable to read namespace from default Pod path" })?
    );

    let restic_path = args.restic_path.unwrap_or("/restic".to_string());

    match args.command {
        Commands::Backup { source_path, snapshot_time, pvc_name } => do_backup(
            restic_path,
            args.k8s_cluster_name,
            args.operator_namespace,
            namespace,
            args.repository_secret,
            source_path,
            snapshot_time,
            pvc_name,
        ),
        Commands::Restore { snapshot } => do_restore(snapshot),
        Commands::Check {} => {
            do_check(restic_path, args.operator_namespace, args.repository_secret)
        }
        Commands::Prune { retain } => {
            do_prune(restic_path, args.operator_namespace, args.repository_secret, retain)
        }
    }
}

#[derive(Clone, Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// The namespace/name of the secret containing the environment of the repository to backup to.
    /// It's done this way, instead of using envFrom, so it's possible to reference a secret in a
    /// separate namespace (using the pod's attached service account).
    #[arg(env)]
    repository_secret: String,

    #[arg(env)]
    k8s_cluster_name: String,

    #[arg(long, env)]
    trace_id: Option<String>,

    #[arg(long, env)]
    namespace: Option<String>,

    #[arg(long, env)]
    operator_namespace: Option<String>,

    #[arg(long, env)]
    restic_path: Option<String>,
}

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(context(false))]
    BackupError {
        source: BackupError,
    },

    ConfigIoError {
        source: io::Error,
        msg: String,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum Commands {
    Backup {
        #[arg(env)]
        source_path: String,
        #[arg(env)]
        snapshot_time: String,
        #[arg(env)]
        pvc_name: String,
    },
    Restore {
        #[arg(long, env)]
        snapshot: String,
    },
    Check {},
    Prune {
        #[arg(long, env)]
        retain: String,
    },
}

fn do_backup(
    restic_path: String, k8s_cluster_name: String, operator_namespace: Option<String>,
    namespace: String, repository_secret: String, source_path: String, snapshot_time: String,
    pvc_name: String,
) -> Result<()> {
    info!("Starting restic, output being redirected...");

    let runtime = Builder::new_current_thread().enable_all().build().unwrap();

    let repo_env_vars = runtime.block_on(async {
        let client = kube::Client::try_default().await.expect("Unable to create Kubernetes client");
        get_repository_secret(client, operator_namespace, repository_secret).await
    });

    println!("<<<<<<<<<< START OUTPUT");
    info!(
        "Backing up to {}, on host {}, with tags {},{}, at time {}",
        repo_env_vars.get("RESTIC_REPOSITORY").map_or("<unset repository>", String::deref),
        k8s_cluster_name,
        pvc_name,
        namespace,
        snapshot_time
    );
    debug!("Starting restic {}", restic_path);

    // Check if source directory is empty
    if read_dir(&source_path).map(|x| x.count()).unwrap_or(0) == 0 {
        warn!("Snapshot of PVC is empty, skipping backup.");
        return Ok(());
    }

    if repo_env_vars.get("INITIALIZE_REPO").map(|x| is_truthy(x.as_str())).unwrap_or(false) {
        info!("Ensuring repository is initialized");
        initialize_repository(&restic_path, &repo_env_vars)?;
    }

    let mut process = Command::new(restic_path)
        .args([
            "backup",
            "--group-by",
            "host,tags",
            "--host",
            &k8s_cluster_name,
            "--time",
            &snapshot_time,
            "--tag",
            &format!("{pvc_name},{namespace}"),
            ".",
        ])
        .envs(repo_env_vars)
        .current_dir(source_path)
        .spawn()
        .with_context(|_| ProcessSpawnSnafu)?;

    // Even though communication pipes have closed, ensure process has also exited.
    let exit_status = process.wait().with_context(|_| ProcessIOSnafu)?;
    println!("END OUTPUT >>>>>>>>>>");

    if !exit_status.success() {
        let exit = ResticExitCodes::new(exit_status);
        error!("Restic backup failed with exit code {}: {}", exit.code(), exit.reason());
        BackupFailedSnafu { exit_code: exit.exit_code() }.fail()?;
    }

    Ok(())
}

fn do_check(
    restic_path: String, operator_namespace: Option<String>, repository_secret: String,
) -> Result<()> {
    info!("Starting restic, output being redirected...");

    let runtime = Builder::new_current_thread().enable_all().build().unwrap();

    let repo_env_vars = runtime.block_on(async {
        let client = kube::Client::try_default().await.expect("Unable to create Kubernetes client");
        get_repository_secret(client, operator_namespace, repository_secret).await
    });

    println!("<<<<<<<<<< START OUTPUT");
    info!(
        "Checking repository {}",
        repo_env_vars.get("RESTIC_REPOSITORY").map_or("<unset repository>", String::deref),
    );
    debug!("Starting restic {}", restic_path);

    if repo_env_vars.get("INITIALIZE_REPO").map(|x| is_truthy(x.as_str())).unwrap_or(false) {
        info!("Ensuring repository is initialized");
        initialize_repository(&restic_path, &repo_env_vars)?;
    }

    let mut process = Command::new(restic_path)
        .args(["check", "--retry-lock", "2h"])
        .envs(repo_env_vars)
        .spawn()
        .with_context(|_| ProcessSpawnSnafu)?;

    // Even though communication pipes have closed, ensure process has also exited.
    let exit_status = process.wait().with_context(|_| ProcessIOSnafu)?;
    println!("END OUTPUT >>>>>>>>>>");

    if !exit_status.success() {
        let exit = ResticExitCodes::new(exit_status);
        error!("Restic check failed with exit code {}: {}", exit.code(), exit.reason());
        BackupFailedSnafu { exit_code: exit.exit_code() }.fail()?;
    }

    Ok(())
}

fn do_prune(
    restic_path: String, operator_namespace: Option<String>, repository_secret: String,
    retain: String,
) -> Result<()> {
    info!("Starting restic, output being redirected...");

    let runtime = Builder::new_current_thread().enable_all().build().unwrap();

    let repo_env_vars = runtime.block_on(async {
        let client = kube::Client::try_default().await.expect("Unable to create Kubernetes client");
        get_repository_secret(client, operator_namespace, repository_secret).await
    });

    println!("<<<<<<<<<< START OUTPUT");
    info!(
        "Purging repository {}",
        repo_env_vars.get("RESTIC_REPOSITORY").map_or("<unset repository>", String::deref),
    );
    debug!("Starting restic {}", restic_path);

    if repo_env_vars.get("INITIALIZE_REPO").map(|x| is_truthy(x.as_str())).unwrap_or(false) {
        info!("Ensuring repository is initialized");
        initialize_repository(&restic_path, &repo_env_vars)?;
    }

    let retention: RetentionSpec = retain.parse().expect("Failed to parse RetentionSpec");
    let mut args: Vec<_> =
        vec!["forget", "--retry-lock", "2h", "--keep-tag", "forever", "--group-by", "host,tags"]
            .into_iter()
            .map(String::from)
            .collect();
    args.append(&mut retention.as_args());

    info!("Forgetting old snapshots");
    let mut process = Command::new(&restic_path)
        .args(args)
        .envs(&repo_env_vars)
        .spawn()
        .with_context(|_| ProcessSpawnSnafu)?;

    let exit_status = process.wait().with_context(|_| ProcessIOSnafu)?;
    if !exit_status.success() {
        let exit = ResticExitCodes::new(exit_status);
        error!("Restic forget failed with exit code {}: {}", exit.code(), exit.reason());
        BackupFailedSnafu { exit_code: exit.exit_code() }.fail()?;
    }

    info!("Pruning after forgetting snapshots");
    let mut process = Command::new(&restic_path)
        .args(["prune", "--retry-lock", "2h"])
        .envs(repo_env_vars)
        .spawn()
        .with_context(|_| ProcessSpawnSnafu)?;

    let exit_status = process.wait().with_context(|_| ProcessIOSnafu)?;
    if !exit_status.success() {
        let exit = ResticExitCodes::new(exit_status);
        error!("Restic prune failed with exit code {}: {}", exit.code(), exit.reason());
        BackupFailedSnafu { exit_code: exit.exit_code() }.fail()?;
    }
    println!("END OUTPUT >>>>>>>>>>");

    Ok(())
}

fn do_restore(snapshot: String) -> Result<()> {
    Ok(())
}

struct ResticExitCodes {
    exit_code: Option<i32>,
}

impl ResticExitCodes {
    pub fn new(status: ExitStatus) -> Self {
        Self { exit_code: status.code() }
    }
    pub fn exit_code(&self) -> Option<ExitCode> {
        self.exit_code.map(|x| ExitCode::from(x as u8))
    }
    pub fn code(&self) -> String {
        match self.exit_code {
            Some(x) => x.to_string(),
            None => "<unknown>".to_string(),
        }
    }
    pub fn reason(&self) -> String {
        match self.exit_code {
            Some(0) => "command was successful".to_string(),
            Some(1) => "there was a fatal error (no snapshot created)".to_string(),
            Some(3) => "source data could not be read (incomplete snapshot created)".to_string(),
            Some(ref x) => format!("command exited with unrecongized exit code {}", x),
            None => "command did not return an exit code (somehow?)".to_string(),
        }
    }
}

fn initialize_repository(
    restic_path: &str, repo_env_vars: &BTreeMap<String, String>,
) -> Result<()> {
    let result = Command::new(restic_path)
        .args(["list", "snapshots"])
        .envs(repo_env_vars)
        .output()
        .with_context(|_| ProcessSpawnSnafu)?;

    let stdout = String::from_utf8_lossy(result.stdout.as_slice()).to_string();
    let stderr = String::from_utf8_lossy(result.stderr.as_slice()).to_string();

    debug!(stderr, stdout, result=?result.status, "test output");

    if result.status.code() == Some(1)
        && String::from_utf8_lossy(result.stderr.as_slice())
            .contains("Is there a repository at the following location?")
    {
        info!("Initializing repository...");
        let mut process = Command::new(restic_path)
            .args(["init"])
            .envs(repo_env_vars)
            .spawn()
            .with_context(|_| ProcessSpawnSnafu)?;

        let result = process.wait().context(ProcessIOSnafu)?;
        if result.code() != Some(0) {
            InitializeRepoSnafu.fail()?;
        }
    }

    Ok(())
}

async fn get_repository_secret(
    client: kube::Client, operator_namespace: Option<String>, resource_secret: String,
) -> BTreeMap<String, String> {
    let mut split = resource_secret.split("/");

    let (ns, name) = match (split.next(), split.next()) {
        (Some(ns), Some(name)) => (Some(ns), name),
        (Some(name), None) => (None, name),
        _ => panic!("Invalid resource_secret provided"),
    };

    let api: Api<Secret> = match (ns, operator_namespace.as_deref()) {
        (Some(ns), _) | (None, Some(ns)) => Api::namespaced(client, ns),
        _ => Api::default_namespaced(client),
    };

    let secret = api.get(name).await.unwrap();

    decode_secret(&secret)
}

fn decode_secret(secret: &Secret) -> BTreeMap<String, String> {
    let mut res = BTreeMap::new();

    if let Some(data) = secret.data.clone() {
        for (k, v) in data {
            if let Ok(d) = std::str::from_utf8(&v.0) {
                res.insert(k, d.to_string());
            }
        }
    }

    res
}

#[derive(Debug, Snafu)]
enum BackupError {
    #[snafu(display("Failed to spawn restic subprocess: {source}\n{backtrace}"))]
    ProcessSpawnError {
        source: io::Error,
        #[snafu(backtrace)]
        backtrace: Backtrace,
    },
    #[snafu(display("Failed to process IO of restic subprocess: {source}\n{backtrace}"))]
    ProcessIOError {
        source: io::Error,
        #[snafu(backtrace)]
        backtrace: Backtrace,
    },
    #[snafu(display("Failed to initialize repository"))]
    InitializeRepoError,
    #[snafu(display("restic failed with exit code: {exit_code:?}"))]
    BackupFailed { exit_code: Option<ExitCode> },
}

fn is_truthy(value: &str) -> bool {
    match value.to_lowercase().as_str() {
        "yes" | "true" | "1" => true,
        "no" | "false" | "0" => false,
        _ => false,
    }
}
