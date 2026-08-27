export interface CorePermissions {
    local_network: boolean;
    file_system_read: boolean;
    file_system_write: boolean;
    background_transfer: boolean;
}

export interface TransferOptions {
    chunk_size: number;
    window_size: number;
    timeout_ticks: number;
}

export interface DiscoverRequest {
    token?: string;
    timeout_secs: number;
    permissions: CorePermissions;
}

export interface SendRequest {
    file_path: string;
    address?: string;
    discovery_token?: string;
    // Omitted -> backend fills in whoami::devicename()
    device_name?: string;
    permissions: CorePermissions;
    options: TransferOptions;
}

export interface ReceiveRequest {
    port: number;
    output_dir: string;
    announce_on_lan: boolean;
    device_name?: string;
    require_pin?: boolean;
    auto_accept?: boolean;
    permissions: CorePermissions;
    options: TransferOptions;
}

export interface IceServer {
    urls: string[];
    username?: string;
    credential?: string;
}

export interface SendRemoteRequest {
    file_path: string;
    relay_server_url: string;
    session_id: string;
    my_peer_id: string;
    ice_servers: IceServer[];
    connect_timeout_secs: number;
    device_name?: string;
    permissions: CorePermissions;
    options: TransferOptions;
}

export interface ReceiveRemoteRequest {
    output_dir: string;
    relay_server_url: string;
    session_id: string;
    my_peer_id: string;
    ice_servers: IceServer[];
    connect_timeout_secs: number;
    auto_accept?: boolean;
    device_name?: string;
    permissions: CorePermissions;
    options: TransferOptions;
}

export interface DiscoverySummary {
    hostname: string;
    address: string;
    token: string;
    pin_required: boolean;
}

export type ConnectionState = "Discovering" | "Listening" | "Connecting" | "SignalingConnected" | "NegotiatingIce" | "Connected" | "Closed";
export type TransferDirection = "Send" | "Receive";
// How the peers are connected: LAN TCP, direct P2P WebRTC, or TURN relay.
export type TransferMode = "Lan" | "Direct" | "Relay";

export type TransferUiPhase =
    | "idle"
    | "discovering"
    | "listening"
    | "connecting"
    | "awaitingApproval"
    | "transferring"
    | "cancelling"
    | "cancelled"
    | "failed"
    | "succeeded";

export interface TransferSummary {
    direction: TransferDirection;
    file_name: string;
    peer?: string;
    // Peer's device name when its build is new enough to send one.
    peer_name?: string;
    mode: TransferMode;
    total_bytes: number;
    transferred_bytes: number;
    resumed_bytes: number;
    elapsed_ms: number;
}

// Struct variants mapped to standard objects inside the enum
export type TransferEvent =
    | { StateChanged: { direction: TransferDirection, state: ConnectionState, peer?: string } }
    | { IncomingRequest: { direction: TransferDirection, file_name: string, total_bytes: number, peer?: string, sender_name?: string } }
    | { ConnectionEstablished: { direction: TransferDirection, mode: TransferMode } }
    | { AwaitingApproval: { direction: TransferDirection, file_name: string } }
    | { Cancelled: { direction: TransferDirection } }
    | { Declined: { direction: TransferDirection, reason: string } }
    | { Failed: { direction: TransferDirection, message: string, recoverable: boolean } }
    | { Started: { direction: TransferDirection, file_name: string, total_bytes: number, resumed_bytes: number } }
    | { Resumed: { direction: TransferDirection, next_sequence: number, resumed_bytes: number } }
    | { Progress: { direction: TransferDirection, transferred_bytes: number, total_bytes: number } }
    | { CheckpointUpdated: { checkpoint_path: string, next_sequence: number, bytes_written: number } }
    | { Completed: TransferSummary };

export type DiscoveryEvent =
    | { SearchStarted: { token?: string, timeout_secs: number } }
    | { BroadcastStarted: { token: string, port: number } }
    | { PeerFound: DiscoverySummary }
    | "PeerNotFound";

export type PlenumEvent =
    | { Log: { level: string, message: string } }
    | { Transfer: TransferEvent }
    | { Discovery: DiscoveryEvent };

// Every plenum-event the desktop backend emits is wrapped with the id of the
// backend session that produced it
export interface PlenumEventEnvelope {
    session_id: number;
    event: PlenumEvent;
}
