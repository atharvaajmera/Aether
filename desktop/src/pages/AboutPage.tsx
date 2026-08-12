import React, { useState, useEffect } from "react";
import { Wifi, Globe, Route, Lock, Gauge, Folder } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";

const InfoRow: React.FC<{ icon: React.ReactNode; title: string; subtitle: string }> = ({ icon, title, subtitle }) => (
  <div className="settings-row" style={{ justifyContent: "flex-start", gap: "12px" }}>
    <div style={{ color: "var(--accent-primary)", flexShrink: 0, marginTop: "2px" }}>{icon}</div>
    <div style={{ display: "flex", flexDirection: "column", gap: "2px" }}>
      <span style={{ color: "var(--text-primary)", fontWeight: 500 }}>{title}</span>
      <span style={{ fontSize: "12px", color: "var(--text-secondary)", lineHeight: 1.4 }}>{subtitle}</span>
    </div>
  </div>
);

const AboutPage: React.FC = () => {
  const [version, setVersion] = useState<string>("…");

  useEffect(() => {
    getVersion().then(setVersion).catch(console.error);
  }, []);

  return (
    <div className="settings-container">
      <h1 className="settings-title">About</h1>

      <div className="settings-card">
        <div className="settings-row" style={{ flexDirection: "column", alignItems: "flex-start", gap: "4px" }}>
          <span style={{ color: "var(--text-primary)", fontWeight: 700, fontSize: "18px" }}>Plenum</span>
          <span style={{ fontSize: "13px", color: "var(--text-secondary)" }}>
            Peer-to-peer file transfer. No account required.
          </span>
        </div>
        <div className="settings-row">
          <span className="settings-label">Version</span>
          <span style={{ fontSize: "13px", color: "var(--text-secondary)" }}>{version}</span>
        </div>
      </div>

      <h3 className="settings-section-title">How it works</h3>
      <div className="settings-card">
        <InfoRow icon={<Wifi size={20} />} title="Local Network" subtitle="Send to nearby devices on the same Wi-Fi. Fastest option." />
        <InfoRow icon={<Globe size={20} />} title="Internet" subtitle="Share a room code to send across different networks." />
      </div>

      <h3 className="settings-section-title">Internet transfers</h3>
      <div className="settings-card">
        <InfoRow icon={<Route size={20} />} title="If a direct link isn't possible" subtitle="On a VPN, mobile data, or a strict network, Plenum routes through a secure relay so the transfer still works." />
        <InfoRow icon={<Lock size={20} />} title="Your files stay private" subtitle="Transfers are encrypted end to end. The relay can't open or read your files." />
        <InfoRow icon={<Gauge size={20} />} title="Slower on relay" subtitle="Relay mode is slower and less ideal for very large files. Use Local Network on the same Wi-Fi when you can." />
      </div>

      <h3 className="settings-section-title">Files & privacy</h3>
      <div className="settings-card">
        <InfoRow icon={<Folder size={20} />} title="Save location" subtitle="Received files go to Downloads on this device." />
        <InfoRow icon={<Lock size={20} />} title="Privacy" subtitle="No account needed. Optional PIN for local receives." />
      </div>
    </div>
  );
};

export default AboutPage;
