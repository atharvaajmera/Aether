import React, { useState, useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { Send, Wifi, Settings } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useSettings } from "../context/SettingsContext";

const HomePage: React.FC = () => {
  const navigate = useNavigate();
  const { settings } = useSettings();
  const [deviceName, setDeviceName] = useState<string | null>("Loading...");
  const [localIp, setLocalIp] = useState<string | null>(null);
  const [username, setUsername] = useState<string | null>(null);

  useEffect(() => {
    invoke<string | null>("get_device_name")
      .then((name) => setDeviceName(name))
      .catch(() => setDeviceName(null));

    invoke<string | null>("get_local_ip")
      .then((ip) => setLocalIp(ip))
      .catch(() => setLocalIp(null));

    invoke<string | null>("get_username")
      .then((user) => setUsername(user))
      .catch(() => setUsername(null));
  }, []);

  return (
    <div className="home-container">
      <div className="ring-wrapper">
        <div className="segmented-ring"></div>
        <div className="core-circle"></div>
      </div>
      
      <h1 className="device-name">{settings.deviceName || deviceName || <span style={{ fontStyle: "italic", opacity: 0.6 }}>Unknown device</span>}</h1>
      <div className="device-id">
        {localIp ?? <span style={{ fontStyle: "italic", opacity: 0.6 }}>Network address unavailable</span>}
        {username ? ` • ${username}` : ''}
      </div>

      <div className="nav-buttons-container">
        <div className="nav-buttons-row">
          <button className="big-nav-btn" onClick={() => navigate("/send")}>
            <Send size={24} />
            <span>Send</span>
          </button>
          <button className="big-nav-btn" onClick={() => navigate("/receive")}>
            <Wifi size={24} />
            <span>Receive</span>
          </button>
        </div>
        <button className="big-nav-btn" style={{ margin: "0 auto", width: "100%" }} onClick={() => navigate("/settings")}>
          <Settings size={24} />
          <span>Settings</span>
        </button>
      </div>
    </div>
  );
};

export default HomePage;
