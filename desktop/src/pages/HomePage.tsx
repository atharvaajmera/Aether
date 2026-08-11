import React, { useState, useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { Send, Wifi, Settings } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useSettings } from "../context/SettingsContext";

const HomePage: React.FC = () => {
  const navigate = useNavigate();
  const { settings } = useSettings();
  const [deviceName, setDeviceName] = useState<string>("Loading...");
  const [localIp, setLocalIp] = useState<string>("");
  const [username, setUsername] = useState<string>("");

  useEffect(() => {
    invoke<string>("get_device_name")
      .then((name) => setDeviceName(name))
      .catch(console.error);
      
    invoke<string>("get_local_ip")
      .then((ip) => setLocalIp(ip))
      .catch(console.error);

    invoke<string>("get_username")
      .then((user) => setUsername(user))
      .catch(console.error);
  }, []);

  return (
    <div className="home-container">
      <div className="ring-wrapper">
        <div className="segmented-ring"></div>
        <div className="core-circle"></div>
      </div>
      
      <h1 className="device-name">{settings.deviceName || deviceName}</h1>
      <div className="device-id">{localIp} {username ? `• ${username}` : ''}</div>

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
