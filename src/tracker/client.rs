use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy};

use super::{
    HistoryEntry, RestoreReport, SERVICE_NAME, SnapshotDetail, SnapshotSummary, TRACKER_INTERFACE,
    TRACKER_PATH, TrackedWindow, TrackerStatus,
};

pub struct TrackerClient {
    connection: Connection,
}

impl TrackerClient {
    pub fn connect() -> Result<Self, String> {
        zbus::blocking::connection::Builder::session()
            .map_err(|err| err.to_string())?
            // Restore can legitimately launch several applications and wait for
            // their desktop helpers. The daemon also claims operations so a
            // retry cannot duplicate an in-flight restore.
            .method_timeout(Duration::from_secs(30))
            .build()
            .map(|connection| Self { connection })
            .map_err(|err| err.to_string())
    }

    fn call(
        &self,
        method: &str,
        body: &(impl serde::ser::Serialize + zbus::zvariant::DynamicType),
    ) -> Result<String, String> {
        Proxy::new(
            &self.connection,
            SERVICE_NAME,
            TRACKER_PATH,
            TRACKER_INTERFACE,
        )
        .map_err(|err| err.to_string())?
        .call(method, body)
        .map_err(|err| err.to_string())
    }

    pub fn status(&self) -> Result<TrackerStatus, String> {
        serde_json::from_str(&self.call("GetStatus", &())?).map_err(|err| err.to_string())
    }
    pub fn windows(&self) -> Result<Vec<TrackedWindow>, String> {
        serde_json::from_str(&self.call("GetWindows", &())?).map_err(|err| err.to_string())
    }
    pub fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, String> {
        decode_result(&self.call("GetHistory", &(limit,))?)
    }
    pub fn snapshots(&self) -> Result<Vec<SnapshotSummary>, String> {
        decode_result(&self.call("GetSnapshots", &())?)
    }
    pub fn snapshot(&self, id: i64) -> Result<Option<SnapshotDetail>, String> {
        decode_result(&self.call("GetSnapshot", &(id,))?)
    }
    pub fn create_snapshot(&self, name: &str) -> Result<i64, String> {
        decode_result(&self.call("CreateSnapshot", &(name,))?)
    }
    pub fn delete_snapshot(&self, id: i64) -> Result<(), String> {
        decode_result(&self.call("DeleteSnapshot", &(id,))?)
    }
    pub fn restore_snapshot(&self, id: i64) -> Result<RestoreReport, String> {
        decode_result(&self.call("RestoreSnapshot", &(id,))?)
    }
    pub fn restore_report(&self, operation_id: &str) -> Result<Option<RestoreReport>, String> {
        serde_json::from_str(&self.call("GetRestoreReport", &(operation_id,))?)
            .map_err(|err| err.to_string())
    }
    pub fn wait_for_restore_report(
        &self,
        mut report: RestoreReport,
    ) -> Result<RestoreReport, String> {
        let Some(operation_id) = report.operation_id.clone() else {
            return Ok(report);
        };
        let deadline = Instant::now() + Duration::from_secs(25);
        while report.in_progress && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            report = self.restore_report(&operation_id)?.ok_or_else(|| {
                format!("Restore operation {operation_id} is no longer available")
            })?;
        }
        Ok(report)
    }
    pub fn restore_recovery(&self) -> Result<RestoreReport, String> {
        decode_result(&self.call("RestoreRecovery", &())?)
    }
    pub fn reopen_history(&self, id: i64) -> Result<RestoreReport, String> {
        decode_result(&self.call("ReopenHistory", &(id,))?)
    }
    pub fn reopen_latest_history(&self) -> Result<RestoreReport, String> {
        decode_result(&self.call("ReopenLatestHistory", &())?)
    }
    pub fn dismiss_recovery(&self) -> Result<(), String> {
        self.call("DismissRecovery", &()).map(|_| ())
    }
    pub fn clear_history(&self) -> Result<(), String> {
        decode_result(&self.call("ClearHistory", &())?)
    }
    pub fn set_auto_enter(&self, enabled: bool) -> Result<(), String> {
        decode_result(&self.call("SetAutoEnter", &(enabled,))?)
    }
}

fn decode_result<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, String> {
    serde_json::from_str::<Result<T, String>>(json).map_err(|err| err.to_string())?
}
