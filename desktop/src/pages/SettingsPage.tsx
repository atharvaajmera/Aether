import React, { useState, useEffect, useRef } from "react";
import { useNavigate } from "react-router-dom";
import { Pencil, Check, ChevronRight } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useSettings } from "../context/SettingsContext";

const SettingsPage: React.FC = () => {
  const { settings, updateSettings } = useSettings();
  const navigate = useNavigate();

  const themes = ["System", "Dark", "Light"];
  const colors = ["Plenum", "Ocean", "Forest"];

  const [hostname, setHostname] = useState<string>("");
  const [editingName, setEditingName] = useState(false);
  const [nameDraft, setNameDraft] = useState(settings.deviceName);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    invoke<string>("get_device_name").then(setHostname).catch(console.error);
  }, []);

  useEffect(() => {
    if (editingName) inputRef.current?.focus();
  }, [editingName]);

  const startEdit = () => {
    setNameDraft(settings.deviceName);
    setEditingName(true);
  };

  const commitName = () => {
    updateSettings({ deviceName: nameDraft.trim() });
    setEditingName(false);
  };

  const Toggle = ({ on, onClick }: { on: boolean; onClick: () => void }) => (
    <div className={`toggle ${on ? "on" : ""}`} onClick={onClick}>
      <div className="toggle-knob"></div>
    </div>
  );

  return (
    <div className="settings-container">
      <h1 className="settings-title">Settings</h1>

      <div className="settings-card">
        <h3 style={{ padding: "16px 24px", fontSize: "14px", fontWeight: 600 }}>General</h3>
        <div className="settings-row">
          <span className="settings-label">Device Name</span>
          {editingName ? (
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <input
                ref={inputRef}
                className="pill-select"
                value={nameDraft}
                placeholder={hostname}
                onChange={(e) => setNameDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") commitName();
                  if (e.key === "Escape") setEditingName(false);
                }}
                style={{ minWidth: "160px" }}
              />
              <button
                className="icon-btn"
                onClick={commitName}
                style={{ background: "none", border: "none", cursor: "pointer", color: "var(--accent-primary)" }}
              >
                <Check size={18} />
              </button>
            </div>
          ) : (
            <div
              style={{ display: "flex", alignItems: "center", gap: "8px", cursor: "pointer" }}
              onClick={startEdit}
            >
              <span style={{ color: "var(--text-secondary)" }}>
                {settings.deviceName || hostname}
              </span>
              <Pencil size={16} />
            </div>
          )}
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

      <div className="settings-card">
        <h3 style={{ padding: "16px 24px", fontSize: "14px", fontWeight: 600 }}>Receive</h3>
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
    </div>
  );
};

export default SettingsPage;
