use std::time::Duration;

use chrono::{DateTime, Utc};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector, Time};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::SecretRef;

pub static BACKUP_SCHEDULE_FINALIZER: &str = "ros.io/backup-schedule";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
// #[cfg_attr(test, derive(Default))]
#[kube(kind = "BackupSchedule", group = "ros.io", version = "v1")]
#[kube(status = "BackupScheduleStatus", shortname = "backup-schedule")]
// #[kube(
//     printcolumn = r#"{"name":"State", "type":"string", "description":"Status of BackupJob", "jsonPath":".status.state"}"#
// )]
#[kube(printcolumn = r#"{"name":"LastBackup", "type":"date", "jsonPath":".status.lastBackupRun"}"#)]
#[kube(
    printcolumn = r#"{"name":"Status", "type":"string", "description":"Status of BackupJob", "jsonPath":".status.state"}"#
)]
#[kube(printcolumn = r#"{"name":"Age", "type":"date", "jsonPath":".metadata.creationTimestamp"}"#)]
#[serde(rename_all = "camelCase")]
pub struct BackupScheduleSpec {
    /// Reference to the secret containing the necessary environment variables to connect to the
    /// Restic repository.
    /// See https://volsync.readthedocs.io/en/stable/usage/restic/index.html for details.
    pub repository: SecretRef,

    pub interval: Option<IntervalSpec>,

    #[serde(default = "default_failed")]
    pub keep_failed: usize,

    #[serde(default = "default_succeeded")]
    pub keep_succeeded: usize,

    pub prune: Option<PruneJobSpec>,
    pub check: Option<CheckJobSpec>,

    /// List of backup plans to run on schedule. The first to match a workload or PVC will be used,
    /// overriding any following plans.
    pub plans: Vec<BackupPlanSpec>,
}

fn default_failed() -> usize {
    3
}
fn default_succeeded() -> usize {
    1
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PruneJobSpec {
    pub interval: IntervalSpec,
    pub retain: RetentionSpec,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckJobSpec {
    pub interval: IntervalSpec,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupPlanSpec {
    /// Type of resource to select, may be any resource with Pod child resources
    #[serde(rename = "type")]
    pub type_: String,

    /// See https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/#label-selectors
    pub label_selector: Option<LabelSelector>,

    /// See https://kubernetes.io/docs/concepts/overview/working-with-objects/field-selectors
    pub field_selector: Option<Vec<FieldSelector>>,

    // TODO - Namespace selector
    /// Any workload resources selected by `selector` will then have their PVCs filtered using this
    /// selector.
    // pub pvc_selector: Vec<Selector>,

    /// Run in the pod a PVC is mounted to before a snapshot is taken of the PVC
    pub before_snapshot: Option<String>,

    /// Run in the pod a PVC is mounted to after a snapshot is taken of the PVC
    pub after_snapshot: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldSelector {
    pub field: String,
    pub operator: FieldOperator,
    pub value: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FieldOperator {
    Equals,
    DoesNotEqual,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetentionSpec {
    pub hourly: Option<u32>,
    pub daily: Option<u32>,
    pub weekly: Option<u32>,
    pub monthly: Option<u32>,
    pub yearly: Option<u32>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackupScheduleStatus {
    pub state: BackupScheduleState,

    /// These reflect the last actual start of a job
    pub last_backup_run: Option<Time>,
    pub last_check_run: Option<String>,
    pub last_prune_run: Option<String>,

    pub conditions: Vec<Condition>,

    /// Contains the timestamps of the last attempted scheduling by the job scheduler. These do not
    /// reflect the last successful run of a job, as they'll also be updated when a job is skipped.
    pub scheduler: SchedulerAttemptTimestamps,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerAttemptTimestamps {
    pub backup_timestamp: Option<Time>,
    pub check_timestamp: Option<Time>,
    pub prune_timestamp: Option<Time>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
pub enum BackupScheduleState {
    #[default]
    Waiting,
    Running,
    Finished,
    FinishedWithFailures,
    CheckFailed,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct IntervalSpec(pub String);

impl IntervalSpec {
    pub fn passed_interval(&self, last_run: &DateTime<Utc>) -> bool {
        Utc::now() > *last_run + self.as_duration()
    }

    // TODO - Error handling
    pub fn as_duration(&self) -> Duration {
        let mut duration = Duration::new(0, 0);
        let mut buffer = String::with_capacity(5);
        for char in self.0.chars() {
            if char.is_ascii_digit() {
                buffer.push(char);
            } else {
                let digits: u64 = buffer.parse().expect("Unable to parse interval value");
                match char {
                    's' => duration += Duration::from_secs(digits),
                    'm' => duration += Duration::from_secs(digits * 60),
                    'h' => duration += Duration::from_secs(digits * 60 * 60),
                    'd' => duration += Duration::from_secs(digits * 60 * 60 * 24),
                    'w' => duration += Duration::from_secs(digits * 60 * 60 * 24 * 7),
                    _ => panic!("Unable to parse interval"),
                }
            }
        }

        duration
    }
}
