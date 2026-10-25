//! Estado que se muestra en la interfaz (serializable a JSON).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Overall {
    NoFolder,
    NoGroup,
    Alone,
    Synced,
    Syncing,
    Conflict,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceState {
    Synced,
    Syncing,
    Receiving,
    Connecting,
    Offline,
    Conflict,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub state: DeviceState,
    pub detail: String,
    pub pending_files: u32,
    pub pending_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub path: String,
    pub peer: String,
    pub download: bool,
    pub size: u64,
    pub done: u64,
    pub rate: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventView {
    pub time_ms: i64,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorView {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupView {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingView {
    pub code: String,
    pub expires_in_secs: u64,
    pub locked: bool,
}

/// Grupo cercano que está aceptando equipos (para la pantalla "Unirse").
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenGroupView {
    pub group_id: String,
    pub group_name: String,
    pub device_id: String,
    pub device_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    pub device_name: String,
    pub device_id: String,
    pub folder: Option<String>,
    pub folder_error: Option<String>,
    pub group: Option<GroupView>,
    pub pairing: Option<PairingView>,
    pub overall: Overall,
    pub devices: Vec<DeviceView>,
    pub transfers: Vec<TransferView>,
    pub pending_files: u32,
    pub pending_bytes: u64,
    pub down_rate: f64,
    pub up_rate: f64,
    pub file_count: usize,
    pub total_bytes: u64,
    pub conflicts: u32,
    pub errors: Vec<ErrorView>,
    pub events: Vec<EventView>,
    pub open_groups: Vec<OpenGroupView>,
    pub network_warning: Option<String>,
}
