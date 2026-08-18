use std::path::PathBuf;

use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use plenum::app::{
    BenchmarkEvent, BenchmarkRequest, ConnectionState, CorePermissions, DiscoverRequest,
    DiscoveryEvent, LogLevel, PlenumCore, PlenumEvent, ReceiveRequest, SendRequest, TransferEvent,
    TransferOptions,
};

#[derive(Parser)]
#[command(author, version, about = "Plenum peer-to-peer file transfer engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Send a file to a peer
    Send {
        /// Path to the file to send
        file_path: PathBuf,
        /// Address of the peer (e.g., 127.0.0.1:8080). If omitted, uses local discovery.
        address: Option<String>,
        /// Pairing token to match a specific receiver during discovery.
        #[arg(short, long)]
        token: Option<String>,
    },
    /// Receive a file from a peer
    Receive {
        /// Port to listen on
        port: u16,
        /// Directory to save the received file
        #[arg(short, long, default_value = ".")]
        output_dir: PathBuf,
        /// Disable local network discovery broadcast
        #[arg(long, default_value_t = false)]
        no_discover: bool,
    },
    /// Discover peers on the local network
    Discover {
        /// Pairing token to filter announcements
        #[arg(short, long)]
        token: Option<String>,
        /// How long to listen for announcements (in seconds)
        #[arg(short = 's', long, default_value_t = 10)]
        timeout: u64,
    },
    /// Benchmark the transfer engine without disk or network setup
    Benchmark {
        /// Payload size in MiB
        #[arg(long, default_value_t = 64)]
        size_mb: usize,
        /// Number of benchmark iterations
        #[arg(long, default_value_t = 3)]
        iterations: usize,
        /// Simulated one-way latency in transport ticks
        #[arg(long, default_value_t = 1)]
        latency_ticks: u64,
    },
}

struct CliEventSink {
    progress: Option<ProgressBar>,
}

impl CliEventSink {
    fn new() -> Self {
        Self { progress: None }
    }

    fn println(&self, message: impl AsRef<str>) {
        if let Some(pb) = &self.progress {
            pb.println(message.as_ref());
        } else {
            println!("{}", message.as_ref());
        }
    }
}

impl plenum::app::EventSink for CliEventSink {
    fn emit(&mut self, event: PlenumEvent) {
        match event {
            PlenumEvent::Log { level, message } => match level {
                LogLevel::Error => eprintln!("{message}"),
                _ => self.println(message),
            },
            PlenumEvent::Discovery(event) => match event {
                DiscoveryEvent::SearchStarted {
                    token,
                    timeout_secs,
                } => {
                    if let Some(token) = token {
                        self.println(format!(
                            "Scanning local network for Plenum peers ({}s timeout, token={})...",
                            timeout_secs, token
                        ));
                    } else {
                        self.println(format!(
                            "Scanning local network for Plenum peers ({}s timeout)...",
                            timeout_secs
                        ));
                    }
                }
                DiscoveryEvent::BroadcastStarted { token, port } => {
                    self.println(format!(
                        "Broadcasting on local network with pairing token {} on port {}",
                        token, port
                    ));
                }
                DiscoveryEvent::PeerFound(summary) => {
                    self.println(format!(
                        "Found peer '{}' at {} (token: {})",
                        summary.hostname, summary.address, summary.token
                    ));
                }
                DiscoveryEvent::PeerNotFound => {
                    self.println("No peers found on the local network.");
                }
            },
            PlenumEvent::Transfer(event) => match event {
                TransferEvent::StateChanged { state, peer, .. } => match state {
                    ConnectionState::Discovering => self.println("Discovering peer..."),
                    ConnectionState::Listening => {
                        if let Some(peer) = peer {
                            self.println(format!("Listening on {peer}..."));
                        }
                    }
                    ConnectionState::Connecting => {
                        if let Some(peer) = peer {
                            self.println(format!("Connecting to {peer}..."));
                        }
                    }
                    ConnectionState::SignalingConnected => {
                        if let Some(peer) = peer {
                            self.println(format!(
                                "Connected to relay server, joining room {peer}..."
                            ));
                        }
                    }
                    ConnectionState::NegotiatingIce => {
                        self.println("Negotiating WebRTC connection...");
                    }
                    ConnectionState::Connected => {
                        if let Some(peer) = peer {
                            self.println(format!("Connected to {peer}."));
                        }
                    }
                    ConnectionState::Closed => {
                        if let Some(pb) = self.progress.take() {
                            pb.finish_and_clear();
                        }
                    }
                },
                TransferEvent::Started {
                    direction,
                    file_name,
                    total_bytes,
                    resumed_bytes,
                } => {
                    self.println(format!(
                        "{:?} '{}' ({} bytes)",
                        direction, file_name, total_bytes
                    ));
                    let pb = ProgressBar::new(total_bytes);
                    pb.set_style(
                        ProgressStyle::with_template(
                            "[{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                        )
                        .unwrap()
                        .progress_chars("#>-"),
                    );
                    pb.set_position(resumed_bytes.min(total_bytes));
                    self.progress = Some(pb);
                }
                TransferEvent::Resumed {
                    next_sequence,
                    resumed_bytes,
                    ..
                } => {
                    self.println(format!(
                        "Resuming transfer from sequence {} ({} bytes)",
                        next_sequence, resumed_bytes
                    ));
                }
                TransferEvent::Progress {
                    transferred_bytes,
                    total_bytes,
                    ..
                } => {
                    if let Some(pb) = &self.progress {
                        pb.set_position(transferred_bytes.min(total_bytes));
                    }
                }
                TransferEvent::CheckpointUpdated {
                    checkpoint_path,
                    next_sequence,
                    bytes_written,
                } => {
                    self.println(format!(
                        "Checkpoint saved: {} (next seq {}, {} bytes)",
                        checkpoint_path.display(),
                        next_sequence,
                        bytes_written
                    ));
                }
                TransferEvent::Completed(summary) => {
                    if let Some(pb) = self.progress.take() {
                        pb.set_position(summary.total_bytes);
                        pb.finish_with_message("Transfer complete");
                    } else {
                        self.println("Transfer complete");
                    }
                    self.println(format!(
                        "Completed {:?} of '{}' in {} ms",
                        summary.direction, summary.file_name, summary.elapsed_ms
                    ));
                }
                TransferEvent::IncomingRequest {
                    file_name,
                    total_bytes,
                    peer,
                    ..
                } => {
                    self.println(format!(
                        "Incoming transfer '{}' ({} bytes) from {}",
                        file_name,
                        total_bytes,
                        peer.unwrap_or_else(|| "unknown peer".to_string())
                    ));
                }
                TransferEvent::AwaitingApproval { .. } => {
                    self.println("Waiting for the receiver to accept the transfer...");
                }
                TransferEvent::ConnectionEstablished { mode, .. } => {
                    self.println(format!("Connection established ({mode:?})"));
                }
                TransferEvent::Cancelled { .. } => {
                    if let Some(pb) = self.progress.take() {
                        pb.finish_and_clear();
                    }
                    self.println("Transfer cancelled.");
                }
                TransferEvent::Declined { reason, .. } => {
                    if let Some(pb) = self.progress.take() {
                        pb.finish_and_clear();
                    }
                    self.println(format!("Transfer refused by peer ({reason})."));
                }
                TransferEvent::Failed { message, .. } => {
                    if let Some(pb) = self.progress.take() {
                        pb.finish_and_clear();
                    }
                    self.println(message);
                }
            },
            PlenumEvent::Benchmark(event) => match event {
                BenchmarkEvent::Started {
                    size_mb,
                    iterations,
                    latency_ticks,
                } => {
                    println!(
                        "Benchmarking {} MiB transfer for {} iteration(s) with latency_ticks={}...",
                        size_mb, iterations, latency_ticks
                    );
                }
                BenchmarkEvent::IterationCompleted(iteration) => {
                    println!(
                        "Iteration {}: {:.2} MiB/s, peak sender buffer {} KiB, peak receiver buffer {} KiB",
                        iteration.iteration,
                        iteration.throughput_mib_s,
                        iteration.peak_sender_buffered_bytes / 1024,
                        iteration.peak_receiver_buffered_bytes / 1024,
                    );
                }
                BenchmarkEvent::Completed(summary) => {
                    println!(
                        "Average throughput: {:.2} MiB/s",
                        summary.average_throughput_mib_s
                    );
                }
            },
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut core = PlenumCore::new();
    let mut sink = CliEventSink::new();
    let permissions = CorePermissions::desktop_defaults();
    let options = TransferOptions::default();

    match cli.command {
        Commands::Send {
            file_path,
            address,
            token,
        } => {
            core.send_file(
                SendRequest {
                    file_path,
                    address,
                    discovery_token: token,
                    device_name: None,
                    permissions,
                    options,
                },
                &mut sink,
            )?;
        }
        Commands::Receive {
            port,
            output_dir,
            no_discover,
        } => {
            core.receive_file(
                ReceiveRequest {
                    port,
                    output_dir,
                    announce_on_lan: !no_discover,
                    device_name: None,
                    require_pin: false,
                    auto_accept: true,
                    permissions,
                    options,
                },
                &mut sink,
            )?;
        }
        Commands::Discover { token, timeout } => {
            let _ = core.discover_peer(
                DiscoverRequest {
                    token,
                    timeout_secs: timeout,
                    permissions,
                },
                &mut sink,
            );
        }
        Commands::Benchmark {
            size_mb,
            iterations,
            latency_ticks,
        } => {
            core.benchmark(
                BenchmarkRequest {
                    size_mb,
                    iterations,
                    latency_ticks,
                    options,
                },
                &mut sink,
            )?;
        }
    }

    Ok(())
}
