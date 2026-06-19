import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import ShellLayout from "./shared/ShellLayout";
import EnglishAnnotated from "./modules/english/EnglishAnnotated";
import EnglishAll from "./modules/english/EnglishAll";
import SpeechPage from "./modules/speech/SpeechPage";
import AudioCleanPage from "./modules/audio-clean/AudioCleanPage";
import CookiePage from "./modules/cookie/CookiePage";
import NetPolicyPage from "./modules/net-policy/NetPolicyPage";
import CodeloopPage from "./modules/codeloop/CodeloopPage";
import ChatSummaryPage from "./modules/chat-summary/ChatSummaryPage";
import G10DeployPage from "./modules/g10-deploy/G10DeployPage";
import SettingsPage from "./modules/settings/SettingsPage";
import MusicPage from "./modules/music/MusicPage";
import ScreenshotPage from "./modules/screenshot/ScreenshotPage";
import { MusicPlayerProvider } from "./modules/music/PlayerContext";

export default function App() {
  return (
    <MusicPlayerProvider>
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<ShellLayout />}>
            <Route index element={<Navigate to="/english/annotated" replace />} />
            <Route path="english/annotated" element={<EnglishAnnotated />} />
            <Route path="english/all" element={<EnglishAll />} />
            <Route path="speech" element={<SpeechPage />} />
            <Route path="audio-clean" element={<AudioCleanPage />} />
            <Route path="cookie" element={<CookiePage />} />
            <Route path="net-policy" element={<NetPolicyPage />} />
            <Route path="codeloop" element={<CodeloopPage />} />
            <Route path="chat-summary" element={<ChatSummaryPage />} />
            <Route path="music" element={<MusicPage />} />
            <Route path="screenshot" element={<ScreenshotPage />} />
            <Route path="g10-deploy" element={<G10DeployPage />} />
            <Route path="settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/english/annotated" replace />} />
          </Route>
        </Routes>
      </BrowserRouter>
    </MusicPlayerProvider>
  );
}
