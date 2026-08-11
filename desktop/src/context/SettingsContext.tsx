import React, { createContext, useContext, useState, useEffect } from "react";

export interface ReceiveSettings {
  autoAccept: boolean; 
  requirePin: boolean;
}

export interface SettingsState {
  themeIndex: number; // 0=System, 1=Dark, 2=Light
  colorIndex: number; // 0=Plenum, 1=Ocean, 2=Forest
  deviceName: string; 
  receive: ReceiveSettings;
}

export interface SettingsContextType {
  settings: SettingsState;
  updateSettings: (newSettings: Partial<SettingsState>) => void;
}

const defaultSettings: SettingsState = {
  themeIndex: 0,
  colorIndex: 0,
  deviceName: "",
  receive: {
    autoAccept: false,
    requirePin: false,
  },
};

const SettingsContext = createContext<SettingsContextType | null>(null);

export const useSettings = () => {
  const context = useContext(SettingsContext);
  if (!context) {
    throw new Error("useSettings must be used within a SettingsProvider");
  }
  return context;
};

export const applyThemeToDom = (settings: SettingsState) => {
  const doc = document.documentElement;

  // Apply Theme
  let isDark = true;
  if (settings.themeIndex === 2) isDark = false; // Light
  else if (settings.themeIndex === 0) { // System
    isDark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  }
  doc.setAttribute("data-theme", isDark ? "dark" : "light");

  // Apply Color
  if (settings.colorIndex === 1) doc.setAttribute("data-color", "ocean");
  else if (settings.colorIndex === 2) doc.setAttribute("data-color", "forest");
  else doc.removeAttribute("data-color"); // Default Plenum
};

export const SettingsProvider: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [settings, setSettings] = useState<SettingsState>(() => {
    try {
      const stored = localStorage.getItem("plenum-settings");
      if (stored) {
        const parsed = JSON.parse(stored);
        return {
          ...defaultSettings,
          themeIndex: parsed.themeIndex ?? defaultSettings.themeIndex,
          colorIndex: parsed.colorIndex ?? defaultSettings.colorIndex,
          deviceName: parsed.deviceName ?? "",
          receive: {
            autoAccept: parsed.receive?.autoAccept ?? parsed.receive?.quickSave ?? false,
            requirePin: parsed.receive?.requirePin ?? false,
          },
        };
      }
      return defaultSettings;
    } catch {
      return defaultSettings;
    }
  });

  // Apply to DOM on initial mount
  useEffect(() => {
    applyThemeToDom(settings);
  }, []);

  // Re-apply on OS theme flips while in System mode (no restart needed).
  useEffect(() => {
    if (settings.themeIndex !== 0) return;
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = () => applyThemeToDom(settings);
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  }, [settings.themeIndex, settings.colorIndex]);

  const updateSettings = (newSettings: Partial<SettingsState>) => {
    setSettings((prev) => {
      const next = { ...prev, ...newSettings };
      localStorage.setItem("plenum-settings", JSON.stringify(next));
      applyThemeToDom(next);
      return next;
    });
  };

  return (
    <SettingsContext.Provider value={{ settings, updateSettings }}>
      {children}
    </SettingsContext.Provider>
  );
};
