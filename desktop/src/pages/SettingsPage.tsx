import React, { useState, useEffect, useRef } from "react";
import { useNavigate } from "react-router-dom";
import { Pencil, ChevronRight } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useSettings } from "../context/SettingsContext";

const SettingsPage: React.FC = () => {
  const { settings, updateSettings } = useSettings();
  const navigate = useNavigate();

  const themes = ["System", "Dark", "Light"];
  const colors = ["Plenum", "Ocean", "Forest"];

  const [hostname, setHostname] = useState<string>("");
  const [renameOpen, setRenameOpen] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    invoke<string>("get_device_name").then(setHostname).catch(console.error);
  }, []);

  useEffect(() => {
    if (renameOpen) {
      setTimeout(() => inputRef.current?.focus(), 50);
    }
  }, [renameOpen]);

  const openRename = () => {
    setNameDraft(settings.deviceName);
    setRenameOpen(true);
  };

  const saveRename = () => {
    updateSettings({ deviceName: nameDraft.trim() });
    setRenameOpen(false);
  };

  const Toggle = ({ on, onClick }: { on: boolean; onClick: () => void }) => (
    <div className={`toggle ${on ? "on" : ""}`} onClick={onClick}>
      <div className="toggle-knob"></div>
    </div>
  );

  const displayName = settings.deviceName || hostname;

  return (
    <div className="settings-container">
      <h1 className="settings-title">Settings</h1>

      <h3 className="settings-section-title">General</h3>
      <div className="settings-card">
        <div className="settings-row" style={{ cursor: "pointer" }} onClick={openRename}>
          <div style={{ display: "flex", flexDirection: "column", gap: "2px" }}>
            <span className="settings-label">Device Name</span>
            <span style={{ fontSize: "12px", color: "var(--text-secondary)" }}>
              {displayName}
            </span>
          </div>
          <Pencil size={16} color="var(--text-secondary)" />
        </div>
        <div className="settings-row">
          <span className="settings-label">Theme</span>
          <select
            className="pill-select"
            value={settings.themeIndex}
            onChange={(e) => updateSettings({ themeIndex: Number(e.target.value) })}
          >
            {themes.map((t, i) => <option key={i} value={i}>{t}</option>)}
          </select>
        </div>
        <div className="settings-row">
          <span className="settings-label">Color</span>
          <select
            className="pill-select"
            value={settings.colorIndex}
            onChange={(e) => updateSettings({ colorIndex: Number(e.target.value) })}
          >
            {colors.map((c, i) => <option key={i} value={i}>{c}</option>)}
          </select>
        </div>
      </div>

      <h3 className="settings-section-title">Receive</h3>
      <div className="settings-card">
        <div className="settings-row">
          <div style={{ display: "flex", flexDirection: "column" }}>
            <span className="settings-label">Auto-accept files</span>
            <span style={{ fontSize: "12px", color: "var(--text-secondary)" }}>
              Automatically receive incoming files
            </span>
          </div>
          <Toggle
            on={settings.receive.autoAccept}
            onClick={() => updateSettings({ receive: { ...settings.receive, autoAccept: !settings.receive.autoAccept } })}
          />
        </div>
        <div className="settings-row">
          <span className="settings-label">Require PIN</span>
          <Toggle
            on={settings.receive.requirePin}
            onClick={() => updateSettings({ receive: { ...settings.receive, requirePin: !settings.receive.requirePin } })}
          />
        </div>
      </div>

      <div className="settings-card">
        <div className="settings-row" style={{ cursor: "pointer" }} onClick={() => navigate("/history")}>
          <span className="settings-label">Transfer History</span>
          <ChevronRight size={18} />
        </div>
      </div>

      <div className="settings-card">
        <div className="settings-row" style={{ cursor: "pointer" }} onClick={() => navigate("/about")}>
          <span className="settings-label">About Plenum</span>
          <ChevronRight size={18} />
        </div>
      </div>

      {renameOpen && (
        <div className="modal-overlay" onClick={() => setRenameOpen(false)}>
          <div className="modal-card" onClick={(e) => e.stopPropagation()}>
            <h2>Rename your device</h2>
            <p className="modal-subtitle">
              Currently: <strong>{displayName}</strong>
            </p>
            <input
              ref={inputRef}
              className="modal-input"
              value={nameDraft}
              placeholder="Name your device"
              onChange={(e) => setNameDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveRename();
                if (e.key === "Escape") setRenameOpen(false);
              }}
            />
            <div className="modal-actions">
              <button className="modal-cancel" onClick={() => setRenameOpen(false)}>
                Cancel
              </button>
              <button className="modal-save" onClick={saveRename}>
                Save
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

export default SettingsPage;
